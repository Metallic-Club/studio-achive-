//! Attributes: `SetAttribute`, `GetAttribute`, `GetAttributes`, `GetAttributeChangedSignal` and
//! the `AttributeChanged` event.
//!
//! The values live in the world (`World::set_attribute`), so every VM and the editor see the
//! same ones. Changes reach scripts through the world's attribute log: once per `step` each VM
//! reads what changed since its own cursor and fires the signals. A handler therefore runs in
//! the next step after the change, whoever made it (this VM, the other VM or the editor), like
//! the other signals.
//!
//! `AttributeChanged` is declared on `Instance`. `SignalKey::arg` holds an interned name id: 0 is
//! the instance-wide `AttributeChanged(name)` event, any other id is
//! `GetAttributeChangedSignal(name)`, which fires without arguments.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use metallic_core::classes::SignalKey;
use metallic_core::{
    AttributeChanges, AttributeCursor, CoreError, Value as CoreValue, check_attribute_name,
};
use mlua::{Function, Lua, Table, UserDataRef, Value};

use super::cframe::cframe;
use super::datatypes::{color, udim2, vector};
use super::instance::LuaInstance;
use super::signals::{LuaSignal, plain_key};
use super::{ApiState, errors, lua_error};

/// Changes read from the world's log at a time.
const PAGE: usize = 256;
/// Most distinct attribute names one VM makes signals for.
const MAX_SIGNAL_NAMES: usize = 4096;

/// Per-VM attribute state.
pub(crate) struct AttributeState {
    /// Interned names of `GetAttributeChangedSignal`, 1-based.
    names: RefCell<HashMap<String, u32>>,
    /// Where this VM has read the world's attribute log up to.
    cursor: Cell<AttributeCursor>,
}

impl AttributeState {
    pub(crate) fn new(cursor: AttributeCursor) -> AttributeState {
        AttributeState {
            names: RefCell::new(HashMap::new()),
            cursor: Cell::new(cursor),
        }
    }
}

/// The attribute value of a Luau value, or an error naming what is wrong.
fn to_attribute(value: &Value) -> mlua::Result<CoreValue> {
    Ok(match value {
        Value::Nil => CoreValue::Nil,
        Value::Boolean(b) => CoreValue::Bool(*b),
        Value::Integer(i) => CoreValue::Number(*i as f64),
        Value::Number(n) => CoreValue::Number(*n),
        Value::String(s) => CoreValue::String(String::from_utf8_lossy(&s.as_bytes()).into_owned()),
        Value::UserData(_) => {
            if let Some(v) = vector(value) {
                CoreValue::Vector3(v)
            } else if let Some(c) = color(value) {
                CoreValue::Color3(c)
            } else if let Some(u) = udim2(value) {
                CoreValue::UDim2(u)
            } else if let Some(cf) = cframe(value) {
                CoreValue::CFrame(Box::new(cf))
            } else {
                return Err(unsupported(value));
            }
        }
        other => return Err(unsupported(other)),
    })
}

fn unsupported(value: &Value) -> mlua::Error {
    lua_error(format!(
        "attributes cannot hold a {} value (use a boolean, number, string, Vector3, Color3, UDim2, CFrame or nil)",
        value.type_name()
    ))
}

/// The reason text of a core error, without the property wording.
fn reason(e: CoreError) -> mlua::Error {
    match e {
        CoreError::InvalidValue { property, reason } => {
            lua_error(format!("attribute '{property}': {reason}"))
        }
        other => lua_error(other),
    }
}

/// An instance argument that must be alive.
fn live(this: &UserDataRef<LuaInstance>) -> mlua::Result<()> {
    this.ensure_alive()
}

pub(crate) fn methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    let set = lua.create_function(
        |_, (this, name, value): (UserDataRef<LuaInstance>, String, Value)| {
            live(&this)?;
            let value = to_attribute(&value)?;
            this.state
                .world_mut()?
                .set_attribute(this.id, &name, value)
                .map_err(reason)
        },
    )?;
    let get = lua.create_function(|lua, (this, name): (UserDataRef<LuaInstance>, String)| {
        live(&this)?;
        let value = this.state.world()?.attribute(this.id, &name).cloned();
        match value {
            Some(v) => this.state.value_to_lua(lua, v),
            None => Ok(Value::Nil),
        }
    })?;
    let get_all = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        live(&this)?;
        let all: Vec<(String, CoreValue)> = this
            .state
            .world()?
            .attributes(this.id)
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let table: Table = lua.create_table()?;
        for (k, v) in all {
            table.set(k, this.state.value_to_lua(lua, v)?)?;
        }
        Ok(table)
    })?;
    let signal = lua.create_function(|lua, (this, name): (UserDataRef<LuaInstance>, String)| {
        live(&this)?;
        // Names are validated like SetAttribute's, so a signal cannot be made for a name no
        // attribute can have (and nothing of any length is stored).
        check_attribute_name(&name)
            .map_err(|e| lua_error(format!("invalid attribute name: {e}")))?;
        let id = this.state.attribute_name_id(&name)?;
        lua.create_userdata(LuaSignal {
            state: this.state.clone(),
            key: SignalKey {
                arg: id,
                ..plain_key(this.id, this.state.instance_signals.attribute_changed)
            },
        })
    })?;
    Ok(vec![
        ("SetAttribute", errors::guarded(lua, set)?),
        ("GetAttribute", errors::guarded(lua, get)?),
        ("GetAttributes", errors::guarded(lua, get_all)?),
        ("GetAttributeChangedSignal", errors::guarded(lua, signal)?),
    ])
}

impl ApiState {
    /// The id of `name` for attribute signals, made on first use.
    fn attribute_name_id(&self, name: &str) -> mlua::Result<u32> {
        let mut names = self.attributes.names.borrow_mut();
        if let Some(&id) = names.get(name) {
            return Ok(id);
        }
        if names.len() >= MAX_SIGNAL_NAMES {
            return Err(lua_error("too many different attribute signals"));
        }
        let id = names.len() as u32 + 1;
        names.insert(name.to_string(), id);
        Ok(id)
    }

    /// Where this VM has read the world's attribute log up to.
    pub(crate) fn attribute_cursor(&self) -> AttributeCursor {
        self.attributes.cursor.get()
    }

    /// Fires `AttributeChanged` and `GetAttributeChangedSignal` handlers for what changed in the
    /// world since this VM last looked. Called once per `step`.
    pub(crate) fn deliver_attribute_changes(self: &Rc<Self>) {
        // Only what was logged before this step: changes that handlers make wait for the next.
        let Ok(until) = self.world().map(|w| w.attribute_cursor()) else {
            return;
        };
        loop {
            let (page, cursor) = {
                let Ok(world) = self.world() else { return };
                let cursor = self.attributes.cursor.get();
                let left = cursor.changes_until(until);
                if left == 0 {
                    return;
                }
                match world.attribute_changes_page(cursor, left.min(PAGE)) {
                    // The log is trimmed by its readers, so this needs hundreds of thousands of
                    // unread changes: say so instead of dropping the events silently.
                    AttributeChanges::Overflow => {
                        // Continue with the oldest change the log kept: only the dropped ones
                        // are lost.
                        self.attributes.cursor.set(world.attribute_oldest_cursor());
                        self.sink.warn(
                            "attribute change events were lost: too many unread attribute changes",
                        );
                        return;
                    }
                    AttributeChanges::Changes(page) => (page, cursor),
                }
            };
            let mut handled = 0;
            for change in &page {
                let named = self.attributes.names.borrow().get(&change.name).copied();
                let any = plain_key(change.id, self.instance_signals.attribute_changed);
                let specific = named.map(|arg| SignalKey { arg, ..any });
                let handlers =
                    self.handler_count(any) + specific.map_or(0, |k| self.handler_count(k));
                if !self.lifecycle_room(handlers) {
                    break;
                }
                if handlers > 0 && self.world().is_ok_and(|w| w.is_alive(change.id)) {
                    self.fire(any, &|lua| {
                        Ok(vec![Value::String(lua.create_string(&change.name)?)])
                    });
                    if let Some(key) = specific {
                        self.fire(key, &|_| Ok(Vec::new()));
                    }
                }
                handled += 1;
            }
            self.attributes.cursor.set(cursor.after(handled));
            if handled < page.len() {
                // Out of time or queue room: the rest is delivered in the next steps, in order.
                return;
            }
        }
    }
}

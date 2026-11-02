//! `RemoteEvent`: `FireServer`, `FireClient`, `FireAllClients`.
//!
//! Arguments are copied into a VM-independent [`Wire`] value when fired and rebuilt, one
//! fresh copy per handler, when delivered by the next `step`. Instances travel by
//! reference, everything else by value.

use std::ffi::c_void;

use metallic_core::classes::EnumItemRef;
use metallic_core::{Color3, InstanceId, UDim2, Vec3};
use mlua::{Function, Lua, MultiValue, UserDataRef, Value};
use std::rc::Rc;

use super::datatypes::{
    LuaColor3, LuaUDim2, LuaVector2, LuaVector3, color, udim2, vector, vector2,
};
use super::enums::enum_item_ref;
use super::instance::LuaInstance;
use super::signals::EventKind;
use super::{ApiState, lua_error};
use crate::vm::Context;

/// Most arguments one Fire call may carry.
const MAX_ARGS: usize = 32;
/// Most values (tables and leaves) one Fire call may carry.
const MAX_VALUES: usize = 10_000;
/// Most string bytes one Fire call may carry.
const MAX_STRING_BYTES: usize = 1 << 20;
/// Most messages queued per VM between two deliveries.
const MAX_QUEUED_MESSAGES: usize = 1_000;
/// Most payload bytes queued per VM between two deliveries.
const MAX_QUEUED_BYTES: usize = 16 << 20;
/// Bytes counted for each value besides its string bytes (queue accounting).
const VALUE_COST: usize = 16;

/// Deepest table nesting a remote argument may have.
const MAX_TABLE_DEPTH: usize = 16;

/// A value in transit between the two VMs.
#[derive(Clone, Debug)]
pub(crate) enum Wire {
    Nil,
    Bool(bool),
    Number(f64),
    Str(Vec<u8>),
    Vector3(Vec3),
    Vector2([f64; 2]),
    Color3(Color3),
    UDim2(UDim2),
    Instance(InstanceId),
    /// The receiving VM gives its own item object.
    EnumItem(EnumItemRef),
    Table(Vec<(Wire, Wire)>),
}

/// Who receives a queued remote.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RemoteTarget {
    /// The server VM's `OnServerEvent`, with `sender` as first argument.
    Server { sender: InstanceId },
    /// The client VM's `OnClientEvent`. `player` is the addressed player, `None` for all.
    Client { player: Option<InstanceId> },
}

/// One fired remote waiting for the next `step`.
pub(crate) struct QueuedRemote {
    pub(crate) remote: InstanceId,
    pub(crate) target: RemoteTarget,
    pub(crate) args: Vec<Wire>,
}

/// What one Fire call has copied so far; shared subtables count once per reference, so a
/// payload that expands exponentially hits the limit instead of exhausting memory.
#[derive(Default)]
struct PayloadBudget {
    values: usize,
    string_bytes: usize,
}

impl PayloadBudget {
    fn take(&mut self, string_bytes: usize) -> mlua::Result<()> {
        self.values += 1;
        self.string_bytes += string_bytes;
        if self.values > MAX_VALUES || self.string_bytes > MAX_STRING_BYTES {
            Err(lua_error("remote payload too large"))
        } else {
            Ok(())
        }
    }
}

impl ApiState {
    /// Copies a Luau value into a [`Wire`]. `path` holds the tables being copied.
    fn to_wire(
        &self,
        value: &Value,
        path: &mut Vec<*const c_void>,
        budget: &mut PayloadBudget,
    ) -> mlua::Result<Wire> {
        let string_len = match value {
            Value::String(s) => s.as_bytes().len(),
            _ => 0,
        };
        budget.take(string_len)?;
        Ok(match value {
            Value::Nil => Wire::Nil,
            Value::Boolean(b) => Wire::Bool(*b),
            Value::Number(n) => Wire::Number(*n),
            Value::Integer(i) => Wire::Number(*i as f64),
            Value::String(s) => Wire::Str(s.as_bytes().to_vec()),
            Value::Function(_) => return Err(lua_error("cannot send function")),
            Value::Table(table) => {
                let ptr = table.to_pointer();
                if path.contains(&ptr) {
                    return Err(lua_error("cannot send cyclic table"));
                }
                if path.len() >= MAX_TABLE_DEPTH {
                    return Err(lua_error(format!(
                        "cannot send table nested deeper than {MAX_TABLE_DEPTH} levels"
                    )));
                }
                path.push(ptr);
                let mut pairs = Vec::new();
                for pair in table.pairs::<Value, Value>() {
                    let (k, v) = pair?;
                    pairs.push((
                        self.to_wire(&k, path, budget)?,
                        self.to_wire(&v, path, budget)?,
                    ));
                }
                path.pop();
                Wire::Table(pairs)
            }
            Value::UserData(ud) => {
                if let Some(v) = vector(value) {
                    Wire::Vector3(v)
                } else if let Some(v) = vector2(value) {
                    Wire::Vector2(v)
                } else if let Some(c) = color(value) {
                    Wire::Color3(c)
                } else if let Some(u) = udim2(value) {
                    Wire::UDim2(u)
                } else if let Ok(inst) = ud.borrow::<LuaInstance>() {
                    if !self.world()?.is_alive(inst.id) {
                        return Err(lua_error(format!("{} has been destroyed", inst.name())));
                    }
                    Wire::Instance(inst.id)
                } else if let Some(item) = enum_item_ref(value) {
                    Wire::EnumItem(item)
                } else {
                    return Err(lua_error("cannot send userdata"));
                }
            }
            other => return Err(lua_error(format!("cannot send {}", other.type_name()))),
        })
    }

    /// Rebuilds a [`Wire`] as a fresh Luau value. Destroyed instances arrive as nil.
    fn from_wire(self: &Rc<Self>, lua: &Lua, wire: &Wire) -> mlua::Result<Value> {
        Ok(match wire {
            Wire::Nil => Value::Nil,
            Wire::Bool(b) => Value::Boolean(*b),
            Wire::Number(n) => Value::Number(*n),
            Wire::Str(bytes) => Value::String(lua.create_string(bytes)?),
            Wire::Vector3(v) => Value::UserData(lua.create_userdata(LuaVector3(*v))?),
            Wire::Vector2(v) => Value::UserData(lua.create_userdata(LuaVector2(*v))?),
            Wire::Color3(c) => Value::UserData(lua.create_userdata(LuaColor3(*c))?),
            Wire::UDim2(u) => Value::UserData(lua.create_userdata(LuaUDim2(*u))?),
            Wire::Instance(id) => {
                let alive = self.world()?.is_alive(*id);
                self.wrap_opt(lua, alive.then_some(*id))?
            }
            Wire::EnumItem(item) => self.enum_ref_value(*item),
            Wire::Table(pairs) => {
                let table = lua.create_table()?;
                for (k, v) in pairs {
                    table.raw_set(self.from_wire(lua, k)?, self.from_wire(lua, v)?)?;
                }
                Value::Table(table)
            }
        })
    }

    /// Delivers a queued remote to the handlers connected in this VM.
    pub(crate) fn deliver_remote(self: &Rc<Self>, message: &QueuedRemote) {
        let (kind, sender) = match message.target {
            RemoteTarget::Server { sender } => (EventKind::OnServerEvent, Some(sender)),
            RemoteTarget::Client { .. } => (EventKind::OnClientEvent, None),
        };
        self.fire((message.remote, kind), &|lua| {
            let mut args = Vec::with_capacity(message.args.len() + 1);
            if let Some(sender) = sender {
                args.push(self.wrap_opt(lua, Some(sender))?);
            }
            for wire in &message.args {
                args.push(self.from_wire(lua, wire)?);
            }
            Ok(args)
        });
    }

    fn queue_remote(
        &self,
        remote: InstanceId,
        target: RemoteTarget,
        args: MultiValue,
    ) -> mlua::Result<()> {
        if args.len() > MAX_ARGS {
            return Err(lua_error("remote payload too large"));
        }
        let mut path = Vec::new();
        let mut budget = PayloadBudget::default();
        let args = args
            .iter()
            .map(|v| self.to_wire(v, &mut path, &mut budget))
            .collect::<mlua::Result<Vec<_>>>()?;
        let cost = budget.string_bytes + budget.values * VALUE_COST;
        let slot = usize::from(self.context == Context::Client);
        let mut queued = self.shared.queued.get();
        if queued[slot].0 >= MAX_QUEUED_MESSAGES || queued[slot].1 + cost > MAX_QUEUED_BYTES {
            return Err(lua_error("remote queue full"));
        }
        queued[slot].0 += 1;
        queued[slot].1 += cost;
        self.shared.queued.set(queued);
        self.shared.remotes.borrow_mut().push(QueuedRemote {
            remote,
            target,
            args,
        });
        Ok(())
    }
}

/// The `RemoteEvent` methods.
pub(crate) fn methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    let fire_server =
        lua.create_function(|_, (this, args): (UserDataRef<LuaInstance>, MultiValue)| {
            this.expect_class("RemoteEvent", "FireServer")?;
            if this.state.context != Context::Client {
                return Err(lua_error("FireServer can only be called from the client"));
            }
            let Some(sender) = this.state.shared.local_player.get() else {
                return Err(lua_error("FireServer needs a local player"));
            };
            this.state
                .queue_remote(this.id, RemoteTarget::Server { sender }, args)
        })?;

    let fire_client = lua.create_function(
        |_, (this, player, args): (UserDataRef<LuaInstance>, Value, MultiValue)| {
            this.expect_class("RemoteEvent", "FireClient")?;
            if this.state.context != Context::Server {
                return Err(lua_error("FireClient can only be called from the server"));
            }
            let player = match &player {
                Value::UserData(ud) => ud.borrow::<LuaInstance>().ok().map(|p| p.id),
                _ => None,
            };
            let Some(player) =
                player.filter(|&p| this.state.world().is_ok_and(|w| w.is_a(p, "Player")))
            else {
                return Err(lua_error(
                    "FireClient expects a Player as its first argument",
                ));
            };
            this.state.queue_remote(
                this.id,
                RemoteTarget::Client {
                    player: Some(player),
                },
                args,
            )
        },
    )?;

    let fire_all =
        lua.create_function(|_, (this, args): (UserDataRef<LuaInstance>, MultiValue)| {
            this.expect_class("RemoteEvent", "FireAllClients")?;
            if this.state.context != Context::Server {
                return Err(lua_error(
                    "FireAllClients can only be called from the server",
                ));
            }
            this.state
                .queue_remote(this.id, RemoteTarget::Client { player: None }, args)
        })?;

    Ok(vec![
        ("FireServer", fire_server),
        ("FireClient", fire_client),
        ("FireAllClients", fire_all),
    ])
}

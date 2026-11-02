//! Tags: `CollectionService` and the `AddTag`, `RemoveTag`, `HasTag` and `GetTags` methods of
//! every instance.
//!
//! The tags live in the world (`World::add_tag` and friends), with an index from tag to
//! instances, so `GetTagged` costs what the tagged instances cost and never scans the tree. An
//! instance counts as tagged while it is in the game tree (it descends from a service);
//! `GetInstanceAddedSignal` / `GetInstanceRemovedSignal` follow that, like Roblox: they fire when a
//! tag is added or removed on an instance in the tree, and when a tagged instance enters the
//! tree, leaves it or is destroyed. Delivery works like the attribute signals: once per `step`
//! each VM reads the world's tag log with its own cursor, so a handler runs in the next step
//! whoever made the change.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use metallic_core::classes::SignalKey;
use metallic_core::{CoreError, TagChanges, TagCursor, check_tag};
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, Table, UserData, UserDataFields, UserDataMethods,
    UserDataRef, Value,
};

use super::instance::LuaInstance;
use super::signals::{LuaSignal, TAG_ADDED, TAG_REMOVED, plain_key};
use super::{ApiState, errors, lua_error};

/// Changes read from the world's log at a time.
const PAGE: usize = 256;
/// Most distinct tags one VM makes signals for.
const MAX_SIGNAL_TAGS: usize = 4096;

/// Per-VM tag state.
pub(crate) struct TagState {
    /// Interned tags of the instance signals, 1-based.
    names: RefCell<HashMap<String, u32>>,
    /// Where this VM has read the world's tag log up to.
    cursor: Cell<TagCursor>,
    /// The `CollectionService` object (made once, so `GetService` returns the same one).
    service: OnceCell<AnyUserData>,
}

impl TagState {
    pub(crate) fn new(cursor: TagCursor) -> TagState {
        TagState {
            names: RefCell::new(HashMap::new()),
            cursor: Cell::new(cursor),
            service: OnceCell::new(),
        }
    }
}

fn reason(e: CoreError) -> mlua::Error {
    match e {
        CoreError::InvalidValue { property, reason } => {
            lua_error(format!("tag '{property}': {reason}"))
        }
        other => lua_error(other),
    }
}

/// The instance behind a Luau value, which must be alive.
fn instance(value: &Value, what: &str) -> mlua::Result<(Rc<ApiState>, metallic_core::InstanceId)> {
    let Value::UserData(ud) = value else {
        return Err(lua_error(format!(
            "invalid argument #1 to '{what}' (Instance expected, got {})",
            value.type_name()
        )));
    };
    let inst = ud.borrow::<LuaInstance>().map_err(|_| {
        lua_error(format!(
            "invalid argument #1 to '{what}' (Instance expected, got {})",
            value.type_name()
        ))
    })?;
    inst.ensure_alive()?;
    Ok((inst.state.clone(), inst.id))
}

fn add_tag(state: &ApiState, id: metallic_core::InstanceId, tag: &str) -> mlua::Result<()> {
    state.world_mut()?.add_tag(id, tag).map_err(reason)
}

fn remove_tag(state: &ApiState, id: metallic_core::InstanceId, tag: &str) -> mlua::Result<()> {
    state.world_mut()?.remove_tag(id, tag).map_err(reason)
}

fn has_tag(state: &ApiState, id: metallic_core::InstanceId, tag: &str) -> mlua::Result<bool> {
    Ok(state.world()?.has_tag(id, tag))
}

fn get_tags(lua: &Lua, state: &ApiState, id: metallic_core::InstanceId) -> mlua::Result<Table> {
    let tags: Vec<String> = state.world()?.tags(id).map(str::to_string).collect();
    let table = lua.create_table()?;
    for (i, tag) in tags.iter().enumerate() {
        table.set(i + 1, tag.as_str())?;
    }
    Ok(table)
}

/// The `AddTag`, `RemoveTag`, `HasTag` and `GetTags` instance methods.
pub(crate) fn methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    let add = lua.create_function(|_, (this, tag): (Value, String)| {
        let (state, id) = instance(&this, "AddTag")?;
        add_tag(&state, id, &tag)
    })?;
    let remove = lua.create_function(|_, (this, tag): (Value, String)| {
        let (state, id) = instance(&this, "RemoveTag")?;
        remove_tag(&state, id, &tag)
    })?;
    let has = lua.create_function(|_, (this, tag): (Value, String)| {
        let (state, id) = instance(&this, "HasTag")?;
        has_tag(&state, id, &tag)
    })?;
    let get = lua.create_function(|lua, this: Value| {
        let (state, id) = instance(&this, "GetTags")?;
        get_tags(lua, &state, id)
    })?;
    Ok(vec![
        ("AddTag", errors::guarded(lua, add)?),
        ("RemoveTag", errors::guarded(lua, remove)?),
        ("HasTag", errors::guarded(lua, has)?),
        ("GetTags", errors::guarded(lua, get)?),
    ])
}

/// The Luau `CollectionService`.
pub(crate) struct LuaCollectionService {
    state: Rc<ApiState>,
    methods: HashMap<&'static str, Function>,
}

impl LuaCollectionService {
    fn new(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<Self> {
        let mut methods: HashMap<&'static str, Function> = HashMap::new();
        // Every method gets the service first (`CollectionService:AddTag(part, "Lava")`).
        methods.insert(
            "AddTag",
            errors::guarded_fn(lua, |_, (_, this, tag): (Value, Value, String)| {
                let (state, id) = instance(&this, "AddTag")?;
                add_tag(&state, id, &tag)
            })?,
        );
        methods.insert(
            "RemoveTag",
            errors::guarded_fn(lua, |_, (_, this, tag): (Value, Value, String)| {
                let (state, id) = instance(&this, "RemoveTag")?;
                remove_tag(&state, id, &tag)
            })?,
        );
        methods.insert(
            "HasTag",
            errors::guarded_fn(lua, |_, (_, this, tag): (Value, Value, String)| {
                let (state, id) = instance(&this, "HasTag")?;
                has_tag(&state, id, &tag)
            })?,
        );
        methods.insert(
            "GetTags",
            errors::guarded_fn(lua, |lua, (_, this): (Value, Value)| {
                let (state, id) = instance(&this, "GetTags")?;
                get_tags(lua, &state, id)
            })?,
        );
        methods.insert(
            "GetTagged",
            errors::guarded_fn(
                lua,
                |lua, (this, tag): (UserDataRef<LuaCollectionService>, String)| {
                    let ids = this.state.world()?.tagged(&tag);
                    let table = lua.create_table()?;
                    for (i, id) in ids.into_iter().enumerate() {
                        table.set(i + 1, this.state.wrap(lua, id)?)?;
                    }
                    Ok(table)
                },
            )?,
        );
        methods.insert(
            "GetAllTags",
            errors::guarded_fn(lua, |lua, this: UserDataRef<LuaCollectionService>| {
                let tags: Vec<String> = this
                    .state
                    .world()?
                    .all_tags()
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                let table = lua.create_table()?;
                for (i, tag) in tags.iter().enumerate() {
                    table.set(i + 1, tag.as_str())?;
                }
                Ok(table)
            })?,
        );
        for (name, added) in [
            ("GetInstanceAddedSignal", true),
            ("GetInstanceRemovedSignal", false),
        ] {
            methods.insert(
                name,
                errors::guarded_fn(
                    lua,
                    move |lua, (this, tag): (UserDataRef<LuaCollectionService>, String)| {
                        let id = this.state.tag_name_id(&tag)?;
                        let signal = if added { TAG_ADDED } else { TAG_REMOVED };
                        let carrier = this
                            .state
                            .world()?
                            .root("RunService")
                            .ok_or_else(|| lua_error("the world has no RunService"))?;
                        lua.create_userdata(LuaSignal {
                            state: this.state.clone(),
                            key: SignalKey {
                                arg: id,
                                ..plain_key(carrier, signal)
                            },
                        })
                    },
                )?,
            );
        }
        Ok(LuaCollectionService {
            state: state.clone(),
            methods,
        })
    }
}

impl UserData for LuaCollectionService {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Instance");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(
                lua,
                |lua, (this, key): (UserDataRef<LuaCollectionService>, String)| match key.as_str() {
                    "Name" | "ClassName" => {
                        Ok(Value::String(lua.create_string("CollectionService")?))
                    }
                    other => this
                        .methods
                        .get(other)
                        .map(|f| Value::Function(f.clone()))
                        .ok_or_else(|| {
                            lua_error(format!(
                                "{other} is not a valid member of CollectionService"
                            ))
                        }),
                },
            )
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("CollectionService".to_string())
        });
    }
}

impl ApiState {
    /// The id of `tag` for instance signals, made on first use.
    fn tag_name_id(&self, tag: &str) -> mlua::Result<u32> {
        // Validated like AddTag's argument, so no signal is made for a tag nothing can carry.
        check_tag(tag).map_err(|e| lua_error(format!("invalid tag: {e}")))?;
        let mut names = self.tags.names.borrow_mut();
        if let Some(&id) = names.get(tag) {
            return Ok(id);
        }
        if names.len() >= MAX_SIGNAL_TAGS {
            return Err(lua_error("too many different tag signals"));
        }
        let id = names.len() as u32 + 1;
        names.insert(tag.to_string(), id);
        Ok(id)
    }

    /// Where this VM has read the world's tag log up to.
    pub(crate) fn tag_cursor(&self) -> TagCursor {
        self.tags.cursor.get()
    }

    /// Fires the tag signals for what changed in the world since this VM last looked. Called
    /// once per `step`.
    pub(crate) fn deliver_tag_changes(self: &Rc<Self>) {
        // Only what was logged before this step: changes that handlers make wait for the next.
        let Ok(until) = self.world().map(|w| w.tag_cursor()) else {
            return;
        };
        loop {
            let (page, cursor, carrier) = {
                let Ok(world) = self.world() else { return };
                let cursor = self.tags.cursor.get();
                let left = cursor.changes_until(until);
                if left == 0 {
                    return;
                }
                let Some(carrier) = world.root("RunService") else {
                    return;
                };
                match world.tag_changes_page(cursor, left.min(PAGE)) {
                    // The log is trimmed by its readers, so this needs hundreds of thousands of
                    // unread changes: say so instead of dropping the events silently.
                    TagChanges::Overflow => {
                        // Continue with the oldest change the log kept: only the dropped ones
                        // are lost.
                        self.tags.cursor.set(world.tag_oldest_cursor());
                        self.sink
                            .warn("tag events were lost: too many unread tag changes");
                        return;
                    }
                    TagChanges::Changes(page) => (page, cursor, carrier),
                }
            };
            let mut handled = 0;
            for change in &page {
                let named = self.tags.names.borrow().get(&change.tag).copied();
                if let Some(name) = named {
                    let signal = if change.added { TAG_ADDED } else { TAG_REMOVED };
                    let key = SignalKey {
                        arg: name,
                        ..plain_key(carrier, signal)
                    };
                    if !self.lifecycle_room(self.handler_count(key)) {
                        break;
                    }
                    // A removal is delivered with the destroyed instance too; an addition only
                    // while the instance still exists.
                    let skip = change.added && !self.world().is_ok_and(|w| w.is_alive(change.id));
                    if !skip && self.handler_count(key) > 0 {
                        self.fire(key, &|lua| {
                            Ok(vec![Value::UserData(self.wrap(lua, change.id)?)])
                        });
                    }
                }
                handled += 1;
            }
            self.tags.cursor.set(cursor.after(handled));
            if handled < page.len() {
                // Out of time or queue room: the rest is delivered in the next steps, in order.
                return;
            }
        }
    }
}

/// Creates the `CollectionService` object of `state`.
pub(crate) fn install_service(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<()> {
    let service = lua.create_userdata(LuaCollectionService::new(lua, state)?)?;
    let _ = state.tags.service.set(service);
    Ok(())
}

/// `CollectionService`, when `name` asks for it.
pub(crate) fn service(state: &ApiState, name: &str) -> Option<Value> {
    if name != "CollectionService" {
        return None;
    }
    state.tags.service.get().map(|s| Value::UserData(s.clone()))
}

//! The Luau `Instance`: a light userdata around an [`InstanceId`], member lookup through the class
//! registry and [`HostApi`], `Instance.new` and `WaitForChild`.
//!
//! Lookup order, for a name that is not one of the arms still waiting for #463 and #464: property,
//! method, signal, callback, child by name. Names are unique across a class's members, so one
//! table lookup covers the first four. See `api/host_api.rs` for the host side.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::ffi::CStr;
use std::rc::Rc;

use metallic_core::classes::{
    ApiError, ApiErrorKind, HostApi, MemberKind, ResolvedClass, is_subclass,
};
use metallic_core::transform;
use metallic_core::{CoreError, InstanceId, Value as CoreValue};
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, MultiValue, Table, Thread, UserData, UserDataFields,
    UserDataMethods, UserDataRef, Value,
};

use super::cframe::{LuaCFrame, cframe};
use super::datatypes::lua_to_value;
use super::enums::enum_write;
use super::errors;
use super::host_api::{LuauHost, method_function};
use super::signals::{EventKind, LuaSignal};
use super::{ApiState, lua_error};
use crate::vm::format_error;

/// How long `WaitForChild` without a timeout waits before it warns (like Roblox).
const INFINITE_YIELD_WARN_SEC: f64 = 5.0;

/// The event named `name` on an instance of `class`, if there is one.
fn event_kind(class: &str, name: &str) -> Option<EventKind> {
    match name {
        "AttributeChanged" => Some(EventKind::AttributeChanged(0)),
        "Touched" if is_subclass(class, "BasePart") => Some(EventKind::Touched),
        "TouchEnded" if is_subclass(class, "BasePart") => Some(EventKind::TouchEnded),
        "Died" if class == "Humanoid" => Some(EventKind::Died),
        "CharacterAdded" if class == "Player" => Some(EventKind::CharacterAdded),
        "PlayerAdded" if class == "Players" => Some(EventKind::PlayerAdded),
        "PlayerRemoving" if class == "Players" => Some(EventKind::PlayerRemoving),
        "Heartbeat" if class == "RunService" => Some(EventKind::Heartbeat),
        "OnServerEvent" if class == "RemoteEvent" => Some(EventKind::OnServerEvent),
        "OnClientEvent" if class == "RemoteEvent" => Some(EventKind::OnClientEvent),
        "Pressed" if class == "InputAction" => Some(EventKind::Pressed),
        "Released" if class == "InputAction" => Some(EventKind::Released),
        "Triggered" if class == "InputAction" => Some(EventKind::Triggered),
        "StateChanged" if class == "InputAction" => Some(EventKind::StateChanged),
        _ => super::gui::GuiEventKind::of(class, name).map(EventKind::Gui),
    }
}

/// A thread parked in `WaitForChild`.
pub(crate) struct Waiter {
    pub(crate) thread: Thread,
    parent: InstanceId,
    name: String,
    started: f64,
    deadline: Option<f64>,
    warned: bool,
}

impl Waiter {
    /// `thread`, waiting from `started` for a child of `parent` named `name`, at most `timeout`
    /// seconds when given.
    pub(crate) fn new(
        thread: Thread,
        parent: InstanceId,
        name: String,
        started: f64,
        timeout: Option<f64>,
    ) -> Waiter {
        Waiter {
            thread,
            parent,
            name,
            started,
            deadline: timeout.map(|t| started + t.max(0.0)),
            warned: false,
        }
    }
}

/// The Luau value of one instance. Cheap: an id plus the VM's shared state.
pub(crate) struct LuaInstance {
    pub(crate) id: InstanceId,
    pub(crate) state: Rc<ApiState>,
    /// Last known name, so a destroyed instance can still be named in errors.
    name_hint: RefCell<String>,
}

fn core_error(e: CoreError) -> mlua::Error {
    lua_error(e)
}

impl LuaInstance {
    /// Errors when the instance has been destroyed.
    pub(crate) fn ensure_alive(&self) -> mlua::Result<()> {
        self.live()
    }

    fn live(&self) -> mlua::Result<()> {
        if self.state.world()?.is_alive(self.id) {
            Ok(())
        } else {
            Err(lua_error(format!(
                "{} has been destroyed",
                self.name_hint.borrow()
            )))
        }
    }

    fn class(&self) -> mlua::Result<&'static str> {
        self.state
            .world()?
            .class_name(self.id)
            .ok_or_else(|| lua_error(format!("{} has been destroyed", self.name_hint.borrow())))
    }

    /// The Luau error of a host error. Core has no name for a destroyed instance, so this gives it
    /// the receiver's last known name, or the name of the destroyed `argument` when that is what
    /// the host refused.
    pub(crate) fn host_error(&self, error: ApiError, argument: Option<String>) -> mlua::Error {
        match error.kind {
            ApiErrorKind::Destroyed { .. } => {
                let name = match argument {
                    Some(name) if self.live().is_ok() => name,
                    _ => self.name_hint.borrow().clone(),
                };
                lua_error(format!("{name} has been destroyed"))
            }
            _ => lua_error(error),
        }
    }

    /// Errors unless this instance is (a subclass of) `class`.
    pub(crate) fn expect_class(&self, class: &str, method: &str) -> mlua::Result<()> {
        let actual = self.class()?;
        if is_subclass(actual, class) {
            Ok(())
        } else {
            Err(not_a_member(method, actual))
        }
    }

    /// The current name, or the last known one once destroyed.
    pub(crate) fn name(&self) -> String {
        match self.state.world() {
            Ok(w) if w.is_alive(self.id) => match w.get(self.id, "Name") {
                Ok(CoreValue::String(s)) => s,
                _ => self.name_hint.borrow().clone(),
            },
            _ => self.name_hint.borrow().clone(),
        }
    }
}

fn not_a_member(member: &str, class: &str) -> mlua::Error {
    lua_error(format!("{member} is not a valid member of {class}"))
}

impl ApiState {
    /// The Luau object of `id`; asking twice gives the same object.
    pub(crate) fn wrap(self: &Rc<Self>, lua: &Lua, id: InstanceId) -> mlua::Result<AnyUserData> {
        // Keyed by the slot index (exact in a Lua number); the stored id tells generations apart.
        let slot = (id.to_bits() & 0xffff_ffff) as f64;
        if let Value::UserData(ud) = self.cache.get::<Value>(slot)?
            && ud.borrow::<LuaInstance>().is_ok_and(|i| i.id == id)
        {
            return Ok(ud);
        }
        let name = match self.world()?.get(id, "Name") {
            Ok(CoreValue::String(s)) => s,
            _ => String::new(),
        };
        let ud = lua.create_userdata(LuaInstance {
            id,
            state: self.clone(),
            name_hint: RefCell::new(name),
        })?;
        self.cache.set(slot, ud.clone())?;
        Ok(ud)
    }

    /// Like [`ApiState::wrap`], nil for `None`.
    pub(crate) fn wrap_opt(
        self: &Rc<Self>,
        lua: &Lua,
        id: Option<InstanceId>,
    ) -> mlua::Result<Value> {
        match id {
            Some(id) => Ok(Value::UserData(self.wrap(lua, id)?)),
            None => Ok(Value::Nil),
        }
    }

    /// Resumes `WaitForChild` threads whose child appeared or whose timeout passed, and
    /// warns once about waits without a timeout that last too long.
    pub(crate) fn wake_waiters(self: &Rc<Self>) {
        let waiting: Vec<Waiter> = std::mem::take(&mut *self.waiters.borrow_mut());
        if waiting.is_empty() {
            return;
        }
        let Some(lua) = self.lua.try_upgrade() else {
            return;
        };
        let now = self.now.get();
        let mut still_waiting = Vec::new();
        for mut waiter in waiting {
            let found = match self.world() {
                Ok(w) if !w.is_alive(waiter.parent) => continue,
                Ok(w) => w.find_first_child(waiter.parent, &waiter.name),
                Err(_) => {
                    still_waiting.push(waiter);
                    continue;
                }
            };
            let answer = match found {
                Some(child) => self.wrap(&lua, child).map(Value::UserData),
                None if waiter.deadline.is_some_and(|d| now >= d) => Ok(Value::Nil),
                None => {
                    if waiter.deadline.is_none()
                        && !waiter.warned
                        && now - waiter.started >= INFINITE_YIELD_WARN_SEC
                    {
                        waiter.warned = true;
                        let owner = self
                            .world()
                            .map(|w| w.full_name(waiter.parent))
                            .unwrap_or_default();
                        self.sink.warn(&format!(
                            "Infinite yield possible on '{owner}:WaitForChild(\"{}\")'",
                            waiter.name
                        ));
                    }
                    still_waiting.push(waiter);
                    continue;
                }
            };
            let result = answer
                .map_err(|e| crate::vm::ScriptError {
                    message: format_error(&e),
                })
                .and_then(|v| self.budget.resume(&waiter.thread, v));
            if let Err(e) = result {
                self.sink.error(&e.message);
            }
        }
        // Waits registered while threads resumed are already in the list.
        self.waiters.borrow_mut().extend(still_waiting);
    }
}

impl UserData for LuaInstance {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Instance");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(
                lua,
                |lua, (this, key): (UserDataRef<LuaInstance>, Value)| index(lua, &this, key),
            )
        });
        fields.add_meta_field_with("__namecall", namecall_metamethod);
        fields.add_meta_field_with(MetaMethod::NewIndex, |lua| {
            errors::guarded_fn(
                lua,
                |_, (this, key, value): (UserDataRef<LuaInstance>, Value, Value)| {
                    new_index(&this, key, &value)
                },
            )
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.name()));
    }
}

/// Owns the Lua string guard for the entire member lookup. Invalid UTF-8 keeps
/// the existing replacement-character lookup/error behavior via an owned fallback.
enum MemberName {
    Borrowed(mlua::BorrowedStr),
    Lossy(String),
}

impl AsRef<str> for MemberName {
    fn as_ref(&self) -> &str {
        match self {
            Self::Borrowed(value) => value.as_ref(),
            Self::Lossy(value) => value.as_str(),
        }
    }
}

fn member_name(key: &Value) -> mlua::Result<MemberName> {
    match key {
        Value::String(s) => {
            // Diagnostic-only switch lets one warmed VM alternate the exact old
            // conversion and the proposal; no branch is present in production.
            #[cfg(test)]
            if read_probe::owned_baseline() {
                return Ok(MemberName::Lossy(
                    String::from_utf8_lossy(&s.as_bytes()).into_owned(),
                ));
            }
            Ok(match s.to_str() {
                Ok(value) => MemberName::Borrowed(value),
                Err(_) => MemberName::Lossy(String::from_utf8_lossy(&s.as_bytes()).into_owned()),
            })
        }
        other => Err(lua_error(format!(
            "an Instance member name must be a string, got {}",
            other.type_name()
        ))),
    }
}

#[cfg(test)]
mod lookup_tests;
#[cfg(test)]
mod read_probe;

/// `__namecall`: `obj:Method(...)` without the `__index` metamethod and its error guard.
///
/// Written in Luau because a method may yield, which a Rust caller would not allow. `resolve` reads
/// the method name the VM left for this call and never raises: anything that is not a function
/// comes back as a function that raises what `__index` plus the call would have.
fn namecall_metamethod(lua: &Lua) -> mlua::Result<Function> {
    let resolve =
        lua.create_function(|lua, this: UserDataRef<LuaInstance>| namecall_target(lua, &this))?;
    lua.load("local resolve = ...\nreturn function(self, ...) return resolve(self)(self, ...) end")
        .set_name("=[engine]")
        .call(resolve)
}

fn namecall_target(lua: &Lua, this: &LuaInstance) -> mlua::Result<Function> {
    // SAFETY: this runs in the C function the namecall's own frame just called, on the thread
    // that made it, so the pointer is that call's interned method name (or null, handled below),
    // and the string stays alive in the calling function's constants.
    let name = unsafe {
        let name = mlua::ffi::lua_namecallatom(lua.state(), std::ptr::null_mut());
        (!name.is_null()).then(|| CStr::from_ptr(name))
    };
    let name = name.map_or(Cow::Borrowed(""), CStr::to_string_lossy);
    let error = match index_member(lua, this, &name) {
        Ok(Value::Function(function)) => return Ok(function),
        // The texts the VM has for a nil method and for calling any other value.
        Ok(Value::Nil) => lua_error(format!(
            "attempt to call missing method '{name}' of Instance"
        )),
        Ok(other) => lua_error(format!(
            "attempt to call a {} value",
            luau_type_name(&other)
        )),
        Err(error) => error,
    };
    errors::guarded_fn(lua, move |_, _: MultiValue| -> mlua::Result<()> {
        Err(error.clone())
    })
}

/// What the VM calls a value in an error: the `__type` name of a userdata.
fn luau_type_name(value: &Value) -> String {
    match value {
        Value::UserData(ud) => ud.type_name().map_or_else(
            |_| "userdata".to_string(),
            |name| name.to_string_lossy().to_string(),
        ),
        Value::Integer(_) => "number".to_string(),
        other => other.type_name().to_string(),
    }
}

fn index(lua: &Lua, this: &LuaInstance, key: Value) -> mlua::Result<Value> {
    let member = member_name(&key)?;
    index_member(lua, this, member.as_ref())
}

fn index_member(lua: &Lua, this: &LuaInstance, key: &str) -> mlua::Result<Value> {
    let state = &this.state;
    let mut host = state.host();
    let resolved = host
        .own_class(this.id)
        .map_err(|e| this.host_error(e, None))?;
    let class = resolved.name();
    match key {
        "ClassName" => return Ok(Value::String(lua.create_string(class)?)),
        "Parent" => {
            let parent = state.world()?.parent(this.id);
            return state.wrap_opt(lua, parent);
        }
        "CFrame" if is_subclass(class, "BasePart") => {
            let cf = super::cframe::part_cframe(&*state.world()?, this.id).map_err(core_error)?;
            return Ok(Value::UserData(lua.create_userdata(LuaCFrame(cf))?));
        }
        "WorldPivot" if is_subclass(class, "Model") => {
            let cf = transform::world_pivot(&*state.world()?, this.id).map_err(core_error)?;
            return Ok(Value::UserData(lua.create_userdata(LuaCFrame(cf))?));
        }
        "CurrentCamera" if class == "Workspace" => {
            let camera = super::camera::current_camera(&*state.world()?);
            return state.wrap_opt(lua, camera);
        }
        "ViewportSize" if class == "Camera" => {
            let [w, h] = state.shared.viewport_size();
            return Ok(Value::UserData(
                lua.create_userdata(super::datatypes::LuaVector2([w, h]))?,
            ));
        }
        "LocalPlayer" if class == "Players" => {
            let player = match state.context {
                crate::vm::Context::Client => state.shared.local_player.get(),
                crate::vm::Context::Server => None,
            };
            return state.wrap_opt(lua, player);
        }
        _ => {}
    }
    state.check_input_member(class, key)?;
    let member = host
        .resolve_member(resolved.id, key)
        .and_then(|id| Some((id, resolved.members.get(usize::from(id.0))?)))
        .filter(|(_, member)| !member.hidden);
    if let Some((id, member)) = member {
        match member.kind {
            MemberKind::Property { .. } => {
                let value = host
                    .get_property(this.id, resolved.id, id)
                    .map_err(|e| this.host_error(e, None))?;
                return state.script_to_lua(lua, value);
            }
            MemberKind::Method { desc, decl } => {
                let function = method_function(lua, state, desc, decl, member.owner, id)?;
                return Ok(Value::Function(function));
            }
            // Connects to the hub; #463 fires it and moves connecting behind `HostApi`.
            MemberKind::Signal { decl, .. } => {
                return Ok(Value::UserData(lua.create_userdata(LuaSignal {
                    state: state.clone(),
                    key: (this.id, EventKind::Declared(decl.0)),
                })?));
            }
            MemberKind::Callback { desc, .. } => {
                let kind = ApiErrorKind::CallbackRead;
                return Err(lua_error(ApiError::new(class, desc.name, kind)));
            }
        }
    }
    if let Some(method) = state.bridge.method(resolved, key) {
        return Ok(Value::Function(method.clone()));
    }
    if let Some(kind) = event_kind(class, key) {
        use crate::vm::Context;
        match (kind, state.context) {
            (EventKind::OnServerEvent, Context::Client) => {
                return Err(lua_error("OnServerEvent can only be used from the server"));
            }
            (EventKind::OnClientEvent, Context::Server) => {
                return Err(lua_error("OnClientEvent can only be used from the client"));
            }
            _ => {}
        }
        return Ok(Value::UserData(lua.create_userdata(LuaSignal {
            state: state.clone(),
            key: (this.id, kind),
        })?));
    }
    let child = state.world()?.find_first_child(this.id, key);
    match child {
        Some(child) => Ok(Value::UserData(state.wrap(lua, child)?)),
        None => Err(not_a_member(key, class)),
    }
}

fn new_index(this: &LuaInstance, key: Value, value: &Value) -> mlua::Result<()> {
    let member = member_name(&key)?;
    let key = member.as_ref();
    let mut host = this.state.host();
    let resolved = host
        .own_class(this.id)
        .map_err(|e| this.host_error(e, None))?;
    let class = resolved.name();
    match key {
        "ClassName" => Err(lua_error("ClassName is read-only")),
        "CurrentCamera" if class == "Workspace" => Err(lua_error("CurrentCamera is read-only")),
        "ViewportSize" if class == "Camera" => Err(lua_error("ViewportSize is read-only")),
        "Parent" => set_parent(this, value),
        "CFrame" if is_subclass(class, "BasePart") => {
            let cf = cframe(value).ok_or_else(|| {
                lua_error(format!(
                    "invalid value for property CFrame: expected CFrame, got {}",
                    value.type_name()
                ))
            })?;
            super::cframe::set_part_cframe(&mut *this.state.world_mut()?, this.id, cf)
                .map_err(core_error)
        }
        "WorldPivot" if is_subclass(class, "Model") => {
            let cf = cframe(value).ok_or_else(|| {
                lua_error(format!(
                    "invalid value for property WorldPivot: expected CFrame, got {}",
                    value.type_name()
                ))
            })?;
            transform::set_world_pivot(&mut *this.state.world_mut()?, this.id, cf)
                .map_err(core_error)
        }
        _ => set_member(this, &mut host, resolved, key, value),
    }
}

/// Writes a registry member: a property or a callback. Anything else that exists is read-only.
fn set_member(
    this: &LuaInstance,
    host: &mut LuauHost<'_>,
    class: &ResolvedClass,
    key: &str,
    value: &Value,
) -> mlua::Result<()> {
    let member = host
        .resolve_member(class.id, key)
        .and_then(|id| Some((id, class.members.get(usize::from(id.0))?)))
        .filter(|(_, member)| !member.hidden);
    let Some((id, member)) = member else {
        return if this.state.bridge.function(key).is_some()
            || event_kind(class.name(), key).is_some()
        {
            Err(lua_error(format!("{key} is read-only")))
        } else {
            Err(not_a_member(key, class.name()))
        };
    };
    let host_error = |e| this.host_error(e, None);
    match member.kind {
        MemberKind::Property { desc, .. } => {
            let mut converted = match desc.enum_type {
                Some(enum_desc) => enum_write(value, enum_desc, key)?,
                None => lua_to_value(value, desc.ty, key)?,
            };
            // Like Roblox, a part's Size is clamped to 0.001..=2048 studs per axis.
            if key == "Size"
                && let CoreValue::Vector3(size) = converted
                && class.is_a_named("BasePart")
            {
                converted = CoreValue::Vector3(transform::clamp_part_size(size));
            }
            host.set_property(this.id, class.id, id, converted.into())
                .map_err(host_error)?;
            if key == "Name"
                && let Value::String(s) = value
            {
                *this.name_hint.borrow_mut() = String::from_utf8_lossy(&s.as_bytes()).into_owned();
            }
            Ok(())
        }
        MemberKind::Callback { .. } => {
            let functions = &this.state.functions;
            let result = match value {
                Value::Nil => host.set_callback(this.id, class.id, id, None),
                Value::Function(f) => {
                    let mark = functions.mark();
                    let reference = functions.add(f.clone());
                    let result = host.set_callback(this.id, class.id, id, Some(reference));
                    functions.release_since(mark);
                    result
                }
                other => {
                    return Err(lua_error(format!(
                        "invalid value for callback {key}: expected function, got {}",
                        other.type_name()
                    )));
                }
            };
            result.map_err(host_error)
        }
        MemberKind::Method { .. } | MemberKind::Signal { .. } => {
            Err(lua_error(format!("{key} is read-only")))
        }
    }
}

fn set_parent(this: &LuaInstance, value: &Value) -> mlua::Result<()> {
    let parent = match value {
        Value::Nil => None,
        Value::UserData(ud) => match ud.borrow::<LuaInstance>() {
            Ok(p) => {
                p.live()?;
                Some(p.id)
            }
            Err(_) => return Err(bad_parent(value)),
        },
        other => return Err(bad_parent(other)),
    };
    let mut world = this.state.world_mut()?;
    if world.is_builtin(this.id) {
        return Err(lua_error(format!(
            "The Parent property of {} is locked",
            this.name()
        )));
    }
    world.set_parent(this.id, parent).map_err(core_error)
}

fn bad_parent(value: &Value) -> mlua::Error {
    lua_error(format!(
        "invalid value for property Parent: expected Instance, got {}",
        value.type_name()
    ))
}

/// Methods every instance has, and the Lua-side `WaitForChild`.
pub(crate) fn methods(
    lua: &Lua,
    waiters: &Rc<RefCell<Vec<Waiter>>>,
    now: &Rc<Cell<f64>>,
) -> mlua::Result<Vec<(&'static str, Function)>> {
    let find = lua.create_function(
        |lua, (this, name, recursive): (UserDataRef<LuaInstance>, String, Option<bool>)| {
            this.live()?;
            let world = this.state.world()?;
            let found = if recursive == Some(true) {
                descendants(&world, this.id).into_iter().find(
                    |&d| matches!(world.get(d, "Name"), Ok(CoreValue::String(ref s)) if *s == name),
                )
            } else {
                world.find_first_child(this.id, &name)
            };
            drop(world);
            this.state.wrap_opt(lua, found)
        },
    )?;
    let children = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        this.live()?;
        let ids = this.state.world()?.children(this.id).to_vec();
        id_list(lua, &this.state, &ids)
    })?;
    let all_descendants = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        this.live()?;
        let ids = {
            let world = this.state.world()?;
            descendants(&world, this.id)
        };
        id_list(lua, &this.state, &ids)
    })?;
    let is_a = lua.create_function(|_, (this, class): (UserDataRef<LuaInstance>, String)| {
        this.live()?;
        Ok(this.state.world()?.is_a(this.id, &class))
    })?;
    let destroy = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        let mut world = this.state.world_mut()?;
        if !world.is_alive(this.id) {
            return Ok(());
        }
        if world.is_builtin(this.id) {
            return Err(lua_error(format!("{} cannot be destroyed", this.name())));
        }
        if let Ok(CoreValue::String(s)) = world.get(this.id, "Name") {
            *this.name_hint.borrow_mut() = s;
        }
        world.destroy(this.id);
        Ok(())
    })?;
    let clone = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        this.live()?;
        let copy = {
            let mut world = this.state.world_mut()?;
            if world.is_builtin(this.id) {
                return Err(lua_error(format!("{} cannot be cloned", this.name())));
            }
            world.clone_subtree(this.id).map_err(core_error)?
        };
        this.state.wrap(lua, copy)
    })?;

    let (waiters, clock) = (waiters.clone(), now.clone());
    let register = lua.create_function(
        move |_,
              (this, name, timeout, thread): (
            UserDataRef<LuaInstance>,
            String,
            Option<f64>,
            Thread,
        )| {
            this.live()?;
            let waiter = Waiter::new(thread, this.id, name, clock.get(), timeout);
            waiters.borrow_mut().push(waiter);
            Ok(())
        },
    )?;
    let coroutine: Table = lua.globals().get("coroutine")?;
    // Written in Luau because only Luau code can yield its own thread.
    let wait_for_child: Function = lua
        .load(
            r#"
local register, yield, running = ...
return function(inst, name, timeout)
    local found = inst:FindFirstChild(name)
    if found ~= nil then return found end
    register(inst, name, timeout, running())
    return yield()
end
"#,
        )
        .set_name("=[engine]")
        .call((
            errors::guarded(lua, register)?,
            coroutine.get::<Function>("yield")?,
            coroutine.get::<Function>("running")?,
        ))?;

    Ok(vec![
        ("FindFirstChild", find),
        ("GetChildren", children),
        ("GetDescendants", all_descendants),
        ("IsA", is_a),
        ("Destroy", destroy),
        ("Clone", clone),
        ("WaitForChild", wait_for_child),
    ])
}

/// All descendants of `id`, parents before their children, in tree order.
fn descendants(world: &metallic_core::World, id: InstanceId) -> Vec<InstanceId> {
    let mut out = Vec::new();
    let mut stack: Vec<InstanceId> = world.children(id).iter().rev().copied().collect();
    while let Some(cur) = stack.pop() {
        out.push(cur);
        stack.extend(world.children(cur).iter().rev().copied());
    }
    out
}

fn id_list(lua: &Lua, state: &Rc<ApiState>, ids: &[InstanceId]) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    for (i, &id) in ids.iter().enumerate() {
        table.set(i + 1, state.wrap(lua, id)?)?;
    }
    Ok(table)
}

/// The global `Instance` table (`Instance.new`).
pub(crate) fn constructor(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<Table> {
    let table = lua.create_table()?;
    let state = state.clone();
    table.set(
        "new",
        errors::guarded_fn(lua, move |lua, (class, parent): (String, Option<Value>)| {
            // Unknown classes are refused after the parent check, as before.
            let mut host = state.host();
            let resolved = host.resolve_class(&class);
            let refused = match resolved {
                Some(id) => state
                    .binding
                    .classes
                    .registry
                    .class(id)
                    .is_none_or(|known| !known.desc.is_creatable()),
                None => super::services::is_host_object(&state, &class),
            };
            if refused {
                return Err(lua_error(format!(
                    "Unable to create an Instance of type \"{class}\""
                )));
            }
            let parent = match parent {
                None | Some(Value::Nil) => None,
                Some(Value::UserData(ud)) => match ud.borrow::<LuaInstance>() {
                    Ok(p) => {
                        p.live()?;
                        Some(p.id)
                    }
                    Err(_) => return Err(lua_error("Instance.new: parent must be an Instance")),
                },
                Some(_) => return Err(lua_error("Instance.new: parent must be an Instance")),
            };
            let Some(resolved) = resolved else {
                return Err(lua_error(format!(
                    "Unable to create an Instance of type \"{class}\""
                )));
            };
            let id = host.new_instance(resolved, parent).map_err(lua_error)?;
            state.wrap(lua, id)
        })?,
    )?;
    table.set_readonly(true);
    Ok(table)
}

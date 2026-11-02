//! The script host's [`HostApi`]: what the Luau binding calls, and a WASM binding will call.
//! Design: `docs/design/class-system.md` sections 5 and 13.2.
//!
//! The binding resolves a member name once per access through the class's name table, then
//! addresses it by id. Declared methods get one Luau function per [`DeclId`] per VM, created on
//! first use. Calling one fills a per-VM argument buffer, so a call allocates no list.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use metallic_core::classes::{
    ApiError, ApiErrorKind, CallContext, CallOutcome, ClassId, DeclId, FunctionRef, Handle,
    HostApi, HostMethods, MemberId, MemberKind, MethodDesc, MethodDispatch, MethodImpl, Registry,
    ResolvedClass, ScriptValue, Side, Ty, UNNAMED_INSTANCE, WaitRequest, instance_class, member_of,
};
use metallic_core::{ConnectionId, InstanceId, World};
use mlua::{Function, IntoLuaMulti, Lua, MultiValue, Table, UserDataRef, Value};

use super::host_methods::{self, HostState};
use super::instance::{LuaInstance, Waiter};
use super::script_value::{lua_to_script, release_functions};
use super::{ApiState, errors, lua_error};
use crate::vm::Context;

/// The classes and `Host` method bodies a VM binds. Always the built-in ones outside tests, where
/// a fixture registry shows the binding names no class.
#[derive(Clone, Copy)]
pub(crate) struct ClassSource {
    pub(crate) registry: &'static Registry,
    pub(crate) methods: &'static HostMethods<HostState>,
}

impl ClassSource {
    /// The built-in classes.
    pub(crate) fn builtin() -> mlua::Result<ClassSource> {
        let methods = host_methods::SHARED.as_ref().map_err(lua_error)?;
        Ok(ClassSource {
            registry: metallic_core::classes::registry(),
            methods,
        })
    }
}

/// One VM's cache of method functions, by [`DeclId`], and the rest of its binding state.
pub(crate) struct Binding {
    pub(crate) classes: ClassSource,
    /// One function per declaration, created on first use.
    declared: RefCell<Vec<Option<Function>>>,
    /// The arguments of the call being made. Taken for the call, so a body that calls back into
    /// script code starts with an empty one rather than a busy one.
    args: RefCell<Vec<ScriptValue>>,
    /// Callbacks by instance and declaration; the host keeps them (design 3.2).
    callbacks: RefCell<HashMap<(InstanceId, DeclId), Function>>,
    trampolines: Trampolines,
}

/// What the Luau trampolines need from Rust; see [`TRAMPOLINES`].
struct Trampolines {
    plain: Function,
    parking: Function,
    /// Returned first by a method that parked its thread.
    park: Table,
    /// Returned first, with the seconds, by a method that wants to sleep.
    sleep: Table,
}

/// Wraps a guarded Rust body. The argument count goes first, because a Rust function taking a fixed
/// list of values cannot tell a nil argument from a missing one. `parking` also does what only Luau
/// code can: yield its own thread when the body asked for that (design 5.4).
const TRAMPOLINES: &str = r##"
local select, yield, wait, park, sleep = ...
local function after(first, ...)
    if first == park then return yield() end
    if first == sleep then wait((...)) return nil end
    return first, ...
end
local function plain(f)
    return function(self, ...) return f(self, select("#", ...), ...) end
end
local function parking(f)
    return function(self, ...) return after(f(self, select("#", ...), ...)) end
end
return plain, parking
"##;

impl Binding {
    pub(crate) fn new(lua: &Lua, classes: ClassSource) -> mlua::Result<Binding> {
        let task: Table = lua.globals().get("task")?;
        let coroutine: Table = lua.globals().get("coroutine")?;
        let (park, sleep) = (lua.create_table()?, lua.create_table()?);
        let (plain, parking) = lua
            .load(TRAMPOLINES)
            .set_name("=[engine]")
            .call::<(Function, Function)>((
                lua.globals().get::<Function>("select")?,
                coroutine.get::<Function>("yield")?,
                task.get::<Function>("wait")?,
                park.clone(),
                sleep.clone(),
            ))?;
        Ok(Binding {
            classes,
            declared: RefCell::new(Vec::new()),
            args: RefCell::new(Vec::new()),
            callbacks: RefCell::new(HashMap::new()),
            trampolines: Trampolines {
                plain,
                parking,
                park,
                sleep,
            },
        })
    }

    /// Forgets callbacks of instances that no longer exist.
    pub(crate) fn prune(&self, world: &World) {
        self.callbacks
            .borrow_mut()
            .retain(|(id, _), _| world.is_alive(*id));
    }

    /// The callback set on `id`, if any. #466 is the first to call one.
    pub(crate) fn callback(&self, id: InstanceId, decl: DeclId) -> Option<Function> {
        self.callbacks.borrow().get(&(id, decl)).cloned()
    }
}

/// The Luau `Side`: the VM's own side. A [`ScriptRole`](crate::ScriptRole) is per host and
/// `Combined` has both, so this is read from the VM the call runs in.
fn side(context: Context) -> Side {
    match context {
        Context::Server => Side::Server,
        Context::Client => Side::Client,
    }
}

fn busy() -> ApiError {
    ApiError::message("the world is busy")
}

fn destroyed() -> ApiError {
    let kind = ApiErrorKind::Destroyed {
        name: UNNAMED_INSTANCE.to_owned(),
    };
    ApiError::new("", "", kind)
}

/// The host interface of one VM. Cheap: a reference to its state.
pub(crate) struct LuauHost<'a> {
    state: &'a Rc<ApiState>,
}

impl ApiState {
    pub(crate) fn host(self: &Rc<Self>) -> LuauHost<'_> {
        LuauHost { state: self }
    }

    fn call_context(&self) -> CallContext {
        let owner = self.budget.current_owner();
        CallContext {
            side: side(self.context),
            // The owner id of a script's threads is its id plus one; 0 is no script.
            script: owner.checked_sub(1).map(InstanceId::from_bits),
            local_player: match self.context {
                Context::Client => self.shared.local_player.get(),
                Context::Server => None,
            },
        }
    }
}

impl LuauHost<'_> {
    fn registry(&self) -> &'static Registry {
        self.state.binding.classes.registry
    }

    /// The instance's class in this VM's registry.
    #[inline]
    pub(crate) fn own_class(&self, h: Handle) -> Result<&'static ResolvedClass, ApiError> {
        let world = self.state.world.try_borrow().map_err(|_| busy())?;
        instance_class(self.registry(), &world, h).ok_or_else(destroyed)
    }
}

impl HostApi for LuauHost<'_> {
    fn resolve_class(&self, name: &str) -> Option<ClassId> {
        self.registry().class_id(name)
    }

    #[inline]
    fn resolve_member(&self, class: ClassId, name: &str) -> Option<MemberId> {
        self.registry().class(class)?.member(name)
    }

    fn class_of(&self, h: Handle) -> Result<ClassId, ApiError> {
        Ok(self.own_class(h)?.id)
    }

    fn is_a(&self, h: Handle, class: ClassId) -> Result<bool, ApiError> {
        Ok(self.own_class(h)?.is_a(class))
    }

    #[inline]
    fn get_property(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
    ) -> Result<ScriptValue, ApiError> {
        let world = self.state.world.try_borrow().map_err(|_| busy())?;
        let (own, member) = member_of(self.registry(), &world, h, class, m)?;
        let MemberKind::Property { desc, .. } = member.kind else {
            return Err(ApiError::new(
                own.name(),
                member.name(),
                ApiErrorKind::NotAMember,
            ));
        };
        // #468 checks `member.context` here.
        let value = world
            .get_ref(h, desc.name)
            .map_err(|error| ApiError::from(error).within(own.name(), desc.name))?;
        Ok(ScriptValue::from(value))
    }

    #[inline]
    fn set_property(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        v: ScriptValue,
    ) -> Result<(), ApiError> {
        let mut world = self.state.world.try_borrow_mut().map_err(|_| busy())?;
        let (own, member) = member_of(self.registry(), &world, h, class, m)?;
        let MemberKind::Property { desc, .. } = member.kind else {
            let kind = match member.kind {
                MemberKind::Method { .. } | MemberKind::Signal { .. } => ApiErrorKind::ReadOnly,
                _ => ApiErrorKind::NotAMember,
            };
            return Err(ApiError::new(own.name(), member.name(), kind));
        };
        // #468 checks `member.context` here.
        if desc.readonly {
            return Err(ApiError::new(own.name(), desc.name, ApiErrorKind::ReadOnly));
        }
        let value = v.into_value(desc.ty).ok_or_else(|| {
            ApiError::message(format!(
                "invalid value for property {}: expected {}",
                desc.name,
                desc.ty.name()
            ))
        })?;
        world
            .set(h, desc.name, value)
            .map_err(|error| ApiError::from(error).within(own.name(), desc.name))
    }

    fn call_method(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        args: &[ScriptValue],
    ) -> Result<CallOutcome, ApiError> {
        let mut world = self.state.world.try_borrow_mut().map_err(|_| busy())?;
        let context = self.state.call_context();
        let mut state = HostState {
            api: self.state.clone(),
        };
        MethodDispatch {
            registry: self.registry(),
            methods: self.state.binding.classes.methods,
            world: &mut world,
            state: &mut state,
            context: &context,
        }
        .call(h, class, m, args)
    }

    fn connect_signal(
        &mut self,
        _: Handle,
        _: ClassId,
        _: MemberId,
        _: u32,
        _: FunctionRef,
    ) -> Result<ConnectionId, ApiError> {
        // Until then a `LuaSignal` connects to the hub itself.
        Err(ApiError::message("signals arrive with #463"))
    }

    fn disconnect(&mut self, _: ConnectionId) {}

    fn set_callback(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        f: Option<FunctionRef>,
    ) -> Result<(), ApiError> {
        let world = self.state.world.try_borrow().map_err(|_| busy())?;
        let (own, member) = member_of(self.registry(), &world, h, class, m)?;
        let MemberKind::Callback { decl, .. } = member.kind else {
            return Err(ApiError::new(
                own.name(),
                member.name(),
                ApiErrorKind::NotAMember,
            ));
        };
        let mut callbacks = self.state.binding.callbacks.borrow_mut();
        let function = f.and_then(|reference| self.state.functions.get(reference));
        match function {
            Some(function) => callbacks.insert((h, decl), function),
            None => callbacks.remove(&(h, decl)),
        };
        Ok(())
    }

    fn new_instance(&mut self, class: ClassId, parent: Option<Handle>) -> Result<Handle, ApiError> {
        let Some(resolved) = self.registry().class(class) else {
            return Err(ApiError::new("", "", ApiErrorKind::NotAMember));
        };
        if !resolved.desc.is_creatable() {
            let kind = ApiErrorKind::NotCreatable {
                class: resolved.name().into(),
            };
            return Err(ApiError::new(resolved.name(), "", kind));
        }
        let mut world = self.state.world.try_borrow_mut().map_err(|_| busy())?;
        Ok(world.create(resolved.name(), parent)?)
    }
}

/// The Luau function of a declared method: one per declaration per VM.
pub(crate) fn method_function(
    lua: &Lua,
    state: &Rc<ApiState>,
    desc: &'static MethodDesc,
    decl: DeclId,
    owner: ClassId,
    m: MemberId,
) -> mlua::Result<Function> {
    let slot = decl.0 as usize;
    if let Some(Some(function)) = state.binding.declared.borrow().get(slot) {
        return Ok(function.clone());
    }
    let function = new_method_function(lua, state, desc, decl, owner, m)?;
    let mut declared = state.binding.declared.borrow_mut();
    if declared.len() <= slot {
        declared.resize(slot + 1, None);
    }
    declared[slot] = Some(function.clone());
    Ok(function)
}

fn new_method_function(
    lua: &Lua,
    state: &Rc<ApiState>,
    desc: &'static MethodDesc,
    decl: DeclId,
    owner: ClassId,
    m: MemberId,
) -> mlua::Result<Function> {
    let classes = state.binding.classes;
    // Not moved yet: the old body still does the work (`api/bridge.rs`).
    if desc.implementation == MethodImpl::Host
        && classes.methods.get(decl).is_none()
        && let Some(body) = state.bridge.function(desc.name)
    {
        return Ok(body.clone());
    }
    let trampolines = &state.binding.trampolines;
    if desc.yields {
        let park = trampolines.park.clone();
        let sleep = trampolines.sleep.clone();
        let body = rust_body(lua, owner, m, move |lua, state, outcome| {
            parked_results(lua, state, outcome, &park, &sleep)
        })?;
        return trampolines.parking.call(body);
    }
    let body = if desc.returns == Ty::Nil {
        rust_body(lua, owner, m, |_, _, _| Ok(()))?
    } else {
        rust_body(lua, owner, m, |lua, state, outcome| {
            results(lua, state, outcome)
        })?
    };
    trampolines.plain.call(body)
}

/// A call's results with no list allocated, for any number of values.
fn results(
    lua: &Lua,
    state: &Rc<ApiState>,
    outcome: CallOutcome,
) -> mlua::Result<(Value, MultiValue)> {
    match outcome {
        CallOutcome::Return(ScriptValue::Tuple(values)) => {
            let mut values = values.into_iter();
            let first = match values.next() {
                Some(first) => state.script_to_lua(lua, first)?,
                None => Value::Nil,
            };
            let mut rest = MultiValue::new();
            for value in values {
                rest.push_back(state.script_to_lua(lua, value)?);
            }
            Ok((first, rest))
        }
        CallOutcome::Return(value) => Ok((state.script_to_lua(lua, value)?, MultiValue::new())),
        // `MethodDispatch` refuses this for a method not declared `yields`.
        CallOutcome::Wait(_) => Err(lua_error("this method cannot wait")),
    }
}

/// [`results`] of a yielding method: a [`WaitRequest`] parks the calling thread, resumed with the
/// value or nil. What it waits for decides who resumes it.
fn parked_results(
    lua: &Lua,
    state: &Rc<ApiState>,
    outcome: CallOutcome,
    park: &Table,
    sleep: &Table,
) -> mlua::Result<(Value, MultiValue)> {
    let CallOutcome::Wait(request) = outcome else {
        return results(lua, state, outcome);
    };
    match request {
        WaitRequest::Child {
            parent,
            name,
            timeout,
        } => {
            let waiter = Waiter::new(
                lua.current_thread(),
                parent,
                name.into(),
                state.now.get(),
                timeout,
            );
            state.waiters.borrow_mut().push(waiter);
            Ok((Value::Table(park.clone()), MultiValue::new()))
        }
        WaitRequest::Seconds(seconds) => {
            let mut rest = MultiValue::new();
            rest.push_back(Value::Number(seconds));
            Ok((Value::Table(sleep.clone()), rest))
        }
        // Nothing can resume these yet.
        WaitRequest::Signal { .. } => Err(lua_error("waiting for a signal arrives with #463")),
        WaitRequest::Reply { .. } => Err(lua_error("waiting for a reply arrives with #466")),
    }
}

/// The arguments of a call as Rust gets them: the instance, how many arguments follow it, then the
/// first ones by value, so all but a very long call fits with no list allocated.
type RawCall = (
    UserDataRef<LuaInstance>,
    usize,
    Value,
    Value,
    Value,
    Value,
    Value,
    Value,
    Value,
    Value,
    MultiValue,
);

/// The guarded Rust body of a declared method; `finish` turns its outcome into results while the
/// function references of the arguments are still valid.
fn rust_body<R: IntoLuaMulti>(
    lua: &Lua,
    class: ClassId,
    m: MemberId,
    finish: impl Fn(&Lua, &Rc<ApiState>, CallOutcome) -> mlua::Result<R> + 'static,
) -> mlua::Result<Function> {
    errors::guarded_fn(lua, move |lua, raw: RawCall| {
        let (this, count, a, b, c, d, e, f, g, h, rest) = raw;
        let inline = [a, b, c, d, e, f, g, h];
        let state = &this.state;
        let mut args = state.binding.args.take();
        args.clear();
        let mut dead_argument = None;
        let values = inline.iter().take(count).chain(rest.iter());
        let converted = values.into_iter().try_for_each(|value| {
            let argument = lua_to_script(state, value)?;
            if let ScriptValue::Instance(id) = argument
                && dead_argument.is_none()
                && !state.world()?.is_alive(id)
                && let Value::UserData(ud) = value
            {
                dead_argument = ud.borrow::<LuaInstance>().ok().map(|i| i.name());
            }
            args.push(argument);
            mlua::Result::Ok(())
        });
        let result = converted.and_then(|()| {
            let outcome = state.host().call_method(this.id, class, m, &args);
            match outcome {
                Ok(outcome) => finish(lua, state, outcome),
                Err(error) => Err(this.host_error(error, dead_argument.take())),
            }
        });
        for argument in &args {
            release_functions(state, argument);
        }
        args.clear();
        state.binding.args.replace(args);
        result
    })
}

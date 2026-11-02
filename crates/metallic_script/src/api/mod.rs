//! The Roblox-named Luau API: `game`, `workspace`, `Instance`, the datatypes (with `CFrame`), services,
//! signals and `RemoteEvent`.
//!
//! Every VM gets one [`ApiState`] that the userdata and functions of this module share. It
//! holds the world, the signal hub and the threads parked in `WaitForChild`. Nothing here
//! keeps a `World` borrow while Lua code runs: handlers may change the world, so the host
//! collects what to call, releases the borrow and only then calls into Lua.

mod attributes;
mod bridge;
mod camera;
mod cframe;
mod datatypes;
mod errors;
mod gui;
mod host_api;
pub(crate) mod host_methods;
mod input;
mod instance;
mod pivot;
mod remote;
mod require;
mod script_value;
mod services;
mod signals;
mod tags;

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use metallic_core::classes::{DeclId, HostMethods, Registry};
use metallic_core::{InstanceId, World};
use mlua::{Error, Function, Table, WeakLua};

use host_methods::HostState;

use crate::vm::{Budget, Context, OutputSink, ScriptError, ScriptVm, format_error};

pub use gui::{GuiEvent, GuiEventKind, GuiPointer, MAX_GUI_EVENTS_PER_STEP};
pub use input::{
    BoundResult, InputActionEvent, InputDevice, InputEventKind, InputValue, KEY_CODES,
    MAX_ACTION_EVENTS_PER_STEP, TouchButtonInfo, USER_INPUT_TYPES,
};
pub(crate) use instance::LuaInstance;
pub(crate) use remote::{QueuedRemote, RemoteTarget};
pub(crate) use signals::{EventKind, Hub, Pacing};

/// State shared by the server VM and the client VM of one [`ScriptHost`](crate::ScriptHost).
#[derive(Default)]
pub(crate) struct HostShared {
    /// Remote events fired since the last `step`, delivered by the next one.
    pub(crate) remotes: RefCell<Vec<QueuedRemote>>,
    /// Messages and bytes queued since the last delivery, for the server (0) and client (1) VM.
    pub(crate) queued: Cell<[(usize, usize); 2]>,
    /// Players whose `PlayerAdded` was delivered (what `GetPlayers` returns).
    pub(crate) joined: RefCell<HashSet<InstanceId>>,
    /// The player of the client VM (`Players.LocalPlayer`).
    pub(crate) local_player: Cell<Option<InstanceId>>,
    /// Input events, action states and device flags pushed by the host.
    pub(crate) input: input::InputShared,
    /// The viewport the camera maths use, in pixels (zero until the host reports it).
    pub(crate) viewport: Cell<[f64; 2]>,
    /// The GUI inset in pixels, for the `Screen` camera methods.
    pub(crate) gui_inset: Cell<f64>,
    /// A test and throttling hook: after this many handler calls in a step, the step counts as
    /// out of time (see `ScriptHost::set_step_handler_limit`).
    pub(crate) step_handler_limit: Cell<Option<usize>>,
}

impl HostShared {
    /// The viewport size, [`camera::DEFAULT_VIEWPORT`] until the host reports one.
    pub(crate) fn viewport_size(&self) -> [f64; 2] {
        let v = self.viewport.get();
        if v[0] > 0.0 && v[1] > 0.0 {
            v
        } else {
            camera::DEFAULT_VIEWPORT
        }
    }
}

/// Everything the API functions of one VM need.
pub(crate) struct ApiState {
    pub(crate) world: Rc<RefCell<World>>,
    pub(crate) context: Context,
    pub(crate) budget: Budget,
    pub(crate) sink: Rc<dyn OutputSink>,
    pub(crate) shared: Rc<HostShared>,
    pub(crate) hub: Hub,
    pub(crate) pacing: Pacing,
    pub(crate) lua: WeakLua,
    /// Weak-valued table `instance id -> userdata`, so one instance is one Lua object.
    cache: Table,
    /// `task.spawn`: every handler runs in its own thread, so it may yield.
    spawn: Function,
    /// The VM clock, set by the host on every step.
    pub(crate) now: Rc<Cell<f64>>,
    pub(crate) waiters: Rc<RefCell<Vec<instance::Waiter>>>,
    /// The methods the registry does not declare yet (`FindFirstChild`, `GetPlayers`, ...).
    pub(crate) bridge: bridge::Bridge,
    /// The method cache, argument buffer and callbacks of the host interface.
    pub(crate) binding: host_api::Binding,
    /// The functions a `FunctionRef` names.
    pub(crate) functions: script_value::ScriptFunctions,
    /// `Enum` and its items.
    enums: input::Enums,
    /// Actions bound with `ContextActionService:BindAction`, by name.
    bound: RefCell<HashMap<String, input::Bound>>,
    /// `Cancel` calls for actions unbound while pressed.
    cancels: RefCell<Vec<input::PendingCancel>>,
    /// Hidden actions to destroy once the world is free: `(action, context)`.
    pending_destroy: RefCell<Vec<(InstanceId, InstanceId)>>,
    /// Wraps a bound handler so its `Sink`/`Pass` result reaches the host.
    bind_wrapper: Function,
    /// `UserInputService` and `ContextActionService`.
    input_services: input::InputServices,
    /// Attribute signal names and this VM's read position in the world's attribute log.
    attributes: attributes::AttributeState,
    /// Tag signal names, this VM's read position in the world's tag log and `CollectionService`.
    tags: tags::TagState,
    /// Which thread is loading which ModuleScript, and who waits for it.
    pub(crate) require: require::RequireState,
}

impl ApiState {
    /// Read access to the world; an error (not a panic) if the host has it borrowed.
    pub(crate) fn world(&self) -> mlua::Result<Ref<'_, World>> {
        self.world
            .try_borrow()
            .map_err(|_| Error::runtime("the world is busy"))
    }

    /// Write access to the world.
    pub(crate) fn world_mut(&self) -> mlua::Result<RefMut<'_, World>> {
        self.world
            .try_borrow_mut()
            .map_err(|_| Error::runtime("the world is busy"))
    }
}

fn setup_failed(e: Error) -> ScriptError {
    ScriptError {
        message: format!("script API setup failed: {}", format_error(&e)),
    }
}

/// Installs the whole API into `vm` and returns the state the host drives it with.
pub(crate) fn install(
    vm: &ScriptVm,
    world: Rc<RefCell<World>>,
    shared: Rc<HostShared>,
    sink: Rc<dyn OutputSink>,
) -> Result<Rc<ApiState>, ScriptError> {
    host_api::ClassSource::builtin()
        .and_then(|classes| build(vm, world, shared, sink, classes))
        .map_err(setup_failed)
}

/// The whole API installed over other classes than the built-in ones, so a test shows the binding
/// names no class: a method added to a fixture registry is callable with no binding change. The
/// fixture's classes must have the names of real classes, since the world only creates those.
#[doc(hidden)]
pub struct FixtureApi(Rc<ApiState>);

impl FixtureApi {
    /// Installs the API into `vm` over `registry` and `methods`.
    pub fn install(
        vm: &ScriptVm,
        world: Rc<RefCell<World>>,
        sink: Rc<dyn OutputSink>,
        registry: &'static Registry,
        methods: &'static HostMethods<HostState>,
    ) -> Result<FixtureApi, ScriptError> {
        let classes = host_api::ClassSource { registry, methods };
        build(vm, world, Rc::new(HostShared::default()), sink, classes)
            .map(FixtureApi)
            .map_err(setup_failed)
    }

    /// The Luau object of `id`.
    pub fn wrap(&self, id: InstanceId) -> mlua::Result<mlua::AnyUserData> {
        let Some(lua) = self.0.lua.try_upgrade() else {
            return Err(Error::runtime("the VM is gone"));
        };
        self.0.wrap(&lua, id)
    }

    /// Advances the clock parked threads see and resumes the waits that are over.
    pub fn wake_waiters(&self, now: f64) {
        self.0.now.set(now);
        self.0.wake_waiters();
    }

    /// The callback set on `id` for the declaration `decl`.
    pub fn callback(&self, id: InstanceId, decl: DeclId) -> Option<Function> {
        self.0.binding.callback(id, decl)
    }

    /// Forgets what belonged to destroyed instances, as a step does.
    pub fn prune(&self) {
        self.0.prune_hub();
    }
}

fn build(
    vm: &ScriptVm,
    world: Rc<RefCell<World>>,
    shared: Rc<HostShared>,
    sink: Rc<dyn OutputSink>,
    classes: host_api::ClassSource,
) -> mlua::Result<Rc<ApiState>> {
    let lua = vm.lua();
    errors::install(lua)?;
    let cache = lua.create_table()?;
    let weak_values = lua.create_table()?;
    weak_values.set("__mode", "v")?;
    cache.set_metatable(Some(weak_values))?;
    let spawn: Function = lua.globals().get::<Table>("task")?.get("spawn")?;

    let now = Rc::new(Cell::new(0.0));
    let waiters = Rc::new(RefCell::new(Vec::new()));
    let mut raw = instance::methods(lua, &waiters, &now)?;
    raw.extend(services::methods(lua)?);
    raw.extend(remote::methods(lua)?);
    raw.extend(pivot::methods(lua)?);
    raw.extend(input::action_methods(lua)?);
    raw.extend(attributes::methods(lua)?);
    raw.extend(tags::methods(lua)?);
    raw.extend(camera::methods(lua)?);
    let mut bodies = Vec::with_capacity(raw.len());
    for (name, function) in raw {
        // `WaitForChild` is Luau code that calls guarded functions itself.
        let function = if name == "WaitForChild" {
            function
        } else {
            errors::guarded(lua, function)?
        };
        bodies.push((name, function));
    }
    let bridge = bridge::Bridge::new(classes.registry, bodies)?;
    let binding = host_api::Binding::new(lua, classes)?;

    let enums = input::Enums::install(lua)?;
    let bind_wrapper = input::bind_wrapper(lua, &enums)?;
    let (world_cursor, tag_cursor) = {
        let w = world.borrow();
        (w.attribute_cursor(), w.tag_cursor())
    };
    let state = Rc::new(ApiState {
        world,
        context: vm.context(),
        budget: vm.budget(),
        sink,
        shared,
        hub: Hub::default(),
        pacing: Pacing::default(),
        lua: lua.weak(),
        cache,
        spawn,
        now,
        waiters,
        bridge,
        binding,
        functions: script_value::ScriptFunctions::default(),
        enums,
        bound: RefCell::new(HashMap::new()),
        cancels: RefCell::new(Vec::new()),
        pending_destroy: RefCell::new(Vec::new()),
        bind_wrapper,
        input_services: input::InputServices::new(),
        attributes: attributes::AttributeState::new(world_cursor),
        tags: tags::TagState::new(tag_cursor),
        require: require::RequireState::default(),
    });
    tags::install_service(lua, &state)?;
    input::install_services(lua, &state)?;

    let set = |name: &str, value: mlua::Value| vm.set_global(name, value);
    let to_err = |e: ScriptError| Error::runtime(e.message);
    set(
        "Instance",
        mlua::Value::Table(instance::constructor(lua, &state)?),
    )
    .map_err(to_err)?;
    set("CFrame", mlua::Value::Table(cframe::constructor(lua)?)).map_err(to_err)?;
    set("Ray", mlua::Value::Table(camera::ray_constructor(lua)?)).map_err(to_err)?;
    for (name, table) in datatypes::constructors(lua)? {
        set(name, mlua::Value::Table(table)).map_err(to_err)?;
    }
    set("Enum", mlua::Value::Table(state.enums.table.clone())).map_err(to_err)?;
    let (lib, shared_g) = vm.env_parts();
    let require = require::install(lua, &state, lib, shared_g)?;
    crate::debug::set_require(lua, &require);
    set("require", mlua::Value::Function(require)).map_err(to_err)?;
    set("game", mlua::Value::UserData(services::game(lua, &state)?)).map_err(to_err)?;
    let workspace = state.world()?.root("Workspace");
    let workspace = match workspace {
        Some(id) => mlua::Value::UserData(state.wrap(lua, id)?),
        None => mlua::Value::Nil,
    };
    set("workspace", workspace).map_err(to_err)?;
    Ok(state)
}

/// Makes a Lua error from anything printable.
pub(crate) fn lua_error(message: impl std::fmt::Display) -> Error {
    Error::runtime(message.to_string())
}

//! Signals (`:Connect`, `RBXScriptConnection:Disconnect`) and how the host fires them.
//!
//! A signal is identified by an instance and an [`EventKind`]. Each VM keeps its own
//! [`Hub`] of connected Lua functions. Firing takes a snapshot of the live connections
//! first, so a `Connect` made by a handler does not run in the same dispatch, and a
//! `Disconnect` made by a handler stops later handlers of that dispatch from running.
//!
//! Limits: a signal holds at most [`MAX_CONNECTIONS`] connections; all handlers of one
//! `step` share one time budget (the rest wait, in order, for the next step); a script whose
//! handlers time out [`MAX_CONSECUTIVE_TIMEOUTS`] times in a row is disconnected.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use metallic_core::InstanceId;
use mlua::{
    Function, Lua, MetaMethod, MultiValue, UserData, UserDataFields, UserDataMethods, UserDataRef,
    Value,
};

use super::{ApiState, errors, lua_error};
use crate::task_lib::TIMEOUT_MSG;
use crate::vm::format_error;

/// Most connections one signal accepts.
pub(crate) const MAX_CONNECTIONS: usize = 10_000;
/// Timeouts in a row after which a script's connections are removed.
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;
/// Most handler calls kept waiting for a later step.
const MAX_DEFERRED: usize = 10_000;

/// The events scripts can connect to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum EventKind {
    /// `BasePart.Touched(hit)`
    Touched,
    /// `BasePart.TouchEnded(hit)`
    TouchEnded,
    /// `Humanoid.Died()`
    Died,
    /// `Player.CharacterAdded(model)`
    CharacterAdded,
    /// `Players.PlayerAdded(player)`
    PlayerAdded,
    /// `Players.PlayerRemoving(player)`
    PlayerRemoving,
    /// `RunService.Heartbeat(dt)`
    Heartbeat,
    /// `RemoteEvent.OnServerEvent(player, ...)`
    OnServerEvent,
    /// `RemoteEvent.OnClientEvent(...)`
    OnClientEvent,
    /// `InputAction.Pressed()`
    Pressed,
    /// `InputAction.Released()`
    Released,
    /// `InputAction.Triggered(value)`
    Triggered,
    /// `InputAction.StateChanged(value)`
    StateChanged,
    /// `UserInputService.LastInputTypeChanged(type)`
    LastInputTypeChanged,
    /// `Instance.AttributeChanged(name)` for id 0, `GetAttributeChangedSignal(name)` for the
    /// interned name id of that attribute (no arguments).
    AttributeChanged(u32),
    /// `CollectionService:GetInstanceAddedSignal(tag)`: the interned tag id; the instance is the
    /// argument.
    TagAdded(u32),
    /// `CollectionService:GetInstanceRemovedSignal(tag)`.
    TagRemoved(u32),
    /// A GUI element's pointer event (`Activated`, `MouseEnter`, ...).
    Gui(super::gui::GuiEventKind),
    /// A signal a class file declares, by its `DeclId`. #463 replaces this enum.
    Declared(u32),
}

/// One signal: an event of one instance.
pub(crate) type SignalKey = (InstanceId, EventKind);

struct Connection {
    id: u64,
    func: Function,
    alive: Rc<Cell<bool>>,
    /// The owner id of the script that connected it (0 for none).
    owner: u64,
}

/// A handler call waiting for the next step because this one ran out of budget.
pub(crate) struct Deferred {
    func: Function,
    alive: Rc<Cell<bool>>,
    owner: u64,
    args: Vec<Value>,
}

/// The connections of one VM.
#[derive(Default)]
pub(crate) struct Hub {
    connections: RefCell<HashMap<SignalKey, Vec<Connection>>>,
    next_id: Cell<u64>,
}

impl Hub {
    fn connect(
        &self,
        key: SignalKey,
        func: Function,
        owner: u64,
    ) -> mlua::Result<(u64, Rc<Cell<bool>>)> {
        let mut all = self.connections.borrow_mut();
        let list = all.entry(key).or_default();
        if list.len() >= MAX_CONNECTIONS {
            return Err(lua_error("too many connections"));
        }
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let alive = Rc::new(Cell::new(true));
        list.push(Connection {
            id,
            func,
            alive: alive.clone(),
            owner,
        });
        Ok((id, alive))
    }

    fn disconnect(&self, key: SignalKey, id: u64) {
        if let Some(list) = self.connections.borrow_mut().get_mut(&key) {
            list.retain(|c| c.id != id);
        }
    }

    /// How many handlers `key` has.
    fn count(&self, key: SignalKey) -> usize {
        self.connections.borrow().get(&key).map_or(0, Vec::len)
    }

    fn snapshot(&self, key: SignalKey) -> Vec<(Function, Rc<Cell<bool>>, u64)> {
        self.connections
            .borrow()
            .get(&key)
            .map(|list| {
                list.iter()
                    .map(|c| (c.func.clone(), c.alive.clone(), c.owner))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Disconnects everything `owner` connected.
    fn remove_owner(&self, owner: u64) {
        for list in self.connections.borrow_mut().values_mut() {
            list.retain(|c| {
                let keep = c.owner != owner;
                if !keep {
                    c.alive.set(false);
                }
                keep
            });
        }
    }

    /// Drops the connections of instances for which `is_alive` is false.
    pub(crate) fn prune(&self, is_alive: impl Fn(InstanceId) -> bool) {
        self.connections.borrow_mut().retain(|(id, _), list| {
            let keep = is_alive(*id);
            if !keep {
                for c in list.iter() {
                    c.alive.set(false);
                }
            }
            keep
        });
    }
}

/// Per-VM bookkeeping of the step budget, deferred handlers and timeout streaks.
#[derive(Default)]
pub(crate) struct Pacing {
    deadline: Cell<Option<Instant>>,
    deferred: RefCell<VecDeque<Deferred>>,
    streaks: RefCell<HashMap<u64, u32>>,
    warned_full: Cell<bool>,
    /// Set by `hold_step_deadline`: the next `begin_step` keeps the running deadline.
    keep_deadline: Cell<bool>,
    /// How many times a handler's arguments were built (a test hook for the dispatch cost).
    args_built: Cell<u64>,
    /// Handler calls made in the current step, for `HostShared::step_handler_limit`.
    handlers_this_step: Cell<usize>,
}

/// Ends only a dispatch budget established by its own external lifecycle call.
pub(crate) struct DispatchBudget<'a> {
    state: &'a ApiState,
    owned: bool,
}

impl Drop for DispatchBudget<'_> {
    fn drop(&mut self) {
        if self.owned {
            self.state.end_step();
        }
    }
}

impl ApiState {
    /// Uses an active/held frame deadline, or establishes the ordinary aggregate deadline
    /// for a call outside step. Dropping the guard never clears a caller-owned deadline.
    pub(crate) fn dispatch_budget(&self) -> DispatchBudget<'_> {
        let owned = self.pacing.deadline.get().is_none();
        if owned {
            self.restart_step();
        }
        DispatchBudget { state: self, owned }
    }

    /// Starts the time budget of one `step`.
    pub(crate) fn begin_step(&self) {
        if self.pacing.keep_deadline.replace(false) {
            return;
        }
        self.pacing.deadline.set(Some(
            Instant::now() + Duration::from_secs_f64(self.budget.step_budget_sec()),
        ));
        self.pacing.warned_full.set(false);
        self.pacing.handlers_this_step.set(0);
    }

    /// Starts a time budget at once, dropping any held one (see `hold_step_deadline`).
    pub(crate) fn restart_step(&self) {
        self.pacing.keep_deadline.set(false);
        self.begin_step();
    }

    /// Lets the next `begin_step` continue this budget, so work done before the `step` of the
    /// same frame and the step itself share one budget.
    pub(crate) fn hold_step_deadline(&self) {
        self.pacing.keep_deadline.set(true);
    }

    /// Ends the step budget (calls outside `step`, like script start-up, are not limited by it).
    pub(crate) fn end_step(&self) {
        self.pacing.deadline.set(None);
    }

    /// How many times handler arguments were built since the VM started.
    pub(crate) fn args_built(&self) -> u64 {
        self.pacing.args_built.get()
    }

    /// Out of step time, or paused in the debugger: handlers wait for a later step.
    fn over_budget(&self) -> bool {
        self.budget.paused()
            || self
                .pacing
                .deadline
                .get()
                .is_some_and(|d| Instant::now() >= d)
            || self
                .shared
                .step_handler_limit
                .get()
                .is_some_and(|n| self.pacing.handlers_this_step.get() >= n)
    }

    /// Whether the lifecycle events (tag added or removed, attribute changed) of a log can be
    /// handed to `handlers` handlers now. They are not dropped by the handler queue: when this step is out of time
    /// or the deferred queue has no room for that many calls, the reader stays at the event and
    /// takes it up again in the next step, in order. The events wait in the world's log (which
    /// holds hundreds of thousands) instead of in the handler queue.
    pub(crate) fn lifecycle_room(&self, handlers: usize) -> bool {
        handlers == 0
            || (!self.over_budget()
                && self.pacing.deferred.borrow().len() + handlers <= MAX_DEFERRED)
    }

    /// How many handlers `key` has.
    pub(crate) fn handler_count(&self, key: SignalKey) -> usize {
        self.hub.count(key)
    }

    /// Runs handler calls that an earlier step had no time for, oldest first.
    pub(crate) fn run_deferred(&self) {
        loop {
            if self.over_budget() {
                return;
            }
            let next = self.pacing.deferred.borrow_mut().pop_front();
            let Some(call) = next else { return };
            if call.alive.get() {
                self.run_handler(call.func, call.owner, call.args);
            }
        }
    }

    /// Calls every handler connected to `key`. `args` builds the arguments for one handler
    /// (called once per handler, so tables can be copied afresh). Each handler runs in its
    /// own thread under the execution budget; its errors go to the output sink. Handlers
    /// that do not fit in the step budget run in a later step.
    pub(crate) fn fire(&self, key: SignalKey, args: &dyn Fn(&Lua) -> mlua::Result<Vec<Value>>) {
        let handlers = self.hub.snapshot(key);
        if handlers.is_empty() {
            return;
        }
        if self.lua.try_upgrade().is_none() {
            return;
        }
        for (func, alive, owner) in handlers {
            if !alive.get() {
                continue;
            }
            if !self.dispatch(func, alive, owner, args) {
                break;
            }
        }
    }

    /// Runs one handler call now, or queues it when this step is out of time. `args` builds
    /// its arguments. Returns false when the queue is full and the rest should be dropped.
    pub(crate) fn dispatch(
        &self,
        func: Function,
        alive: Rc<Cell<bool>>,
        owner: u64,
        args: &dyn Fn(&Lua) -> mlua::Result<Vec<Value>>,
    ) -> bool {
        let Some(lua) = self.lua.try_upgrade() else {
            return false;
        };
        // Decide first, so a full queue costs no argument copying: once it is full the
        // rest of the handlers are dropped without building anything.
        let defer = self.over_budget();
        if defer && self.pacing.deferred.borrow().len() >= MAX_DEFERRED {
            if !self.pacing.warned_full.replace(true) {
                self.sink
                    .warn("too many script handlers are waiting; some calls were dropped");
            }
            return false;
        }
        self.pacing.args_built.set(self.pacing.args_built.get() + 1);
        let values = match args(&lua) {
            Ok(values) => values,
            Err(e) => {
                self.sink.error(&format_error(&e));
                return true;
            }
        };
        if defer {
            self.pacing.deferred.borrow_mut().push_back(Deferred {
                func,
                alive,
                owner,
                args: values,
            });
        } else {
            self.run_handler(func, owner, values);
        }
        true
    }

    /// Runs one handler in its own thread; tracks consecutive timeouts of its script.
    fn run_handler(&self, func: Function, owner: u64, values: Vec<Value>) {
        self.pacing
            .handlers_this_step
            .set(self.pacing.handlers_this_step.get() + 1);
        let name = func.info().short_src.unwrap_or_default();
        let mut all = Vec::with_capacity(values.len() + 1);
        all.push(Value::Function(func));
        all.extend(values);
        let result =
            self.budget
                .call_as::<MultiValue>(owner, &self.spawn, MultiValue::from_vec(all));
        let timed_out = match result {
            Ok(_) => false,
            Err(e) => {
                self.sink.error(&e.message);
                e.message.ends_with(TIMEOUT_MSG)
            }
        };
        if owner == 0 {
            return;
        }
        if !timed_out {
            self.pacing.streaks.borrow_mut().remove(&owner);
            return;
        }
        let streak = {
            let mut streaks = self.pacing.streaks.borrow_mut();
            let n = streaks.entry(owner).or_insert(0);
            *n += 1;
            *n
        };
        if streak >= MAX_CONSECUTIVE_TIMEOUTS {
            self.stop_owner(owner);
            self.sink.error(&format!(
                "{name}: script disconnected after {MAX_CONSECUTIVE_TIMEOUTS} consecutive timeouts"
            ));
        }
    }

    /// Stops a script: removes its connections, queued calls, waits and sleeping threads.
    pub(crate) fn stop_owner(&self, owner: u64) {
        self.hub.remove_owner(owner);
        self.pacing
            .deferred
            .borrow_mut()
            .retain(|d| d.owner != owner);
        self.waiters
            .borrow_mut()
            .retain(|w| self.budget.owner_of(&w.thread) != Some(owner));
        self.pacing.streaks.borrow_mut().remove(&owner);
        self.require.release_owner(owner);
        self.drop_bound_actions(owner);
        self.budget.cancel_owner(owner);
    }

    /// Drops the connections of instances that no longer exist.
    pub(crate) fn prune_hub(&self) {
        if let Ok(world) = self.world() {
            self.hub.prune(|id| world.is_alive(id));
            self.binding.prune(&world);
        }
    }
}

/// The Luau `RBXScriptSignal`.
pub(crate) struct LuaSignal {
    pub(crate) state: Rc<ApiState>,
    pub(crate) key: SignalKey,
}

impl UserData for LuaSignal {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "RBXScriptSignal");
        // `signal:Connect(fn)`; guarded, so a bad argument is a positioned script error.
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            let members = lua.create_table()?;
            members.set(
                "Connect",
                errors::guarded_fn(
                    lua,
                    |lua, (this, func): (UserDataRef<LuaSignal>, Function)| {
                        let owner = this.state.budget.current_owner();
                        let (id, alive) = this.state.hub.connect(this.key, func, owner)?;
                        lua.create_userdata(LuaConnection {
                            state: this.state.clone(),
                            key: this.key,
                            id,
                            alive,
                        })
                    },
                )?,
            )?;
            Ok(members)
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("RBXScriptSignal".to_string())
        });
    }
}

/// The Luau `RBXScriptConnection`.
pub(crate) struct LuaConnection {
    state: Rc<ApiState>,
    key: SignalKey,
    id: u64,
    alive: Rc<Cell<bool>>,
}

impl UserData for LuaConnection {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "RBXScriptConnection");
        fields.add_field_method_get("Connected", |_, this| Ok(this.alive.get()));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_method("Disconnect", |_, this, ()| {
            this.alive.set(false);
            this.state.hub.disconnect(this.key, this.id);
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("RBXScriptConnection".to_string())
        });
    }
}

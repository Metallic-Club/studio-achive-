//! Signals (`:Connect`, `:Once`, `:Wait`, `RBXScriptConnection:Disconnect`) and how the host fires
//! them.
//!
//! A signal is a member a class file declares, keyed by [`SignalKey`]: instance, member id, and an
//! interned id for a parameterized signal (0 otherwise). Nothing here names a class or an event:
//! the raising side looks the signal up by name in the instance's class ([`ApiState::signal_key`]).
//! Each VM keeps its own [`Hub`] of connected Lua functions and of threads parked in `:Wait()`.
//! Firing takes a snapshot of the live connections and waiters first, so a `Connect` or `Wait` made
//! by a handler does not run in the same dispatch, and a `Disconnect` made by a handler stops later
//! handlers of that dispatch from running. A `:Once` connection disconnects itself just before its
//! one call. Waiters resume after the handlers, with the same arguments, under the normal resume
//! budget; they are dropped with their script ([`ApiState::stop_owner`]) or their instance
//! ([`ApiState::prune_hub`]).
//!
//! Limits: a signal holds at most [`MAX_CONNECTIONS`] connections (and as many waiters); all
//! handlers of one `step` share one time budget (the rest wait, in order, for the next step); a
//! script whose handlers time out [`MAX_CONSECUTIVE_TIMEOUTS`] times in a row is disconnected.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use metallic_core::classes::{
    ApiError, ApiErrorKind, MemberCategory, MemberContext, MemberId, MemberKind, Registry,
    ResolvedClass, Side, SignalKey, instance_class,
};
use metallic_core::{ConnectionId, InstanceId, QueuedSignal, World};
use mlua::{
    Function, Lua, MetaMethod, MultiValue, Table, Thread, UserData, UserDataFields,
    UserDataMethods, UserDataRef, Value,
};

use super::{ApiState, errors, lua_error};
use crate::task_lib::TIMEOUT_MSG;
use crate::vm::{Context, format_error};

/// Most connections one signal accepts (and most threads waiting on it).
pub(crate) const MAX_CONNECTIONS: usize = 10_000;
/// Timeouts in a row after which a script's connections are removed.
const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;
/// Most handler calls kept waiting for a later step.
const MAX_DEFERRED: usize = 10_000;

/// `UserInputService.LastInputTypeChanged(type)`, on the carrier instance. `UserInputService` and
/// `CollectionService` are host objects, not classes, until #466 and #467, so their three signals
/// have no member id. These ids are past any class's members, so they mean nothing else.
pub(crate) const LAST_INPUT_TYPE_CHANGED: MemberId = MemberId(u16::MAX);
/// `CollectionService:GetInstanceAddedSignal(tag)`; `arg` is the interned tag.
pub(crate) const TAG_ADDED: MemberId = MemberId(u16::MAX - 1);
/// `CollectionService:GetInstanceRemovedSignal(tag)`; `arg` is the interned tag.
pub(crate) const TAG_REMOVED: MemberId = MemberId(u16::MAX - 2);

/// The signals every instance has. By the prefix rule each has one id in every class, so the
/// attribute and property logs can key thousands of changes without looking at a class.
#[derive(Clone, Copy)]
pub(crate) struct InstanceSignals {
    pub(crate) attribute_changed: MemberId,
    pub(crate) changed: MemberId,
}

impl InstanceSignals {
    pub(crate) fn resolve(registry: &Registry) -> mlua::Result<InstanceSignals> {
        let find = |name| {
            registry
                .class_by_name("Instance")
                .and_then(|class| class.member(name))
                .ok_or_else(|| lua_error(format!("the registry has no Instance.{name}")))
        };
        Ok(InstanceSignals {
            attribute_changed: find("AttributeChanged")?,
            changed: find("Changed")?,
        })
    }
}

/// The key of the whole signal `signal` of `instance`.
pub(crate) fn plain_key(instance: InstanceId, signal: MemberId) -> SignalKey {
    SignalKey {
        instance,
        signal,
        arg: 0,
    }
}

struct Connection {
    id: ConnectionId,
    func: Function,
    alive: Rc<Cell<bool>>,
    /// The owner id of the script that connected it (0 for none).
    owner: u64,
    /// Made by `:Once`: disconnected just before its first call.
    once: bool,
}

/// A connection as a dispatch saw it when it started.
struct Snapshot {
    id: ConnectionId,
    func: Function,
    alive: Rc<Cell<bool>>,
    owner: u64,
    once: bool,
}

/// A thread parked in `signal:Wait()`.
struct SignalWaiter {
    thread: Thread,
    /// The owner id of the script that waits (0 for none).
    owner: u64,
}

/// What a dispatch runs: a connected handler, or a waiting thread to resume.
enum Call {
    Handler(Function),
    Resume(Thread),
}

/// A handler call or waiter resume kept for the next step because this one ran out of budget.
pub(crate) struct Deferred {
    call: Call,
    alive: Rc<Cell<bool>>,
    owner: u64,
    args: Vec<Value>,
}

/// The connections and `:Wait()` threads of one VM.
#[derive(Default)]
pub(crate) struct Hub {
    connections: RefCell<HashMap<SignalKey, Vec<Connection>>>,
    /// Which signal each connection is on, for a disconnect that has only the connection.
    keys: RefCell<HashMap<ConnectionId, SignalKey>>,
    waiters: RefCell<HashMap<SignalKey, Vec<SignalWaiter>>>,
    /// The keys this VM told the world it listens to.
    published: RefCell<HashSet<SignalKey>>,
    /// `published` may not match the listeners: fixed at the end of the step. Listening a little
    /// longer than needed only queues a fire that finds nobody.
    stale: Cell<bool>,
}

impl Hub {
    /// `None` when the signal is full.
    fn connect(
        &self,
        key: SignalKey,
        func: Function,
        owner: u64,
        once: bool,
    ) -> Option<(ConnectionId, Rc<Cell<bool>>)> {
        let mut all = self.connections.borrow_mut();
        let list = all.entry(key).or_default();
        if list.len() >= MAX_CONNECTIONS {
            return None;
        }
        let id = ConnectionId::allocate();
        let alive = Rc::new(Cell::new(true));
        list.push(Connection {
            id,
            func,
            alive: alive.clone(),
            owner,
            once,
        });
        self.keys.borrow_mut().insert(id, key);
        Some((id, alive))
    }

    /// Removes a connection. One already gone is fine.
    pub(crate) fn disconnect(&self, id: ConnectionId) {
        let Some(key) = self.keys.borrow_mut().remove(&id) else {
            return;
        };
        let mut all = self.connections.borrow_mut();
        if let Some(list) = all.get_mut(&key) {
            list.retain(|c| {
                let keep = c.id != id;
                if !keep {
                    c.alive.set(false);
                }
                keep
            });
            if list.is_empty() {
                all.remove(&key);
                self.stale.set(true);
            }
        }
    }

    /// How many handlers and `:Wait()` threads `key` has (both are calls a fire makes).
    fn count(&self, key: SignalKey) -> usize {
        self.connections.borrow().get(&key).map_or(0, Vec::len)
            + self.waiters.borrow().get(&key).map_or(0, Vec::len)
    }

    fn snapshot(&self, key: SignalKey) -> Vec<Snapshot> {
        self.connections
            .borrow()
            .get(&key)
            .map(|list| {
                list.iter()
                    .map(|c| Snapshot {
                        id: c.id,
                        func: c.func.clone(),
                        alive: c.alive.clone(),
                        owner: c.owner,
                        once: c.once,
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Parks `thread` until `key` next fires. False when too many wait on it.
    fn add_waiter(&self, key: SignalKey, thread: Thread, owner: u64) -> bool {
        let mut all = self.waiters.borrow_mut();
        let list = all.entry(key).or_default();
        if list.len() >= MAX_CONNECTIONS {
            return false;
        }
        list.push(SignalWaiter { thread, owner });
        true
    }

    /// Removes and returns the threads waiting on `key`.
    fn take_waiters(&self, key: SignalKey) -> Vec<SignalWaiter> {
        let mut all = self.waiters.borrow_mut();
        // Every fire comes here, and usually nothing waits on anything: no need to hash the key.
        if all.is_empty() {
            return Vec::new();
        }
        let taken = all.remove(&key);
        if taken.is_some() {
            self.stale.set(true);
        }
        taken.unwrap_or_default()
    }

    /// Parks `waiters` on `key` again, ahead of any that started waiting since.
    fn restore_waiters(&self, key: SignalKey, waiters: impl Iterator<Item = SignalWaiter>) {
        let mut all = self.waiters.borrow_mut();
        let list = all.entry(key).or_default();
        let newer = std::mem::take(list);
        list.extend(waiters);
        list.extend(newer);
    }

    /// How many threads wait on any signal.
    fn waiter_count(&self) -> usize {
        self.waiters.borrow().values().map(Vec::len).sum()
    }

    /// The instances with a listener on any key of `signal`.
    pub(crate) fn instances_listening(&self, signal: MemberId) -> HashSet<InstanceId> {
        let connections = self.connections.borrow();
        let waiters = self.waiters.borrow();
        connections
            .keys()
            .chain(waiters.keys())
            .filter(|key| key.signal == signal)
            .map(|key| key.instance)
            .collect()
    }

    /// Disconnects everything `owner` connected and drops its waiting threads.
    fn remove_owner(&self, owner: u64) {
        let mut keys = self.keys.borrow_mut();
        let mut all = self.connections.borrow_mut();
        for list in all.values_mut() {
            list.retain(|c| {
                let keep = c.owner != owner;
                if !keep {
                    c.alive.set(false);
                    keys.remove(&c.id);
                }
                keep
            });
        }
        all.retain(|_, list| !list.is_empty());
        let mut waiters = self.waiters.borrow_mut();
        for list in waiters.values_mut() {
            list.retain(|w| w.owner != owner);
        }
        waiters.retain(|_, list| !list.is_empty());
        self.stale.set(true);
    }

    /// Drops the connections and waiting threads of instances for which `is_alive` is false.
    pub(crate) fn prune(&self, is_alive: impl Fn(InstanceId) -> bool) {
        let mut keys = self.keys.borrow_mut();
        self.connections.borrow_mut().retain(|key, list| {
            let keep = is_alive(key.instance);
            if !keep {
                self.stale.set(true);
                for c in list.iter() {
                    c.alive.set(false);
                    keys.remove(&c.id);
                }
            }
            keep
        });
        self.waiters.borrow_mut().retain(|key, list| {
            let keep = !list.is_empty() && is_alive(key.instance);
            if !keep {
                self.stale.set(true);
            }
            keep
        });
    }

    /// Makes what the world knows match the listeners.
    fn sync_published(&self, world: &mut World) {
        let connections = self.connections.borrow();
        let waiters = self.waiters.borrow();
        let mut published = self.published.borrow_mut();
        published.retain(|key| {
            let keep = connections.contains_key(key) || waiters.contains_key(key);
            if !keep {
                world.signals_mut().unwatch(*key);
            }
            keep
        });
        for key in connections.keys().chain(waiters.keys()) {
            if published.insert(*key) {
                world.signals_mut().watch(*key);
            }
        }
        self.stale.set(false);
    }

    /// Tells the world this VM listens to nothing any more.
    fn unpublish_all(&self, world: &mut World) {
        for key in self.published.borrow_mut().drain() {
            world.signals_mut().unwatch(key);
        }
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

    /// How many calls a fire of `key` makes: its handlers plus the threads in `:Wait()`.
    pub(crate) fn handler_count(&self, key: SignalKey) -> usize {
        self.hub.count(key)
    }

    /// How many threads wait in `signal:Wait()`.
    pub(crate) fn signal_waiters(&self) -> usize {
        self.hub.waiter_count()
    }

    /// The whole signal `name` of `instance`, if its class has it and shows it to scripts. `None`
    /// also for a destroyed instance, which nothing can fire on. Allocates nothing.
    pub(crate) fn signal_key(&self, instance: InstanceId, name: &str) -> Option<SignalKey> {
        let world = self.world.try_borrow().ok()?;
        let class = instance_class(self.binding.classes.registry, &world, instance)?;
        declared_signal(class, instance, name)
    }

    /// The whole signal `name` of `instance`, which the caller knows is a `class`. Needs no world.
    pub(crate) fn signal_key_of(
        &self,
        instance: InstanceId,
        class: &str,
        name: &str,
    ) -> Option<SignalKey> {
        let class = self.binding.classes.registry.class_by_name(class)?;
        declared_signal(class, instance, name)
    }

    /// Refuses a signal of the other side, as `HostApi` and reading the member both must.
    pub(crate) fn check_signal_context(
        &self,
        class: &'static str,
        name: &'static str,
        context: MemberContext,
    ) -> Result<(), ApiError> {
        let required = match (context, self.context) {
            (MemberContext::Server, Context::Client) => Side::Server,
            (MemberContext::Client, Context::Server) => Side::Client,
            _ => return Ok(()),
        };
        let kind = ApiErrorKind::WrongContext {
            required,
            member_kind: MemberCategory::Signal,
        };
        Err(ApiError::new(class, name, kind))
    }

    /// Adds a handler to `key`. `None` when the signal is full. Every way of connecting ends here.
    pub(crate) fn connect_listener(
        &self,
        key: SignalKey,
        func: Function,
        once: bool,
    ) -> Option<(ConnectionId, Rc<Cell<bool>>)> {
        let owner = self.budget.current_owner();
        let connection = self.hub.connect(key, func, owner, once)?;
        self.listener_added(key);
        Some(connection)
    }

    /// Parks `thread` until `key` next fires, so cancelling its owner also blocks it.
    pub(crate) fn wait_for_signal(&self, key: SignalKey, thread: Thread) -> mlua::Result<()> {
        let owner = self.budget.current_owner();
        self.budget.adopt(&thread)?;
        if !self.hub.add_waiter(key, thread, owner) {
            return Err(lua_error("too many threads waiting on this signal"));
        }
        self.listener_added(key);
        Ok(())
    }

    /// `key` has a listener: the world and the property journal reader must know.
    fn listener_added(&self, key: SignalKey) {
        if self.hub.published.borrow_mut().insert(key) {
            match self.world.try_borrow_mut() {
                Ok(mut world) => world.signals_mut().watch(key),
                // Something holds the world while a script runs: the end of the step catches up.
                Err(_) => {
                    self.hub.published.borrow_mut().remove(&key);
                    self.hub.stale.set(true);
                }
            }
        }
        if key.signal == self.instance_signals.changed {
            self.watch_properties(key.instance);
        }
    }

    /// Delivers one fire from the world's queue to this VM's handlers.
    pub(crate) fn deliver_signal(self: &Rc<Self>, queued: &QueuedSignal) {
        if !self.world().is_ok_and(|w| w.is_alive(queued.key.instance)) {
            return;
        }
        self.fire(queued.key, &|lua| {
            queued
                .args
                .iter()
                .map(|arg| self.script_to_lua(lua, arg.clone()))
                .collect()
        });
    }

    /// Runs handler calls and waiter resumes that an earlier step had no time for, oldest first.
    pub(crate) fn run_deferred(&self) {
        loop {
            if self.over_budget() {
                return;
            }
            let next = self.pacing.deferred.borrow_mut().pop_front();
            let Some(deferred) = next else { return };
            if deferred.alive.get() {
                self.run_call(deferred.call, deferred.owner, deferred.args);
            }
        }
    }

    /// Calls every handler connected to `key`, then resumes every thread waiting on it. `args`
    /// builds the arguments for one handler or waiter (called once per call, so tables can be
    /// copied afresh). Each handler runs in its own thread under the execution budget; its errors
    /// go to the output sink. Calls that do not fit in the step budget run in a later step.
    pub(crate) fn fire(&self, key: SignalKey, args: &dyn Fn(&Lua) -> mlua::Result<Vec<Value>>) {
        let handlers = self.hub.snapshot(key);
        let waiters = self.hub.take_waiters(key);
        if handlers.is_empty() && waiters.is_empty() {
            return;
        }
        if self.lua.try_upgrade().is_none() {
            return;
        }
        for handler in handlers {
            if !handler.alive.get() {
                continue;
            }
            let alive = if handler.once {
                // Disconnected before the call: `Connected` is false inside it and no later fire
                // sees it. The call itself still runs, even when it is deferred.
                self.hub.disconnect(handler.id);
                Rc::new(Cell::new(true))
            } else {
                handler.alive
            };
            if !self.dispatch_call(Call::Handler(handler.func), alive, handler.owner, args) {
                break;
            }
        }
        let mut waiters = waiters.into_iter();
        while let Some(waiter) = waiters.next() {
            // Its script may have stopped while the handlers ran.
            if !self.budget.can_resume_parked(&waiter.thread) {
                continue;
            }
            let call = Call::Resume(waiter.thread.clone());
            if !self.dispatch_call(call, Rc::new(Cell::new(true)), waiter.owner, args) {
                // No room to queue them: they keep waiting for the next fire.
                self.hub
                    .restore_waiters(key, std::iter::once(waiter).chain(waiters));
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
        self.dispatch_call(Call::Handler(func), alive, owner, args)
    }

    fn dispatch_call(
        &self,
        call: Call,
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
                call,
                alive,
                owner,
                args: values,
            });
        } else {
            self.run_call(call, owner, values);
        }
        true
    }

    fn run_call(&self, call: Call, owner: u64, values: Vec<Value>) {
        match call {
            Call::Handler(func) => self.run_handler(func, owner, values),
            Call::Resume(thread) => self.resume_waiter(&thread, values),
        }
    }

    /// Resumes a thread parked in `signal:Wait()` with the fire's arguments, under the resume
    /// budget. A thread whose script stopped in the meantime stays put.
    fn resume_waiter(&self, thread: &Thread, values: Vec<Value>) {
        if !self.budget.can_resume_parked(thread) {
            return;
        }
        self.pacing
            .handlers_this_step
            .set(self.pacing.handlers_this_step.get() + 1);
        if let Err(e) = self.budget.resume(thread, MultiValue::from_vec(values)) {
            self.sink.error(&e.message);
        }
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

    /// Drops the connections and `:Wait()` threads of instances that no longer exist, and brings
    /// what the world knows about this VM's listeners up to date.
    pub(crate) fn prune_hub(&self) {
        if let Ok(world) = self.world() {
            self.hub.prune(|id| world.is_alive(id));
            self.binding.prune(&world);
        }
        if self.hub.stale.get()
            && let Ok(mut world) = self.world.try_borrow_mut()
        {
            self.hub.sync_published(&mut world);
        }
    }

    /// This VM is shutting down: it listens to nothing any more. A busy world only means a fire
    /// keeps being queued for nobody.
    pub(crate) fn unpublish_signals(&self) {
        if let Ok(mut world) = self.world.try_borrow_mut() {
            self.hub.unpublish_all(&mut world);
        }
    }
}

/// The signal `name` of `class`, on `instance`.
fn declared_signal(class: &ResolvedClass, instance: InstanceId, name: &str) -> Option<SignalKey> {
    let id = class.member(name)?;
    let member = class.members.get(usize::from(id.0))?;
    (matches!(member.kind, MemberKind::Signal { .. }) && !member.hidden)
        .then_some(plain_key(instance, id))
}

/// The Luau `RBXScriptSignal`.
pub(crate) struct LuaSignal {
    pub(crate) state: Rc<ApiState>,
    pub(crate) key: SignalKey,
}

/// `signal:Connect(fn)` and `signal:Once(fn)`; guarded, so a bad argument is a positioned script
/// error. The signal was checked when it was made, so this connects even once the instance is
/// gone: the connection is dropped at the end of the step, as it always was.
fn connect_fn(lua: &Lua, once: bool) -> mlua::Result<Function> {
    errors::guarded_fn(
        lua,
        move |lua, (this, func): (UserDataRef<LuaSignal>, Function)| {
            let (id, alive) = this
                .state
                .connect_listener(this.key, func, once)
                .ok_or_else(|| lua_error("too many connections"))?;
            lua.create_userdata(LuaConnection {
                state: this.state.clone(),
                id,
                alive,
            })
        },
    )
}

/// `signal:Wait()`: Luau code, because only Luau code can yield its own thread. Outside a yieldable
/// thread it yields anyway, so it fails exactly like `task.wait` there.
fn wait_fn(lua: &Lua) -> mlua::Result<Function> {
    let register = errors::guarded_fn(
        lua,
        |_, (this, thread): (UserDataRef<LuaSignal>, Thread)| {
            this.state.wait_for_signal(this.key, thread)
        },
    )?;
    let coroutine: Table = lua.globals().get("coroutine")?;
    lua.load(
        r#"
local register, yield, running, isyieldable = ...
return function(signal)
    if not isyieldable() then return yield() end
    register(signal, running())
    return yield()
end
"#,
    )
    .set_name("=[engine]")
    .call((
        register,
        coroutine.get::<Function>("yield")?,
        coroutine.get::<Function>("running")?,
        coroutine.get::<Function>("isyieldable")?,
    ))
}

impl UserData for LuaSignal {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "RBXScriptSignal");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            let members = lua.create_table()?;
            members.set("Connect", connect_fn(lua, false)?)?;
            members.set("Once", connect_fn(lua, true)?)?;
            members.set("Wait", wait_fn(lua)?)?;
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
    id: ConnectionId,
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
            this.state.hub.disconnect(this.id);
            Ok(())
        });
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("RBXScriptConnection".to_string())
        });
    }
}

//! The `task` library (`task.wait`, `task.spawn`, `task.delay`), the scheduler that runs
//! script threads, and the execution budget.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{
    AnyUserData, Error, Function, IntoLuaMulti, LightUserData, Lua, MultiValue, Result, Table,
    Thread, UserData, Value, WeakLua,
};

use crate::debug::{self, DebugCore, Outcome};
use crate::vm::{OutputSink, format_error, initial_resume_budget, initial_step_budget_sec, lossy};

/// Text of the error raised when a thread runs out of budget.
pub(crate) const TIMEOUT_MSG: &str = "Script timeout: exhausted allowed execution time";

/// Markers (light userdata) that script threads use to talk to the scheduler. A script
/// cannot forge them: it has no way to create light userdata.
static TAGS: [u8; 3] = [0; 3];

fn tag(i: usize) -> Value {
    Value::LightUserData(LightUserData(&TAGS[i] as *const u8 as *mut c_void))
}

fn is_tag(v: Option<&Value>, i: usize) -> bool {
    matches!(v, Some(Value::LightUserData(p)) if p.0 == &TAGS[i] as *const u8 as *mut c_void)
}

/// Yield marker of `task.wait`: `yield(SLEEP, seconds)`.
const SLEEP: usize = 0;
/// First result of a guarded thread body that finished normally.
const OK: usize = 1;
/// First result of a guarded thread body that raised an error (second result: message).
const ERR: usize = 2;

/// A fresh allocation identifies one queue admission without a wrapping counter.
struct QueueLease;

/// This state never references its thread or Lua; the weak-key registry owns its handle.
struct ThreadState {
    owner: u64,
    cancelled: Cell<bool>,
    queued: RefCell<Option<Rc<QueueLease>>>,
}

struct ThreadStateHandle(Rc<ThreadState>);
impl UserData for ThreadStateHandle {}

/// A suspended coroutine waiting for its wake-up time.
struct Pending {
    wake: f64,
    seq: u64,
    /// Time at which the thread went to sleep (to compute what `task.wait` returns).
    started: f64,
    /// `true` for `task.wait` (resume with elapsed time), `false` for `task.delay`.
    is_wait: bool,
    thread: Thread,
    args: Vec<Value>,
    state: Rc<ThreadState>,
    lease: Rc<QueueLease>,
}

/// State shared between the VM, the `task` functions and the interrupt hook.
pub(crate) struct Scheduler {
    pub(crate) now: Cell<f64>,
    pub(crate) deadline: Cell<Option<Instant>>,
    /// Time one resume may run ([`crate::RESUME_BUDGET`] unless the host changed it).
    pub(crate) resume_budget: Cell<Duration>,
    /// Time one step may spend ([`crate::STEP_BUDGET_SEC`] unless the host changed it).
    pub(crate) step_budget_sec: Cell<f64>,
    /// Set by the interrupt hook when the budget ran out: `"<chunk>:<line>: "` of the
    /// running code (possibly empty).
    pub(crate) timeout_loc: RefCell<Option<String>>,
    pending: RefCell<Vec<Pending>>,
    /// Weak thread keys avoid pointer-reuse aliases and keep cancelled live threads blocked.
    threads: RefCell<Option<Table>>,
    lua: RefCell<Option<WeakLua>>,
    /// The owner of the code that is running now; new threads and connections inherit it.
    current: Cell<u64>,
    seq: Cell<u64>,
    pub(crate) sink: Rc<dyn OutputSink>,
    /// Lua function `guard(f, ...)`: runs `f` under `xpcall` with the error normaliser.
    guard: RefCell<Option<Function>>,
    /// Creates an equivalent guard with its function and complete argument tuple bound.
    bind_guard: RefCell<Option<Function>>,
    /// The debugger of this VM (inert until breakpoints or a pause are set).
    pub(crate) debug: DebugCore,
}

impl Scheduler {
    pub(crate) fn new(sink: Rc<dyn OutputSink>) -> Self {
        Scheduler {
            now: Cell::new(0.0),
            deadline: Cell::new(None),
            resume_budget: Cell::new(initial_resume_budget()),
            step_budget_sec: Cell::new(initial_step_budget_sec()),
            timeout_loc: RefCell::new(None),
            pending: RefCell::new(Vec::new()),
            threads: RefCell::new(None),
            lua: RefCell::new(None),
            current: Cell::new(0),
            seq: Cell::new(0),
            sink,
            guard: RefCell::new(None),
            bind_guard: RefCell::new(None),
            debug: DebugCore::new(),
        }
    }

    /// Forgets every sleeping thread.
    pub(crate) fn clear(&self) {
        self.debug.clear();
        self.pending.borrow_mut().clear();
        *self.threads.borrow_mut() = None;
        *self.guard.borrow_mut() = None;
        *self.bind_guard.borrow_mut() = None;
        *self.lua.borrow_mut() = None;
    }

    pub(crate) fn guard(&self) -> Result<Function> {
        self.guard
            .borrow()
            .clone()
            .ok_or_else(|| Error::runtime("script VM is shut down"))
    }

    pub(crate) fn lua(&self) -> Result<Lua> {
        self.lua
            .borrow()
            .as_ref()
            .and_then(WeakLua::try_upgrade)
            .ok_or_else(|| Error::runtime("script VM is shut down"))
    }

    fn registry(&self) -> Result<Table> {
        let _lua = self.lua()?;
        self.threads
            .borrow()
            .clone()
            .ok_or_else(|| Error::runtime("script VM is shut down"))
    }

    fn state(&self, thread: &Thread) -> Result<Option<Rc<ThreadState>>> {
        let value: Option<AnyUserData> = self.registry()?.raw_get(thread.clone())?;
        value
            .map(|value| {
                value
                    .borrow::<ThreadStateHandle>()
                    .map(|state| state.0.clone())
            })
            .transpose()
    }

    fn register(&self, lua: &Lua, thread: &Thread) -> Result<Rc<ThreadState>> {
        if let Some(state) = self.state(thread)? {
            return Ok(state);
        }
        let state = Rc::new(ThreadState {
            owner: self.current.get(),
            cancelled: Cell::new(false),
            queued: RefCell::new(None),
        });
        let handle = lua.create_userdata(ThreadStateHandle(state.clone()))?;
        self.registry()?.raw_set(thread.clone(), handle)?;
        Ok(state)
    }

    fn can_resume(&self, thread: &Thread) -> Result<bool> {
        Ok(self
            .state(thread)?
            .is_none_or(|state| !state.cancelled.get() && state.queued.borrow().is_none()))
    }

    pub(crate) fn begin_resume(&self, thread: &Thread) -> Result<()> {
        if let Some(state) = self.state(thread)? {
            if state.cancelled.get() {
                return Err(Error::runtime("cannot resume a cancelled task thread"));
            }
            state.queued.borrow_mut().take();
        }
        Ok(())
    }

    fn finish_resume(&self, thread: &Thread) -> Result<()> {
        if matches!(
            thread.status(),
            mlua::thread::ThreadStatus::Finished | mlua::thread::ThreadStatus::Error
        ) {
            self.registry()?.raw_remove(thread.clone())?;
        }
        Ok(())
    }

    pub(crate) fn cancel_thread(&self, thread: &Thread) {
        self.debug.thread_gone(thread.to_pointer());
        if let Ok(Some(state)) = self.state(thread) {
            state.cancelled.set(true);
            state.queued.borrow_mut().take();
            self.pending
                .borrow_mut()
                .retain(|pending| !Rc::ptr_eq(&pending.state, &state));
        }
    }

    /// Creates a guarded thread with its original function and arguments already bound.
    pub(crate) fn new_thread(
        &self,
        lua: &Lua,
        f: Function,
        args: Vec<Value>,
    ) -> Result<(Thread, Vec<Value>)> {
        let bind = self
            .bind_guard
            .borrow()
            .clone()
            .ok_or_else(|| Error::runtime("script VM is shut down"))?;
        let mut all = Vec::with_capacity(args.len() + 1);
        all.push(Value::Function(f));
        all.extend(args);
        let body: Function = bind.call(MultiValue::from_vec(all))?;
        let thread = lua.create_thread(body)?;
        self.register(lua, &thread)?;
        Ok((thread, Vec::new()))
    }

    /// The owner id of the script a thread belongs to.
    pub(crate) fn owner_of(&self, thread: &Thread) -> Option<u64> {
        self.state(thread)
            .ok()
            .flatten()
            .and_then(|state| (state.owner != 0 && !state.cancelled.get()).then_some(state.owner))
    }

    /// The owner of the code that is running now (0 outside any script).
    pub(crate) fn current_owner(&self) -> u64 {
        self.current.get()
    }

    /// Runs `run` with `owner` as the current owner, restoring the previous one after.
    pub(crate) fn as_owner<T>(&self, owner: u64, run: impl FnOnce() -> T) -> T {
        let prev = self.current.replace(owner);
        let out = run();
        self.current.set(prev);
        out
    }

    /// Forgets the sleeping threads of `owner`; they are never resumed again.
    pub(crate) fn cancel_owner(&self, owner: u64) {
        if owner == 0 {
            return;
        }
        if let Ok(registry) = self.registry() {
            for (_, value) in registry.pairs::<Thread, AnyUserData>().flatten() {
                if let Ok(state) = value.borrow::<ThreadStateHandle>()
                    && state.0.owner == owner
                {
                    state.0.cancelled.set(true);
                    state.0.queued.borrow_mut().take();
                }
            }
        }
        self.pending.borrow_mut().retain(|p| p.state.owner != owner);
    }

    fn push(
        &self,
        lua: &Lua,
        wake: f64,
        is_wait: bool,
        thread: Thread,
        args: Vec<Value>,
    ) -> Result<()> {
        let state = self.register(lua, &thread)?;
        if state.cancelled.get() || state.queued.borrow().is_some() {
            return Err(Error::runtime(
                "cannot schedule an already owned task thread",
            ));
        }
        let lease = Rc::new(QueueLease);
        *state.queued.borrow_mut() = Some(lease.clone());
        let seq = self.seq.get();
        self.seq.set(seq + 1);
        let started = self.now.get();
        self.pending.borrow_mut().push(Pending {
            wake,
            seq,
            started,
            is_wait,
            thread,
            args,
            state,
            lease,
        });
        Ok(())
    }

    /// Interprets the outcome of one resume: queue sleepers, report guarded errors.
    pub(crate) fn after_resume(
        &self,
        thread: &Thread,
        raw: Result<MultiValue>,
    ) -> std::result::Result<(), String> {
        self.finish_resume(thread).map_err(|e| format_error(&e))?;
        let values = raw.map_err(|e| format_error(&e))?;
        if thread.status() == mlua::thread::ThreadStatus::Resumable {
            if is_tag(values.front(), SLEEP) {
                let secs = match values.get(1) {
                    Some(Value::Number(n)) => *n,
                    Some(Value::Integer(n)) => *n as f64,
                    _ => 0.0,
                };
                let lua = self.lua().map_err(|e| format_error(&e))?;
                self.push(
                    &lua,
                    self.now.get() + secs.max(0.0),
                    true,
                    thread.clone(),
                    Vec::new(),
                )
                .map_err(|e| format_error(&e))?;
            }
            // Any other yield parks the thread; its owner resumes it later.
            return Ok(());
        }
        if is_tag(values.front(), ERR) {
            return Err(match values.get(1) {
                Some(Value::String(s)) => lossy(&s.as_bytes()),
                _ => "unknown script error".to_string(),
            });
        }
        Ok(())
    }

    /// Runs `run` with a time budget. A nested call keeps the outer deadline if it is
    /// earlier. Returns the timeout error text if the budget ran out.
    pub(crate) fn budgeted<T>(&self, run: impl FnOnce() -> T) -> (T, Option<String>) {
        let prev = self.deadline.get();
        if prev.is_none() {
            *self.timeout_loc.borrow_mut() = None;
        }
        let mine = Instant::now() + self.resume_budget.get();
        self.deadline.set(Some(prev.map_or(mine, |p| p.min(mine))));
        let out = run();
        self.deadline.set(prev);
        let loc = if prev.is_none() {
            self.timeout_loc.borrow_mut().take()
        } else {
            self.timeout_loc.borrow().clone()
        };
        (out, loc.map(|l| format!("{l}{TIMEOUT_MSG}")))
    }

    /// Resumes `thread` once: through the debugger while it is armed (a break is then an
    /// outcome, not an error), else through mlua.
    fn run_thread(&self, thread: &Thread, args: Vec<Value>) -> Result<Outcome> {
        if self.debug.armed() {
            let lua = self.lua()?;
            return debug::raw_resume(&lua, &self.debug, thread, args);
        }
        thread
            .resume::<MultiValue>(MultiValue::from_vec(args))
            .map(Outcome::Done)
    }

    /// Resumes `thread` under a fresh (or the enclosing, if earlier) budget. While the
    /// game is paused the resume waits until it continues.
    pub(crate) fn resume_with_budget(
        &self,
        thread: &Thread,
        args: Vec<Value>,
    ) -> std::result::Result<(), String> {
        if self.debug.is_paused() {
            self.debug.defer(thread.clone(), args);
            return Ok(());
        }
        let owner = self.owner_of(thread).unwrap_or(0);
        self.begin_resume(thread).map_err(|e| format_error(&e))?;
        let (raw, timeout) =
            self.as_owner(owner, || self.budgeted(|| self.run_thread(thread, args)));
        if let Ok(Outcome::Break) = raw {
            self.debug.scheduler_break(thread, owner);
            return Ok(());
        }
        if let Some(msg) = timeout {
            self.cancel_thread(thread);
            return Err(msg);
        }
        self.after_resume(thread, raw.map(Outcome::values))
    }

    /// Resumes `thread` inside the caller's budget (used by `task.spawn`). True when it
    /// paused (the spawner then pauses too).
    fn resume_inline(
        &self,
        thread: &Thread,
        args: Vec<Value>,
    ) -> std::result::Result<bool, String> {
        if self.debug.is_paused() {
            self.debug.defer(thread.clone(), args);
            return Ok(false);
        }
        let owner = self.owner_of(thread).unwrap_or(self.current.get());
        self.begin_resume(thread).map_err(|e| format_error(&e))?;
        let raw = self.as_owner(owner, || self.run_thread(thread, args));
        if let Ok(Outcome::Break) = raw {
            self.debug.scheduler_break(thread, owner);
            return Ok(true);
        }
        self.after_resume(thread, raw.map(Outcome::values))
            .map(|()| false)
    }

    /// Drops a paused thread that will not run again (it stopped at an error).
    pub(crate) fn forget(&self, thread: &Thread) {
        self.cancel_thread(thread);
        if let Ok(registry) = self.registry() {
            let _ = registry.raw_remove(thread.clone());
        }
    }

    /// Calls `f(args)` under the budget, through the error normaliser.
    pub(crate) fn call_guarded(
        &self,
        lua: &Lua,
        owner: u64,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> std::result::Result<MultiValue, String> {
        let guard = self.guard().map_err(|e| format_error(&e))?;
        let mut all = vec![Value::Function(f.clone())];
        all.extend(args.into_lua_multi(lua).map_err(|e| format_error(&e))?);
        let (raw, timeout) = self.as_owner(owner, || {
            self.budgeted(|| guard.call::<MultiValue>(MultiValue::from_vec(all)))
        });
        if let Some(msg) = timeout {
            return Err(msg);
        }
        let mut values = raw.map_err(|e| format_error(&e))?;
        if is_tag(values.front(), ERR) {
            return Err(match values.get(1) {
                Some(Value::String(s)) => lossy(&s.as_bytes()),
                _ => "unknown script error".to_string(),
            });
        }
        values.pop_front();
        Ok(values)
    }

    /// Resumes due threads, stopping early once the step budget is used up; the rest
    /// stay due and run on the next step. Errors go to the sink.
    pub(crate) fn step(&self, now: f64) {
        if self.debug.is_paused() {
            return;
        }
        self.now.set(now);
        let mut due = {
            let mut pending = self.pending.borrow_mut();
            let (due, later): (Vec<_>, Vec<_>) = pending.drain(..).partition(|p| p.wake <= now);
            *pending = later;
            due
        };
        due.sort_by(|a, b| a.wake.total_cmp(&b.wake).then(a.seq.cmp(&b.seq)));
        let started = Instant::now();
        let mut due = due.into_iter();
        while let Some(p) = due.next() {
            if started.elapsed().as_secs_f64() > self.step_budget_sec.get()
                || self.debug.is_paused()
            {
                let mut pending = self.pending.borrow_mut();
                pending.push(p);
                pending.extend(due);
                break;
            }
            let current = !p.state.cancelled.get()
                && p.state
                    .queued
                    .borrow()
                    .as_ref()
                    .is_some_and(|lease| Rc::ptr_eq(lease, &p.lease));
            if !current {
                continue;
            }
            if p.thread.status() != mlua::thread::ThreadStatus::Resumable {
                p.state.queued.borrow_mut().take();
                if let Err(error) = self.finish_resume(&p.thread) {
                    self.sink.error(&format_error(&error));
                }
                continue;
            }
            let args = if p.is_wait {
                vec![Value::Number(now - p.started)]
            } else {
                p.args
            };
            if let Err(e) = self.resume_with_budget(&p.thread, args) {
                self.sink.error(&e);
            }
        }
    }
}

/// Registers the global `task` table and the thread guard.
pub(crate) fn install(lua: &Lua, sched: &Rc<Scheduler>) -> Result<()> {
    *sched.lua.borrow_mut() = Some(lua.weak());
    DebugCore::install(sched, lua)?;
    let threads = lua.create_table()?;
    let weak = lua.create_table()?;
    weak.set("__mode", "k")?;
    threads.set_metatable(Some(weak))?;
    *sched.threads.borrow_mut() = Some(threads);
    let globals = lua.globals();
    let xpcall: Value = globals.get("xpcall")?;
    let type_fn: Value = globals.get("type")?;
    let tostring: Value = globals.get("tostring")?;
    let sub: Value = globals.get::<mlua::Table>("string")?.get("sub")?;
    let find: Value = globals.get::<mlua::Table>("string")?.get("find")?;
    let info: Value = globals.get::<mlua::Table>("debug")?.get("info")?;
    let yield_fn: Value = globals.get::<mlua::Table>("coroutine")?.get("yield")?;
    let tables: Table = globals.get("table")?;
    let pack: Function = tables.get("pack")?;
    let unpack: Function = tables.get("unpack")?;

    // The guard runs every script thread. Its handler turns any error value into a
    // string without running script code (no `__tostring` of script tables) and adds the
    // `<chunk>:<line>:` of the nearest Lua frame when the message has none.
    let (guard, bind_guard): (Function, Function) = lua
        .load(
            r#"
local xpcall, type, tostring, sub, info, okTag, errTag, find, pack, unpack = ...
local function describe(e)
    local t = type(e)
    if t == "string" then return e end
    if t == "nil" or t == "number" or t == "boolean" then return tostring(e) end
    if t == "userdata" then
        local s = tostring(e)
        if sub(s, 1, 15) == "runtime error: " then s = sub(s, 16) end
        local cut = find(s, "\nstack traceback", 1, true)
        if cut then s = sub(s, 1, cut - 1) end
        return s
    end
    return "<" .. t .. ">"
end
local function handler(e)
    local msg = describe(e)
    local level = 2
    while true do
        local src, line = info(level, "sl")
        if src == nil then return msg end
        if line ~= nil and line >= 0 and src ~= "[C]" and src ~= "[engine]" then
            if sub(msg, 1, #src + 1) == src .. ":" then return msg end
            return src .. ":" .. line .. ": " .. msg
        end
        level += 1
    end
end
local function finish(ok, ...)
    if ok then return okTag, ... end
    return errTag, ...
end
local function guard(f, ...) return finish(xpcall(f, handler, ...)) end
local function bind(f, ...)
    local args = pack(...)
    return function() return finish(xpcall(f, handler, unpack(args, 1, args.n))) end
end
return guard, bind
"#,
        )
        .set_name("=[engine]")
        .call((
            xpcall,
            type_fn,
            tostring,
            sub,
            info,
            tag(OK),
            tag(ERR),
            find,
            pack,
            unpack,
        ))?;
    *sched.guard.borrow_mut() = Some(guard);
    *sched.bind_guard.borrow_mut() = Some(bind_guard);

    install_coroutines(lua, sched)?;

    let task = lua.create_table()?;

    // `task.wait` yields `(SLEEP, seconds)` to the scheduler.
    let wait: Function = lua
        .load(r#"
local yield, sleep, tonumber, type, error = ...
return function(n)
    if n == nil then n = 0 else
        local value = tonumber(n)
        if value == nil then error("bad argument #1 to 'wait' (number expected, got "..type(n)..")", 2) end
        n = value
    end
    return yield(sleep, n)
end
"#)
        .set_name("=[engine]")
        .call((yield_fn, tag(SLEEP), globals.get::<Function>("tonumber")?, globals.get::<Function>("type")?, globals.get::<Function>("error")?))?;
    task.set("wait", wait)?;

    // `task.spawn` is a Luau wrapper so that, when the spawned thread pauses in the
    // debugger, the spawner can pause right after the call (a Rust function cannot break).
    let s = sched.clone();
    let spawn_raw = lua.create_function(move |lua, (f, args): (Value, MultiValue)| {
        let args: Vec<Value> = args.into_iter().collect();
        let (thread, first) = match f {
            Value::Function(f) => s.new_thread(lua, f, args)?,
            Value::Thread(t) => {
                if !s.can_resume(&t)? {
                    return Err(Error::runtime(
                        "cannot schedule an already owned task thread",
                    ));
                }
                s.register(lua, &t)?;
                (t, args)
            }
            other => return Err(bad_target(&other)),
        };
        let broke = match s.resume_inline(&thread, first) {
            Ok(broke) => broke,
            Err(e) => {
                // A timeout is reported once, by the resume that owns the budget.
                if s.timeout_loc.borrow().is_none() {
                    s.sink.error(&e);
                }
                false
            }
        };
        Ok((thread, broke))
    })?;
    // SAFETY: `break_spawner` follows the Lua C function protocol.
    let break_spawner = unsafe { lua.create_c_function(debug::break_spawner)? };
    let spawn: Function = lua
        .load(
            r#"
local raw, pause = ...
return function(...)
    local thread, broke = raw(...)
    if broke then pause() end
    return thread
end
"#,
        )
        .set_name("=[engine]")
        .call((spawn_raw, break_spawner))?;
    task.set("spawn", spawn)?;

    let s = sched.clone();
    task.set(
        "delay",
        lua.create_function(
            move |lua, (secs, f, args): (Option<f64>, Value, MultiValue)| {
                let args: Vec<Value> = args.into_iter().collect();
                let (thread, first) = match f {
                    Value::Function(f) => s.new_thread(lua, f, args)?,
                    Value::Thread(t) => {
                        if !s.can_resume(&t)? {
                            return Err(Error::runtime(
                                "cannot schedule an already owned task thread",
                            ));
                        }
                        s.register(lua, &t)?;
                        (t, args)
                    }
                    other => return Err(bad_target(&other)),
                };
                s.push(
                    lua,
                    s.now.get() + secs.unwrap_or(0.0).max(0.0),
                    false,
                    thread.clone(),
                    first,
                )?;
                Ok(thread)
            },
        )?,
    )?;

    globals.set("task", task)
}

/// Keep ordinary coroutine values in Lua, including opaque error userdata identities.
fn install_coroutines(lua: &Lua, sched: &Rc<Scheduler>) -> Result<()> {
    let globals = lua.globals();
    let coroutines: Table = globals.get("coroutine")?;
    let native_resume: Function = coroutines.get("resume")?;
    let native_create: Function = coroutines.get("create")?;
    let s = sched.clone();
    let admitted = lua.create_function(move |_, thread: Thread| s.can_resume(&thread))?;
    let s = sched.clone();
    let finish = lua.create_function(move |_, thread: Thread| s.finish_resume(&thread))?;
    let s = sched.clone();
    let sleep = lua.create_function(move |lua, (thread, seconds): (Thread, f64)| {
        s.push(
            lua,
            s.now.get() + seconds.max(0.0),
            true,
            thread,
            Vec::new(),
        )
    })?;
    let tables: Table = globals.get("table")?;
    let (resume, wrap): (Function, Function) = lua.load(r#"
local native_resume, create, admitted, finish, sleep, sleepTag, okTag, errTag,
    type, typename, tostring, error, select, pack, unpack = ...
local function bad_argument(name, expected, count, value)
    if count == 0 then
        error("missing argument #1 to '"..name.."' ("..expected.." expected)", 3)
    end
    error("invalid argument #1 to '"..name.."' ("..expected.." expected, got "..typename(value)..")", 3)
end
local function outcome(thread, ok, ...)
    if not ok then finish(thread) return false, ... end
    local first = ...
    if first == sleepTag then
        sleep(thread, select(2, ...))
        return true
    end
    finish(thread)
    if first == okTag then return true, select(2, ...) end
    if first == errTag then return false, select(2, ...) end
    return true, ...
end
local function resume(...)
    local thread = ...
    if type(thread) ~= 'thread' then bad_argument('resume', 'thread', select('#', ...), thread) end
    if not admitted(thread) then return false, 'cannot resume a task-owned coroutine' end
    return outcome(thread, native_resume(...))
end
local function wrap(...)
    local f = ...
    if type(f) ~= 'function' then bad_argument('wrap', 'function', select('#', ...), f) end
    local thread = create(f)
    return function(...)
        local values = pack(resume(thread, ...))
        if not values[1] then
            local value = values[2]
            if type(value) == 'number' then value = tostring(value) end
            error(value, 2)
        end
        return unpack(values, 2, values.n)
    end
end
return resume, wrap
"#).set_name("=[engine]").call((native_resume, native_create, admitted, finish, sleep,
        tag(SLEEP), tag(OK), tag(ERR), globals.get::<Function>("type")?, globals.get::<Function>("typeof")?,
        globals.get::<Function>("tostring")?, globals.get::<Function>("error")?, globals.get::<Function>("select")?,
        tables.get::<Function>("pack")?, tables.get::<Function>("unpack")?))?;
    coroutines.set("resume", resume)?;
    coroutines.set("wrap", wrap)?;
    Ok(())
}

fn bad_target(v: &Value) -> Error {
    Error::runtime(format!(
        "invalid argument (function or thread expected, got {})",
        v.type_name()
    ))
}

#[cfg(test)]
mod ownership_tests {
    use super::*;

    struct Quiet;
    impl OutputSink for Quiet {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    #[derive(Default)]
    struct Trace(RefCell<Vec<String>>);
    impl OutputSink for Trace {
        fn print(&self, line: &str) {
            self.0.borrow_mut().push(line.into());
        }
        fn warn(&self, line: &str) {
            self.0.borrow_mut().push(format!("warn:{line}"));
        }
        fn error(&self, line: &str) {
            self.0.borrow_mut().push(format!("error:{line}"));
        }
    }

    fn assert_cancellation_survives_failed_resume(source: &str) {
        let trace = Rc::new(Trace::default());
        let mut vm =
            crate::vm::ScriptVm::with_output(crate::vm::Context::Server, trace.clone()).unwrap();
        let owner = u64::MAX - 17;
        vm.run_script_owned("RetainOwner", source, Vec::new(), owner)
            .unwrap();
        assert_eq!(*trace.0.borrow(), ["armed"]);
        vm.run_script_owned(
            "Control",
            "task.delay(1,function() print('control') end)",
            Vec::new(),
            owner + 1,
        )
        .unwrap();
        vm.budget().cancel_owner(owner);
        vm.step(1.0);
        assert_eq!(*trace.0.borrow(), ["armed", "control"]);
    }

    #[test]
    fn failed_resume_of_running_task_retains_ownership_until_cancellation() {
        assert_cancellation_survives_failed_resume(
            r#"
local ok=coroutine.resume(coroutine.running())
assert(not ok)
print('armed')
task.wait(1)
print('escaped')
"#,
        );
    }

    #[test]
    fn failed_resume_of_normal_parent_retains_ownership_until_cancellation() {
        assert_cancellation_survives_failed_resume(
            r#"
local parent=coroutine.running()
local nested=coroutine.create(function() assert(not coroutine.resume(parent)) end)
assert(coroutine.resume(nested))
print('armed')
task.wait(1)
print('escaped')
"#,
        );
    }

    #[test]
    fn cancelled_live_thread_is_blocked_without_retaining_dead_weak_keys() {
        let lua = Lua::new();
        let scheduler = Rc::new(Scheduler::new(Rc::new(Quiet)));
        install(&lua, &scheduler).unwrap();
        let function = lua.create_function(|_, ()| Ok(())).unwrap();
        let owner = u64::MAX - 7;
        let (thread, args) = scheduler
            .as_owner(owner, || scheduler.new_thread(&lua, function, Vec::new()))
            .unwrap();
        assert_eq!(scheduler.owner_of(&thread), Some(owner));
        scheduler
            .push(&lua, 10.0, false, thread.clone(), args)
            .unwrap();
        scheduler.cancel_owner(owner - 1);
        assert_eq!(scheduler.pending.borrow().len(), 1);
        scheduler.cancel_owner(owner);
        assert!(scheduler.pending.borrow().is_empty());
        assert!(!scheduler.can_resume(&thread).unwrap());
        assert_eq!(scheduler.owner_of(&thread), None);
        let registry = scheduler.registry().unwrap();
        assert_eq!(registry.clone().pairs::<Value, Value>().count(), 1);
        drop(thread);
        lua.gc_collect().unwrap();
        lua.gc_collect().unwrap();
        assert_eq!(registry.pairs::<Value, Value>().count(), 0);
    }
}

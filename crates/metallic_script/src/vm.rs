//! The sandboxed Luau virtual machine.

use std::borrow::Cow;
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mlua::{
    Error, FromLuaMulti, Function, IntoLuaMulti, Lua, LuaOptions, MultiValue, StdLib, Table,
    Thread, Value, VmState, WeakLua,
};

use crate::task_lib::{self, Scheduler, TIMEOUT_MSG};

/// Execution budget of one resume of a script thread (0.1 s).
pub const RESUME_BUDGET: Duration = Duration::from_millis(100);

/// Total time one [`ScriptVm::step`] may spend resuming threads (0.25 s). Threads still
/// due when it is used up run on the next step.
pub const STEP_BUDGET_SEC: f64 = 0.25;

/// Resume budget after [`use_test_budgets_on_this_thread`] (60 s).
pub const TEST_RESUME_BUDGET: Duration = Duration::from_secs(60);

/// Step budget after [`use_test_budgets_on_this_thread`] (60 s).
pub const TEST_STEP_BUDGET_SEC: f64 = 60.0;

/// Memory cap of one VM (256 MB).
pub const MEMORY_LIMIT_BYTES: usize = 256 * 1024 * 1024;

thread_local! {
    static TEST_BUDGETS: Cell<bool> = const { Cell::new(false) };
}

/// For test rigs: VMs and hosts this thread creates from now on start with
/// [`TEST_RESUME_BUDGET`] and [`TEST_STEP_BUDGET_SEC`] instead of the real limits.
///
/// Both budgets are wall-clock time, so a busy or paging machine can stall a trivial script past
/// them and a test then fails for no reason. Only a test calls this; nothing in the engine, the
/// Player or Studio does, so creators always get the real limits. A test of the limits themselves
/// sets them back on its VM or host, which wins over this: [`ScriptVm::set_resume_budget`] with
/// [`RESUME_BUDGET`], [`ScriptVm::set_step_budget_sec`] with [`STEP_BUDGET_SEC`].
///
/// It is per thread because each `#[test]` normally has its own thread, so a test that does not
/// ask keeps the real limits. A VM stays on the thread that made it (it is not `Send`).
pub fn use_test_budgets_on_this_thread() {
    TEST_BUDGETS.set(true);
}

/// For the `main` of a binary that tests run as a child process: applies
/// [`use_test_budgets_on_this_thread`] if the variable `METALLIC_TEST_BUDGETS` is set. The check
/// exists only in debug builds; a release or `dist` build never reads the variable.
pub fn use_test_budgets_if_a_test_asked() {
    #[cfg(debug_assertions)]
    if std::env::var_os("METALLIC_TEST_BUDGETS").is_some() {
        use_test_budgets_on_this_thread();
    }
}

/// The resume budget a new VM or host starts with.
pub(crate) fn initial_resume_budget() -> Duration {
    if TEST_BUDGETS.get() {
        TEST_RESUME_BUDGET
    } else {
        RESUME_BUDGET
    }
}

/// The step budget a new VM or host starts with.
pub(crate) fn initial_step_budget_sec() -> f64 {
    if TEST_BUDGETS.get() {
        TEST_STEP_BUDGET_SEC
    } else {
        STEP_BUDGET_SEC
    }
}

/// Which side of the game a VM belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    /// The authoritative server VM (runs `Script`s).
    Server,
    /// A player's client VM (runs `LocalScript`s).
    Client,
}

/// A script error, formatted as `<Instance full name>:<line>: <message>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScriptError {
    /// The formatted error text.
    pub message: String,
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ScriptError {}

/// Receives everything scripts print, warn and (for failing threads) report as errors.
pub trait OutputSink {
    /// A `print(...)` line.
    fn print(&self, line: &str);
    /// A `warn(...)` line.
    fn warn(&self, line: &str);
    /// An error raised by a script thread outside `run_script` (e.g. in a delayed task).
    fn error(&self, line: &str);
}

/// Default sink: prints to stdout, warnings and errors to stderr.
struct StdioSink;

impl OutputSink for StdioSink {
    fn print(&self, line: &str) {
        println!("{line}");
    }
    fn warn(&self, line: &str) {
        eprintln!("Warning: {line}");
    }
    fn error(&self, line: &str) {
        eprintln!("{line}");
    }
}

/// Converts script bytes to text, replacing invalid UTF-8.
pub(crate) fn lossy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

/// Turns an `mlua` error into text, dropping tracebacks.
pub(crate) fn format_error(e: &Error) -> String {
    let text = match e {
        Error::CallbackError { cause, .. } => return format_error(cause),
        Error::RuntimeError(m) | Error::MemoryError(m) => m.clone(),
        Error::SyntaxError { message, .. } => message.clone(),
        other => other.to_string(),
    };
    match text.find("\nstack traceback") {
        Some(i) => text[..i].trim_end().to_string(),
        None => text,
    }
}

fn setup_failed(e: Error) -> ScriptError {
    ScriptError {
        message: format!("script VM setup failed: {e}"),
    }
}

/// Lua that bounds `string.rep` (results over 64 MB are rejected, also when the count is
/// a numeric string) and `table.sort` (above 256 elements, comparisons that would stay in C go through a Lua
/// closure so the deadline can interrupt, #274). The pattern functions are bounded in `pattern.rs`.
const STRING_LIMITS: &str = r#"
local string, type, error, tonumber, tostring = string, type, error, tonumber, tostring
local rep = string.rep
local MAX_REP = 67108864
string.rep = function(s, n)
    local count = tonumber(n)
    if count ~= nil and (type(s) == "string" or type(s) == "number") and #tostring(s) * count > MAX_REP then
        error("string.rep: result too large (limit 64 MB)", 2)
    end
    return rep(s, n)
end

-- #274: table.sort runs in C, where the time budget's interrupt cannot fire between
-- comparisons (a C comparator such as rawequal, a C __lt, or long string compares never return
-- to Lua). The sort runs in C only where its cost is bounded or the comparator is a Lua
-- function (already interruptible): a table of at most NUM_C_SORT numbers, or any table with a
-- Lua comparator. Everything else above SORT_VIA_LUA elements goes through a Lua closure per
-- comparison, so each one crosses a Lua call and the deadline check runs. Ordering and
-- "invalid order function" are Luau's own; a type error from the default comparison loses only
-- the closure's position prefix.
local table, string, utf8, rawlen, rawget, type, pcall, error = table, string, utf8, rawlen, rawget, type, pcall, error
local info = debug.info
local sort = table.sort
local SORT_VIA_LUA = 256
local NUM_C_SORT = 262144
local PREFIX = "^%[engine%]:%d+: "
local function finish(ok, ...)
    if ok then return ... end
    local e = ...
    if type(e) == "string" then
        local _, stop = e:find(PREFIX)
        if stop then e = e:sub(stop + 1) end
    end
    error(e, 0)
end
local function all_numbers(t, n)
    for i = 1, n do
        if type(rawget(t, i)) ~= "number" then return false end
    end
    return true
end
table.sort = function(t, comp)
    if type(t) == "table" then
        local n = rawlen(t)
        if n > SORT_VIA_LUA then
            if comp == nil then
                if n <= NUM_C_SORT and all_numbers(t, n) then
                    return sort(t)
                end
                return finish(pcall(sort, t, function(a, b) return a < b end))
            elseif type(comp) == "function" and info(comp, "s") == "[C]" then
                return finish(pcall(sort, t, function(a, b) return comp(a, b) end))
            end
        end
    end
    return sort(t, comp)
end

-- #344: the remaining single C calls whose work grows with their input.
-- table.concat joins ranges of more than CONCAT_CHUNK elements chunk by chunk (each chunk is one
-- C call of bounded size; the loop between chunks is interruptible). The chunking follows the
-- range the C function would walk: bounds are read as it reads them (numbers and numeric
-- strings truncated toward zero), so a fractional bound is chunked too. A bound that overflows
-- 32 bits, or is NaN or infinite, is converted by the platform, so the C function is asked
-- which integer it reads (`c_int`) and the range is chunked from that. Luau has no tail calls, so an error raised by the C function would carry this
-- wrapper's position: every call therefore runs under pcall, and a failure is run once more
-- inside a closure (which gives the C function the same call-site name) and re-raised at the
-- caller's position, so text, index and position are those of the plain function.
local concat = table.concat
local CONCAT_CHUNK = 8192
local function concat_failed(t, sep, i, j)
    local ok, e = pcall(function() return concat(t, sep, i, j) end)
    if ok then return e end
    if type(e) == "string" then
        local _, stop = e:find(PREFIX)
        if stop then e = e:sub(stop + 1) end
    end
    error(e, 3) -- 1 = here, 2 = table.concat, 3 = its caller
end
local floor, ceil, tonumber = math.floor, math.ceil, tonumber
-- Asks the C function which integer it reads for a bound that is NaN, infinite or outside 32
-- bits (the conversion is up to the platform: x86 gives the smallest integer, ARM saturates
-- and turns NaN into 0). A one-element range [x, x] over a table with a value at 0 and 1 and
-- nothing elsewhere answers: "0" or "1", or an error that names the index it read.
local PROBE = { [0] = "0", "1" }
local function c_int(x)
    local ok, r = pcall(concat, PROBE, "", x, x)
    if ok then return r == "0" and 0 or 1 end
    return type(r) == "string" and tonumber(r:match("at index (-?%d+)")) or nil
end
-- The integer the C function reads for a bound (numbers and numeric strings, truncated toward
-- zero), or nil when it is not a number and the C function is to report it.
local function bound_int(x)
    local n = x
    if type(n) == "string" then n = tonumber(n) end
    if type(n) ~= "number" then return nil end
    if n ~= n or n >= 2147483648 or n <= -2147483649 then return c_int(x) end
    if n >= 0 then return floor(n) end
    return ceil(n)
end
table.concat = function(t, sep, i, j)
    if type(t) == "table" then
        local first = i == nil and 1 or bound_int(i)
        local last = j == nil and rawlen(t) or bound_int(j)
        if first and last and last - first >= CONCAT_CHUNK then
            local parts, count = {}, 0
            local from = first
            while from <= last do
                local to = from + CONCAT_CHUNK - 1
                if to > last then to = last end
                local ok, piece = pcall(concat, t, sep, from, to)
                if not ok then
                    if piece == "not enough memory" then error(piece, 0) end
                    return concat_failed(t, sep, from, to)
                end
                count = count + 1
                parts[count] = piece
                from = to + 1
            end
            return concat(parts, sep)
        end
    end
    local ok, r = pcall(concat, t, sep, i, j)
    if ok then return r end
    return concat_failed(t, sep, i, j)
end

-- string.pack / packsize / unpack walk their format string item by item: refuse formats over
-- MAX_FORMAT bytes (real formats are a few dozen), and string.pack also refuses a format whose
-- `c<n>` items add up to more than MAX_PACK bytes (`c200000000`, or 300 x `c999999`: each pads
-- n bytes). Every other item is at most 16 bytes, so 64 KB of format cannot reach MAX_PACK. upper / lower / reverse / utf8.len /
-- utf8.offset make one pass over the string at 3 ns per byte: refuse strings over MAX_TEXT
-- (16 MB). string.split makes one table slot per piece: refuse subjects over MAX_SPLIT (4 MB).
local MAX_FORMAT = 65536
local MAX_TEXT = 16777216
local MAX_SPLIT = 4194304
local MAX_PACK = 16777216
for _, name in { "pack", "packsize", "unpack" } do
    local f = string[name]
    string[name] = function(fmt, ...)
        if type(fmt) == "string" and #fmt > MAX_FORMAT then
            error("string." .. name .. ": format string too long (limit 65536)", 2)
        end
        if name == "pack" and type(fmt) == "string" and fmt:find("c%d") then
            local total = 0
            for digits in fmt:gmatch("c(%d+)") do
                total = total + tonumber(digits)
            end
            if total > MAX_PACK then
                error("string.pack: result too large (limit 16777216 bytes)", 2)
            end
        end
        return f(fmt, ...)
    end
end
for _, name in { "upper", "lower", "reverse" } do
    local f = string[name]
    string[name] = function(s)
        if type(s) == "string" and #s > MAX_TEXT then
            error("string." .. name .. ": string too long (limit 16777216 bytes)", 2)
        end
        return f(s)
    end
end
local utf8_len = utf8.len
utf8.len = function(s, i, j, lax)
    if type(s) == "string" and #s > MAX_TEXT then
        error("utf8.len: string too long (limit 16777216 bytes)", 2)
    end
    return utf8_len(s, i, j, lax)
end
local utf8_offset = utf8.offset
utf8.offset = function(s, n, i)
    if type(s) == "string" and #s > MAX_TEXT then
        error("utf8.offset: string too long (limit 16777216 bytes)", 2)
    end
    return utf8_offset(s, n, i)
end
local split = string.split
if split then
    string.split = function(s, sep)
        if type(s) == "string" and #s > MAX_SPLIT then
            error("string.split: string too long (limit 4194304 bytes)", 2)
        end
        return split(s, sep)
    end
end
"#;

/// A cloneable handle for running Lua functions under the VM's execution budget (see
/// [`ScriptVm::budget`]). It holds the Lua state weakly, so closures registered on the VM
/// may capture it without keeping the VM alive; once the VM is gone its methods return an
/// error.
#[derive(Clone)]
pub struct Budget {
    lua: WeakLua,
    sched: Rc<Scheduler>,
}

impl Budget {
    /// Seconds one step may spend (see [`ScriptVm::set_step_budget_sec`]).
    pub(crate) fn step_budget_sec(&self) -> f64 {
        self.sched.step_budget_sec.get()
    }

    fn lua(&self) -> Result<Lua, ScriptError> {
        self.lua.try_upgrade().ok_or_else(|| ScriptError {
            message: "script VM no longer exists".to_string(),
        })
    }

    /// Calls `f(args)` with a 0.1 s budget (or the enclosing budget's remainder when
    /// called from inside a running script, whichever ends first). Errors, including
    /// timeouts, are returned as [`ScriptError`] with the `<chunk>:<line>:` prefix.
    pub fn call<R: FromLuaMulti>(
        &self,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> Result<R, ScriptError> {
        self.call_as(self.sched.current_owner(), f, args)
    }

    /// Like [`Budget::call`], with `owner` (a script's owner id, 0 for none) as the owner
    /// of the threads and connections the call creates.
    pub(crate) fn call_as<R: FromLuaMulti>(
        &self,
        owner: u64,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> Result<R, ScriptError> {
        let lua = self.lua()?;
        let values = self
            .sched
            .call_guarded(&lua, owner, f, args)
            .map_err(|message| ScriptError { message })?;
        R::from_lua_multi(values, &lua).map_err(|e| ScriptError {
            message: format_error(&e),
        })
    }

    /// The owner id of the script a thread belongs to.
    pub(crate) fn owner_of(&self, thread: &Thread) -> Option<u64> {
        self.sched.owner_of(thread)
    }

    /// True while the debugger has this VM paused (handlers then wait).
    pub(crate) fn paused(&self) -> bool {
        self.sched.debug.is_paused()
    }

    /// The owner id of the script whose code is running now (0 outside any script).
    pub(crate) fn current_owner(&self) -> u64 {
        self.sched.current_owner()
    }

    /// Cancels the sleeping threads of a script.
    pub(crate) fn cancel_owner(&self, owner: u64) {
        self.sched.cancel_owner(owner);
    }

    /// Resumes a thread that parked itself with a plain `coroutine.yield` (for example
    /// a future `Event:Wait()`), under the same budget. If it yields again it is
    /// parked or put to sleep as usual.
    pub fn resume(&self, thread: &Thread, args: impl IntoLuaMulti) -> Result<(), ScriptError> {
        let lua = self.lua()?;
        let args = args.into_lua_multi(&lua).map_err(|e| ScriptError {
            message: format_error(&e),
        })?;
        self.sched
            .resume_with_budget(thread, args.into_iter().collect())
            .map_err(|message| ScriptError { message })
    }
}

/// One sandboxed Luau VM with its own thread scheduler.
pub struct ScriptVm {
    lua: Lua,
    sched: Rc<Scheduler>,
    context: Context,
    /// The read-only library table every script environment inherits from.
    lib: Table,
    /// The writable `_G` shared by all scripts of this VM.
    shared_g: Table,
}

impl ScriptVm {
    /// Creates a VM that prints to stdout/stderr.
    ///
    /// # Errors
    /// Returns an error if the Lua state or its sandbox cannot be set up.
    pub fn new(context: Context) -> Result<ScriptVm, ScriptError> {
        Self::with_output(context, Rc::new(StdioSink))
    }

    /// Creates a VM whose `print`/`warn` and thread errors go to `output`.
    ///
    /// # Errors
    /// Returns an error if the Lua state or its sandbox cannot be set up.
    pub fn with_output(
        context: Context,
        output: Rc<dyn OutputSink>,
    ) -> Result<ScriptVm, ScriptError> {
        let lua = Lua::new_with(StdLib::ALL_SAFE, LuaOptions::default()).map_err(setup_failed)?;
        let sched = Rc::new(Scheduler::new(output));
        let lib = Self::setup(&lua, &sched).map_err(setup_failed)?;
        let shared_g = lua.create_table().map_err(setup_failed)?;
        Ok(ScriptVm {
            lua,
            sched,
            context,
            lib,
            shared_g,
        })
    }

    fn setup(lua: &Lua, sched: &Rc<Scheduler>) -> mlua::Result<Table> {
        let globals = lua.globals();
        let tostring: Function = globals.get("tostring")?;
        // Full debug info (local and upvalue names) for the debugger; no runtime cost.
        lua.set_compiler(mlua::chunk::Compiler::new().set_debug_level(2));

        // Needs `debug`, `xpcall`, `coroutine`: must run before they are removed.
        task_lib::install(lua, sched)?;
        crate::pattern::install(lua, sched)?;
        lua.load(STRING_LIMITS).set_name("=[engine]").exec()?;

        // Remove everything the sandbox forbids.
        for name in [
            "io",
            "debug",
            "loadstring",
            "load",
            "getfenv",
            "setfenv",
            "require",
            "dofile",
            "loadfile",
            "package",
            "newproxy",
        ] {
            globals.set(name, Value::Nil)?;
        }
        let os = lua.create_table()?;
        let real_os: Table = globals.get("os")?;
        for name in ["clock", "time"] {
            os.set(name, real_os.get::<Value>(name)?)?;
        }
        globals.set("os", os)?;

        let real_collect: Function = globals.get("collectgarbage")?;
        globals.set(
            "collectgarbage",
            lua.create_function(move |_, option: Option<String>| match option.as_deref() {
                Some("count") => real_collect.call::<f64>("count"),
                _ => Err(Error::runtime("collectgarbage: only \"count\" is allowed")),
            })?,
        )?;

        // print / warn go to the sink (invalid UTF-8 is replaced).
        let join = Rc::new(move |args: MultiValue| -> mlua::Result<String> {
            let mut parts = Vec::with_capacity(args.len());
            for v in args {
                let s: mlua::LuaString = tostring.call(v)?;
                parts.push(lossy(&s.as_bytes()));
            }
            Ok(parts.join(" "))
        });
        let (s, j) = (sched.clone(), join.clone());
        globals.set(
            "print",
            lua.create_function(move |_, args: MultiValue| {
                s.sink.print(&j(args)?);
                Ok(())
            })?,
        )?;
        let s = sched.clone();
        globals.set(
            "warn",
            lua.create_function(move |_, args: MultiValue| {
                s.sink.warn(&join(args)?);
                Ok(())
            })?,
        )?;

        // Execution budget: abort the running thread once its deadline has passed. The
        // position is remembered so the timeout error can carry `<chunk>:<line>:`.
        let s = sched.clone();
        lua.set_interrupt(move |lua| match s.deadline.get() {
            Some(d) if Instant::now() >= d => {
                if s.timeout_loc.borrow().is_none() {
                    let loc = script_location(lua, 0);
                    *s.timeout_loc.borrow_mut() = Some(loc);
                }
                Err(Error::runtime(TIMEOUT_MSG))
            }
            _ => Ok(VmState::Continue),
        });

        lua.set_memory_limit(MEMORY_LIMIT_BYTES)?;

        // The string metatable is shared by every string: make it read-only.
        let string_mt: Table = lua.load("return getmetatable('')").eval()?;
        string_mt.set_readonly(true);

        let lib = globals.clone();
        lua.sandbox(true)?;
        Ok(lib)
    }

    /// The tables every script environment is made from (`lib` and `_G`), for code that loads
    /// chunks itself (`require`).
    pub(crate) fn env_parts(&self) -> (Table, Table) {
        (self.lib.clone(), self.shared_g.clone())
    }

    /// Which side this VM runs on.
    pub fn context(&self) -> Context {
        self.context
    }

    /// The underlying `mlua::Lua`, for host APIs (task 7) and the debugger (task 15),
    /// which reaches the raw `lua_State` through `mlua::ffi`.
    ///
    /// Calls made directly through this handle run **without** an execution budget; use
    /// [`ScriptVm::call_with_budget`] / [`ScriptVm::budget`] for anything that runs
    /// script code. The budget uses `Lua::set_interrupt`; Luau's debugger callbacks
    /// (`lua_callbacks`: `debugbreak`, `debugstep`) are separate and do not conflict.
    pub fn lua(&self) -> &Lua {
        &self.lua
    }

    /// Sets how long one resume may run before it times out ([`RESUME_BUDGET`] by default).
    /// Test rigs raise it with [`use_test_budgets_on_this_thread`]; tests of the timeout itself
    /// set [`RESUME_BUDGET`] here.
    pub fn set_resume_budget(&self, budget: Duration) {
        self.sched.resume_budget.set(budget);
    }

    /// Sets how many seconds one step may spend before the rest waits for the next step
    /// ([`STEP_BUDGET_SEC`] by default). See [`use_test_budgets_on_this_thread`].
    pub fn set_step_budget_sec(&self, seconds: f64) {
        self.sched.step_budget_sec.set(seconds);
    }

    /// Adds (or replaces) a global visible to every script of this VM, e.g. `game` or
    /// `Instance`. The library table is read-only to scripts; only the host can write it.
    pub fn set_global(&self, name: &str, value: impl mlua::IntoLua) -> Result<(), ScriptError> {
        let to_err = |e: Error| ScriptError {
            message: format_error(&e),
        };
        self.lib.set_readonly(false);
        let result = self.lib.set(name, value).map_err(to_err);
        self.lib.set_readonly(true);
        result
    }

    /// A handle for calling functions and resuming parked threads under the budget. It
    /// can be moved into host callbacks registered on [`ScriptVm::lua`].
    pub fn budget(&self) -> Budget {
        Budget {
            lua: self.lua.weak(),
            sched: self.sched.clone(),
        }
    }

    /// Calls `f(args)` under the execution budget; see [`Budget::call`].
    pub fn call_with_budget<R: FromLuaMulti>(
        &self,
        f: &Function,
        args: impl IntoLuaMulti,
    ) -> Result<R, ScriptError> {
        self.budget().call(f, args)
    }

    /// Compiles and runs `source` as a new script thread named `chunk_name` (the
    /// instance's full name). Returns when the script finishes or first yields
    /// (e.g. in `task.wait`); the rest runs from [`ScriptVm::step`].
    ///
    /// Each script gets its own global environment (assignments to globals stay local
    /// to the script); library tables are shared and read-only, and `_G` is one
    /// writable table shared by all scripts of this VM.
    pub fn run_script(&mut self, chunk_name: &str, source: &str) -> Result<(), ScriptError> {
        self.run_script_with(chunk_name, source, Vec::new())
    }

    /// Like [`ScriptVm::run_script`], and also defines `globals` (for example `script`) in
    /// this script's own environment only.
    pub fn run_script_with(
        &mut self,
        chunk_name: &str,
        source: &str,
        globals: Vec<(&str, Value)>,
    ) -> Result<(), ScriptError> {
        self.run_script_owned(chunk_name, source, globals, 0)
    }

    /// Like [`ScriptVm::run_script_with`]. Every thread and connection the script creates
    /// carries `owner` (not 0), so the host can stop exactly this script later even when
    /// other scripts share its chunk name.
    pub fn run_script_owned(
        &mut self,
        chunk_name: &str,
        source: &str,
        globals: Vec<(&str, Value)>,
        owner: u64,
    ) -> Result<(), ScriptError> {
        let to_err = |e: Error| ScriptError {
            message: format_error(&e),
        };
        let env = script_env(&self.lua, &self.lib, &self.shared_g, globals).map_err(to_err)?;
        let function = self
            .lua
            .load(source)
            .set_name(format!("={chunk_name}"))
            .set_environment(env.clone())
            .into_function()
            .map_err(to_err)?;
        self.sched
            .debug
            .breakpoints
            .on_load(&self.lua, owner, &function, &env);
        let (thread, args) = self
            .sched
            .as_owner(owner, || {
                self.sched.new_thread(&self.lua, function, Vec::new())
            })
            .map_err(to_err)?;
        let result = self.sched.resume_with_budget(&thread, args);
        if result.is_err() {
            // Release whatever the failed script allocated (e.g. after the memory cap).
            let _ = self.lua.gc_collect();
        }
        result.map_err(|message| ScriptError { message })
    }

    /// Advances the VM clock to `now` (seconds) and resumes due threads (at most
    /// [`STEP_BUDGET_SEC`] of work per call). Errors in those threads are reported to
    /// the output sink.
    pub fn step(&mut self, now: f64) {
        self.sched.step(now);
    }

    /// The scheduler, for the debugger.
    pub(crate) fn sched(&self) -> &Scheduler {
        &self.sched
    }

    /// Drops the VM together with every pending `task.wait`/`task.delay`.
    pub fn shutdown(self) {
        self.sched.clear();
    }
}

/// A script environment: an empty table that reads through to the library, with the shared `_G`
/// and `globals` (for example `script`) set in it.
pub(crate) fn script_env(
    lua: &Lua,
    lib: &Table,
    shared_g: &Table,
    globals: Vec<(&str, Value)>,
) -> mlua::Result<Table> {
    let env = lua.create_table()?;
    let meta = lua.create_table()?;
    meta.set("__index", lib.clone())?;
    env.set_metatable(Some(meta))?;
    env.set("_G", shared_g.clone())?;
    for (name, value) in globals {
        env.set(name, value)?;
    }
    Ok(env)
}

/// `"<chunk>:<line>: "` of the nearest script frame at or above stack `level`, skipping C
/// and engine frames; empty if there is none.
pub(crate) fn script_location(lua: &Lua, level: usize) -> String {
    for l in level..level + 8 {
        let found = lua.inspect_stack(l, |dbg| {
            let src = dbg
                .source()
                .short_src
                .map(Cow::into_owned)
                .unwrap_or_default();
            (src, dbg.current_line())
        });
        match found {
            None => break,
            Some((src, Some(line))) if !src.starts_with('[') && !src.is_empty() => {
                return format!("{src}:{line}: ");
            }
            Some(_) => {}
        }
    }
    String::new()
}

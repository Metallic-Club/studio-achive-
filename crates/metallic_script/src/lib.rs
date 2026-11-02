//! Sandboxed Luau scripting for the Metallic Engine.
//!
//! [`ScriptVm`] wraps one Luau virtual machine (via `mlua`) with the engine's sandbox
//! rules: no `io`, `debug`, `require`, `load`/`loadstring` or `getfenv`/`setfenv`, only
//! `os.clock` and `os.time`, `collectgarbage("count")` only, a 0.1 s execution budget per
//! resume and a 256 MB memory cap. Scripts run as coroutines, so `task.wait`,
//! `task.spawn` and `task.delay` work; the host drives them by calling [`ScriptVm::step`].
//! Queued task threads reject manual coroutine resumes. User coroutines that call
//! `task.wait` are adopted until the wait ends; ordinary yields and return/error values
//! retain their coroutine semantics without exposing scheduler markers.
//!
//! [`ScriptHost`] sits on top: it runs a world's `Script`s and `LocalScript`s in a server
//! and a client VM and gives them the Roblox-named API (`game`, `Instance`, services,
//! signals, `RemoteEvent`; see `docs/luau-api.md`).
//! [`ScriptHost::new_server`] selects only the server VM, without synthetic local-player
//! setup or client-script execution. This scripting role adds no network or server process.
//! [`ScriptHost::with_role`] selects explicit [`ScriptRole`]s, including a Client VM with
//! deferred local-player binding. Player registration/removal uses trusted World-local IDs.
//!
//! Instance members come from the class registry: `api/instance.rs` resolves a name through the
//! instance's class and calls `HostApi` (`api/host_api.rs`), so a member a class file declares is
//! visible to scripts with no change here. [`host_methods`] is the table of methods core declares
//! `Host` and this crate implements.
//!
//! ## Budgets in tests
//!
//! The execution budgets are wall-clock time, so a busy or paging machine can stall a trivial
//! script past them. A test rig calls [`use_test_budgets_on_this_thread`] before it makes a VM,
//! host, `Game` or Play session; a test of the limits sets them back. A binary that tests run as
//! a child process gets the same from [`use_test_budgets_if_a_test_asked`]. Nothing shipped calls
//! the first, and the second reads its variable in debug builds only, so creators always get the
//! real limits.

#![warn(missing_docs)]

mod api;
mod debug;
mod host;
mod pattern;
mod task_lib;
mod vm;

#[doc(hidden)]
pub use api::FixtureApi;
pub use api::host_methods::{
    HostCall, HostMethodFn, HostState, NOT_MIGRATED_HOST_METHODS, host_methods,
};
pub use api::{
    BoundResult, InputActionEvent, InputDevice, InputEventKind, InputValue, KEY_CODES,
    MAX_ACTION_EVENTS_PER_STEP, TouchButtonInfo, USER_INPUT_TYPES,
};
pub use api::{GuiEvent, GuiEventKind, GuiPointer, MAX_GUI_EVENTS_PER_STEP};
pub use debug::{Breakpoint, DebugState, Debugger, Frame, PauseReason, Scope, VarHandle, VarNode};
pub use host::{LOCAL_PLAYER_NAME, ScriptHost, ScriptRole};
pub use vm::{
    Budget, Context, MEMORY_LIMIT_BYTES, OutputSink, RESUME_BUDGET, STEP_BUDGET_SEC, ScriptError,
    ScriptVm, TEST_RESUME_BUDGET, TEST_STEP_BUDGET_SEC, use_test_budgets_if_a_test_asked,
    use_test_budgets_on_this_thread,
};

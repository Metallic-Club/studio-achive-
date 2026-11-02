//! Behaviour tests for the sandboxed Luau VM.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use metallic_script::{Context, OutputSink, ScriptVm};
use mlua::{Function, Thread};

#[derive(Default)]
struct Capture {
    lines: RefCell<Vec<String>>,
}

impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.lines.borrow_mut().push(format!("print:{line}"));
    }
    fn warn(&self, line: &str) {
        self.lines.borrow_mut().push(format!("warn:{line}"));
    }
    fn error(&self, line: &str) {
        self.lines.borrow_mut().push(format!("error:{line}"));
    }
}

fn vm() -> (ScriptVm, Rc<Capture>) {
    metallic_script::use_test_budgets_on_this_thread();
    let sink = Rc::new(Capture::default());
    (
        ScriptVm::with_output(Context::Server, sink.clone()).unwrap(),
        sink,
    )
}

/// A VM on the real limits, for the tests of those limits.
fn strict_vm() -> (ScriptVm, Rc<Capture>) {
    let (vm, sink) = vm();
    vm.set_resume_budget(metallic_script::RESUME_BUDGET);
    vm.set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
    (vm, sink)
}

#[test]
fn runs_basic_code() {
    let (mut vm, sink) = vm();
    vm.run_script("Workspace.Ok", "print('hi', 1 + 2)").unwrap();
    assert_eq!(*sink.lines.borrow(), vec!["print:hi 3".to_string()]);
}

#[test]
fn forbidden_globals_are_nil() {
    let (mut vm, _) = vm();
    for name in [
        "io",
        "debug",
        "loadstring",
        "load",
        "getfenv",
        "setfenv",
        "require",
        "dofile",
    ] {
        vm.run_script("T", &format!("assert({name} == nil, '{name}')"))
            .unwrap();
    }
    vm.run_script(
        "T",
        "assert(os.execute == nil and os.getenv == nil and os.remove == nil and os.date == nil)",
    )
    .unwrap();
    vm.run_script(
        "T",
        "assert(type(os.clock()) == 'number' and type(os.time()) == 'number')",
    )
    .unwrap();
    vm.run_script("T", "assert(type(collectgarbage('count')) == 'number')")
        .unwrap();
    assert!(vm.run_script("T", "collectgarbage('collect')").is_err());
}

#[test]
fn endless_loop_times_out() {
    let (mut vm, sink) = strict_vm();
    let start = Instant::now();
    let err = vm
        .run_script("Workspace.Loop", "while true do end")
        .unwrap_err();
    assert!(start.elapsed() < Duration::from_secs(1));
    assert!(
        err.message
            .contains("Script timeout: exhausted allowed execution time"),
        "{}",
        err.message
    );
    vm.run_script("Workspace.Next", "print('still alive')")
        .unwrap();
    assert_eq!(sink.lines.borrow().last().unwrap(), "print:still alive");
}

#[test]
fn memory_cap_stops_script() {
    let (mut vm, _) = vm();
    let err = vm
        .run_script(
            "Workspace.Hog",
            "local t = {} t[1] = table.create(2 ^ 22) t[2] = table.create(2 ^ 24)",
        )
        .unwrap_err();
    assert!(err.message.contains("not enough memory"), "{}", err.message);
    vm.run_script("Workspace.After", "local x = 1 + 1").unwrap();
}

#[test]
fn error_message_has_chunk_and_line() {
    let (mut vm, _) = vm();
    let err = vm
        .run_script("Workspace.Bad.Script", "local x = nil + 1")
        .unwrap_err();
    assert!(
        err.message.starts_with("Workspace.Bad.Script:1:"),
        "{}",
        err.message
    );
    assert!(
        err.message.contains("attempt to perform arithmetic"),
        "{}",
        err.message
    );
    let err = vm
        .run_script("Workspace.Bad.Script", "syntax error here")
        .unwrap_err();
    assert!(
        err.message.starts_with("Workspace.Bad.Script:1:"),
        "{}",
        err.message
    );
}

#[test]
fn task_wait_resumes_after_step() {
    let (mut vm, sink) = vm();
    vm.run_script(
        "T",
        "print('a') local e = task.wait(1) print('b', e >= 1) task.spawn(function(x) print('spawn', x) end, 7)
         task.delay(2, function(x) print('delayed', x) end, 'z')",
    )
    .unwrap();
    assert_eq!(sink.lines.borrow().len(), 1);
    vm.step(0.5);
    assert_eq!(sink.lines.borrow().len(), 1);
    vm.step(1.0);
    assert_eq!(
        *sink.lines.borrow(),
        vec!["print:a", "print:b true", "print:spawn 7"]
    );
    vm.step(3.0);
    assert_eq!(sink.lines.borrow().last().unwrap(), "print:delayed z");
}

#[test]
fn shutdown_runs_no_callback_and_releases_everything() {
    let (mut vm, sink) = vm();
    vm.run_script(
        "T",
        "task.delay(1, function() print('late') end) task.spawn(function() task.wait(1) print('late2') end)",
    )
    .unwrap();
    assert!(Rc::strong_count(&sink) > 1);
    vm.shutdown();
    // The VM (and with it every closure holding the sink) is gone: nothing can run.
    assert_eq!(Rc::strong_count(&sink), 1, "VM state leaked after shutdown");
    assert!(sink.lines.borrow().is_empty(), "{:?}", sink.lines.borrow());
}

#[test]
fn thread_error_goes_to_sink() {
    let (mut vm, sink) = vm();
    vm.run_script("Workspace.T", "task.delay(1, function() error('boom') end)")
        .unwrap();
    vm.step(2.0);
    let lines = sink.lines.borrow();
    assert!(
        lines[0].starts_with("error:Workspace.T:1: boom"),
        "{lines:?}"
    );
}

#[test]
fn timeout_error_has_chunk_prefix() {
    let (mut vm, sink) = strict_vm();
    let err = vm
        .run_script("Workspace.Loop", "local x = 1\n\nwhile true do end")
        .unwrap_err();
    assert!(
        err.message.starts_with("Workspace.Loop:3: Script timeout"),
        "{}",
        err.message
    );
    // Same inside a stepped thread (goes to the sink).
    vm.run_script("Workspace.Late", "task.wait(1)\nwhile true do end")
        .unwrap();
    vm.step(2.0);
    assert!(
        sink.lines.borrow()[0].starts_with("error:Workspace.Late:2: Script timeout"),
        "{:?}",
        sink.lines.borrow()
    );
    // Errors raised by Rust callbacks carry the position too.
    let err = vm
        .run_script("T", "\ncollectgarbage('collect')")
        .unwrap_err();
    assert!(
        err.message.starts_with("T:2: collectgarbage"),
        "{}",
        err.message
    );
}

#[test]
fn table_error_is_formatted_safely() {
    let (mut vm, sink) = vm();
    let e = vm.run_script("T", "error({})").unwrap_err();
    assert_eq!(e.message, "T:1: <table>");
    let e = vm.run_script("T", "error(nil)").unwrap_err();
    assert_eq!(e.message, "T:1: nil");
    let e = vm.run_script("T", "\nerror('x', 0)").unwrap_err();
    assert_eq!(e.message, "T:2: x");
    let e = vm
        .run_script(
            "T",
            "error(setmetatable({}, {__tostring = function() print('RAN') return 'evil' end}))",
        )
        .unwrap_err();
    assert_eq!(e.message, "T:1: <table>");
    assert!(
        sink.lines.borrow().is_empty(),
        "script __tostring ran: {:?}",
        sink.lines.borrow()
    );
}

#[test]
fn nested_resume_keeps_outer_budget() {
    let (mut vm, _) = strict_vm();
    let budget = vm.budget();
    let host = vm
        .lua()
        .create_function(move |_, f: Function| {
            budget.call::<()>(&f, ()).map_err(mlua::Error::external)
        })
        .unwrap();
    vm.set_global("host_call", host).unwrap();
    let start = Instant::now();
    let err = vm
        .run_script("T", "host_call(function() end) while true do end")
        .unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn call_with_budget_times_out_and_returns_values() {
    let (mut vm, _) = strict_vm();
    let f: Function = vm
        .lua()
        .load("return function(a, b) return a + b end")
        .eval()
        .unwrap();
    assert_eq!(vm.call_with_budget::<i32>(&f, (2, 3)).unwrap(), 5);
    let g: Function = vm
        .lua()
        .load("return function() while true do end end")
        .eval()
        .unwrap();
    let err = vm.call_with_budget::<()>(&g, ()).unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    vm.run_script("T", "print('ok')").unwrap();
}

#[test]
fn many_threads_cannot_stall_one_step() {
    let (mut vm, _) = strict_vm();
    for i in 0..60 {
        vm.run_script(
            &format!("T{i}"),
            "task.wait() local t = os.clock() while os.clock() - t < 0.05 do end",
        )
        .unwrap();
    }
    let start = Instant::now();
    vm.step(1.0);
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    // The rest keeps running on later steps.
    let again = Instant::now();
    vm.step(2.0);
    assert!(again.elapsed() > Duration::from_millis(100));
}

#[test]
fn pathological_pattern_is_rejected_or_budgeted() {
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    for src in [
        "string.find(string.rep('a', 100000), 'a-a-a-a-a-b')",
        "string.match(string.rep('a', 100000), '.-.-.-.-c')",
        "string.rep('xxxxxxxxxx', 100000000)",
        "(('x'):rep(1000)):rep(1000000)",
        "string.format('%999999999d', 1)",
    ] {
        let err = vm.run_script("T", src).unwrap_err();
        assert!(err.message.starts_with("T:1:"), "{src}: {}", err.message);
    }
    assert!(
        start.elapsed() < Duration::from_secs(1),
        "{:?}",
        start.elapsed()
    );
    vm.run_script(
        "T",
        "assert(('hello world'):find('o w') == 5 and ('abc'):rep(3) == 'abcabcabc')",
    )
    .unwrap();
    vm.run_script(
        "T",
        "assert(string.match('  hi  ', '^%s*(.-)%s*$') == 'hi')",
    )
    .unwrap();
}

#[test]
fn script_cannot_shadow_library_for_others() {
    let (mut vm, sink) = vm();
    vm.run_script(
        "A",
        "task = nil print = function() end string = nil mine = 1",
    )
    .unwrap();
    vm.run_script(
        "B",
        "assert(task ~= nil and string ~= nil and mine == nil) print('b ok')",
    )
    .unwrap();
    assert_eq!(*sink.lines.borrow(), vec!["print:b ok".to_string()]);
}

#[test]
#[allow(non_snake_case)]
fn shared_G_is_writable() {
    let (mut vm, sink) = vm();
    vm.run_script("A", "_G.score = 5 _G.list = {1, 2}").unwrap();
    vm.run_script("B", "_G.score += 1 print(_G.score, #_G.list)")
        .unwrap();
    assert_eq!(*sink.lines.borrow(), vec!["print:6 2".to_string()]);
}

#[test]
fn string_metatable_is_read_only() {
    let (mut vm, _) = vm();
    assert!(vm.run_script("T", "getmetatable('').__index = {}").is_err());
    assert!(
        vm.run_script("T", "getmetatable('').__add = function() end")
            .is_err()
    );
    assert!(vm.run_script("T", "string.upper = nil").is_err());
    vm.run_script("T", "assert(('x'):upper() == 'X')").unwrap();
}

#[test]
fn rawset_on_library_fails() {
    let (mut vm, _) = vm();
    assert!(vm.run_script("T", "rawset(string, 'evil', 1)").is_err());
    assert!(vm.run_script("T", "rawset(task, 'wait', nil)").is_err());
    assert!(vm.run_script("T", "table.insert = nil").is_err());
    assert!(vm.run_script("T", "setmetatable(math, {})").is_err());
    vm.run_script("T", "assert(task.wait ~= nil and string.evil == nil)")
        .unwrap();
}

#[test]
fn pcall_wrapped_loop_still_times_out() {
    let (mut vm, _) = strict_vm();
    let start = Instant::now();
    let err = vm
        .run_script(
            "T",
            "while true do pcall(function() while true do end end) end",
        )
        .unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    let err = vm
        .run_script(
            "T",
            "pcall(function() while true do end end) print('survived')",
        )
        .unwrap_err();
    assert!(err.message.contains("Script timeout"), "{}", err.message);
    assert!(start.elapsed() < Duration::from_secs(1));
}

#[test]
fn print_accepts_invalid_utf8() {
    let (mut vm, sink) = vm();
    vm.run_script("T", "print('a\\255b')").unwrap();
    assert_eq!(sink.lines.borrow()[0], "print:a\u{fffd}b");
}

#[test]
fn parked_thread_resumes_through_budget_handle() {
    let (mut vm, sink) = vm();
    let slot: Rc<RefCell<Option<Thread>>> = Rc::default();
    let s = slot.clone();
    let capture = vm
        .lua()
        .create_function(move |_, t: Thread| {
            *s.borrow_mut() = Some(t);
            Ok(())
        })
        .unwrap();
    vm.set_global("capture", capture).unwrap();
    vm.run_script(
        "T",
        "capture(task.spawn(function() local v = coroutine.yield() print('got', v) end))",
    )
    .unwrap();
    vm.step(100.0); // a parked thread is not woken by time
    assert!(sink.lines.borrow().is_empty());
    let thread = slot.borrow().clone().unwrap();
    vm.budget().resume(&thread, 42).unwrap();
    assert_eq!(*sink.lines.borrow(), vec!["print:got 42".to_string()]);
}

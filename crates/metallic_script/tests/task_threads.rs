//! Scheduler ownership, opaque task bookkeeping and native coroutine compatibility.

mod common;

use metallic_script::{Context, OutputSink, ScriptVm};
use mlua::Function;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Capture(RefCell<Vec<String>>);
impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(format!("print:{line}"));
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warn:{line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error:{line}"));
    }
}

fn rig() -> (ScriptVm, Rc<Capture>) {
    let output = Rc::new(Capture::default());
    metallic_script::use_test_budgets_on_this_thread();
    let vm = ScriptVm::with_output(Context::Server, output.clone()).unwrap();
    (vm, output)
}

#[test]
fn queued_delay_rejects_foreign_resume_and_keeps_original_function_arguments() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Queued",
        r#"
_G.ran = false
local thread = task.delay(1, function(...)
    local args = table.pack(...)
    assert(args.n == 4 and args[1]=='due' and args[2]==nil and args[3]=='last' and args[4]==nil)
    _G.ran = true
    print('scheduled')
end, 'due', nil, 'last', nil)
local values = table.pack(coroutine.resume(thread, function() print('hijacked') end))
assert(values[1] == false, 'queued resume unexpectedly succeeded')
assert(not _G.ran)
"#,
    )
    .unwrap();
    vm.step(0.99);
    assert!(output.0.borrow().is_empty(), "{:?}", output.0.borrow());
    vm.step(1.0);
    assert_eq!(*output.0.borrow(), ["print:scheduled"]);
}

#[test]
fn waiting_task_rejects_foreign_elapsed_values() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Waiting",
        r#"
local thread = task.spawn(function()
    local elapsed = task.wait(1)
    assert(elapsed == 1, tostring(elapsed))
    print('woke')
end)
local ok = coroutine.resume(thread, 999)
assert(not ok, 'waiting resume unexpectedly succeeded')
"#,
    )
    .unwrap();
    vm.step(1.0);
    assert_eq!(*output.0.borrow(), ["print:woke"]);
}

#[test]
fn user_coroutine_wait_hides_internal_tags_and_is_resumed_by_the_scheduler() {
    let (mut vm, output) = rig();
    vm.run_script(
        "UserWait",
        r#"
_G.child = coroutine.create(function()
    local elapsed = task.wait(1)
    print('user-woke:'..elapsed)
    return 'finished', nil
end)
local result = table.pack(coroutine.resume(_G.child))
assert(result.n == 1 and result[1] == true, 'internal wait values escaped')
"#,
    )
    .unwrap();
    vm.step(0.99);
    assert!(output.0.borrow().is_empty());
    vm.step(1.0);
    assert_eq!(*output.0.borrow(), ["print:user-woke:1"]);
    vm.run_script("Done", "assert(coroutine.status(_G.child) == 'dead')")
        .unwrap();
}

#[test]
fn native_wrap_cannot_bypass_ownership_of_an_adopted_thread() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Wrapped",
        r#"
local wrapped = coroutine.wrap(function()
    _G.child = coroutine.running()
    local a,b,c = coroutine.yield('parked')
    assert(a=='due' and b==nil and c=='last')
    print('wrapped-due')
end)
assert(wrapped() == 'parked')
task.delay(1, _G.child, 'due', nil, 'last')
local ok = pcall(wrapped, 'foreign')
assert(not ok, 'wrapped function bypassed queued ownership')
"#,
    )
    .unwrap();
    vm.step(1.0);
    assert_eq!(*output.0.borrow(), ["print:wrapped-due"]);
}

#[test]
fn wait_coerces_numeric_strings_instead_of_scheduling_them_at_zero() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Coerce",
        "task.spawn(function() local elapsed=task.wait('5') assert(elapsed==5) print('five') end)",
    )
    .unwrap();
    vm.step(0.0);
    vm.step(4.99);
    assert!(output.0.borrow().is_empty(), "{:?}", output.0.borrow());
    vm.step(5.0);
    assert_eq!(*output.0.borrow(), ["print:five"]);
}

#[test]
fn delay_defaults_nil_duration_to_zero() {
    let (mut vm, output) = rig();
    vm.run_script(
        "NilDelay",
        "task.delay(nil, function() print('nil-due') end)",
    )
    .unwrap();
    assert!(output.0.borrow().is_empty());
    vm.step(0.0);
    assert_eq!(*output.0.borrow(), ["print:nil-due"]);
}

#[test]
fn delay_preserves_existing_numeric_string_coercion() {
    let (mut vm, output) = rig();
    vm.run_script(
        "StringDelay",
        "task.delay('2.5', function() print('string-due') end)",
    )
    .unwrap();
    vm.step(2.49);
    assert!(output.0.borrow().is_empty());
    vm.step(2.5);
    assert_eq!(*output.0.borrow(), ["print:string-due"]);
}

#[test]
fn adopted_waiter_releases_ownership_for_a_later_ordinary_yield() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Adopt",
        r#"
_G.child = coroutine.create(function()
    assert(task.wait(1) == 1)
    local args = table.pack(coroutine.yield('ordinary',nil,'yielded'))
    assert(args.n==3 and args[1]=='manual' and args[2]==nil and args[3]=='last')
    return 'returned',nil,'last',nil
end)
local first=table.pack(coroutine.resume(_G.child))
assert(first.n==1 and first[1])
"#,
    )
    .unwrap();
    vm.step(1.0);
    vm.run_script("Manual", r#"
local values=table.pack(coroutine.resume(_G.child,'manual',nil,'last'))
assert(values.n==5 and values[1] and values[2]=='returned' and values[3]==nil and values[4]=='last' and values[5]==nil)
"#).unwrap();
    assert!(output.0.borrow().is_empty(), "{:?}", output.0.borrow());
}

#[test]
fn duplicate_admission_is_rejected_and_closed_queued_threads_are_silent() {
    let (mut vm, output) = rig();
    vm.run_script(
        "Queue",
        r#"
local thread=task.delay(1,function() print('once') end)
assert(not pcall(task.spawn,thread))
assert(not pcall(task.delay,2,thread))
local closed=task.delay(1,function() print('closed-ran') end)
assert(coroutine.close(closed))
"#,
    )
    .unwrap();
    vm.step(1.0);
    vm.step(2.0);
    assert_eq!(*output.0.borrow(), ["print:once"]);
}

#[test]
fn ordinary_coroutines_preserve_opaque_rust_error_object_identity() {
    let (mut vm, output) = rig();
    let failure = vm
        .lua()
        .create_function(|_, ()| Err::<(), _>(mlua::Error::runtime("opaque host failure")))
        .unwrap();
    vm.set_global("native_failure", failure).unwrap();
    vm.run_script(
        "Identity",
        r#"
local ok, marker=pcall(native_failure)
assert(not ok and type(marker)=='userdata')
local child=coroutine.create(function() error(marker,0) end)
local resumed, value=coroutine.resume(child)
assert(not resumed and rawequal(value,marker),'resume changed the error object')
local wrapped=coroutine.wrap(function() error(marker,0) end)
local wrapped_ok, wrapped_value=pcall(wrapped)
assert(not wrapped_ok and rawequal(wrapped_value,marker),'wrap changed the error object')
"#,
    )
    .unwrap();
    assert!(output.0.borrow().is_empty());
}

#[test]
fn retained_thread_and_budget_handles_do_not_keep_the_vm_alive() {
    let (mut vm, _) = rig();
    let held = Rc::new(RefCell::new(None::<mlua::Thread>));
    let target = held.clone();
    let hold = vm
        .lua()
        .create_function(move |_, thread: mlua::Thread| {
            *target.borrow_mut() = Some(thread);
            Ok(())
        })
        .unwrap();
    vm.set_global("hold", hold).unwrap();
    vm.run_script("Lifetime", "hold(task.delay(100,function() end))")
        .unwrap();
    let weak = vm.lua().weak();
    let function = vm.lua().create_function(|_, ()| Ok(())).unwrap();
    let budget = vm.budget();
    drop(vm);
    assert!(weak.try_upgrade().is_none());
    assert!(budget.call::<()>(&function, ()).is_err());
    drop(held);
}

#[test]
fn wrapped_waits_do_not_expose_tags_and_resume_at_the_scheduled_time() {
    let (mut vm, output) = rig();
    vm.run_script(
        "WrapWait",
        r#"
local wrapped=coroutine.wrap(function()
    assert(task.wait('1')==1)
    print('wrapped-woke')
end)
local initial=table.pack(wrapped())
assert(initial.n==0,'wrapped wait exposed bookkeeping values')
"#,
    )
    .unwrap();
    vm.step(0.99);
    assert!(output.0.borrow().is_empty());
    vm.step(1.0);
    assert_eq!(*output.0.borrow(), ["print:wrapped-woke"]);
}

#[test]
fn stale_queue_lease_cannot_resume_a_newer_wait_early() {
    let (mut vm, output) = rig();
    let held = Rc::new(RefCell::new(None::<mlua::Thread>));
    let target = held.clone();
    let hold = vm
        .lua()
        .create_function(move |_, thread: mlua::Thread| {
            *target.borrow_mut() = Some(thread);
            Ok(())
        })
        .unwrap();
    vm.set_global("hold", hold).unwrap();
    vm.run_script(
        "Lease",
        r#"
hold(task.spawn(function()
    assert(task.wait(1)==0.25)
    print('first')
    assert(task.wait(5)==5)
    print('second')
end))
"#,
    )
    .unwrap();
    let thread = held.borrow().clone().unwrap();
    // A trusted host resume supersedes the original queue lease. Its stale due
    // entry must not become valid again when the thread queues another wait.
    vm.budget().resume(&thread, 0.25).unwrap();
    vm.step(1.0);
    vm.step(4.99);
    assert_eq!(*output.0.borrow(), ["print:first"]);
    vm.step(5.0);
    assert_eq!(*output.0.borrow(), ["print:first", "print:second"]);
}

#[test]
fn task_created_thread_ordinary_yields_keep_nil_results_without_guard_tags() {
    let (mut vm, output) = rig();
    vm.run_script("GuardValues", r#"
local thread=task.spawn(function()
    local args=table.pack(coroutine.yield('ordinary',nil))
    assert(args.n==3 and args[1]=='manual' and args[2]==nil and args[3]=='last')
    return 'returned',nil,'last',nil
end)
local values=table.pack(coroutine.resume(thread,'manual',nil,'last'))
assert(values.n==5 and values[1] and values[2]=='returned' and values[3]==nil and values[4]=='last' and values[5]==nil,'guard bookkeeping escaped')
"#).unwrap();
    assert!(output.0.borrow().is_empty());
}

#[test]
fn one_thousand_ordinary_coroutines_fit_the_unchanged_default_budget() {
common::real_limits_test(|| {
    let (vm, _) = rig();
    vm.set_resume_budget(metallic_script::RESUME_BUDGET);
    let function: Function = vm.lua().load(r#"
local create,resume,wrap,yield,pack=coroutine.create,coroutine.resume,coroutine.wrap,coroutine.yield,table.pack
for i=1,500 do
    local child=create(function(a) local b=yield(a,nil) return b,nil end)
    local first=pack(resume(child,i))
    assert(first.n==3 and first[1] and first[2]==i and first[3]==nil)
    local last=pack(resume(child,i+1))
    assert(last.n==3 and last[1] and last[2]==i+1 and last[3]==nil)
    local wrapped=wrap(function(a) local b=yield(a,nil) return b,nil end)
    local wrapped_first=pack(wrapped(i))
    assert(wrapped_first.n==2 and wrapped_first[1]==i and wrapped_first[2]==nil)
    local wrapped_last=pack(wrapped(i+1))
    assert(wrapped_last.n==2 and wrapped_last[1]==i+1 and wrapped_last[2]==nil)
end
"#).set_name("=CoroutineSmoke").into_function().unwrap();
    let start = std::time::Instant::now();
    vm.call_with_budget::<()>(&function, ()).unwrap();
    eprintln!(
        "1000 precompiled ordinary coroutine smoke, uncontrolled host load: {:?}",
        start.elapsed()
    );
});
}

#[test]
fn ordinary_coroutines_match_native_results_errors_and_frames() {
    const PROBE: &str = r#"
local marker = setmetatable({}, {__tostring=function() error('must not stringify') end})
local lines = {}
local function record(label, ...)
    local values, parts = table.pack(...), {}
    parts[1] = label .. ':' .. values.n
    for i=1,values.n do
        local v = values[i]
        local text = rawequal(v,marker) and '<marker>' or tostring(v)
        parts[#parts+1] = type(v)..'='..text
    end
    lines[#lines+1] = table.concat(parts,'|')
end
local function body(...)
    local args = table.pack(...)
    local resumed = table.pack(coroutine.yield('yielded',args.n,table.unpack(args,1,args.n)))
    return 'returned',resumed.n,table.unpack(resumed,1,resumed.n)
end
local child = coroutine.create(body)
record('created',coroutine.status(child))
record('resume-first',coroutine.resume(child,17,nil,'last',nil))
record('suspended',coroutine.status(child))
record('resume-last',coroutine.resume(child,1,nil,3,nil))
record('dead',coroutine.resume(child))
local wrapped = coroutine.wrap(body)
record('wrap-first',wrapped(17,nil,'last',nil))
record('wrap-last',wrapped(1,nil,3,nil))
record('wrap-dead',pcall(wrapped))
for _, case in {
    {'string-zero',function() error('literal\nstack traceback:\nkept',0) end},
    {'string-one',function() error('message',1) end},
    {'string-three',function() error('message',3) end},
    {'table',function() error(marker,0) end},
    {'false',function() error(false,0) end},
    {'nil',function() error(nil,0) end},
    {'number',function() error(17,0) end},
} do
    record('resume-error-'..case[1],coroutine.resume(coroutine.create(case[2])))
    record('wrap-error-'..case[1],pcall(coroutine.wrap(case[2])))
end
for _, bad in {false,17,{},function() end,'text'} do
    record('bad-resume-'..type(bad),pcall(coroutine.resume,bad))
    if type(bad) ~= 'function' then record('bad-wrap-'..type(bad),pcall(coroutine.wrap,bad)) end
end
record('missing-resume',pcall(coroutine.resume))
record('missing-wrap',pcall(coroutine.wrap))
return table.concat(lines,'\n')
"#;
    let reference = mlua::Lua::new();
    let (vm, _) = rig();
    let expected: String = reference
        .load(PROBE)
        .set_name("=CoroutineCompatibility")
        .eval()
        .unwrap();
    let actual: String = vm
        .lua()
        .load(PROBE)
        .set_name("=CoroutineCompatibility")
        .eval()
        .unwrap();
    assert_eq!(actual, expected);
    for name in ["pcall", "xpcall"] {
        let function: Function = vm.lua().globals().get(name).unwrap();
        assert_eq!(function.info().what, "C", "{name}");
    }
}

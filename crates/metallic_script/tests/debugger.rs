//! The script debugger: breakpoints, stepping, inspection and pausing the host.

mod common;

use common::Rig;
use metallic_core::InstanceId;
use metallic_script::{Breakpoint, DebugState, PauseReason, RESUME_BUDGET, Scope, VarNode};

/// A rig with one server script, breakpoints set before the game starts.
fn rig_with(source: &str, breakpoints: &[(u32, Breakpoint)]) -> (Rig, InstanceId) {
    let mut rig = Rig::new();
    let id = rig.server_script("Main", source);
    for (line, kind) in breakpoints {
        rig.host.debugger().set_breakpoint(id, *line, kind.clone());
    }
    (rig, id)
}

/// Line and reason of the innermost paused frame.
fn paused_at(rig: &mut Rig) -> Option<(u32, PauseReason)> {
    match rig.host.debugger().state() {
        DebugState::Paused { reason, stack } => Some((stack.first()?.line, reason)),
        DebugState::Running => None,
    }
}

fn find<'a>(vars: &'a [VarNode], name: &str) -> &'a VarNode {
    vars.iter()
        .find(|v| v.name == name)
        .unwrap_or_else(|| panic!("{name} not in {vars:?}"))
}

#[test]
fn breakpoint_pauses_on_line() {
    let (mut rig, id) = rig_with(
        "print('a')\nprint('b')\nprint('c')\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:a"]);
    assert!(rig.host.is_paused());
    match rig.host.debugger().state() {
        DebugState::Paused { reason, stack } => {
            assert_eq!(reason, PauseReason::Breakpoint);
            assert_eq!(stack[0].line, 2);
            assert_eq!(stack[0].script, Some(id));
            assert_eq!(stack[0].function, "(main)");
        }
        DebugState::Running => panic!("not paused"),
    }
    // Paused: steps run nothing.
    rig.run(0.0, 3);
    assert_eq!(rig.lines(), ["print:a"]);
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:a", "print:b", "print:c"]);
    assert_eq!(rig.host.debugger().state(), DebugState::Running);
}

#[test]
fn conditional_breakpoint_only_when_true() {
    let (mut rig, _) = rig_with(
        "for i = 1, 5 do\n    print(i)\nend\n",
        &[(2, Breakpoint::Condition("i == 3".into()))],
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:1", "print:2"]);
    assert_eq!(paused_at(&mut rig).map(|p| p.0), Some(2));
    let locals = rig.host.debugger().variables(0, Scope::Locals);
    assert_eq!(find(&locals, "i").display, "3");
    rig.host.debugger().resume();
    assert_eq!(rig.lines().len(), 5);
    assert!(!rig.host.is_paused());
}

#[test]
fn logpoint_prints_without_pausing() {
    let (mut rig, _) = rig_with(
        "local x = 41\nx += 1\nprint('done')\n",
        &[(3, Breakpoint::Log("x is {x}, twice {x * 2}".into()))],
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:x is 42, twice 84", "print:done"]);
    assert!(!rig.host.is_paused());
}

#[test]
fn break_on_error_pauses_at_error_line() {
    let (mut rig, _) = rig_with(
        "local ok = pcall(function() error('handled') end)\nprint('before', ok)\nlocal t = nil\nprint(t.field)\nprint('after')\n",
        &[],
    );
    rig.host.debugger().break_on_error(true);
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:before false"]);
    let (line, reason) = paused_at(&mut rig).expect("paused at the error");
    assert_eq!(line, 4);
    assert!(
        matches!(reason, PauseReason::Error(ref m) if m.contains("field")),
        "{reason:?}"
    );
    rig.host.debugger().resume();
    let lines = rig.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(lines[1].starts_with("error:") && lines[1].contains("field"));
    assert!(!rig.host.is_paused());
}

const CALLS: &str = "local function f()\n    print('in f')\nend\nf()\nprint('next')\n";

#[test]
fn step_over_skips_call_body() {
    let (mut rig, _) = rig_with(CALLS, &[(4, Breakpoint::Plain)]);
    rig.host.start().unwrap();
    assert_eq!(paused_at(&mut rig), Some((4, PauseReason::Breakpoint)));
    rig.host.debugger().step_over();
    assert_eq!(rig.lines(), ["print:in f"]);
    assert_eq!(paused_at(&mut rig), Some((5, PauseReason::Step)));
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:in f", "print:next"]);
}

#[test]
fn step_into_enters_function() {
    let (mut rig, _) = rig_with(CALLS, &[(4, Breakpoint::Plain)]);
    rig.host.start().unwrap();
    rig.host.debugger().step_into();
    assert!(rig.lines().is_empty());
    match rig.host.debugger().state() {
        DebugState::Paused { reason, stack } => {
            assert_eq!(reason, PauseReason::Step);
            assert_eq!((stack[0].line, stack[0].function.as_str()), (2, "f"));
            assert_eq!(stack[1].line, 4);
        }
        DebugState::Running => panic!("not paused"),
    }
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:in f", "print:next"]);
}

#[test]
fn step_out_returns_to_caller() {
    let (mut rig, _) = rig_with(CALLS, &[(2, Breakpoint::Plain)]);
    rig.host.start().unwrap();
    assert_eq!(paused_at(&mut rig).map(|p| p.0), Some(2));
    rig.host.debugger().step_out();
    assert_eq!(rig.lines(), ["print:in f"]);
    match rig.host.debugger().state() {
        DebugState::Paused { reason, stack } => {
            assert_eq!(reason, PauseReason::Step);
            assert_eq!(stack[0].function, "(main)");
            assert_eq!(stack[0].line, 5);
        }
        DebugState::Running => panic!("not paused"),
    }
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:in f", "print:next"]);
}

#[test]
fn locals_and_upvalues_listed_with_values() {
    let (mut rig, _) = rig_with(
        "local up = 'outer'\nlocal function g(a)\n    local b = a * 2\n    print(up, b)\nend\ng(21)\n",
        &[(4, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    let locals = rig.host.debugger().variables(0, Scope::Locals);
    assert_eq!(find(&locals, "a").display, "21");
    assert_eq!(find(&locals, "b").display, "42");
    let upvalues = rig.host.debugger().variables(0, Scope::Upvalues);
    assert_eq!(find(&upvalues, "up").display, "\"outer\"");
    // The caller's frame has its own locals.
    let main = rig.host.debugger().variables(1, Scope::Locals);
    assert_eq!(find(&main, "up").display, "\"outer\"");
    assert!(find(&main, "g").display.starts_with("function"));
    let globals = rig.host.debugger().variables(0, Scope::Globals);
    assert!(find(&globals, "script").display.contains("Main"));
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:outer 42"]);
}

#[test]
fn expand_table_and_instance() {
    let (mut rig, _) = rig_with(
        "local t = { x = 1, list = {10, 20} }\nlocal part = Instance.new('Part')\npart.Name = 'Crate'\npart.Parent = workspace\nprint('ok')\n",
        &[(5, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    let dbg = rig.host.debugger();
    let locals = dbg.variables(0, Scope::Locals);
    let t = find(&locals, "t");
    assert_eq!(t.display, "table (2 entries)");
    let entries = dbg.expand(t.children.expect("table expands"));
    assert_eq!(find(&entries, "x").display, "1");
    let list = dbg.expand(find(&entries, "list").children.expect("list expands"));
    assert_eq!(
        list.iter()
            .map(|v| (v.name.as_str(), v.display.as_str()))
            .collect::<Vec<_>>(),
        [("[1]", "10"), ("[2]", "20")]
    );
    let part = find(&locals, "part");
    assert_eq!(part.display, "Crate (Part)");
    let rows = dbg.expand(part.children.expect("instance expands"));
    assert_eq!(find(&rows, "Name").display, "\"Crate\"");
    assert_eq!(find(&rows, "ClassName").display, "\"Part\"");
    assert_eq!(find(&rows, "Parent").display, "Workspace (Workspace)");
    let parent = dbg.expand(find(&rows, "Parent").children.expect("parent expands"));
    assert_eq!(find(&parent, "Crate").display, "Crate (Part)");
}

#[test]
fn evaluate_watch_expression() {
    let (mut rig, _) = rig_with(
        "local a = 5\nlocal t = { n = 3 }\nprint(a)\n",
        &[(3, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    let dbg = rig.host.debugger();
    assert_eq!(dbg.evaluate(0, "a * t.n + 1").unwrap(), "16");
    assert_eq!(dbg.evaluate(0, "math.max(a, 9)").unwrap(), "9");
    assert_eq!(dbg.evaluate(0, "script.Name").unwrap(), "\"Main\"");
    assert!(dbg.evaluate(0, "nosuch.x").is_err());
    assert!(dbg.evaluate(0, "a +").is_err());
    // A watch cannot loop forever.
    rig.host.set_resume_budget(RESUME_BUDGET);
    let e = rig
        .host
        .debugger()
        .evaluate(0, "(function() while true do end end)()")
        .unwrap_err();
    assert!(e.message.contains("timeout"), "{e}");
    assert!(rig.host.is_paused());
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:5"]);
}

#[test]
fn set_local_changes_value_after_resume() {
    // A local never assigned after a constant initialiser is folded by the compiler (there is
    // no variable to change), so these locals are computed.
    let (mut rig, _) = rig_with(
        "local x = tonumber('1')\nprint(x)\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    rig.host.debugger().set_local(0, "x", "99").unwrap();
    assert_eq!(
        find(&rig.host.debugger().variables(0, Scope::Locals), "x").display,
        "99"
    );
    assert!(rig.host.debugger().set_local(0, "nope", "1").is_err());
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:99"]);

    // A caller's local (not the paused instruction's frame) is written at once.
    let (mut rig, _) = rig_with(
        "local y = tonumber('1')\nlocal function f()\n    print('f')\nend\nf()\nprint(y)\n",
        &[(3, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    rig.host.debugger().set_local(1, "y", "'seven'").unwrap();
    rig.host.debugger().resume();
    assert_eq!(
        rig.lines(),
        [
            "warn:debugger: y changed type from number to string",
            "print:f",
            "print:seven"
        ]
    );
}

#[test]
fn endless_loop_still_times_out_when_not_paused() {
    common::real_limits_test(|| {
        let mut rig = Rig::new();
        rig.host.set_resume_budget(RESUME_BUDGET);
        let other = rig.server_script("Other", "local function never()\n    print('never')\nend\n");
        rig.host
            .debugger()
            .set_breakpoint(other, 2, Breakpoint::Plain);
        rig.server_script("Spin", "while true do end\n");
        rig.host.start().unwrap();
        let lines = rig.lines();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("Script timeout"), "{lines:?}");
        assert!(!rig.host.is_paused());
    });
}

#[test]
fn spawned_thread_pauses_its_spawner_until_it_continues() {
    let (mut rig, _) = rig_with(
        "task.spawn(function()\n    print('child')\nend)\nprint('parent')\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert!(rig.lines().is_empty());
    match rig.host.debugger().state() {
        DebugState::Paused { stack, .. } => {
            assert_eq!(stack.iter().map(|f| f.line).collect::<Vec<_>>(), [2, 1]);
        }
        DebugState::Running => panic!("not paused"),
    }
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:child", "print:parent"]);
}

#[test]
fn coroutine_break_pauses_its_resumer() {
    let (mut rig, _) = rig_with(
        "local co = coroutine.create(function()\n    print('co')\n    coroutine.yield(5)\nend)\nlocal ok, v = coroutine.resume(co)\nprint('resumed', ok, v)\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    match rig.host.debugger().state() {
        DebugState::Paused { stack, .. } => {
            assert_eq!(stack.iter().map(|f| f.line).collect::<Vec<_>>(), [2, 5]);
        }
        DebugState::Running => panic!("not paused"),
    }
    rig.host.debugger().step_over();
    assert_eq!(rig.lines(), ["print:co"]);
    assert_eq!(paused_at(&mut rig).map(|p| p.0), Some(3));
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:co", "print:resumed true 5"]);
}

#[test]
fn handler_breakpoint_pauses_and_holds_the_rest_of_the_step() {
    let (mut rig, id) = rig_with(
        "local rs = game:GetService('RunService')\nrs.Heartbeat:Connect(function()\n    print('beat')\nend)\nrs.Heartbeat:Connect(function()\n    print('second')\nend)\ntask.delay(0, function() print('delayed') end)\n",
        &[(3, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert!(!rig.host.is_paused());
    let now = rig.run(0.0, 1);
    assert!(rig.host.is_paused());
    assert!(rig.lines().is_empty());
    rig.run(now, 5);
    assert!(rig.lines().is_empty(), "paused game ran {:?}", rig.lines());
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:beat"]);
    // The rest of the held step runs on the next one; the breakpoint pauses again.
    rig.host.debugger().clear_breakpoint(id, 3);
    rig.run(now, 1);
    let lines = rig.lines();
    assert!(lines.contains(&"print:second".to_string()), "{lines:?}");
    assert!(lines.contains(&"print:delayed".to_string()), "{lines:?}");
}

#[test]
fn pause_then_step_stops_in_the_next_script_code() {
    let (mut rig, _) = rig_with(
        "game:GetService('RunService').Heartbeat:Connect(function()\n    print('beat')\nend)\n",
        &[],
    );
    rig.host.start().unwrap();
    rig.host.debugger().pause();
    assert_eq!(
        rig.host.debugger().state(),
        DebugState::Paused {
            reason: PauseReason::Pause,
            stack: Vec::new()
        }
    );
    let now = rig.run(0.0, 3);
    assert!(rig.lines().is_empty());
    rig.host.debugger().step_into();
    assert!(!rig.host.is_paused());
    rig.run(now, 1);
    assert_eq!(paused_at(&mut rig), Some((2, PauseReason::Pause)));
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:beat"]);
}

#[test]
fn stop_while_paused_cleans_up_and_breakpoints_stay() {
    let (mut rig, id) = rig_with("print('a')\nprint('b')\n", &[(2, Breakpoint::Plain)]);
    rig.host.start().unwrap();
    assert!(rig.host.is_paused());
    rig.host.stop();
    assert!(!rig.host.is_paused());
    assert_eq!(rig.host.debugger().state(), DebugState::Running);
    rig.host.start().unwrap();
    assert!(rig.host.is_paused());
    assert_eq!(rig.lines(), ["print:a", "print:a"]);
    assert_eq!(
        rig.host.debugger().breakpoints(),
        [(id, 2, Breakpoint::Plain)]
    );
    rig.host.debugger().clear_breakpoint(id, 2);
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:a", "print:a", "print:b"]);
}

#[test]
fn breakpoint_set_while_running_applies_to_the_loaded_script() {
    let (mut rig, id) = rig_with(
        "game:GetService('RunService').Heartbeat:Connect(function(dt)\n    print('beat')\nend)\n",
        &[],
    );
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    assert_eq!(rig.lines(), ["print:beat", "print:beat"]);
    rig.host.debugger().set_breakpoint(id, 2, Breakpoint::Plain);
    assert_eq!(rig.host.debugger().resolved_line(id, 2), Some(2));
    rig.run(now, 1);
    assert_eq!(paused_at(&mut rig), Some((2, PauseReason::Breakpoint)));
    assert!(
        find(&rig.host.debugger().variables(0, Scope::Locals), "dt")
            .display
            .starts_with("0.01")
    );
    rig.host.debugger().clear_breakpoint(id, 2);
    rig.host.debugger().resume();
    assert_eq!(rig.lines().len(), 3);
}

/// A ModuleScript `Lib` under ReplicatedStorage with `source`.
fn module(rig: &Rig, source: &str) -> InstanceId {
    rig.add_script("ModuleScript", "Lib", rig.root("ReplicatedStorage"), source)
}

const REQUIRE_LIB: &str =
    "local M = require(game:GetService('ReplicatedStorage').Lib)\nM.hi()\nprint('after')\n";

#[test]
fn breakpoint_in_a_required_module() {
    let mut rig = Rig::new();
    let lib = module(
        &rig,
        "local M = {}\nfunction M.hi()\n    print('hi')\nend\nreturn M\n",
    );
    rig.host
        .debugger()
        .set_breakpoint(lib, 3, Breakpoint::Plain);
    let main = rig.server_script("Main", REQUIRE_LIB);
    rig.host.start().unwrap();
    match rig.host.debugger().state() {
        DebugState::Paused { stack, .. } => {
            assert_eq!(stack[0].script, Some(lib));
            assert_eq!(stack[0].line, 3);
            assert_eq!(stack[1].script, Some(main));
            assert_eq!(stack[1].line, 2);
        }
        DebugState::Running => panic!("not paused in the module"),
    }
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:hi", "print:after"]);
}

#[test]
fn break_on_error_stops_inside_a_module() {
    let mut rig = Rig::new();
    let lib = module(&rig, "local t = nil\nreturn t.field\n");
    rig.server_script("Main", REQUIRE_LIB);
    rig.host.debugger().break_on_error(true);
    rig.host.start().unwrap();
    match rig.host.debugger().state() {
        DebugState::Paused { reason, stack } => {
            assert!(matches!(reason, PauseReason::Error(_)), "{reason:?}");
            assert_eq!((stack[0].script, stack[0].line), (Some(lib), 2));
        }
        DebugState::Running => panic!("the module's error did not pause"),
    }
    rig.host.debugger().resume();
    assert!(!rig.host.is_paused());
    let lines = rig.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("error:") && l.contains("field")),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("after")), "{lines:?}");
}

#[test]
fn step_over_a_wait_stops_on_the_next_line_of_the_same_thread() {
    let (mut rig, _) = rig_with(
        "game:GetService('RunService').Heartbeat:Connect(function() local _ = 1 end)\nprint('a')\ntask.wait(0.05)\nprint('b')\n",
        &[(3, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert_eq!(paused_at(&mut rig).map(|p| p.0), Some(3));
    rig.host.debugger().step_over();
    // The thread sleeps: the game runs (the handler included) without stopping in it.
    assert!(!rig.host.is_paused());
    let now = rig.run(0.0, 1);
    assert!(
        !rig.host.is_paused(),
        "Step Over does not stop in other threads"
    );
    rig.run(now, 10);
    assert_eq!(paused_at(&mut rig), Some((4, PauseReason::Step)));
    assert_eq!(rig.lines(), ["print:a"]);
    rig.host.debugger().resume();
    assert_eq!(rig.lines(), ["print:a", "print:b"]);
    rig.run(1.0, 5);
    assert!(!rig.host.is_paused(), "the step is over");
}

#[test]
fn a_step_over_waiting_forever_leaves_other_threads_at_full_speed() {
    let (mut rig, _) = rig_with(
        "game:GetService('RunService').Heartbeat:Connect(function()
    local n = 0
    for i = 1, 20 do n = n + i end
end)
task.wait(1e6)
print('never')
",
        &[(5, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert_eq!(paused_at(&mut rig).map(|p| p.0), Some(5));
    rig.host.debugger().step_over();
    let before = rig.host.debugger().step_hook_calls();
    let now = rig.run(0.0, 10);
    assert!(!rig.host.is_paused());
    assert_eq!(
        rig.host.debugger().step_hook_calls(),
        before,
        "only the stepped thread single-steps; the Heartbeat handler runs normally"
    );
    // Step Into, by contrast, stops in the next thread that runs.
    rig.host.debugger().pause();
    rig.run(now, 1);
    assert!(rig.host.is_paused());
    rig.host.debugger().step_into();
    rig.run(now + 1.0, 2);
    assert!(rig.host.debugger().step_hook_calls() > before);
}

#[test]
fn code_resumed_after_a_break_keeps_its_script() {
    let (mut rig, id) = rig_with(
        "local co = coroutine.create(function()\n    print('co')\n    game:GetService('RunService').Heartbeat:Connect(function() print('beat') end)\nend)\ncoroutine.resume(co)\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    assert!(rig.host.is_paused());
    rig.host.debugger().resume();
    let now = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:co", "print:beat"]);
    // The connection belongs to the script: removing the script ends it.
    rig.world.borrow_mut().destroy(id);
    rig.run(now, 3);
    assert_eq!(rig.lines(), ["print:co", "print:beat"]);
}

#[test]
fn setting_a_local_to_another_type_warns() {
    let (mut rig, _) = rig_with(
        "local x = tonumber('1')\nprint(x)\n",
        &[(2, Breakpoint::Plain)],
    );
    rig.host.start().unwrap();
    rig.host.debugger().set_local(0, "x", "'one'").unwrap();
    rig.host.debugger().resume();
    let lines = rig.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("x changed type from number to string")),
        "{lines:?}"
    );
    assert_eq!(lines.last().map(String::as_str), Some("print:one"));
}

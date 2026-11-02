//! `ModuleScript` and `require`: run once per VM, cached, cycles, errors, sandbox limits.

mod common;

use common::Rig;
use metallic_core::Value;

fn module(rig: &Rig, name: &str, source: &str) -> metallic_core::InstanceId {
    rig.add_script("ModuleScript", name, rig.root("ReplicatedStorage"), source)
}

fn errors(rig: &Rig) -> Vec<String> {
    rig.lines()
        .into_iter()
        .filter(|l| l.starts_with("error:"))
        .collect()
}

#[test]
fn a_module_runs_once_and_every_caller_gets_the_same_value() {
    let mut rig = Rig::new();
    module(
        &rig,
        "Counter",
        "print('loading') return { hits = 0, name = script.Name }",
    );
    rig.server_script(
        "A",
        "local m = require(game.ReplicatedStorage.Counter)
         m.hits += 1
         local again = require(game.ReplicatedStorage.Counter)
         print('same', m == again, again.hits, again.name)",
    );
    rig.server_script(
        "B",
        "local m = require(game.ReplicatedStorage.Counter)
         print('B sees', m.hits)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 3);
    let lines = rig.lines();
    assert_eq!(
        lines.iter().filter(|l| *l == "print:loading").count(),
        1,
        "{lines:?}"
    );
    assert!(
        lines.contains(&"print:same true 1 Counter".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"print:B sees 1".to_string()), "{lines:?}");
}

#[test]
fn a_module_may_return_any_single_value() {
    let mut rig = Rig::new();
    module(&rig, "Fn", "return function(a) return a * 2 end");
    module(&rig, "Num", "return 42");
    module(&rig, "Nil", "return nil");
    module(&rig, "Str", "return 'text'");
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         print(require(R.Fn)(21), require(R.Num), require(R.Nil), require(R.Str))",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert!(
        rig.lines().contains(&"print:42 42 nil text".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn a_module_must_return_exactly_one_value() {
    let mut rig = Rig::new();
    module(&rig, "None", "local x = 1");
    module(&rig, "Two", "return 1, 2");
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         print(pcall(require, R.None))
         print(pcall(require, R.Two))",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let lines = rig.lines();
    for name in ["None", "Two"] {
        let want = format!(
            "print:false ReplicatedStorage.{name}: Module code did not return exactly one value"
        );
        assert!(lines.contains(&want), "{lines:?}");
    }
}

#[test]
fn a_cyclic_require_is_an_error_not_a_hang() {
    let mut rig = Rig::new();
    module(
        &rig,
        "A",
        "local b = require(script.Parent.B) return { b = b }",
    );
    module(
        &rig,
        "B",
        "local a = require(script.Parent.A) return { a = a }",
    );
    rig.server_script(
        "Main",
        "local ok, err = pcall(require, game.ReplicatedStorage.A)
         print(ok, err)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let lines = rig.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("print:false") && l.contains("required recursively")),
        "{lines:?}"
    );
}

#[test]
fn a_module_that_requires_itself_is_a_cycle() {
    let mut rig = Rig::new();
    module(&rig, "Self", "return require(script)");
    rig.server_script("Main", "print(pcall(require, game.ReplicatedStorage.Self))");
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert!(
        rig.lines()
            .iter()
            .any(|l| l.contains("required recursively")),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn errors_carry_the_module_name_and_are_cached() {
    let mut rig = Rig::new();
    module(&rig, "Broken", "print('running broken')\nerror('boom')");
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         local ok1, e1 = pcall(require, R.Broken)
         local ok2, e2 = pcall(require, R.Broken)
         print(ok1, e1)
         print(ok2, e2 == e1)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let lines = rig.lines();
    assert!(
        lines.contains(&"print:false ReplicatedStorage.Broken:2: boom".to_string()),
        "{lines:?}"
    );
    assert!(lines.contains(&"print:false true".to_string()), "{lines:?}");
    assert_eq!(
        lines
            .iter()
            .filter(|l| *l == "print:running broken")
            .count(),
        1,
        "a failed module does not run again"
    );
}

#[test]
fn a_syntax_error_in_a_module_names_the_module() {
    let mut rig = Rig::new();
    module(&rig, "Bad", "return +");
    rig.server_script("Main", "print(pcall(require, game.ReplicatedStorage.Bad))");
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert!(
        rig.lines()
            .iter()
            .any(|l| l.starts_with("print:false ReplicatedStorage.Bad:1:")),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn an_uncaught_module_error_reaches_the_requiring_script_output() {
    let mut rig = Rig::new();
    module(&rig, "Broken", "error('boom')");
    rig.server_script("Main", "require(game.ReplicatedStorage.Broken)");
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let errs = errors(&rig);
    assert!(
        errs.iter()
            .any(|e| e.contains("ReplicatedStorage.Broken:1: boom")),
        "{errs:?}"
    );
}

#[test]
fn require_accepts_only_module_scripts() {
    let mut rig = Rig::new();
    rig.add("Folder", "NotAModule", rig.root("ReplicatedStorage"));
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         print(pcall(require, R.NotAModule))
         print(pcall(require, 123))
         print(pcall(require, 'x'))
         print(pcall(require))",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let n = rig
        .lines()
        .iter()
        .filter(|l| {
            l.starts_with("print:false") && l.contains("Attempted to call require with invalid")
        })
        .count();
    assert_eq!(n, 4, "{:?}", rig.lines());
}

#[test]
fn a_module_may_yield_while_loading() {
    let mut rig = Rig::new();
    module(&rig, "Slow", "task.wait(0.5) print('slow loaded') return 7");
    rig.server_script(
        "Main",
        "print('before') print('got', require(game.ReplicatedStorage.Slow)) print('after')",
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 3);
    assert!(!rig.lines().contains(&"print:got 7".to_string()));
    rig.run(t, 60);
    let lines = rig.lines();
    assert!(lines.contains(&"print:got 7".to_string()), "{lines:?}");
    assert!(lines.contains(&"print:after".to_string()));
}

#[test]
fn a_module_runs_in_the_sandbox_with_its_own_script_global() {
    let mut rig = Rig::new();
    module(
        &rig,
        "Probe",
        "return { io = io, load = loadstring, name = script.Name, class = script.ClassName }",
    );
    rig.server_script(
        "Main",
        "local m = require(game.ReplicatedStorage.Probe)
         print(m.io, m.load, m.name, m.class)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert!(
        rig.lines()
            .contains(&"print:nil nil Probe ModuleScript".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn a_module_that_never_returns_times_out_and_the_vm_survives() {
    common::real_limits_test(|| {
        let mut rig = Rig::new();
        rig.host
            .set_resume_budget(std::time::Duration::from_millis(200));
        module(&rig, "Spin", "while true do end");
        rig.server_script(
            "Main",
            "local ok, e = pcall(require, game.ReplicatedStorage.Spin)
         print('result', ok)",
        );
        rig.server_script("Other", "print('other ran')");
        rig.host.start().unwrap();
        rig.run(0.0, 3);
        let lines = rig.lines();
        assert!(lines.contains(&"print:other ran".to_string()), "{lines:?}");
        assert!(
            errors(&rig).iter().any(|e| e.contains("Script timeout")),
            "{lines:?}"
        );
    });
}

#[test]
fn the_cached_value_is_returned_even_after_the_source_changes() {
    let mut rig = Rig::new();
    let m = module(&rig, "Late", "return 'first'");
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         print(require(R.Late))
         task.wait(0.2)
         print(require(R.Late))",
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 2);
    rig.world
        .borrow_mut()
        .set(m, "Source", Value::String("return 'second'".into()))
        .unwrap();
    rig.run(t, 60);
    let lines = rig.lines();
    assert_eq!(
        lines.iter().filter(|l| *l == "print:first").count(),
        2,
        "the cached value is returned: {lines:?}"
    );
}

#[test]
fn server_and_client_vms_each_run_a_module_once() {
    let mut rig = Rig::new();
    module(&rig, "Shared", "print('shared loaded') return 1");
    rig.server_script("S", "require(game.ReplicatedStorage.Shared)");
    rig.local_script("C", "require(game.ReplicatedStorage.Shared)");
    rig.host.start().unwrap();
    rig.run(0.0, 3);
    let n = rig
        .lines()
        .iter()
        .filter(|l| *l == "print:shared loaded")
        .count();
    assert_eq!(n, 2, "{:?}", rig.lines());
}

#[test]
fn modules_can_require_other_modules() {
    let mut rig = Rig::new();
    module(&rig, "Base", "return { value = 5 }");
    module(
        &rig,
        "Derived",
        "local base = require(script.Parent.Base) return { value = base.value + 1 }",
    );
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         print(require(R.Derived).value, require(R.Base).value)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert!(
        rig.lines().contains(&"print:6 5".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn module_scripts_do_not_run_by_themselves() {
    let mut rig = Rig::new();
    module(&rig, "Idle", "print('should not run')");
    rig.add_script(
        "ModuleScript",
        "InWorkspace",
        rig.root("Workspace"),
        "print('should not run either')",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 3);
    assert!(rig.lines().is_empty(), "{:?}", rig.lines());
}

#[test]
fn a_second_caller_waits_for_a_module_that_yields_while_loading() {
    let mut rig = Rig::new();
    module(
        &rig,
        "Slow",
        "print('slow start') task.wait(0.5) print('slow done') return { v = 1 }",
    );
    rig.server_script(
        "A",
        "local m = require(game.ReplicatedStorage.Slow) print('A got', m.v)",
    );
    rig.server_script(
        "B",
        "local m = require(game.ReplicatedStorage.Slow) print('B got', m.v)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 90);
    let lines = rig.lines();
    assert_eq!(
        lines.iter().filter(|l| *l == "print:slow start").count(),
        1,
        "{lines:?}"
    );
    assert!(lines.contains(&"print:A got 1".to_string()), "{lines:?}");
    assert!(lines.contains(&"print:B got 1".to_string()), "{lines:?}");
    assert!(
        errors(&rig).is_empty(),
        "no false cycle: {:?}",
        errors(&rig)
    );
}

#[test]
fn the_waiting_caller_gets_the_same_table() {
    let mut rig = Rig::new();
    module(&rig, "Slow", "task.wait(0.2) return {}");
    rig.server_script(
        "Main",
        "local R = game.ReplicatedStorage
         local first
         task.spawn(function() first = require(R.Slow) end)
         local second = require(R.Slow)
         task.wait(0.1)
         print(first == second)",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 60);
    assert!(
        rig.lines().contains(&"print:true".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn a_cycle_across_two_yielding_threads_is_detected() {
    let mut rig = Rig::new();
    module(&rig, "X", "task.wait(0.1) return require(script.Parent.Y)");
    module(&rig, "Y", "return require(script.Parent.X)");
    rig.server_script(
        "One",
        "print('one', pcall(require, game.ReplicatedStorage.X))",
    );
    rig.server_script(
        "Two",
        "print('two', pcall(require, game.ReplicatedStorage.Y))",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 90);
    let lines = rig.lines();
    for who in ["one", "two"] {
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with(&format!("print:{who} false"))
                    && l.contains("required recursively")),
            "{who} must fail with the cycle error: {lines:?}"
        );
    }
}

#[test]
fn a_stopped_script_releases_the_module_it_was_loading() {
    let mut rig = Rig::new();
    let m = module(&rig, "Hang", "task.wait(1000) return 'never'");
    let hanging = rig.server_script("Hanging", "require(game.ReplicatedStorage.Hang)");
    rig.host.start().unwrap();
    let t = rig.run(0.0, 5);
    // The script is destroyed: the host stops it; the module is not stuck loading.
    rig.world.borrow_mut().destroy(hanging);
    let t = rig.run(t, 5);
    rig.world
        .borrow_mut()
        .set(m, "Source", Value::String("return 'fresh'".into()))
        .unwrap();
    rig.server_script(
        "Next",
        "print('next got', require(game.ReplicatedStorage.Hang))",
    );
    rig.run(t, 10);
    assert!(
        rig.lines().contains(&"print:next got fresh".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn a_waiter_whose_loader_was_stopped_loads_the_module_itself() {
    let mut rig = Rig::new();
    let m = module(&rig, "Hang", "task.wait(1000) return 'never'");
    let loader = rig.server_script("Loader", "require(game.ReplicatedStorage.Hang)");
    rig.server_script(
        "Waiter",
        "task.wait(0.1) print('waiter got', require(game.ReplicatedStorage.Hang))",
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 20);
    rig.world
        .borrow_mut()
        .set(m, "Source", Value::String("return 'second try'".into()))
        .unwrap();
    rig.world.borrow_mut().destroy(loader);
    rig.run(t, 30);
    assert!(
        rig.lines()
            .contains(&"print:waiter got second try".to_string()),
        "{:?}",
        rig.lines()
    );
}

#[test]
fn a_timeout_while_loading_is_cached_like_any_error() {
    common::real_limits_test(|| {
        let mut rig = Rig::new();
        rig.host
            .set_resume_budget(std::time::Duration::from_millis(200));
        module(&rig, "Spin", "print('spin ran') while true do end");
        rig.server_script("First", "require(game.ReplicatedStorage.Spin)");
        rig.host.start().unwrap();
        let t = rig.run(0.0, 3);
        rig.server_script(
            "Second",
            "local ok, e = pcall(require, game.ReplicatedStorage.Spin) print('second', ok, e)",
        );
        rig.run(t, 3);
        let lines = rig.lines();
        assert_eq!(
            lines.iter().filter(|l| *l == "print:spin ran").count(),
            1,
            "the module does not run again: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("print:second false")
                && l.contains("ReplicatedStorage.Spin: Requested module experienced an error")),
            "{lines:?}"
        );
    });
}

#[test]
fn a_client_cannot_require_modules_under_server_script_service() {
    let mut rig = Rig::new();
    rig.add_script(
        "ModuleScript",
        "Secret",
        rig.root("ServerScriptService"),
        "return 'secret'",
    );
    module(&rig, "Shared", "return 'shared'");
    rig.local_script(
        "Client",
        "local ok, e = pcall(require, game.ServerScriptService.Secret)
         print('client secret', ok, e)
         print('client shared', require(game.ReplicatedStorage.Shared))",
    );
    rig.server_script(
        "Server",
        "print('server secret', require(game.ServerScriptService.Secret))",
    );
    rig.host.start().unwrap();
    rig.run(0.0, 4);
    let lines = rig.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("print:client secret false")
                && l.contains("only available on the server")),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"print:client shared shared".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"print:server secret secret".to_string()),
        "{lines:?}"
    );
}

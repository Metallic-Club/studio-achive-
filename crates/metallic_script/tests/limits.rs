//! Resource limits and lifetime rules: remote payload/queue bounds, the per-step budget,
//! repeated timeouts, locked metatables and scripts that are destroyed or disabled.

mod common;

use std::sync::{Mutex, MutexGuard};
use std::time::Instant;

use common::Rig;
use metallic_core::Value;

/// Depth of the nested-table bomb; the unit test run for the red/green record may lower it.
const BOMB_LEVELS: usize = 15;

static LIMITS_TEST: Mutex<()> = Mutex::new(());

// Deliberate CPU-budget exhaustion must not starve queue/payload correctness
// fixtures in this binary. Acquire before constructing VMs or starting timers;
// production budgets and each safety assertion remain unchanged.
fn isolate_limits_test() -> MutexGuard<'static, ()> {
    LIMITS_TEST
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn rig_with_remote() -> Rig {
    let rig = Rig::new();
    rig.add("RemoteEvent", "Ping", rig.root("ReplicatedStorage"));
    rig
}

#[test]
fn remote_rejects_exponential_shared_tables() {
    common::real_limits_test(|| {
        let _isolation = isolate_limits_test();
        let mut rig = rig_with_remote();
        rig.local_script(
            "Client",
            &format!(
                r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        local t = {{}}
        for i = 1, {BOMB_LEVELS} do t = {{t, t, t, t, t, t, t, t, t, t}} end
        print(pcall(function() ping:FireServer(t) end))
        "#
            ),
        );
        let began = Instant::now();
        rig.host.start().unwrap();
        assert!(began.elapsed().as_secs_f64() < 2.0, "must fail fast");
        let lines = rig.lines();
        assert!(
            lines[0].starts_with("print:false") && lines[0].contains("remote payload too large"),
            "{lines:?}"
        );
    });
}

#[test]
fn remote_rejects_too_many_arguments_values_and_bytes() {
    let _isolation = isolate_limits_test();
    let mut rig = rig_with_remote();
    rig.local_script(
        "Client",
        r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        local many = {}
        for i = 1, 33 do many[i] = i end
        print(pcall(function() ping:FireServer(table.unpack(many)) end))
        print(pcall(function() ping:FireServer(table.unpack(many, 1, 32)) end))
        local wide = {}
        for i = 1, 10001 do wide[i] = i end
        print(pcall(function() ping:FireServer(wide) end))
        print(pcall(function() ping:FireServer(string.rep("x", 1048577)) end))
        print(pcall(function() ping:FireServer(string.rep("x", 1000000)) end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert!(lines[0].contains("remote payload too large"), "{lines:?}");
    assert_eq!(lines[1], "print:true");
    assert!(lines[2].contains("remote payload too large"), "{lines:?}");
    assert!(lines[3].contains("remote payload too large"), "{lines:?}");
    assert_eq!(lines[4], "print:true");
}

#[test]
fn remote_queue_is_bounded() {
    let _isolation = isolate_limits_test();
    let mut rig = rig_with_remote();
    rig.local_script(
        "Client",
        r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        local sent, lastError = 0, nil
        for i = 1, 1100 do
            local ok, err = pcall(function() ping:FireServer(i) end)
            if ok then sent += 1 else lastError = err end
        end
        print(sent, lastError)
        "#,
    );
    rig.local_script(
        "Client2",
        r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        local payload = string.rep("x", 1000000)
        local sent, lastError = 0, nil
        for i = 1, 30 do
            local ok, err = pcall(function() ping:FireServer(payload) end)
            if ok then sent += 1 else lastError = err end
        end
        print(sent, lastError)
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert!(
        lines[0].starts_with("print:1000 ") && lines[0].ends_with("remote queue full"),
        "{lines:?}"
    );
    // The second script starts with 1000 small messages already queued, so it is refused at once.
    assert!(
        lines[1].starts_with("print:0 ") && lines[1].ends_with("remote queue full"),
        "{lines:?}"
    );
    // After a step the queue is empty again and the byte cap applies (16 MB / ~1 MB).
    rig.run(0.0, 1);
    rig.local_script(
        "Client3",
        r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        local payload = string.rep("x", 1000000)
        local sent = 0
        for i = 1, 30 do
            if pcall(function() ping:FireServer(payload) end) then sent += 1 end
        end
        print("bytes", sent)
        "#,
    );
    rig.run(0.0, 1);
    let last = rig.lines().pop().unwrap();
    assert_eq!(last, "print:bytes 16");
}

#[test]
fn too_many_connections_on_one_signal() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        r#"
        local pad = Instance.new("Part", workspace)
        local signal, handler = pad.Touched, function() end
        for i = 1, 10000 do signal:Connect(handler) end
        print(pcall(function() signal:Connect(handler) end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert!(
        lines[0].starts_with("print:false") && lines[0].ends_with("too many connections"),
        "{lines:?}"
    );
}

#[test]
fn many_spinning_handlers_do_not_stall_a_step() {
    common::real_limits_test(|| {
        let _isolation = isolate_limits_test();
        let mut rig = Rig::new();
        rig.host
            .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
        rig.host.set_resume_budget(metallic_script::RESUME_BUDGET);
        let ws = rig.root("Workspace");
        let pad = rig.add("Part", "Pad", ws);
        let ball = rig.add("Part", "Ball", ws);
        for i in 0..20 {
            rig.server_script(
                &format!("Spin{i}"),
                "workspace.Pad.Touched:Connect(function() while true do end end)",
            );
        }
        rig.host.start().unwrap();
        rig.host.touched(pad, ball);
        let mut now = 0.0;
        let mut longest = 0.0f64;
        let mut steps = 0;
        while rig.lines().len() < 20 && steps < 60 {
            let began = Instant::now();
            now = rig.run(now, 1);
            longest = longest.max(began.elapsed().as_secs_f64());
            steps += 1;
        }
        assert!(longest < 0.6, "a step took {longest:.2} s");
        let timeouts = rig
            .lines()
            .iter()
            .filter(|l| l.contains("Script timeout"))
            .count();
        assert_eq!(timeouts, 20, "deferred handlers still ran: {steps} steps");
        assert!(steps > 1, "the work was spread over several steps");
    });
}

#[test]
fn repeat_timeouts_disconnect_script() {
    common::real_limits_test(|| {
        let _isolation = isolate_limits_test();
        let mut rig = Rig::new();
        rig.host.set_resume_budget(metallic_script::RESUME_BUDGET);
        rig.server_script(
            "Spinner",
            r#"
        game:GetService("RunService").Heartbeat:Connect(function() while true do end end)
        "#,
        );
        rig.server_script(
            "Other",
            r#"
        game:GetService("RunService").Heartbeat:Connect(function() print("other beat") end)
        "#,
        );
        rig.host.start().unwrap();
        let now = rig.run(0.0, 6);
        let lines = rig.lines();
        let timeouts = lines
            .iter()
            .filter(|l| l.contains("Script timeout"))
            .count();
        let notices: Vec<_> = lines
            .iter()
            .filter(|l| l.contains("disconnected"))
            .collect();
        assert_eq!(timeouts, 3, "{lines:?}");
        assert_eq!(notices.len(), 1, "{lines:?}");
        assert!(
            notices[0].starts_with("error:ServerScriptService.Spinner"),
            "{lines:?}"
        );
        let before = lines.len();
        rig.run(now, 3);
        let after = rig.lines();
        assert_eq!(
            after[before..],
            ["print:other beat", "print:other beat", "print:other beat"],
            "only the other script keeps running"
        );
    });
}

#[test]
fn userdata_metatables_are_locked() {
    let _isolation = isolate_limits_test();
    // mlua protects every userdata metatable with `__metatable = false`: getmetatable returns
    // false and setmetatable errors (the string the lead asked for cannot be set).
    let mut rig = Rig::new();
    rig.server_script(
        "T",
        r#"
        local pad = Instance.new("Part", workspace)
        local conn = pad.Touched:Connect(function() end)
        local things = {
            workspace, game, Vector3.new(1, 2, 3), Color3.fromRGB(1, 2, 3), UDim2.new(0, 1, 0, 1),
            pad.Touched, conn,
        }
        for i, thing in things do
            print(i, getmetatable(thing), pcall(setmetatable, thing, {}))
        end
        print(pcall(function() getmetatable(workspace).__index = function() return 1 end end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    for i in 1..=7 {
        let line = &lines[i - 1];
        assert!(
            line.starts_with(&format!("print:{i} false false")),
            "{lines:?}"
        );
    }
    assert!(lines[7].starts_with("print:false"), "{lines:?}");
}

#[test]
fn connect_and_operator_errors_are_positioned() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        r#"
        local pad = Instance.new("Part", workspace)
        print(pcall(function() pad.Touched:Connect(nil) end))
        print(pcall(function() return Vector3.new(1, 2, 3) + 1 end))
        print(pcall(function() return Vector3.new(1, 2, 3) * "a" end))
        "#,
    );
    rig.host.start().unwrap();
    for line in rig.lines() {
        assert!(
            line.starts_with("print:false ServerScriptService.S:"),
            "{line}"
        );
        assert!(!line.contains("traceback"), "{line}");
    }
}

#[test]
fn destroyed_script_stops_and_its_handlers_stop_firing() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    let victim = rig.server_script(
        "Victim",
        r#"
        game:GetService("RunService").Heartbeat:Connect(function() print("beat") end)
        workspace.Pad.Touched:Connect(function() print("touched") end)
        while true do task.wait() print("tick") end
        "#,
    );
    rig.server_script(
        "Bystander",
        "game:GetService('RunService').Heartbeat:Connect(function() print('bystander') end)",
    );
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    assert!(rig.lines().iter().any(|l| l == "print:tick"));
    rig.world.borrow_mut().destroy(victim);
    let now = rig.run(now, 1);
    let mark = rig.lines().len();
    rig.host.touched(pad, ball);
    rig.run(now, 5);
    let after: Vec<_> = rig.lines()[mark..].to_vec();
    assert!(after.iter().all(|l| l == "print:bystander"), "{after:?}");
    assert_eq!(after.len(), 5, "{after:?}");
}

#[test]
fn disabled_script_stops() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    let id = rig.server_script(
        "S",
        r#"
        game:GetService("RunService").Heartbeat:Connect(function() print("beat") end)
        while true do task.wait() print("tick") end
        "#,
    );
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    assert!(rig.lines().iter().any(|l| l == "print:beat"));
    rig.world
        .borrow_mut()
        .set(id, "Enabled", Value::Bool(false))
        .unwrap();
    let now = rig.run(now, 1);
    let mark = rig.lines().len();
    rig.run(now, 5);
    assert_eq!(
        rig.lines().len(),
        mark,
        "nothing runs after Enabled = false"
    );
}

#[test]
fn script_moved_out_of_a_valid_place_stops() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    let id = rig.server_script(
        "S",
        "game:GetService('RunService').Heartbeat:Connect(function() print('beat') end)",
    );
    rig.host.start().unwrap();
    let now = rig.run(0.0, 1);
    let lighting = rig.root("Lighting");
    rig.world
        .borrow_mut()
        .set_parent(id, Some(lighting))
        .unwrap();
    let now = rig.run(now, 1);
    let mark = rig.lines().len();
    rig.run(now, 3);
    assert_eq!(rig.lines().len(), mark);
}

#[test]
fn heartbeat_1000_parts_read_write_cost() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    for i in 0..1000 {
        rig.add("Part", &format!("P{i}"), ws);
    }
    rig.server_script(
        "S",
        r#"
        local parts = workspace:GetChildren()
        game:GetService("RunService").Heartbeat:Connect(function(dt)
            for _, p in parts do
                local pos = p.Position
                p.Position = Vector3.new(pos.X + dt, pos.Y, pos.Z)
            end
        end)
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    let began = Instant::now();
    rig.run(1.0, 20);
    let per_beat_ms = began.elapsed().as_secs_f64() * 1000.0 / 20.0;
    println!("1000 parts read+write per Heartbeat: {per_beat_ms:.2} ms");
    #[cfg(not(debug_assertions))]
    assert!(per_beat_ms < 50.0, "{per_beat_ms} ms per Heartbeat");
}

#[test]
fn cloned_scripts_with_same_name_stop_independently() {
    let _isolation = isolate_limits_test();
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let ball = rig.add("Part", "Ball", ws);
    let source = r#"
        local hit = script.Parent.Hit
        local tag = tostring(hit.Transparency)
        game:GetService("RunService").Heartbeat:Connect(function() print("beat" .. tag) end)
        hit.Touched:Connect(function() print("touched" .. tag) end)
        task.spawn(function()
            while true do task.wait() print("tick" .. tag) end
        end)
    "#;
    let mut bricks = Vec::new();
    for transparency in [0.25, 0.75] {
        let model = rig.add("Model", "KillBrick", ws);
        let hit = rig.add("Part", "Hit", model);
        rig.world
            .borrow_mut()
            .set(hit, "Transparency", Value::Number(transparency))
            .unwrap();
        let script = rig.add_script("Script", "Script", model, source);
        bricks.push((script, hit));
    }
    rig.host.start().unwrap();
    let now = rig.run(0.0, 2);
    let lines = rig.lines();
    for tag in ["0.25", "0.75"] {
        assert!(lines.contains(&format!("print:beat{tag}")), "{lines:?}");
        assert!(lines.contains(&format!("print:tick{tag}")), "{lines:?}");
    }
    rig.world.borrow_mut().destroy(bricks[0].0);
    let now = rig.run(now, 1);
    let mark = rig.lines().len();
    rig.host.touched(bricks[0].1, ball);
    rig.host.touched(bricks[1].1, ball);
    rig.run(now, 5);
    let after: Vec<_> = rig.lines()[mark..].to_vec();
    assert!(after.iter().all(|l| !l.contains("0.25")), "{after:?}");
    for kind in ["beat", "tick", "touched"] {
        assert!(
            after.iter().any(|l| l == &format!("print:{kind}0.75")),
            "{kind} missing: {after:?}"
        );
    }
}

#[test]
fn full_deferral_queue_stops_building_arguments() {
    common::real_limits_test(|| {
        let _isolation = isolate_limits_test();
        let mut rig = Rig::new();
        rig.host
            .set_step_budget_sec(metallic_script::STEP_BUDGET_SEC);
        rig.server_script(
            "Many",
            r#"
        local beat = game:GetService("RunService").Heartbeat
        for i = 1, 10000 do
            beat:Connect(function()
                local x = 0
                for j = 1, 300000 do x = x + j end
            end)
        end
        "#,
        );
        rig.host.start().unwrap();
        rig.run(0.0, 8);
        // Without the early stop every step built 10,000 argument lists (80,000 here).
        let built = rig.host.dispatch_args_built();
        assert!(built < 20_000, "built {built} argument lists");
    });
}

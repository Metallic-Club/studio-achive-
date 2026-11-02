//! `RBXScriptSignal:Wait()` and `RBXScriptSignal:Once()` (#404).

mod common;

use common::Rig;
use metallic_core::InstanceId;

/// A rig with `Workspace.Pad` and `Workspace.Ball`.
fn pad_rig() -> (Rig, InstanceId, InstanceId) {
    let rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    (rig, pad, ball)
}

/// Queues `pad.Touched(ball)` and delivers it with one step after `now`.
fn touch(rig: &mut Rig, pad: InstanceId, ball: InstanceId, now: f64) -> f64 {
    rig.host.touched(pad, ball);
    rig.run(now, 1)
}

/// Output lines after the first `from`.
fn since(rig: &Rig, from: usize) -> Vec<String> {
    rig.lines()[from..].to_vec()
}

#[test]
fn wait_returns_the_fire_arguments() {
    let (mut rig, pad, ball) = pad_rig();
    rig.add("RemoteEvent", "Ping", rig.root("ReplicatedStorage"));
    rig.server_script(
        "Toucher",
        r#"
        local hit = workspace.Pad.Touched:Wait()
        print("touched", hit.Name, hit == workspace.Ball)
        "#,
    );
    rig.server_script(
        "Remote",
        r#"
        local ping = game:GetService("ReplicatedStorage").Ping
        local player, a, b, t = ping.OnServerEvent:Wait()
        print("remote", player.Name, a, b, t.k)
        "#,
    );
    rig.local_script(
        "Client",
        r#"
        game:GetService("ReplicatedStorage"):WaitForChild("Ping"):FireServer(7, "x", { k = "v" })
        "#,
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), Vec::<String>::new(), "nothing fired yet");
    assert_eq!(rig.host.signal_waiters(), 2);
    let now = touch(&mut rig, pad, ball, 0.0);
    let lines = rig.lines();
    assert!(
        lines.contains(&"print:touched Ball true".to_string()),
        "{lines:?}"
    );
    assert!(
        lines.contains(&"print:remote Player1 7 x v".to_string()),
        "{lines:?}"
    );
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(rig.host.signal_waiters(), 0);
    touch(&mut rig, pad, ball, now);
    assert_eq!(
        rig.lines().len(),
        2,
        "a finished wait does not resume again"
    );
}

#[test]
fn several_waiters_resume_on_one_fire() {
    let (mut rig, pad, ball) = pad_rig();
    rig.server_script(
        "Waiters",
        r#"
        for i = 1, 3 do
            task.spawn(function()
                local hit = workspace.Pad.Touched:Wait()
                print("waiter", i, hit.Name)
            end)
        end
        "#,
    );
    rig.server_script("Other", "print('other', workspace.Pad.Touched:Wait().Name)");
    rig.host.start().unwrap();
    assert_eq!(rig.host.signal_waiters(), 4);
    let now = touch(&mut rig, pad, ball, 0.0);
    let mut lines = rig.lines();
    lines.sort();
    assert_eq!(
        lines,
        [
            "print:other Ball",
            "print:waiter 1 Ball",
            "print:waiter 2 Ball",
            "print:waiter 3 Ball",
        ]
    );
    assert_eq!(rig.host.signal_waiters(), 0);
    touch(&mut rig, pad, ball, now);
    assert_eq!(rig.lines().len(), 4, "each waiter resumed once");
}

#[test]
fn wait_inside_a_handler_waits_for_the_next_fire() {
    let (mut rig, pad, ball) = pad_rig();
    rig.server_script(
        "Handler",
        r#"
        local pad = workspace.Pad
        local n = 0
        pad.Touched:Connect(function(hit)
            n += 1
            local me = n
            print("handler", me)
            local again = pad.Touched:Wait()
            print("resumed", me, again.Name)
        end)
        "#,
    );
    rig.host.start().unwrap();
    let now = touch(&mut rig, pad, ball, 0.0);
    assert_eq!(
        rig.lines(),
        ["print:handler 1"],
        "not resumed by its own fire"
    );
    assert_eq!(rig.host.signal_waiters(), 1);
    let now = touch(&mut rig, pad, ball, now);
    assert_eq!(
        rig.lines(),
        ["print:handler 1", "print:handler 2", "print:resumed 1 Ball"]
    );
    assert_eq!(rig.host.signal_waiters(), 1, "handler 2 waits now");
    touch(&mut rig, pad, ball, now);
    assert_eq!(
        since(&rig, 3),
        ["print:handler 3", "print:resumed 2 Ball"],
        "each wait resumes on the fire after the one that started it"
    );
}

#[test]
fn stopped_script_waiter_is_cancelled_without_leak() {
    let (mut rig, pad, ball) = pad_rig();
    let victim = rig.server_script(
        "Victim",
        r#"
        local pad = workspace.Pad
        coroutine.wrap(function()
            pad.Touched:Wait()
            print("coroutine escaped")
        end)()
        task.spawn(function()
            pad.Touched:Wait()
            print("task escaped")
        end)
        game:GetService("RunService").Heartbeat:Wait()
        print("beat")
        pad.Touched:Wait()
        print("main escaped")
        "#,
    );
    rig.server_script(
        "Bystander",
        r#"
        while true do
            local hit = workspace.Pad.Touched:Wait()
            print("bystander", hit.Name)
        end
        "#,
    );
    rig.host.start().unwrap();
    assert_eq!(rig.host.signal_waiters(), 4);
    let now = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:beat"]);
    assert_eq!(rig.host.signal_waiters(), 4);
    rig.world.borrow_mut().destroy(victim);
    let now = rig.run(now, 1);
    assert_eq!(rig.host.signal_waiters(), 1, "only the bystander waits");
    let mark = rig.lines().len();
    let now = touch(&mut rig, pad, ball, now);
    touch(&mut rig, pad, ball, now);
    assert_eq!(
        since(&rig, mark),
        ["print:bystander Ball", "print:bystander Ball"]
    );
    assert_eq!(rig.host.signal_waiters(), 1);
}

#[test]
fn wait_on_a_destroyed_instance_never_resumes_and_is_dropped() {
    let (mut rig, pad, ball) = pad_rig();
    rig.add("Part", "Doomed", rig.root("Workspace"));
    rig.server_script(
        "Waiter",
        r#"
        task.spawn(function()
            workspace.Pad.Touched:Wait()
            print("pad never")
        end)
        local doomed = workspace.Doomed
        local signal = doomed.Touched
        doomed:Destroy()
        task.spawn(function()
            signal:Wait()
            print("doomed never")
        end)
        print("waiting")
        "#,
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:waiting"]);
    assert_eq!(rig.host.signal_waiters(), 2);
    let now = rig.run(0.0, 1);
    assert_eq!(rig.host.signal_waiters(), 1, "Doomed's waiter dropped");
    rig.world.borrow_mut().destroy(pad);
    rig.host.touched(pad, ball);
    let now = rig.run(now, 1);
    assert_eq!(rig.host.signal_waiters(), 0, "Pad's waiter dropped");
    rig.run(now, 3);
    assert_eq!(rig.lines(), ["print:waiting"]);
}

#[test]
fn heartbeat_wait_works_as_a_frame_wait() {
    let mut rig = Rig::new();
    rig.server_script(
        "Frames",
        r#"
        local heartbeat = game:GetService("RunService").Heartbeat
        for i = 1, 3 do
            local dt = heartbeat:Wait()
            print("frame", i, string.format("%.4f", dt))
        end
        print("done")
        "#,
    );
    rig.host.start().unwrap();
    assert!(rig.lines().is_empty());
    let now = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:frame 1 0.0167"], "one frame per step");
    rig.run(now, 2);
    assert_eq!(
        rig.lines(),
        [
            "print:frame 1 0.0167",
            "print:frame 2 0.0167",
            "print:frame 3 0.0167",
            "print:done",
        ]
    );
    assert_eq!(rig.host.signal_waiters(), 0);
}

#[test]
fn once_fires_only_once() {
    let (mut rig, pad, ball) = pad_rig();
    rig.server_script(
        "Once",
        r#"
        local conn
        conn = workspace.Pad.Touched:Once(function(hit)
            print("once", hit.Name, conn.Connected)
        end)
        workspace.Pad.Touched:Connect(function() print("always") end)
        "#,
    );
    rig.host.start().unwrap();
    let mut now = 0.0;
    for _ in 0..3 {
        now = touch(&mut rig, pad, ball, now);
    }
    assert_eq!(
        rig.lines(),
        [
            "print:once Ball false",
            "print:always",
            "print:always",
            "print:always",
        ]
    );
}

#[test]
fn once_disconnect_before_fire_prevents_the_call() {
    let (mut rig, pad, ball) = pad_rig();
    rig.server_script(
        "Once",
        r#"
        local conn = workspace.Pad.Touched:Once(function() print("called") end)
        print("before", conn.Connected)
        conn:Disconnect()
        print("after", conn.Connected)
        "#,
    );
    rig.host.start().unwrap();
    let now = touch(&mut rig, pad, ball, 0.0);
    touch(&mut rig, pad, ball, now);
    assert_eq!(rig.lines(), ["print:before true", "print:after false"]);
}

#[test]
fn once_returns_a_connection() {
    let (mut rig, pad, ball) = pad_rig();
    rig.server_script(
        "Once",
        r#"
        local conn = workspace.Pad.Touched:Once(function() end)
        print(typeof(conn), tostring(conn), conn.Connected, typeof(conn.Disconnect))
        local ok, err = pcall(function() return workspace.Pad.Touched:Once(5) end)
        print(ok, err)
        workspace.Pad.Touched:Connect(function() print("later", conn.Connected) end)
        "#,
    );
    rig.host.start().unwrap();
    touch(&mut rig, pad, ball, 0.0);
    let lines = rig.lines();
    assert_eq!(
        lines[0],
        "print:RBXScriptConnection RBXScriptConnection true function"
    );
    assert!(
        lines[1].starts_with("print:false ServerScriptService.Once:4: bad argument"),
        "{lines:?}"
    );
    assert_eq!(lines[2], "print:later false", "{lines:?}");
}

#[test]
fn wait_outside_a_yieldable_thread_fails_like_task_wait() {
    let (mut rig, _pad, _ball) = pad_rig();
    rig.server_script(
        "Meta",
        r#"
        local signal = workspace.Pad.Touched
        local byWait = setmetatable({}, { __index = function() return signal:Wait() end })
        local byTask = setmetatable({}, { __index = function() return task.wait() end })
        local ok1, e1 = pcall(function() return byWait.x end)
        local ok2, e2 = pcall(function() return byTask.x end)
        print(ok1, ok2, e1 == e2)
        print(e1)
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert_eq!(lines[0], "print:false false true", "{lines:?}");
    assert!(lines[1].contains("yield"), "{lines:?}");
    assert_eq!(rig.host.signal_waiters(), 0);
}

/// Tag events only fire when something listens: a thread in `:Wait()` counts as a listener.
#[test]
fn wait_on_a_tag_added_signal_resumes() {
    let (mut rig, _pad, _ball) = pad_rig();
    rig.server_script(
        "Waiter",
        r#"
        local cs = game:GetService("CollectionService")
        local part = cs:GetInstanceAddedSignal("Hot"):Wait()
        print("tagged", part.Name)
        "#,
    );
    rig.server_script(
        "Tagger",
        r#"
        task.wait()
        workspace.Ball:AddTag("Hot")
        "#,
    );
    rig.host.start().unwrap();
    assert_eq!(rig.host.signal_waiters(), 1);
    rig.run(0.0, 5);
    let lines = rig.lines();
    assert!(
        lines.contains(&"print:tagged Ball".to_string()),
        "{lines:?}"
    );
    assert_eq!(rig.host.signal_waiters(), 0);
}

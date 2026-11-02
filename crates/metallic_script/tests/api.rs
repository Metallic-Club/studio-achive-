//! Behaviour tests for the Luau API (`game`, `Instance`, datatypes, services, signals).

mod common;

use common::Rig;
use metallic_core::{Value, Vec3};

fn start(rig: &mut Rig) {
    rig.host.start().expect("host starts");
}

#[test]
fn script_parent_name() {
    let mut rig = Rig::new();
    rig.server_script(
        "Hello",
        "print(script.Parent.Name, script.Name, script.ClassName, script.Parent == game:GetService('ServerScriptService'))",
    );
    start(&mut rig);
    assert_eq!(rig.lines(), ["print:ServerScriptService Hello Script true"]);
}

#[test]
fn find_first_child_and_dot_access() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let model = rig.add("Model", "Tower", ws);
    rig.add("Part", "Roof", model);
    rig.server_script(
        "S",
        r#"
        local tower = workspace:FindFirstChild("Tower")
        print(tower.Name, tower.Roof.Name, tower:FindFirstChild("Roof") == tower.Roof)
        print(workspace:FindFirstChild("Nope"), workspace:FindFirstChild("Roof"), workspace:FindFirstChild("Roof", true) == tower.Roof)
        print(#tower:GetChildren(), #workspace:GetDescendants(), tower:IsA("Model"), tower.Roof:IsA("BasePart"), tower:IsA("BasePart"))
        local ok, err = pcall(function() return tower.Missing end)
        print(ok, err)
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[0], "print:Tower Roof true");
    assert_eq!(lines[1], "print:nil nil true");
    assert_eq!(lines[2], "print:1 2 true true false");
    assert!(
        lines[3].starts_with("print:false")
            && lines[3].ends_with("Missing is not a valid member of Model"),
        "{lines:?}"
    );
}

#[test]
fn set_position_moves_part() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let part = rig.add("Part", "Brick", ws);
    rig.server_script(
        "Mover",
        r#"
        local p = workspace.Brick
        p.Position = Vector3.new(1, 2, 3)
        p.Anchored = true
        p.Color = Color3.fromRGB(10, 20, 30)
        p.Size = p.Size * 2
        print(p.Position.X, p.Position.Y, p.Position.Z, (p.Position + Vector3.new(1, 1, 1)).Magnitude > 0)
        print(Vector3.new(3, 4, 0).Magnitude, Vector3.new(0, 5, 0).Unit.Y, tostring(Vector3.new(1, 2, 3) - Vector3.new(1, 1, 1)))
        "#,
    );
    start(&mut rig);
    let world = rig.world.borrow();
    assert_eq!(
        world.get(part, "Position").unwrap(),
        Value::Vector3(Vec3::new(1.0, 2.0, 3.0))
    );
    assert_eq!(world.get(part, "Anchored").unwrap(), Value::Bool(true));
    assert_eq!(
        world.get(part, "Size").unwrap(),
        Value::Vector3(Vec3::new(8.0, 2.0, 4.0))
    );
    drop(world);
    assert_eq!(rig.lines(), ["print:1 2 3 true", "print:5 1 0, 1, 2"]);
}

#[test]
fn wrong_type_property_errors() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    rig.add("Part", "Brick", ws);
    rig.server_script(
        "S",
        r#"
        local p = workspace.Brick
        print(pcall(function() p.Position = 5 end))
        print(pcall(function() p.Anchored = "yes" end))
        print(pcall(function() p.Material = "Lava" end))
        print(pcall(function() p.Size = Vector3.new(0 / 0, 1, 1) end))
        print(pcall(function() p.Nope = 1 end))
        print(pcall(function() p.ClassName = "Model" end))
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert!(
        lines[0].contains("Position")
            && lines[0].contains("Vector3")
            && lines[0].contains("number"),
        "{lines:?}"
    );
    assert!(
        lines[1].contains("Anchored") && lines[1].contains("Bool"),
        "{lines:?}"
    );
    assert!(lines[2].contains("Material"), "{lines:?}");
    assert!(
        lines[3].contains("Size") && lines[3].contains("finite"),
        "{lines:?}"
    );
    assert!(
        lines[4].contains("Nope is not a valid member of Part"),
        "{lines:?}"
    );
    assert!(lines[5].contains("ClassName is read-only"), "{lines:?}");
}

#[test]
fn touched_connects_and_disconnects() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    rig.server_script(
        "S",
        r#"
        local count = 0
        local conn
        conn = workspace.Pad.Touched:Connect(function(hit)
            count += 1
            print("touched", hit.Name, count, conn.Connected)
            if count == 2 then conn:Disconnect() print(conn.Connected) end
        end)
        "#,
    );
    start(&mut rig);
    let t = rig.run(0.0, 1);
    assert!(rig.lines().is_empty(), "nothing before a touch");
    rig.host.touched(pad, ball);
    let t = rig.run(t, 1);
    rig.host.touched(pad, ball);
    rig.host.touched(pad, ball);
    rig.run(t, 1);
    assert_eq!(
        rig.lines(),
        [
            "print:touched Ball 1 true",
            "print:touched Ball 2 true",
            "print:false"
        ]
    );
}

#[test]
fn touch_ended_still_fires_when_the_touching_part_was_destroyed() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let foot = rig.add("Part", "LeftFoot", ws);
    rig.server_script(
        "S",
        r#"
        workspace.Pad.Touched:Connect(function(hit) print("on", hit.Name) end)
        workspace.Pad.TouchEnded:Connect(function(hit)
            print("off", typeof(hit), (pcall(function() return hit.Name end)))
        end)
        "#,
    );
    start(&mut rig);
    let t = rig.run(0.0, 1);
    rig.host.touched(pad, foot);
    let t = rig.run(t, 1);
    // The character goes before the scripts hear about the end of the contact.
    rig.world.borrow_mut().destroy(foot);
    rig.host.touch_ended(pad, foot);
    // The destroyed side's own TouchEnded has nobody left to fire on.
    rig.host.touch_ended(foot, pad);
    rig.run(t, 1);
    assert_eq!(
        rig.lines(),
        ["print:on LeftFoot", "print:off Instance false"]
    );
}

#[test]
fn touch_ended_fires_with_the_part_that_left() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let hand = rig.add("Part", "LeftHand", ws);
    rig.server_script(
        "S",
        r#"
        workspace.Pad.Touched:Connect(function(hit) print("on", hit.Name) end)
        workspace.Pad.TouchEnded:Connect(function(hit) print("off", hit.Name) end)
        "#,
    );
    start(&mut rig);
    let t = rig.run(0.0, 1);
    rig.host.touched(pad, hand);
    let t = rig.run(t, 1);
    rig.host.touch_ended(pad, hand);
    rig.run(t, 1);
    assert_eq!(rig.lines(), ["print:on LeftHand", "print:off LeftHand"]);
}

#[test]
fn connect_inside_handler_does_not_fire_in_same_dispatch() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    rig.server_script(
        "S",
        r#"
        local pad = workspace.Pad
        local late
        pad.Touched:Connect(function()
            print("first")
            if not late then
                late = pad.Touched:Connect(function() print("late") end)
            end
        end)
        local victim
        pad.Touched:Connect(function() print("second") if victim then victim:Disconnect() end end)
        victim = pad.Touched:Connect(function() print("victim") end)
        "#,
    );
    start(&mut rig);
    rig.host.touched(pad, ball);
    let t = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:first", "print:second"]);
    rig.host.touched(pad, ball);
    rig.run(t, 1);
    assert_eq!(
        &rig.lines()[2..],
        ["print:first", "print:second", "print:late"]
    );
}

#[test]
fn is_server_only_in_script() {
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        "local rs = game:GetService('RunService') print('server', rs:IsServer(), rs:IsClient(), game.Players.LocalPlayer)",
    );
    rig.local_script(
        "L",
        "local rs = game:GetService('RunService') print('client', rs:IsServer(), rs:IsClient(), game.Players.LocalPlayer.Name)",
    );
    start(&mut rig);
    assert_eq!(
        rig.lines(),
        [
            "print:server true false nil",
            "print:client false true Player1"
        ]
    );
}

#[test]
fn error_in_one_script_does_not_stop_others() {
    let mut rig = Rig::new();
    rig.server_script("Bad", "print('before')\nerror('boom')");
    rig.server_script("Worse", "local x = nil\nprint(x.field)");
    rig.server_script("Good", "print('still runs')");
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[0], "print:before");
    assert_eq!(lines[1], "error:ServerScriptService.Bad:2: boom");
    assert!(
        lines[2].starts_with("error:ServerScriptService.Worse:2: "),
        "{lines:?}"
    );
    assert_eq!(lines[3], "print:still runs");
}

#[test]
fn stop_then_start_connects_once() {
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        "game:GetService('RunService').Heartbeat:Connect(function(dt) print('beat') end)",
    );
    start(&mut rig);
    let t = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:beat"]);
    rig.host.stop();
    rig.run(t, 3);
    assert_eq!(rig.lines().len(), 1, "stopped host does nothing");
    start(&mut rig);
    rig.run(t, 1);
    assert_eq!(rig.lines(), ["print:beat", "print:beat"]);
    let players = rig.root("Players");
    assert_eq!(
        rig.world.borrow().children(players).len(),
        1,
        "one Player1, not two"
    );
}

#[test]
fn destroyed_instance_errors_with_its_name() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    rig.add("Part", "Brick", ws);
    rig.server_script(
        "S",
        r#"
        local p = workspace.Brick
        p:Destroy()
        print(workspace:FindFirstChild("Brick"))
        print(pcall(function() return p.Position end))
        print(pcall(function() p.Name = "x" end))
        print(pcall(function() workspace:Destroy() end))
        print(pcall(function() workspace.Parent = nil end))
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[0], "print:nil");
    assert!(
        lines[1].starts_with("print:false") && lines[1].ends_with("Brick has been destroyed"),
        "{lines:?}"
    );
    assert!(lines[2].ends_with("Brick has been destroyed"), "{lines:?}");
    assert!(lines[3].contains("cannot be destroyed"), "{lines:?}");
    assert!(lines[4].contains("locked"), "{lines:?}");
}

#[test]
fn instance_new_clone_and_identity() {
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        r#"
        local part = Instance.new("Part", workspace)
        part.Name = "Made"
        local twin = part:Clone()
        print(twin.Parent, twin.Name, twin ~= part, workspace.Made == part, workspace:GetChildren()[1] == part)
        twin.Parent = workspace
        local seen = {}
        seen[part] = true
        print(seen[workspace.Made], #workspace:GetChildren())
        print(pcall(Instance.new, "Nope"))
        print(pcall(Instance.new, "Workspace"))
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[0], "print:nil Made true true true");
    assert_eq!(lines[1], "print:true 2");
    assert!(
        lines[2].contains("Unable to create an Instance of type \"Nope\""),
        "{lines:?}"
    );
    assert!(lines[3].contains("Unable to create"), "{lines:?}");
}

#[test]
fn services_players_and_humanoid() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let model = rig.add("Model", "Player1", ws);
    let hum = rig.add("Humanoid", "Humanoid", model);
    rig.server_script(
        "S",
        r#"
        local Players = game:GetService("Players")
        print(#Players:GetPlayers())
        Players.PlayerAdded:Connect(function(p)
            print("added", p.Name, #Players:GetPlayers(), p.UserId, p.ClassName)
            print(Players:GetPlayerFromCharacter(workspace.Player1), Players:GetPlayerFromCharacter(workspace))
            p.CharacterAdded:Connect(function(c) print("char", c.Name, p.Character == c, Players:GetPlayerFromCharacter(c) == p) end)
        end)
        local h = workspace.Player1.Humanoid
        h:TakeDamage(30)
        print(h.Health)
        h:TakeDamage(500)
        print(h.Health)
        h.Died:Connect(function() print("died") end)
        print(pcall(game.GetService, game, "Nope"))
        print(game.Workspace == workspace, game:GetService("Lighting").Name)
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[..3], ["print:0", "print:70", "print:0"]);
    assert!(
        lines[3].starts_with("print:false")
            && lines[3].ends_with("'Nope' is not a valid Service name"),
        "{lines:?}"
    );
    assert_eq!(lines[4], "print:true Lighting");
    let player = rig
        .world
        .borrow()
        .find_first_child(rig.root("Players"), "Player1")
        .unwrap();
    let t = rig.run(0.0, 1);
    rig.host.character_added(player, model);
    rig.host.humanoid_died(hum);
    rig.run(t, 1);
    let lines = rig.lines();
    assert_eq!(lines[5], "print:added Player1 1 0 Player");
    assert_eq!(lines[6], "print:nil nil");
    assert_eq!(lines[7], "print:char Player1 true true");
    assert_eq!(lines[8], "print:died");
}

#[test]
fn wait_for_child_resumes_times_out_and_warns() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    rig.server_script(
        "S",
        r#"
        print("have", workspace:WaitForChild("Late").Name)
        print("timeout", workspace:WaitForChild("Never", 1))
        print("soon", workspace:WaitForChild("Later").Name)
        "#,
    );
    start(&mut rig);
    assert!(rig.lines().is_empty());
    let mut t = rig.run(0.0, 2);
    rig.add("Folder", "Late", ws);
    t = rig.run(t, 1);
    assert_eq!(rig.lines(), ["print:have Late"]);
    t = rig.run(t, 70);
    assert_eq!(rig.lines()[1], "print:timeout nil");
    // "Later" has now waited about 1 s; after 5 s without a timeout a warning appears, once.
    t = rig.run(t, 60 * 5);
    let warns: Vec<_> = rig
        .lines()
        .into_iter()
        .filter(|l| l.starts_with("warn:"))
        .collect();
    assert_eq!(
        warns,
        ["warn:Infinite yield possible on 'Workspace:WaitForChild(\"Later\")'"]
    );
    rig.add("Folder", "Later", ws);
    rig.run(t, 1);
    assert_eq!(rig.lines().last().unwrap(), "print:soon Later");
}

#[test]
fn runaway_handler_times_out_and_engine_continues() {
common::real_limits_test(|| {
    let mut rig = Rig::new();
    rig.host.set_resume_budget(metallic_script::RESUME_BUDGET);
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    rig.server_script(
        "S",
        r#"
        workspace.Pad.Touched:Connect(function() while true do end end)
        workspace.Pad.Touched:Connect(function() print("second handler ran") end)
        "#,
    );
    start(&mut rig);
    rig.host.touched(pad, ball);
    let began = std::time::Instant::now();
    rig.run(0.0, 1);
    assert!(began.elapsed().as_secs_f64() < 5.0);
    let lines = rig.lines();
    assert!(
        lines[0].starts_with("error:ServerScriptService.S:2: ")
            && lines[0].contains("Script timeout"),
        "{lines:?}"
    );
    assert_eq!(lines[1], "print:second handler ran");
});
}

#[test]
fn handlers_may_yield() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let pad = rig.add("Part", "Pad", ws);
    let ball = rig.add("Part", "Ball", ws);
    rig.server_script(
        "S",
        "workspace.Pad.Touched:Connect(function() print('a') task.wait(0.5) print('b') end)",
    );
    start(&mut rig);
    rig.host.touched(pad, ball);
    let t = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:a"]);
    rig.run(t, 40);
    assert_eq!(rig.lines(), ["print:a", "print:b"]);
}

#[test]
fn scripts_added_later_start_on_next_step_in_the_right_vm() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    start(&mut rig);
    let t = rig.run(0.0, 1);
    // A LocalScript in the player's PlayerGui runs in the client VM...
    let player = rig
        .world
        .borrow()
        .find_first_child(rig.root("Players"), "Player1")
        .unwrap();
    let gui = rig
        .world
        .borrow()
        .find_first_child(player, "PlayerGui")
        .expect("PlayerGui exists");
    let screen = rig.add("ScreenGui", "Hud", gui);
    rig.add_script(
        "LocalScript",
        "HudScript",
        screen,
        "print('hud', game:GetService('RunService'):IsClient())",
    );
    // ...a Script in Workspace in the server VM, a LocalScript in Workspace nowhere.
    rig.add_script(
        "Script",
        "Late",
        ws,
        "print('late', game:GetService('RunService'):IsServer())",
    );
    rig.add_script("LocalScript", "Ignored", ws, "print('never')");
    // StarterGui is a template: nothing runs there.
    let starter_gui = rig.root("StarterGui");
    rig.add_script("LocalScript", "Template", starter_gui, "print('never')");
    assert!(rig.lines().is_empty());
    rig.run(t, 1);
    assert_eq!(rig.lines(), ["print:hud true", "print:late true"]);
    rig.run(t, 3);
    assert_eq!(rig.lines().len(), 2, "each script runs once");
}

#[test]
fn disabled_script_does_not_run() {
    let mut rig = Rig::new();
    let id = rig.server_script("Off", "print('no')");
    rig.world
        .borrow_mut()
        .set(id, "Enabled", Value::Bool(false))
        .unwrap();
    start(&mut rig);
    rig.run(0.0, 1);
    assert!(rig.lines().is_empty());
}

#[test]
fn api_tables_are_read_only() {
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        r#"
        print(pcall(function() Instance.new = nil end))
        print(pcall(function() Vector3.new = nil end))
        print(pcall(function() game.Workspace = nil end))
        print(typeof(Vector3.new(1, 2, 3)), Vector3.new(1, 2, 3) == Vector3.new(1, 2, 3), UDim2.new(0.5, 4, 1, 0).X.Offset, Color3.fromRGB(255, 0, 0).R)
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert!(lines[0].starts_with("print:false"), "{lines:?}");
    assert!(lines[1].starts_with("print:false"), "{lines:?}");
    assert!(lines[2].starts_with("print:false"), "{lines:?}");
    assert_eq!(lines[3], "print:Vector3 true 4 1");
}

#[test]
fn lighting_values_from_scripts_stay_in_range() {
    let mut rig = Rig::new();
    rig.server_script(
        "S",
        r#"
        local L = game:GetService("Lighting")
        L.Brightness = 1e39
        L.ExposureCompensation = -1e300
        L.ClockTime = 26
        L.Ambient = Color3.fromRGB(255, 0, 0)
        print(L.Brightness, L.ExposureCompensation, L.ClockTime)
        print(pcall(function() L.Brightness = math.huge end), L.Brightness)
        print(pcall(function() L.Brightness = 0/0 end), L.Brightness)
        "#,
    );
    start(&mut rig);
    let lines = rig.lines();
    assert_eq!(lines[0], "print:10 -5 2");
    assert!(lines[1].starts_with("print:false"), "{}", lines[1]);
    assert!(lines[1].ends_with(" 10"), "{}", lines[1]);
    assert!(lines[2].starts_with("print:false"), "{}", lines[2]);
    assert!(lines[2].ends_with(" 10"), "{}", lines[2]);
}

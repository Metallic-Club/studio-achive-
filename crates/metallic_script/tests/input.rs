//! Input in Luau: `InputAction`, `ContextActionService`, `Vector2`, the enums and the
//! last-input members of `UserInputService` (docs/design/input-actions.md section 5).

mod common;

use common::Rig;
use metallic_core::{InstanceId, Value};
use metallic_script::{
    InputActionEvent, InputDevice, InputEventKind, InputValue, MAX_ACTION_EVENTS_PER_STEP,
    TouchButtonInfo,
};

/// A rig with `ReplicatedStorage.Input.Gameplay.Dash` (an `InputAction`).
fn input_rig() -> (Rig, InstanceId) {
    let rig = Rig::new();
    let input = rig.add("Folder", "Input", rig.root("ReplicatedStorage"));
    let gameplay = rig.add("InputContext", "Gameplay", input);
    let dash = rig.add("InputAction", "Dash", gameplay);
    (rig, dash)
}

fn event(
    action: InstanceId,
    kind: InputEventKind,
    value: InputValue,
    time: f64,
) -> InputActionEvent {
    InputActionEvent {
        action,
        kind,
        value,
        time,
    }
}

fn press(action: InstanceId, time: f64) -> InputActionEvent {
    event(
        action,
        InputEventKind::Pressed,
        InputValue::Bool(true),
        time,
    )
}

fn release(action: InstanceId, time: f64) -> InputActionEvent {
    event(
        action,
        InputEventKind::Released,
        InputValue::Bool(false),
        time,
    )
}

fn trigger(action: InstanceId, time: f64) -> InputActionEvent {
    event(
        action,
        InputEventKind::Triggered,
        InputValue::Bool(true),
        time,
    )
}

/// The hidden `InputAction` that `ContextActionService:BindAction(name, ...)` made.
fn bound_action(rig: &Rig, name: &str) -> Option<InstanceId> {
    let world = rig.world.borrow();
    let player = world.find_first_child(world.root("Players")?, "Player1")?;
    world
        .children(player)
        .iter()
        .copied()
        .filter(|&c| world.class_name(c) == Some("InputContext"))
        .find_map(|ctx| world.find_first_child(ctx, name))
}

fn prop(rig: &Rig, id: InstanceId, name: &str) -> Value {
    rig.world.borrow().get(id, name).expect("property")
}

#[test]
fn triggered_pressed_and_released_fire_on_the_client() {
    let (mut rig, dash) = input_rig();
    rig.local_script(
        "Client",
        r##"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Pressed:Connect(function(...) print("pressed", select("#", ...)) end)
        dash.Released:Connect(function() print("released") end)
        dash.Triggered:Connect(function(v) print("triggered", v) end)
        dash.StateChanged:Connect(function(v) print("changed", v) end)
        "##,
    );
    rig.host.start().unwrap();
    rig.host.input_action_event(press(dash, 0.10));
    rig.host.input_action_event(trigger(dash, 0.10));
    rig.host.input_action_event(release(dash, 0.20));
    rig.host.input_action_event(event(
        dash,
        InputEventKind::StateChanged,
        InputValue::Bool(false),
        0.20,
    ));
    rig.run(0.0, 1);
    assert_eq!(
        rig.lines(),
        [
            "print:pressed 0",
            "print:triggered true",
            "print:released",
            "print:changed false"
        ]
    );
    rig.run(1.0, 2);
    assert_eq!(rig.lines().len(), 4, "each event is delivered once");
}

#[test]
fn events_are_delivered_in_event_time_order() {
    let (mut rig, dash) = input_rig();
    rig.local_script(
        "Client",
        r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Released:Connect(function() print("released") end)
        dash.Pressed:Connect(function() print("pressed") end)
        "#,
    );
    rig.host.start().unwrap();
    rig.host.input_action_event(release(dash, 0.30));
    rig.host.input_action_event(press(dash, 0.10));
    rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:pressed", "print:released"]);
}

#[test]
fn get_state_returns_the_frame_value_by_type() {
    let (mut rig, dash) = input_rig();
    let ctx = rig.world.borrow().parent(dash).unwrap();
    let run = rig.add("InputAction", "Run", ctx);
    let mv = rig.add("InputAction", "Move", ctx);
    let look = rig.add("InputAction", "Look3", ctx);
    let zoom = rig.add("InputAction", "Zoom", ctx);
    {
        let mut w = rig.world.borrow_mut();
        for (id, ty) in [
            (mv, "Direction2D"),
            (look, "Direction3D"),
            (zoom, "Direction1D"),
        ] {
            w.set(id, "Type", Value::String(ty.into())).unwrap();
        }
    }
    rig.local_script(
        "Client",
        r#"
        local g = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay
        game:GetService("RunService").Heartbeat:Connect(function()
            local m = g.Move:GetState()
            local l = g.Look3:GetState()
            print(g.Dash:GetState(), g.Run:GetState(), typeof(m), m.X, m.Y, typeof(l), l.Z,
                g.Zoom:GetState(), g.Dash:GetHoldProgress())
        end)
        "#,
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 1);
    assert_eq!(
        rig.lines(),
        ["print:false false Vector2 0 0 Vector3 0 0 0"],
        "zero before the host reports anything"
    );
    rig.host.set_action_state(dash, InputValue::Bool(true), 0.5);
    rig.host.set_action_state(run, InputValue::Bool(true), 0.0);
    rig.host
        .set_action_state(mv, InputValue::Axis2([0.5, 1.0]), 0.0);
    rig.host
        .set_action_state(look, InputValue::Axis3([0.0, 0.0, 2.0]), 0.0);
    rig.host.set_action_state(zoom, InputValue::Axis1(3.0), 0.0);
    let t = rig.run(t, 1);
    assert_eq!(
        rig.lines()[1],
        "print:true true Vector2 0.5 1 Vector3 2 3 0.5"
    );
    // A disabled action reads zero; so does an action in a disabled context.
    rig.world
        .borrow_mut()
        .set(dash, "Enabled", Value::Bool(false))
        .unwrap();
    let t = rig.run(t, 1);
    assert_eq!(
        rig.lines()[2],
        "print:false true Vector2 0.5 1 Vector3 2 3 0"
    );
    rig.world
        .borrow_mut()
        .set(ctx, "Enabled", Value::Bool(false))
        .unwrap();
    rig.run(t, 1);
    assert_eq!(
        rig.lines()[3],
        "print:false false Vector2 0 0 Vector3 0 0 0"
    );
}

#[test]
fn disabled_actions_fire_nothing() {
    let (mut rig, dash) = input_rig();
    rig.local_script(
        "Client",
        r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Triggered:Connect(function() print("triggered") end)
        "#,
    );
    rig.host.start().unwrap();
    rig.world
        .borrow_mut()
        .set(dash, "Enabled", Value::Bool(false))
        .unwrap();
    rig.host.input_action_event(trigger(dash, 0.1));
    rig.run(0.0, 2);
    assert!(rig.lines().is_empty(), "{:?}", rig.lines());
}

#[test]
fn server_scripts_cannot_use_input_actions() {
    let (mut rig, dash) = input_rig();
    rig.server_script(
        "Server",
        r#"
        local dash = game:GetService("ReplicatedStorage").Input.Gameplay.Dash
        print(pcall(function() return dash:GetState() end))
        print(pcall(function() return dash.Triggered:Connect(function() end) end))
        print(pcall(function() return dash:GetHoldProgress() end))
        print(dash.Name, dash.ClassName)
        "#,
    );
    rig.host.start().unwrap();
    rig.host.input_action_event(trigger(dash, 0.1));
    rig.run(0.0, 2);
    let lines = rig.lines();
    assert_eq!(lines.len(), 4, "{lines:?}");
    for line in &lines[..3] {
        assert!(
            line.starts_with("print:false")
                && line.ends_with("InputAction can only be used from a LocalScript"),
            "{line}"
        );
    }
    assert_eq!(
        lines[3], "print:Dash InputAction",
        "other members still work"
    );
}

#[test]
fn context_action_service_is_client_only() {
    let mut rig = Rig::new();
    rig.server_script(
        "Server",
        r#"
        local cas = game:GetService("ContextActionService")
        print(pcall(function() cas:BindAction("X", function() end, false, Enum.KeyCode.X) end))
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    let lines = rig.lines();
    assert!(
        lines[0].starts_with("print:false")
            && lines[0].ends_with("ContextActionService can only be used from a LocalScript"),
        "{lines:?}"
    );
}

#[test]
fn bind_action_makes_a_hidden_action_and_sink_and_pass_reach_the_host() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Sinker", function(name, state, input)
            print("sink", name, state.Name, input.UserInputState.Name, input.KeyCode.Name)
            return Enum.ContextActionResult.Sink
        end, false, Enum.KeyCode.F)
        CAS:BindAction("Passer", function(name, state)
            print("pass", name, state.Name)
            return Enum.ContextActionResult.Pass
        end, false, Enum.KeyCode.G, Enum.KeyCode.ButtonA)
        CAS:BindActionAtPriority("Quiet", function() end, true, Enum.ContextActionPriority.High,
            Enum.UserInputType.MouseButton1)
        "#,
    );
    rig.host.start().unwrap();
    let sinker = bound_action(&rig, "Sinker").expect("Sinker action");
    let passer = bound_action(&rig, "Passer").expect("Passer action");
    let quiet = bound_action(&rig, "Quiet").expect("Quiet action");

    // Spec 5.2: a hidden InputAction with one binding per key, in a context of that priority.
    let keys = |id: InstanceId| -> Vec<Value> {
        let w = rig.world.borrow();
        w.children(id)
            .iter()
            .map(|&b| w.get(b, "KeyCode").unwrap())
            .collect()
    };
    assert_eq!(keys(sinker), [Value::String("F".into())]);
    assert_eq!(
        keys(passer),
        [Value::String("G".into()), Value::String("ButtonA".into())]
    );
    assert_eq!(keys(quiet), [Value::String("MouseButton1".into())]);
    assert_eq!(prop(&rig, quiet, "ShowOnTouch"), Value::Bool(true));
    assert_eq!(prop(&rig, sinker, "ShowOnTouch"), Value::Bool(false));
    let priority = |id: InstanceId| {
        let ctx = rig.world.borrow().parent(id).unwrap();
        prop(&rig, ctx, "Priority")
    };
    assert_eq!(priority(sinker), Value::Number(2000.0));
    assert_eq!(priority(quiet), Value::Number(3000.0));

    rig.host.input_action_event(press(sinker, 0.1));
    rig.host.input_action_event(press(passer, 0.2));
    rig.host.input_action_event(press(quiet, 0.3));
    rig.host.input_action_event(release(sinker, 0.4));
    rig.run(0.0, 1);
    assert_eq!(
        rig.lines(),
        [
            "print:sink Sinker Begin Begin Unknown",
            "print:pass Passer Begin",
            "print:sink Sinker End End Unknown"
        ]
    );
    let results: Vec<_> = rig
        .host
        .take_bound_results()
        .into_iter()
        .map(|r| (r.action, r.sink))
        .collect();
    assert_eq!(
        results,
        [
            (sinker, true),
            (passer, false),
            (quiet, true),
            (sinker, true)
        ],
        "Sink, Pass, and no return value sinks"
    );
    assert!(rig.host.take_bound_results().is_empty());
}

#[test]
fn bind_action_change_state_and_touch_setters() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Steer", function(name, state, input)
            print(name, state.Name, input.Position.X, input.Position.Y)
        end, true, Enum.KeyCode.Thumbstick1)
        CAS:SetTitle("Steer", "Go")
        CAS:SetImage("Steer", "rbxasset://x.png")
        CAS:SetPosition("Steer", UDim2.new(0.5, 4, 0.25, 8))
        print(CAS:GetButton("Steer"))
        "#,
    );
    rig.host.start().unwrap();
    let steer = bound_action(&rig, "Steer").expect("Steer action");
    assert_eq!(
        prop(&rig, steer, "Type"),
        Value::String("Direction2D".into())
    );
    assert_eq!(prop(&rig, steer, "TouchLabel"), Value::String("Go".into()));
    assert_eq!(
        prop(&rig, steer, "TouchImage"),
        Value::String("rbxasset://x.png".into())
    );
    assert_eq!(
        prop(&rig, steer, "TouchLayout"),
        Value::String("Custom".into())
    );
    match prop(&rig, steer, "TouchPosition") {
        Value::UDim2(u) => assert_eq!(
            (u.x_scale, u.x_offset, u.y_scale, u.y_offset),
            (0.5, 4.0, 0.25, 8.0)
        ),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        rig.lines(),
        ["print:nil"],
        "GetButton returns nil until the overlay places the button"
    );
    rig.host.input_action_event(event(
        steer,
        InputEventKind::StateChanged,
        InputValue::Axis2([0.5, -1.0]),
        0.1,
    ));
    rig.run(0.0, 1);
    assert_eq!(rig.lines()[1], "print:Steer Change 0.5 -1");
}

#[test]
fn rebinding_a_name_replaces_the_action() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Fire", function() print("old") end, false, Enum.KeyCode.F)
        CAS:BindAction("Fire", function() print("new") end, false, Enum.KeyCode.G)
        "#,
    );
    rig.host.start().unwrap();
    let fire = bound_action(&rig, "Fire").expect("action");
    {
        let w = rig.world.borrow();
        let kids = w.children(fire);
        assert_eq!(kids.len(), 1);
        assert_eq!(
            w.get(kids[0], "KeyCode").unwrap(),
            Value::String("G".into())
        );
    }
    rig.host.input_action_event(press(fire, 0.1));
    rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:new"]);
}

#[test]
fn unbind_action_cancels_a_pressed_action() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        local function handler(name, state)
            print(name, state.Name)
        end
        CAS:BindAction("Hold", handler, false, Enum.KeyCode.H)
        CAS:BindAction("Idle", handler, false, Enum.KeyCode.J)
        task.delay(0.05, function()
            CAS:UnbindAction("Hold")
            CAS:UnbindAction("Idle")
            CAS:UnbindAction("Nothing")
        end)
        "#,
    );
    rig.host.start().unwrap();
    let hold = bound_action(&rig, "Hold").unwrap();
    rig.host.input_action_event(press(hold, 0.01));
    let t = rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:Hold Begin"]);
    let t = rig.run(t, 6);
    assert_eq!(
        rig.lines(),
        ["print:Hold Begin", "print:Hold Cancel"],
        "only the pressed action is cancelled"
    );
    assert!(bound_action(&rig, "Hold").is_none(), "the action is gone");
    assert!(bound_action(&rig, "Idle").is_none());
    // Events for the old action are dropped.
    rig.host.input_action_event(release(hold, 1.0));
    rig.run(t, 2);
    assert_eq!(rig.lines().len(), 2);
}

#[test]
fn disabling_the_context_cancels_a_pressed_bound_action() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        game:GetService("ContextActionService"):BindAction("Hold", function(name, state)
            print(name, state.Name)
        end, false, Enum.KeyCode.H)
        "#,
    );
    rig.host.start().unwrap();
    let hold = bound_action(&rig, "Hold").unwrap();
    rig.host.input_action_event(press(hold, 0.01));
    let t = rig.run(0.0, 1);
    let ctx = rig.world.borrow().parent(hold).unwrap();
    rig.world
        .borrow_mut()
        .set(ctx, "Enabled", Value::Bool(false))
        .unwrap();
    let t = rig.run(t, 1);
    assert_eq!(rig.lines(), ["print:Hold Begin", "print:Hold Cancel"]);
    rig.run(t, 2);
    assert_eq!(rig.lines().len(), 2, "cancelled once");
}

#[test]
fn a_stopped_script_loses_its_bound_actions() {
    let mut rig = Rig::new();
    let script = rig.local_script(
        "Client",
        r#"
        game:GetService("ContextActionService"):BindAction("Hold", function() end, false, Enum.KeyCode.H)
        "#,
    );
    rig.host.start().unwrap();
    assert!(bound_action(&rig, "Hold").is_some());
    rig.world.borrow_mut().destroy(script);
    rig.run(0.0, 1);
    assert!(bound_action(&rig, "Hold").is_none());
}

#[test]
fn vector2_arithmetic_and_remote_round_trip() {
    let mut rig = Rig::new();
    rig.add("RemoteEvent", "Move", rig.root("ReplicatedStorage"));
    rig.server_script(
        "Server",
        r#"
        local a, b = Vector2.new(3, 4), Vector2.new(1, 2)
        print(a.X, a.Y, a.Magnitude, a.Unit.X, a.Unit.Y)
        print(tostring(a + b), tostring(a - b), tostring(a * 2), tostring(2 * a), tostring(a * b), tostring(a / 2))
        print(a == Vector2.new(3, 4), a == b, typeof(a), tostring(-a), Vector2.new().Unit.X)
        local move = game:GetService("ReplicatedStorage").Move
        move.OnServerEvent:Connect(function(player, dir, list)
            print("server", typeof(dir), dir.X, dir.Y, dir.Magnitude, list[1].Y)
            move:FireClient(player, dir * 2)
        end)
        "#,
    );
    rig.local_script(
        "Client",
        r#"
        local move = game:GetService("ReplicatedStorage"):WaitForChild("Move")
        move.OnClientEvent:Connect(function(v) print("client", typeof(v), v.X, v.Y) end)
        move:FireServer(Vector2.new(0.6, 0.8), { Vector2.new(1, 7) })
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 3);
    assert_eq!(
        rig.lines(),
        [
            "print:3 4 5 0.6 0.8",
            "print:4, 6 2, 2 6, 8 6, 8 3, 8 1.5, 2",
            "print:true false Vector2 -3, -4 0",
            "print:server Vector2 0.6 0.8 1 7",
            "print:client Vector2 1.2 1.6"
        ]
    );
}

#[test]
fn vector2_errors_are_script_errors() {
    let mut rig = Rig::new();
    rig.server_script(
        "Server",
        r#"
        print(pcall(function() return Vector2.new(1, 2) * "x" end))
        print(pcall(function() return Vector2.new(1, 2) + 1 end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert_eq!(lines.len(), 2);
    assert!(
        lines.iter().all(|l| l.starts_with("print:false")),
        "{lines:?}"
    );
}

#[test]
fn enums_have_the_input_names() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        print(Enum.KeyCode.Space.Name, tostring(Enum.KeyCode.Space), Enum.KeyCode.Space == Enum.KeyCode.Space)
        print(Enum.KeyCode.Space == Enum.KeyCode.W, Enum.UserInputState.Begin.Name, Enum.UserInputType.MouseButton1.Name)
        print(Enum.InputActionType.Direction2D.Name, Enum.InputTrigger.DoubleTap.Name, Enum.TouchSlot.Look.Name)
        print(Enum.ContextActionResult.Sink.Name, Enum.PreferredInput.Touch.Name, Enum.ContextActionPriority.Medium.Value)
        print(pcall(function() return Enum.KeyCode.Nope end))
        print(pcall(function() return Enum.Nope end))
        print(pcall(function() Enum.KeyCode.Space = 1 end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert_eq!(lines[0], "print:Space Enum.KeyCode.Space true");
    assert_eq!(lines[1], "print:false Begin MouseButton1");
    assert_eq!(lines[2], "print:Direction2D DoubleTap Look");
    assert_eq!(lines[3], "print:Sink Touch 2000");
    assert!(
        lines[4].starts_with("print:false")
            && lines[4].ends_with("Nope is not a valid member of Enum.KeyCode"),
        "{lines:?}"
    );
    assert!(
        lines[5].ends_with("Nope is not a valid member of Enum"),
        "{lines:?}"
    );
    assert!(lines[6].starts_with("print:false"), "{lines:?}");
}

#[test]
fn user_input_service_last_input_and_devices() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local uis = game:GetService("UserInputService")
        print(uis:GetLastInputType().Name, uis.PreferredInput.Name, uis.TouchEnabled, uis.KeyboardEnabled, uis.GamepadEnabled)
        uis.LastInputTypeChanged:Connect(function(t) print("changed", t.Name, uis:GetLastInputType() == t) end)
        game:GetService("RunService").Heartbeat:Connect(function()
            print(uis.PreferredInput.Name, uis.TouchEnabled, uis.GamepadEnabled)
        end)
        print(pcall(function() return uis.Nope end))
        "#,
    );
    rig.host.start().unwrap();
    assert_eq!(
        rig.lines()[0],
        "print:None KeyboardAndMouse false true false"
    );
    assert!(rig.lines()[1].starts_with("print:false"));
    rig.host.set_input_devices(true, false, true);
    rig.host.set_preferred_input(InputDevice::Gamepad);
    assert!(rig.host.set_last_input_type("Gamepad1"));
    assert!(!rig.host.set_last_input_type("NotAType"));
    let t = rig.run(0.0, 1);
    assert_eq!(rig.lines()[2], "print:changed Gamepad1 true");
    assert_eq!(rig.lines()[3], "print:Gamepad true true");
    // The same type again fires nothing.
    rig.host.set_last_input_type("Gamepad1");
    rig.run(t, 1);
    assert_eq!(rig.lines().len(), 5);
}

#[test]
fn instance_new_refuses_the_input_services() {
    let mut rig = Rig::new();
    rig.server_script(
        "Server",
        r#"
        print(pcall(function() return Instance.new("UserInputService") end))
        print(game:GetService("UserInputService") == game:GetService("UserInputService"))
        print(game.ContextActionService == game:GetService("ContextActionService"))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert!(lines[0].starts_with("print:false"), "{lines:?}");
    assert_eq!(lines[1], "print:true");
    assert_eq!(lines[2], "print:true");
}

#[test]
fn dash_example_from_the_spec() {
    let (mut rig, dash) = input_rig();
    rig.add("RemoteEvent", "DashRequest", rig.root("ReplicatedStorage"));
    rig.local_script(
        "DashClient",
        r#"
        local ReplicatedStorage = game:GetService("ReplicatedStorage")
        local dash = ReplicatedStorage.Input.Gameplay.Dash
        local request = ReplicatedStorage:WaitForChild("DashRequest")

        dash.Triggered:Connect(function()
            request:FireServer()
        end)
        "#,
    );
    rig.server_script(
        "DashServer",
        r#"
        local ReplicatedStorage = game:GetService("ReplicatedStorage")
        local request = ReplicatedStorage.DashRequest
        local cooling = {}

        request.OnServerEvent:Connect(function(player)
            if cooling[player] then return end
            cooling[player] = true
            print("dash", player.Name)
            task.delay(2, function() cooling[player] = nil end)
        end)
        "#,
    );
    rig.host.start().unwrap();
    let mut t = rig.run(0.0, 1);
    // The first request dashes; one a moment later is ignored (inside the cooldown).
    rig.host.input_action_event(trigger(dash, t));
    t = rig.run(t, 2);
    assert_eq!(rig.lines(), ["print:dash Player1"]);
    rig.host.input_action_event(trigger(dash, t));
    t = rig.run(t, 2);
    assert_eq!(rig.lines().len(), 1, "cooldown: {:?}", rig.lines());
    // After 2 seconds the cooldown is over.
    t = rig.run(t, 130);
    rig.host.input_action_event(trigger(dash, t));
    rig.run(t, 2);
    assert_eq!(
        rig.lines(),
        ["print:dash Player1", "print:dash Player1"],
        "second dash after the cooldown"
    );
}

#[test]
fn at_most_16_events_are_dispatched_per_step() {
    assert_eq!(MAX_ACTION_EVENTS_PER_STEP, 16);
    let (mut rig, dash) = input_rig();
    rig.local_script(
        "Client",
        r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        local n = 0
        dash.Triggered:Connect(function() n += 1 end)
        game:GetService("RunService").Heartbeat:Connect(function() print(n) end)
        "#,
    );
    rig.host.start().unwrap();
    for i in 0..40 {
        rig.host
            .input_action_event(trigger(dash, 0.001 * f64::from(i)));
    }
    rig.run(0.0, 4);
    assert_eq!(
        rig.lines(),
        ["print:16", "print:32", "print:40", "print:40"]
    );
}

#[test]
fn the_event_queue_is_bounded() {
    let (mut rig, dash) = input_rig();
    rig.host.start().unwrap();
    for i in 0..1500 {
        rig.host.input_action_event(trigger(dash, f64::from(i)));
    }
    assert_eq!(rig.host.dropped_action_events(), 1500 - 1024);
}

#[test]
fn a_spinning_input_handler_times_out_and_others_still_run() {
    common::real_limits_test(|| {
        let (mut rig, dash) = input_rig();
        rig.host.set_resume_budget(metallic_script::RESUME_BUDGET);
        rig.local_script(
            "Spinner",
            r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Triggered:Connect(function() while true do end end)
        "#,
        );
        rig.local_script(
            "Other",
            r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Triggered:Connect(function() print("other") end)
        "#,
        );
        rig.host.start().unwrap();
        rig.host.input_action_event(trigger(dash, 0.1));
        let began = std::time::Instant::now();
        rig.run(0.0, 3);
        let lines = rig.lines();
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("error:") && l.contains("Script timeout")),
            "{lines:?}"
        );
        assert!(lines.contains(&"print:other".to_string()), "{lines:?}");
        assert!(began.elapsed().as_secs_f64() < 1.0, "bounded by the budget");
    });
}

fn descendant_count(rig: &Rig) -> usize {
    fn walk(w: &metallic_core::World, id: InstanceId) -> usize {
        1 + w.children(id).iter().map(|&c| walk(w, c)).sum::<usize>()
    }
    let w = rig.world.borrow();
    let player = w
        .find_first_child(w.root("Players").unwrap(), "Player1")
        .unwrap();
    walk(&w, player)
}

#[test]
fn bind_action_is_capped_per_script() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        local ok, failed, message = 0, 0, nil
        for i = 1, 300 do
            local good, err = pcall(function()
                CAS:BindAction("A" .. i, function() end, false, Enum.KeyCode.A)
            end)
            if good then ok += 1 else failed += 1; message = err end
        end
        -- Binding an existing name again is not a new action.
        print(ok, failed, pcall(function()
            CAS:BindAction("A1", function() end, false, Enum.KeyCode.B)
        end))
        print(message)
        CAS:UnbindAction("A2")
        print(pcall(function() CAS:BindAction("Again", function() end, false, Enum.KeyCode.C) end))
        "#,
    );
    rig.host.start().unwrap();
    let lines = rig.lines();
    assert_eq!(lines[0], "print:256 44 true");
    assert!(
        lines[1].contains("too many bound actions") && lines[1].contains("256"),
        "{lines:?}"
    );
    assert_eq!(lines[2], "print:true");
}

#[test]
fn a_failed_rebind_keeps_the_old_binding_and_leaves_nothing_behind() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Hold", function(name, state) print("old", state.Name) end, false, Enum.KeyCode.H)
        print(pcall(function()
            CAS:BindAction("Hold", function() print("new") end, false, Enum.KeyCode.G, 42)
        end))
        print(pcall(function()
            CAS:BindActionAtPriority("Hold", function() print("new") end, false, "high", Enum.KeyCode.G)
        end))
        "#,
    );
    rig.host.start().unwrap();
    let hold = bound_action(&rig, "Hold").expect("old action still there");
    let before = descendant_count(&rig);
    let lines = rig.lines();
    assert!(lines[0].starts_with("print:false"), "{lines:?}");
    assert!(lines[1].starts_with("print:false"), "{lines:?}");
    rig.host.input_action_event(press(hold, 0.1));
    rig.run(0.0, 1);
    assert_eq!(
        rig.lines()[2],
        "print:old Begin",
        "the old handler still works"
    );
    assert_eq!(
        bound_action(&rig, "Hold"),
        Some(hold),
        "same action instance"
    );
    assert_eq!(descendant_count(&rig), before, "no new instances");
}

#[test]
fn unbinding_inside_a_handler_leaves_no_hidden_instances() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Once", function(name, state)
            CAS:UnbindAction("Once")
        end, false, Enum.KeyCode.H)
        "#,
    );
    rig.host.start().unwrap();
    let once = bound_action(&rig, "Once").unwrap();
    rig.host.input_action_event(press(once, 0.1));
    rig.run(0.0, 2);
    assert!(bound_action(&rig, "Once").is_none());
    assert_eq!(
        descendant_count(&rig),
        2,
        "only the player and its PlayerGui are left"
    );
}

#[test]
fn get_button_reads_the_overlay_placement() {
    let mut rig = Rig::new();
    rig.local_script(
        "Client",
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Dash", function() end, true, Enum.KeyCode.E)
        task.wait(0.05)
        local b = CAS:GetButton("Dash")
        print(typeof(b), b.ClassName, b.Name, b.Text, b.Image, b.Visible)
        print(b.AbsolutePosition.X, b.AbsolutePosition.Y, b.AbsoluteSize.X, b.Size.X.Offset)
        print(pcall(function() return b.Nope end))
        "#,
    );
    rig.host.start().unwrap();
    let dash = bound_action(&rig, "Dash").expect("Dash action");
    rig.host.set_touch_buttons(&[TouchButtonInfo {
        action: dash,
        position: [624.0, 282.0],
        size: [56.0, 56.0],
        title: "Dash".into(),
        image: "ui/dash.png".into(),
        visible: true,
    }]);
    rig.run(0.0, 10);
    let lines = rig.lines();
    assert_eq!(
        lines[0],
        "print:TouchButton TouchButton Dash Dash ui/dash.png true"
    );
    assert_eq!(lines[1], "print:624 282 56 56");
    assert!(
        lines[2].contains("Nope is not a valid member of TouchButton"),
        "{}",
        lines[2]
    );
}

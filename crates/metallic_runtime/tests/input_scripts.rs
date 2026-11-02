#![cfg(feature = "presentation")]
//! End to end: a scripted key press goes through `Game::input_frame` to LocalScript handlers,
//! and the `Sink` / `Pass` answer of a `ContextActionService` handler decides whether the
//! default character controls see the key (spec 3.7, 5.2).

use metallic_core::{Value, Vec3, World};
use metallic_runtime::input::{Control, Device, RawEvent};
use metallic_runtime::{Game, InputRuntime, TICK};
use metallic_script::OutputSink;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Lines(RefCell<Vec<String>>);

impl OutputSink for Lines {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.to_string());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warn: {line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error: {line}"));
    }
}

struct Rig {
    game: Game,
    input: InputRuntime,
    out: Rc<Lines>,
    clock: f64,
}

impl Rig {
    /// A started game on a flat floor whose LocalScript is `source`.
    fn new(source: &str, extra: impl FnOnce(&mut World)) -> Rig {
        let mut w = World::new_with_services();
        let ws = w.root("Workspace").unwrap();
        let floor = w.create("Part", Some(ws)).unwrap();
        w.set(floor, "Size", Value::Vector3(Vec3::new(400.0, 1.0, 400.0)))
            .unwrap();
        w.set(floor, "Position", Value::Vector3(Vec3::new(0.0, -0.5, 0.0)))
            .unwrap();
        w.set(floor, "Anchored", Value::Bool(true)).unwrap();
        let scripts = w
            .find_first_child(w.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
            .unwrap();
        let script = w.create("LocalScript", Some(scripts)).unwrap();
        w.set(script, "Source", Value::String(source.to_string()))
            .unwrap();
        extra(&mut w);
        let out = Rc::new(Lines::default());
        metallic_script::use_test_budgets_on_this_thread();
        let mut game = Game::from_world("input".into(), w, out.clone());
        game.start().unwrap();
        let input = InputRuntime::from_world(&game.world().borrow());
        Rig {
            game,
            input,
            out,
            clock: 10.0,
        }
    }

    /// One frame; returns the height of the character's root part.
    fn frame(&mut self) -> f64 {
        self.clock += TICK;
        let fi = self
            .game
            .input_frame(&mut self.input, self.clock, TICK as f32);
        self.game.frame(&fi, TICK);
        self.root_y()
    }

    fn frames(&mut self, n: usize) -> f64 {
        let mut highest = f64::MIN;
        for _ in 0..n {
            highest = highest.max(self.frame());
        }
        highest
    }

    fn key(&mut self, control: Control, down: bool) {
        self.input.push(RawEvent::button(
            Device::KeyboardMouse,
            control,
            if down { 1.0 } else { 0.0 },
            self.clock,
        ));
    }

    fn root_y(&self) -> f64 {
        let world = self.game.world();
        let world = world.borrow();
        let root = world
            .find_first_child(self.game.character().unwrap(), "HumanoidRootPart")
            .unwrap();
        match world.get(root, "Position") {
            Ok(Value::Vector3(v)) => v.y,
            other => panic!("{other:?}"),
        }
    }

    fn lines(&self) -> Vec<String> {
        self.out.0.borrow().clone()
    }
}

fn bind_space(answer: &str) -> String {
    format!(
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Special", function(name, state)
            print(name, state.Name)
            return {answer}
        end, false, Enum.KeyCode.Space)
        "#
    )
}

/// Settles on the floor, holds Space for half a second and returns the highest root height.
fn hold_space(rig: &mut Rig) -> (f64, f64) {
    let ground = rig.frames(30);
    rig.key(Control::Space, true);
    let highest = rig.frames(30);
    rig.key(Control::Space, false);
    rig.frames(2);
    (ground, highest)
}

#[test]
fn pass_lets_the_default_jump_through() {
    let mut rig = Rig::new(&bind_space("Enum.ContextActionResult.Pass"), |_| {});
    let (ground, highest) = hold_space(&mut rig);
    assert_eq!(
        rig.lines(),
        ["Special Begin", "Special End"],
        "the handler fired"
    );
    assert!(
        highest > ground + 1.0,
        "the character jumped: {ground} -> {highest}"
    );
}

#[test]
fn sink_blocks_the_default_jump() {
    let mut rig = Rig::new(&bind_space("Enum.ContextActionResult.Sink"), |_| {});
    let (ground, highest) = hold_space(&mut rig);
    assert_eq!(rig.lines(), ["Special Begin", "Special End"]);
    assert!(
        highest < ground + 0.1,
        "the character stayed down: {ground} -> {highest}"
    );
}

#[test]
fn no_return_value_sinks_and_other_keys_still_reach_the_character() {
    let mut rig = Rig::new(&bind_space("nil").replace("return nil", ""), |_| {});
    let (ground, highest) = hold_space(&mut rig);
    assert!(highest < ground + 0.1, "{ground} -> {highest}");
    // The sink is per control: W still reaches the default controls.
    rig.key(Control::W, true);
    rig.frames(20);
    let z = {
        let world = rig.game.world();
        let world = world.borrow();
        let root = world
            .find_first_child(rig.game.character().unwrap(), "HumanoidRootPart")
            .unwrap();
        match world.get(root, "Position") {
            Ok(Value::Vector3(v)) => v.z,
            other => panic!("{other:?}"),
        }
    };
    assert!(z < -1.0, "W walked the character: z = {z}");
}

#[test]
fn a_sink_in_a_higher_context_hides_the_key_from_a_lower_scene_action() {
    // A scene action in a context below the bound one also reads Space; the Sink hides it.
    let mut rig = Rig::new(&bind_space("Enum.ContextActionResult.Sink"), |w| {
        let rs = w.root("ReplicatedStorage").unwrap();
        let ctx = w.create("InputContext", Some(rs)).unwrap();
        w.set(ctx, "Name", Value::String("Low".into())).unwrap();
        let action = w.create("InputAction", Some(ctx)).unwrap();
        w.set(action, "Name", Value::String("Low".into())).unwrap();
        let binding = w.create("InputBinding", Some(action)).unwrap();
        w.set(binding, "KeyCode", Value::String("Space".into()))
            .unwrap();
    });
    rig.frames(3);
    rig.key(Control::Space, true);
    rig.frames(3);
    let id = rig.input.find_action("Low/Low").expect("scene action");
    assert!(!rig.input.bool(id), "the sunk key never reaches Low/Low");
    rig.key(Control::Space, false);
    rig.frames(3);
}

#[test]
fn scene_actions_reach_their_scripts_through_the_game() {
    let source = r#"
        local dash = game:GetService("ReplicatedStorage"):WaitForChild("Input").Gameplay.Dash
        dash.Pressed:Connect(function() print("pressed", dash:GetState()) end)
        dash.Released:Connect(function() print("released", dash:GetState()) end)
        dash.Triggered:Connect(function() print("triggered") end)
    "#;
    let mut rig = Rig::new(source, |w| {
        let rs = w.root("ReplicatedStorage").unwrap();
        let folder = w.create("Folder", Some(rs)).unwrap();
        w.set(folder, "Name", Value::String("Input".into()))
            .unwrap();
        let ctx = w.create("InputContext", Some(folder)).unwrap();
        w.set(ctx, "Name", Value::String("Gameplay".into()))
            .unwrap();
        let action = w.create("InputAction", Some(ctx)).unwrap();
        w.set(action, "Name", Value::String("Dash".into())).unwrap();
        let binding = w.create("InputBinding", Some(action)).unwrap();
        w.set(binding, "KeyCode", Value::String("Q".into()))
            .unwrap();
    });
    rig.frames(3);
    rig.key(Control::Q, true);
    rig.frames(3);
    rig.key(Control::Q, false);
    rig.frames(3);
    assert_eq!(rig.lines(), ["pressed true", "triggered", "released false"]);
}

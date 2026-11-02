//! End to end (spec 14, #252): a finger on an automatic touch button fires the `InputAction`
//! for a LocalScript, and `ContextActionService:GetButton` returns the overlay's button.
#![cfg(feature = "presentation")]

use metallic_core::{Value, World};
use metallic_runtime::input::touch::{DeviceProfile, TouchButtonInfo, TouchControls, TouchPhase};
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
    touch: TouchControls,
    infos: Vec<TouchButtonInfo>,
    out: Rc<Lines>,
    clock: f64,
}

impl Rig {
    fn new(source: &str, scene: impl FnOnce(&mut World)) -> Rig {
        let mut w = World::new_with_services();
        let scripts = w
            .find_first_child(w.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
            .unwrap();
        let script = w.create("LocalScript", Some(scripts)).unwrap();
        w.set(script, "Source", Value::String(source.to_string()))
            .unwrap();
        scene(&mut w);
        let out = Rc::new(Lines::default());
        metallic_script::use_test_budgets_on_this_thread();
        let mut game = Game::from_world("touch".into(), w, out.clone());
        game.start().unwrap();
        let input = InputRuntime::from_world(&game.world().borrow());
        Rig {
            game,
            input,
            touch: TouchControls::new(DeviceProfile::phone()),
            infos: Vec::new(),
            out,
            clock: 10.0,
        }
    }

    /// One Player frame: the overlay follows the actions and tells the scripts where its buttons
    /// are, then input is evaluated and the game steps.
    fn frame(&mut self) {
        self.clock += TICK;
        if let Ok(w) = self.game.world().try_borrow() {
            self.input.sync(&w);
        }
        if self.touch.sync(&mut self.input, self.clock) {
            self.touch.button_infos(true, &mut self.infos);
            self.game.set_touch_buttons(&self.infos);
        }
        let fi = self
            .game
            .input_frame(&mut self.input, self.clock, TICK as f32);
        self.game.frame(&fi, TICK);
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn button(&self, label: &str) -> [f32; 2] {
        let set = self.touch.set();
        self.touch
            .layout()
            .buttons
            .iter()
            .find(|b| set.buttons[b.index].label == label)
            .unwrap_or_else(|| panic!("no {label} button"))
            .center
    }

    fn finger(&mut self, id: u64, phase: TouchPhase, at: [f32; 2]) {
        self.touch.touch(&mut self.input, id, phase, at, self.clock);
    }

    fn lines(&self) -> Vec<String> {
        self.out.0.borrow().clone()
    }
}

#[test]
fn a_touch_press_fires_the_input_action() {
    let mut rig = Rig::new(
        r#"
        local fire = game:GetService("ReplicatedStorage"):WaitForChild("Game"):WaitForChild("Fire")
        fire.Pressed:Connect(function() print("pressed") end)
        fire.Triggered:Connect(function() print("triggered") end)
        fire.Released:Connect(function() print("released") end)
        "#,
        |w| {
            let rs = w.root("ReplicatedStorage").unwrap();
            let ctx = w.create("InputContext", Some(rs)).unwrap();
            w.set(ctx, "Name", Value::String("Game".into())).unwrap();
            let fire = w.create("InputAction", Some(ctx)).unwrap();
            w.set(fire, "Name", Value::String("Fire".into())).unwrap();
            w.set(fire, "ShowOnTouch", Value::Bool(true)).unwrap();
            w.set(fire, "TouchOrder", Value::Number(-1.0)).unwrap();
            w.set(fire, "TouchLabel", Value::String("Fire".into()))
                .unwrap();
        },
    );
    rig.frames(3);
    let fire = rig.button("Fire");
    assert_eq!(
        rig.touch.layout().buttons[0].center,
        fire,
        "TouchOrder -1 is the primary"
    );
    rig.finger(7, TouchPhase::Down, fire);
    rig.frames(2);
    assert_eq!(rig.lines(), ["pressed", "triggered"]);
    rig.finger(7, TouchPhase::Up, fire);
    rig.frames(2);
    assert_eq!(rig.lines(), ["pressed", "triggered", "released"]);
}

#[test]
fn get_button_returns_the_overlay_button_of_a_bound_action() {
    let mut rig = Rig::new(
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Dash", function(name, state)
            print(name, state.Name)
        end, true, Enum.KeyCode.E)
        CAS:SetTitle("Dash", "Go")
        CAS:BindAction("Hidden", function() end, false, Enum.KeyCode.Q)
        task.wait(0.1)
        local b = CAS:GetButton("Dash")
        print(typeof(b), b.Name, b.Text, b.Visible, b.AbsoluteSize.X)
        print(CAS:GetButton("Hidden"), CAS:GetButton("Nope"))
        "#,
        |_| {},
    );
    rig.frames(12);
    let dash = rig.button("Go");
    let lines = rig.lines();
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert!(
        lines[0].starts_with("TouchButton Dash Go true "),
        "{}",
        lines[0]
    );
    assert_eq!(lines[1], "nil nil");
    rig.finger(1, TouchPhase::Down, dash);
    rig.frames(2);
    rig.finger(1, TouchPhase::Up, dash);
    rig.frames(2);
    assert_eq!(&rig.lines()[2..], ["Dash Begin", "Dash End"]);
}

/// The review case of #325: a title change while a finger holds the button keeps the press,
/// and the lift still ends it normally.
#[test]
fn set_title_while_held_keeps_the_press() {
    let mut rig = Rig::new(
        r#"
        local CAS = game:GetService("ContextActionService")
        CAS:BindAction("Dash", function(name, state)
            print(name, state.Name)
        end, true, Enum.KeyCode.E)
        task.wait(0.2)
        CAS:SetTitle("Dash", "Go")
        print("retitled")
        "#,
        |_| {},
    );
    rig.frames(3);
    let dash = rig.button("Dash");
    let generation = rig.input.touch_generation();
    rig.finger(1, TouchPhase::Down, dash);
    rig.frames(20);
    assert!(rig.input.touch_generation() > generation, "a new touch set");
    assert_eq!(rig.button("Go"), dash);
    rig.finger(1, TouchPhase::Up, dash);
    rig.frames(2);
    assert_eq!(rig.lines(), ["Dash Begin", "retitled", "Dash End"]);
}

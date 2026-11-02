#![cfg(feature = "presentation")]
//! Clickable GUI end to end (#79): pointer events through `Game::gui_pointer` reach LocalScript
//! handlers of TextButton and ImageButton events, overlapping buttons resolve by ZIndex, and a
//! press the GUI takes is reported so the caller keeps it from the game's input.

use metallic_core::{UDim2, Value, Vec3, World};
use metallic_runtime::gui_input::{PointerId, PointerPhase};
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

const SCREEN: [f32; 2] = [800.0, 600.0];

/// Logs every event of the buttons Play (front, ZIndex 2) and Shop (behind, ZIndex 1), which
/// overlap from x 150 to 200, and of the Frame Panel behind both.
const LOGGER: &str = r#"
local hud = script.Parent
local function log(name, event, ...)
    local parts = {name, event}
    for _, v in {...} do
        if typeof(v) == "table" then
            table.insert(parts, v.UserInputType.Name .. "/" .. v.UserInputState.Name
                .. "@" .. v.Position.X .. "," .. v.Position.Y)
        else
            table.insert(parts, tostring(v))
        end
    end
    print(table.concat(parts, " "))
end
for _, name in {"Play", "Shop"} do
    local b = hud[name]
    for _, e in {"Activated", "MouseButton1Click", "MouseButton1Down", "MouseButton1Up",
                 "MouseEnter", "MouseLeave", "InputBegan", "InputEnded"} do
        b[e]:Connect(function(...) log(name, e, ...) end)
    end
end
hud.Panel.InputBegan:Connect(function(input) log("Panel", "InputBegan", input) end)
"#;

struct Rig {
    game: Game,
    input: InputRuntime,
    out: Rc<Lines>,
    clock: f64,
}

impl Rig {
    fn new() -> Rig {
        let mut w = World::new_with_services();
        let ws = w.root("Workspace").unwrap();
        let floor = w.create("Part", Some(ws)).unwrap();
        w.set(floor, "Size", Value::Vector3(Vec3::new(400.0, 1.0, 400.0)))
            .unwrap();
        w.set(floor, "Position", Value::Vector3(Vec3::new(0.0, -0.5, 0.0)))
            .unwrap();
        w.set(floor, "Anchored", Value::Bool(true)).unwrap();
        let starter = w.root("StarterGui").unwrap();
        let hud = w.create("ScreenGui", Some(starter)).unwrap();
        w.set(hud, "Name", Value::String("Hud".into())).unwrap();
        let add = |w: &mut World, class: &str, name: &str, rect: [f64; 4], z: f64| {
            let e = w.create(class, Some(hud)).unwrap();
            w.set(e, "Name", Value::String(name.into())).unwrap();
            w.set(
                e,
                "Position",
                Value::UDim2(UDim2::new(0.0, rect[0], 0.0, rect[1])),
            )
            .unwrap();
            w.set(
                e,
                "Size",
                Value::UDim2(UDim2::new(0.0, rect[2], 0.0, rect[3])),
            )
            .unwrap();
            w.set(e, "ZIndex", Value::Number(z)).unwrap();
        };
        add(&mut w, "Frame", "Panel", [0.0, 0.0, 400.0, 400.0], 0.0);
        add(
            &mut w,
            "TextButton",
            "Play",
            [150.0, 100.0, 100.0, 50.0],
            2.0,
        );
        add(
            &mut w,
            "ImageButton",
            "Shop",
            [100.0, 100.0, 100.0, 50.0],
            1.0,
        );
        let script = w.create("LocalScript", Some(hud)).unwrap();
        w.set(script, "Source", Value::String(LOGGER.into()))
            .unwrap();
        let out = Rc::new(Lines::default());
        metallic_script::use_test_budgets_on_this_thread();
        let mut game = Game::from_world("gui".into(), w, out.clone());
        game.start().unwrap();
        let input = InputRuntime::from_world(&game.world().borrow());
        let mut rig = Rig {
            game,
            input,
            out,
            clock: 10.0,
        };
        rig.frame();
        rig.lines();
        rig
    }

    fn frame(&mut self) {
        self.clock += TICK;
        let fi = self
            .game
            .input_frame(&mut self.input, self.clock, TICK as f32);
        self.game.frame(&fi, TICK);
    }

    fn pointer(&mut self, id: PointerId, phase: PointerPhase, at: [f32; 2]) -> bool {
        self.game.gui_pointer(id, phase, at, SCREEN)
    }

    /// The lines printed since the last call (errors fail the test).
    fn lines(&mut self) -> Vec<String> {
        let lines = std::mem::take(&mut *self.out.0.borrow_mut());
        assert!(
            !lines.iter().any(|l| l.starts_with("error")),
            "script errors: {lines:?}"
        );
        lines
    }
}

#[test]
fn a_click_on_a_button_fires_its_events_once_in_order() {
    let mut r = Rig::new();
    let at = [220.0, 120.0];
    assert!(!r.pointer(PointerId::Mouse, PointerPhase::Move, at));
    r.frame();
    assert_eq!(r.lines(), ["Play MouseEnter 220 120"]);
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Down, at));
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Up, at));
    r.frame();
    assert_eq!(
        r.lines(),
        [
            "Play InputBegan MouseButton1/Begin@220,120",
            "Play MouseButton1Down 220 120",
            "Play InputEnded MouseButton1/End@220,120",
            "Play MouseButton1Up 220 120",
            "Play MouseButton1Click",
            "Play Activated MouseButton1/End@220,120 1",
        ]
    );
    r.frame();
    assert!(r.lines().is_empty(), "nothing fires twice");
    assert!(!r.pointer(PointerId::Mouse, PointerPhase::Move, [600.0, 500.0]));
    r.frame();
    assert_eq!(r.lines(), ["Play MouseLeave 600 500"]);
}

#[test]
fn overlapping_buttons_resolve_by_zindex() {
    let mut r = Rig::new();
    // In the overlap Play (ZIndex 2) is in front of Shop (ZIndex 1), though Shop is later.
    r.pointer(PointerId::Mouse, PointerPhase::Down, [175.0, 120.0]);
    r.pointer(PointerId::Mouse, PointerPhase::Up, [175.0, 120.0]);
    r.frame();
    let lines = r.lines();
    assert!(lines.contains(&"Play Activated MouseButton1/End@175,120 1".to_string()));
    assert!(
        !lines
            .iter()
            .any(|l| l.starts_with("Shop") && !l.contains("Mouse")),
        "Shop gets no press: {lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("Panel")),
        "the button takes the press before the Panel behind it: {lines:?}"
    );
    // Only Shop is under x 120.
    r.pointer(PointerId::Mouse, PointerPhase::Down, [120.0, 120.0]);
    r.pointer(PointerId::Mouse, PointerPhase::Up, [120.0, 120.0]);
    r.frame();
    assert!(
        r.lines()
            .contains(&"Shop Activated MouseButton1/End@120,120 1".to_string())
    );
}

#[test]
fn a_release_off_the_button_does_not_click() {
    let mut r = Rig::new();
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Down, [220.0, 120.0]));
    assert!(
        r.pointer(PointerId::Mouse, PointerPhase::Move, [700.0, 500.0]),
        "a drag that began on a button stays the GUI's"
    );
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Up, [700.0, 500.0]));
    r.frame();
    let lines = r.lines();
    assert!(lines.contains(&"Play MouseButton1Down 220 120".to_string()));
    assert!(lines.contains(&"Play InputEnded MouseButton1/End@700,500".to_string()));
    assert!(
        !lines
            .iter()
            .any(|l| l.contains("Click") || l.contains("Activated")),
        "{lines:?}"
    );
}

#[test]
fn a_touch_tap_activates_a_button() {
    let mut r = Rig::new();
    let at = [220.0, 130.0];
    assert!(r.pointer(PointerId::Touch(7), PointerPhase::Down, at));
    assert!(r.pointer(PointerId::Touch(7), PointerPhase::Up, at));
    r.frame();
    let lines = r.lines();
    assert_eq!(
        lines,
        [
            "Play InputBegan Touch/Begin@220,130",
            "Play MouseButton1Down 220 130",
            "Play InputEnded Touch/End@220,130",
            "Play MouseButton1Up 220 130",
            "Play MouseButton1Click",
            "Play Activated Touch/End@220,130 1",
        ],
        "a finger does not hover"
    );
    // A cancelled finger does not click.
    r.pointer(PointerId::Touch(8), PointerPhase::Down, at);
    r.pointer(PointerId::Touch(8), PointerPhase::Cancel, at);
    r.frame();
    assert!(!r.lines().iter().any(|l| l.contains("Activated")));
}

#[test]
fn clicks_off_buttons_go_to_the_game_and_hidden_buttons_take_nothing() {
    let mut r = Rig::new();
    // The Panel is not Active: its InputBegan fires, but the press is the game's.
    assert!(!r.pointer(PointerId::Mouse, PointerPhase::Down, [50.0, 300.0]));
    assert!(!r.pointer(PointerId::Mouse, PointerPhase::Up, [50.0, 300.0]));
    assert!(!r.pointer(PointerId::Touch(1), PointerPhase::Down, [700.0, 500.0]));
    assert!(!r.pointer(PointerId::Touch(1), PointerPhase::Move, [710.0, 500.0]));
    r.frame();
    assert_eq!(
        r.lines(),
        ["Panel InputBegan MouseButton1/Begin@50,300"],
        "no button events"
    );
    {
        let world = r.game.world();
        let mut w = world.borrow_mut();
        let gui = metallic_runtime::gui::local_player_gui(&w).unwrap();
        let hud = w.find_first_child(gui, "Hud").unwrap();
        let play = w.find_first_child(hud, "Play").unwrap();
        w.set(play, "Visible", Value::Bool(false)).unwrap();
        let panel = w.find_first_child(hud, "Panel").unwrap();
        w.set(panel, "Active", Value::Bool(true)).unwrap();
    }
    // Play is hidden: Shop is not under x 220 (it ends at 200), so the Active Panel takes it.
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Down, [220.0, 120.0]));
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Up, [220.0, 120.0]));
    r.frame();
    let lines = r.lines();
    assert!(!lines.iter().any(|l| l.starts_with("Play")), "{lines:?}");
    assert!(lines.contains(&"Panel InputBegan MouseButton1/Begin@220,120".to_string()));
}

/// The caller's side of the contract, routed like the Player window (the GUI first, the
/// game's input only when the GUI declines): a click on a button never reaches a
/// `ContextActionService` action bound to the left mouse button; a click elsewhere does.
#[test]
fn a_click_on_a_button_does_not_reach_game_input() {
    let mut r = Rig::new();
    {
        let world = r.game.world();
        let mut w = world.borrow_mut();
        let scripts = w
            .find_first_child(w.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
            .unwrap();
        let s = w.create("LocalScript", Some(scripts)).unwrap();
        w.set(
            s,
            "Source",
            Value::String(
                r#"
                game:GetService("ContextActionService"):BindAction("Fire", function(_, state)
                    print("Fire", state.Name)
                    return Enum.ContextActionResult.Pass
                end, false, Enum.UserInputType.MouseButton1)
                "#
                .into(),
            ),
        )
        .unwrap();
    }
    r.frame();
    r.lines();
    let click = |r: &mut Rig, at: [f32; 2]| {
        for down in [true, false] {
            let phase = if down {
                PointerPhase::Down
            } else {
                PointerPhase::Up
            };
            if !r.pointer(PointerId::Mouse, phase, at) {
                let value = f32::from(u8::from(down));
                let e =
                    RawEvent::button(Device::KeyboardMouse, Control::MouseButton1, value, r.clock);
                r.input.push(e);
            }
            r.frame();
        }
        r.lines()
    };
    let on_button = click(&mut r, [220.0, 120.0]);
    assert!(
        on_button.contains(&"Play Activated MouseButton1/End@220,120 1".to_string()),
        "{on_button:?}"
    );
    assert!(
        !on_button.iter().any(|l| l.starts_with("Fire")),
        "the game saw the click: {on_button:?}"
    );
    let elsewhere = click(&mut r, [700.0, 500.0]);
    let fire: Vec<&String> = elsewhere.iter().filter(|l| l.starts_with("Fire")).collect();
    assert_eq!(fire, ["Fire Begin", "Fire End"]);
}

/// The persistent Combined host must retain GUI events across zero-tick or borrowed frames.
#[test]
fn button_events_wait_for_a_successful_simulation_tick() {
    let mut r = Rig::new();
    let at = [220.0, 120.0];
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Down, at));
    assert!(r.pointer(PointerId::Mouse, PointerPhase::Up, at));
    let idle = metallic_runtime::FrameInput::default();
    assert_eq!(r.game.frame(&idle, 0.0).ticks, 0);
    assert!(r.lines().is_empty());
    let world = r.game.world();
    let held = world.borrow_mut();
    assert_eq!(r.game.frame(&idle, TICK).ticks, 0);
    assert!(r.lines().is_empty());
    drop(held);
    assert_eq!(r.game.frame(&idle, 0.0).ticks, 1);
    let lines = r.lines();
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.starts_with("Play Activated "))
            .count(),
        1,
        "{lines:?}"
    );
    assert_eq!(
        lines
            .iter()
            .filter(|line| line.as_str() == "Play MouseButton1Click")
            .count(),
        1,
        "{lines:?}"
    );
    r.frame();
    assert!(r.lines().is_empty(), "GUI events must not replay");
}

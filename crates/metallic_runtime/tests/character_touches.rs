#![cfg(feature = "presentation")]
//! Character touches through the whole frame loop: the body volumes report `Touched` and
//! `TouchEnded` with the touching body part, and both sides fire.
use metallic_core::{Value, Vec3, World};
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::OutputSink;
use std::cell::RefCell;
use std::rc::Rc;

#[derive(Default)]
struct Captured(RefCell<Vec<String>>);

impl OutputSink for Captured {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.to_owned());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warning: {line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error: {line}"));
    }
}

const SCRIPT: &str = r#"
local pad = workspace.Pad
pad.Touched:Connect(function(hit) print("pad on " .. hit.Name) end)
pad.TouchEnded:Connect(function(hit) print("pad off " .. hit.Name) end)
local foot = workspace:WaitForChild("Player1"):WaitForChild("LeftFoot")
foot.Touched:Connect(function(other) print("foot on " .. other.Name) end)
foot.TouchEnded:Connect(function(other) print("foot off " .. other.Name) end)
print("ready")
"#;

fn world() -> World {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    for (name, pos, size, solid) in [
        (
            "Floor",
            Vec3::new(0.0, -2.0, 0.0),
            Vec3::new(256.0, 4.0, 256.0),
            true,
        ),
        // A thin sensor on the floor where the character spawns.
        (
            "Pad",
            Vec3::new(0.0, 0.05, 0.0),
            Vec3::new(6.0, 0.1, 6.0),
            false,
        ),
    ] {
        let id = world.create("Part", Some(ws)).unwrap();
        world.set(id, "Name", Value::String(name.into())).unwrap();
        world.set(id, "Position", Value::Vector3(pos)).unwrap();
        world.set(id, "Size", Value::Vector3(size)).unwrap();
        world.set(id, "Anchored", Value::Bool(true)).unwrap();
        world.set(id, "CanCollide", Value::Bool(solid)).unwrap();
    }
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    world
        .set(script, "Source", Value::String(SCRIPT.into()))
        .unwrap();
    world
}

fn run(game: &mut Game, seconds: f64) {
    for _ in 0..(seconds / TICK).round() as usize {
        game.frame(&FrameInput::default(), TICK);
    }
}

/// Moves the character's root to `at` (the body parts follow on the next step).
fn teleport(game: &mut Game, at: Vec3) {
    let shared = game.world();
    let mut w = shared.borrow_mut();
    let character = game.character().unwrap();
    let root = w.find_first_child(character, "HumanoidRootPart").unwrap();
    w.set(root, "Position", Value::Vector3(at)).unwrap();
}

#[test]
fn game_frame_reports_body_part_touches_on_both_sides() {
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("touches".into(), world(), output.clone());
    game.start().unwrap();
    run(&mut game, 1.0);
    {
        let lines = output.0.borrow();
        let has = |l: &str| lines.iter().any(|x| x == l);
        assert!(has("ready"), "{lines:?}");
        assert!(has("pad on LeftFoot"), "{lines:?}");
        assert!(has("pad on HumanoidRootPart"), "{lines:?}");
    }

    // The script waited for the character, so the foot's handlers connected after it had landed
    // on the pad: step off and back on so they see a fresh touch.
    let (off, on) = (Vec3::new(40.0, 3.0, 0.0), Vec3::new(0.0, 3.0, 0.0));
    teleport(&mut game, off);
    run(&mut game, 0.5);
    teleport(&mut game, on);
    run(&mut game, 1.0);
    {
        let lines = output.0.borrow();
        assert!(
            lines.iter().any(|x| x == "foot on Pad"),
            "the limb's own Touched fires too: {lines:?}"
        );
    }

    // Off again: every contact ends, on both sides.
    teleport(&mut game, off);
    run(&mut game, 0.5);
    let lines = output.0.borrow().clone();
    let count = |l: &str| lines.iter().filter(|x| *x == l).count();
    assert!(!lines.iter().any(|l| l.starts_with("error")), "{lines:?}");
    for (on, off) in [
        ("pad on LeftFoot", "pad off LeftFoot"),
        ("pad on HumanoidRootPart", "pad off HumanoidRootPart"),
    ] {
        assert_eq!(count(off), count(on), "{on}: {lines:?}");
    }
    // One more end than start for the foot: the first contact began before it connected.
    assert_eq!(count("foot off Pad"), count("foot on Pad") + 1, "{lines:?}");
}

const PROBE_SCRIPT: &str = r#"
local probe = workspace.Probe
probe.Touched:Connect(function(hit) print("probe on " .. hit.Name) end)
probe.TouchEnded:Connect(function() print("probe off") end)
"#;

/// Only the left foot reaches this spot just ahead of its toe (studs from the root, facing -Z).
const AHEAD_OF_LEFT_TOE: Vec3 = Vec3::new(-0.7, -2.82, -0.88);

fn probe_world() -> World {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    for (name, pos, size, solid) in [
        (
            "Floor",
            Vec3::new(0.0, -2.0, 0.0),
            Vec3::new(256.0, 4.0, 256.0),
            true,
        ),
        // Parked away until the character stands still.
        (
            "Probe",
            Vec3::new(0.0, 0.2, 60.0),
            Vec3::new(0.3, 0.3, 0.3),
            false,
        ),
    ] {
        let id = world.create("Part", Some(ws)).unwrap();
        world.set(id, "Name", Value::String(name.into())).unwrap();
        world.set(id, "Position", Value::Vector3(pos)).unwrap();
        world.set(id, "Size", Value::Vector3(size)).unwrap();
        world.set(id, "Anchored", Value::Bool(true)).unwrap();
        world.set(id, "CanCollide", Value::Bool(solid)).unwrap();
    }
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    world
        .set(script, "Source", Value::String(PROBE_SCRIPT.into()))
        .unwrap();
    world
}

#[test]
fn a_destroyed_or_replaced_character_ends_its_touch_once_in_scripts() {
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("probe".into(), probe_world(), output.clone());
    game.start().unwrap();
    run(&mut game, 0.5);
    let root = game.root_position().unwrap();
    {
        let shared = game.world();
        let mut w = shared.borrow_mut();
        let ws = w.root("Workspace").unwrap();
        let probe = w.find_first_child(ws, "Probe").unwrap();
        w.set(probe, "Position", Value::Vector3(root + AHEAD_OF_LEFT_TOE))
            .unwrap();
    }
    run(&mut game, 0.5);
    let lines = || output.0.borrow().clone();
    let count = |l: &str| output.0.borrow().iter().filter(|x| *x == l).count();
    assert_eq!(lines(), ["probe on LeftFoot"]);

    // Destroyed outright (by a script, say): exactly one TouchEnded on the probe.
    let first = game.character().unwrap();
    game.world().borrow_mut().destroy(first);
    run(&mut game, 0.3);
    assert_eq!(count("probe off"), 1, "{:?}", lines());

    // The game respawns the player in the same spot, and the new foot touches again.
    run(&mut game, metallic_physics::RESPAWN_DELAY + 1.0);
    assert_eq!(count("probe on LeftFoot"), 2, "{:?}", lines());
    assert_eq!(count("probe off"), 1, "{:?}", lines());

    // Replaced without dying (a fresh character for the same player): one more TouchEnded.
    let player = game.player().unwrap();
    metallic_physics::spawn_character(&mut game.world().borrow_mut(), player).unwrap();
    run(&mut game, 0.3);
    assert_eq!(count("probe off"), 2, "{:?}", lines());
}

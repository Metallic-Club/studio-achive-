//! Character touches through the CPU-only Simulation: the body volumes report `Touched` and
//! `TouchEnded` with the touching body part, and both sides fire.
use metallic_core::{Value, Vec3, World};
use metallic_physics::spawn_character;
use metallic_runtime::{Simulation, TICK};
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
    let players = world.root("Players").unwrap();
    let player = world.create("Player", Some(players)).unwrap();
    world
        .set(player, "Name", Value::String("Player1".into()))
        .unwrap();
    spawn_character(&mut world, player).unwrap();
    world
}

fn run(game: &mut Simulation, seconds: f64) {
    for _ in 0..(seconds / TICK).round() as usize {
        assert!(game.tick_once(&[]));
    }
}

/// Moves the character's root to `at` (the body parts follow on the next step).
fn teleport(game: &mut Simulation, at: Vec3) {
    let shared = game.world();
    let mut w = shared.borrow_mut();
    let players = w.root("Players").unwrap();
    let player = w.find_first_child(players, "Player1").unwrap();
    let character = match w.get(player, "Character").unwrap() {
        Value::Ref(Some(id)) => id,
        _ => panic!("no character"),
    };
    let root = w.find_first_child(character, "HumanoidRootPart").unwrap();
    w.set(root, "Position", Value::Vector3(at)).unwrap();
}

#[test]
fn cpu_simulation_reports_body_part_touches_on_both_sides() {
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Simulation::from_world("touches".into(), world(), output.clone());
    game.start().unwrap();
    assert_eq!(
        game.world()
            .borrow()
            .children(game.world().borrow().root("Players").unwrap())
            .len(),
        1
    );
    run(&mut game, 1.0);
    {
        let lines = output.0.borrow();
        let has = |l: &str| lines.iter().any(|x| x == l);
        assert!(has("ready"), "{lines:?}");
        assert!(has("pad on LeftFoot"), "{lines:?}");
        assert!(has("pad on HumanoidRootPart"), "{lines:?}");
    }

    // The server-owned character exists before startup, so both sides connect before landing.
    // Step off and back on to verify fresh touches on the limb's own signal too.
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
    // All foot contacts began after its handlers connected and have now ended.
    assert!(count("foot on Pad") >= 2, "{lines:?}");
    assert_eq!(count("foot off Pad"), count("foot on Pad"), "{lines:?}");
}

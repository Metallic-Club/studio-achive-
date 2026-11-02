//! External, unexecuted diagnostic for PR419 d03379b: no production/source patch.
use metallic_core::{InstanceId, Value, World};
use metallic_physics::{RESPAWN_DELAY, spawn_character};
use metallic_runtime::{Simulation, TICK};
use metallic_script::OutputSink;
use std::{cell::RefCell, rc::Rc};

#[derive(Default)]
struct Capture(RefCell<Vec<String>>);
impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.into());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warning:{line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error:{line}"));
    }
}

struct Fixture {
    sim: Simulation,
    output: Rc<Capture>,
    player: InstanceId,
    original: Option<InstanceId>,
}

fn setup(preexisting_character: bool, startup_mutation: &str) -> Fixture {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    // No floor/contact pairs: keep independent persistent-island issue405 out of this probe.
    // A falling live control must not die at the default -500 before the timer check ends.
    world
        .set(ws, "FallenPartsDestroyHeight", Value::Number(-1_000_000.0))
        .unwrap();
    let players = world.root("Players").unwrap();
    let player = world.create("Player", Some(players)).unwrap();
    world
        .set(player, "Name", Value::String("Existing".into()))
        .unwrap();
    let original = preexisting_character.then(|| spawn_character(&mut world, player).unwrap());
    let service = world.root("ServerScriptService").unwrap();
    let script = world.create("Script", Some(service)).unwrap();
    let source = format!(
        r#"
local player = game:GetService("Players"):FindFirstChild("Existing")
assert(player ~= nil)
player.CharacterAdded:Connect(function() print("replacement-added") end)
{startup_mutation}
print("startup-complete")
"#
    );
    world.set(script, "Source", Value::String(source)).unwrap();
    let output = Rc::new(Capture::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut sim = Simulation::from_world("PR419 startup diagnostic".into(), world, output.clone());
    sim.start().unwrap();
    // Prove eager startup, rather than assuming the mutation runs before first tick.
    assert_eq!(sim.tick_count(), 0);
    assert_eq!(sim.time(), 0.0);
    assert!(
        output
            .0
            .borrow()
            .iter()
            .any(|line| line == "startup-complete"),
        "startup did not execute eagerly or script failed: {:?}",
        output.0.borrow()
    );
    let fixture = Fixture {
        sim,
        output,
        player,
        original,
    };
    assert_no_script_errors(&fixture);
    fixture
}

fn current_character(f: &Fixture) -> Option<InstanceId> {
    let shared = f.sim.world();
    let value = {
        let world = shared.borrow();
        world.get(f.player, "Character").unwrap()
    };
    match value {
        Value::Ref(value) => value,
        value => panic!("unexpected Character value {value:?}"),
    }
}

fn assert_no_script_errors(f: &Fixture) {
    assert!(
        f.output
            .0
            .borrow()
            .iter()
            .all(|line| !line.starts_with("error:")),
        "script errors invalidate diagnostic: {:?}",
        f.output.0.borrow()
    );
}

fn advance_past_respawn(f: &mut Fixture) {
    // Bounded simulated time, no wall-clock sleep; margin avoids threshold-rounding ambiguity.
    let ticks = (RESPAWN_DELAY / TICK).ceil() as usize + 5;
    for _ in 0..ticks {
        assert!(f.sim.tick_once(&[]));
    }
    assert_no_script_errors(f);
}

fn assert_one_replacement(f: &Fixture) {
    let character = current_character(f).expect("previously spawned Player should respawn");
    assert_ne!(Some(character), f.original);
    let shared = f.sim.world();
    let world = shared.borrow();
    assert!(world.is_alive(character) && world.is_a(character, "Model"));
    assert_eq!(world.parent(character), world.root("Workspace"));
    assert!(
        world
            .find_first_child(character, "HumanoidRootPart")
            .is_some()
    );
    let added = f
        .output
        .0
        .borrow()
        .iter()
        .filter(|line| *line == "replacement-added")
        .count();
    assert_eq!(added, 1, "expected exactly one replacement CharacterAdded");
}

#[test]
fn startup_destroy_of_preexisting_character_should_respawn() {
    let mut f = setup(
        true,
        "assert(player.Character ~= nil) player.Character:Destroy()",
    );
    let shared = f.sim.world();
    assert!(!shared.borrow().is_alive(f.original.unwrap()));
    assert_eq!(
        current_character(&f),
        None,
        "startup destroy must clear canonical reference"
    );
    assert_eq!(
        f.sim.tick_count(),
        0,
        "destruction must precede the first tick"
    );
    advance_past_respawn(&mut f);
    assert_one_replacement(&f);
}

#[test]
fn startup_clear_of_preexisting_character_should_respawn() {
    let mut f = setup(
        true,
        "assert(player.Character ~= nil) player.Character = nil",
    );
    assert_eq!(current_character(&f), None);
    assert!(
        f.sim.world().borrow().is_alive(f.original.unwrap()),
        "this case clears the reference without destroying its Model"
    );
    assert_eq!(f.sim.tick_count(), 0);
    advance_past_respawn(&mut f);
    assert_one_replacement(&f);
}

#[test]
fn after_first_tick_destroy_control_should_respawn() {
    let mut f = setup(true, "assert(player.Character ~= nil)");
    assert_eq!(current_character(&f), f.original);
    assert!(
        f.sim.tick_once(&[]),
        "first tick must observe the live Character"
    );
    f.sim.world().borrow_mut().destroy(f.original.unwrap());
    assert_eq!(current_character(&f), None);
    advance_past_respawn(&mut f);
    assert_one_replacement(&f);
}

#[test]
fn never_spawned_control_must_still_have_no_character() {
    let mut f = setup(false, "assert(player.Character == nil)");
    assert_eq!(current_character(&f), None);
    advance_past_respawn(&mut f);
    assert_eq!(
        current_character(&f),
        None,
        "first spawn remains outside this contract"
    );
    assert_eq!(
        f.output
            .0
            .borrow()
            .iter()
            .filter(|line| *line == "replacement-added")
            .count(),
        0
    );
}

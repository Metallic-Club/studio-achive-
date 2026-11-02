#![cfg(feature = "presentation")]
//! Character-membership transitions through the public runtime frame/touch path.

use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::OutputSink;
use std::cell::RefCell;
use std::rc::Rc;

const READY: &str = "membership-listener-ready";
const TOUCH: &str = "membership-sensor-touch";

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

#[derive(Clone, Copy)]
enum Scenario {
    OrdinaryControl,
    CharacterControl,
    CreateHumanoid,
    RemoveHumanoid,
    ReparentHumanoidIn,
    ReparentHumanoidOut,
}

fn part(world: &mut World, parent: InstanceId, name: &str, x: f64, solid: bool) -> InstanceId {
    let id = world.create("Part", Some(parent)).unwrap();
    for (property, value) in [
        ("Name", Value::String(name.to_owned())),
        ("Position", Value::Vector3(Vec3::new(x, 10.0, 0.0))),
        ("Size", Value::Vector3(Vec3::new(2.0, 2.0, 2.0))),
        ("Anchored", Value::Bool(true)),
        ("CanCollide", Value::Bool(solid)),
    ] {
        world.set(id, property, value).unwrap();
    }
    id
}

fn observed_touches(scenario: Scenario) -> usize {
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let model = world.create("Model", Some(workspace)).unwrap();
    let other_model = world.create("Model", Some(workspace)).unwrap();
    part(&mut world, model, "SensorPart", 30.0, false);
    let target = part(&mut world, workspace, "TouchTarget", 40.0, true);
    let humanoid = match scenario {
        Scenario::CharacterControl | Scenario::RemoveHumanoid | Scenario::ReparentHumanoidOut => {
            Some(world.create("Humanoid", Some(model)).unwrap())
        }
        Scenario::ReparentHumanoidIn => Some(world.create("Humanoid", Some(other_model)).unwrap()),
        Scenario::OrdinaryControl | Scenario::CreateHumanoid => None,
    };
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                r#"
workspace.TouchTarget.Touched:Connect(function(hit)
    if hit.Name == "SensorPart" then
        print("membership-sensor-touch")
    end
end)
print("membership-listener-ready")
"#
                .to_owned(),
            ),
        )
        .unwrap();
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world(
        "runtime-membership-fixture".to_owned(),
        world,
        output.clone(),
    );
    game.start().unwrap();
    for _ in 0..2 {
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    }
    assert_eq!(
        output
            .0
            .borrow()
            .iter()
            .filter(|line| *line == READY)
            .count(),
        1
    );

    // Only Humanoid membership changes here: no BasePart shape changes can mask the gate.
    {
        let shared = game.world();
        let mut world = shared.borrow_mut();
        match scenario {
            Scenario::CreateHumanoid => {
                world.create("Humanoid", Some(model)).unwrap();
            }
            Scenario::RemoveHumanoid => world.destroy(humanoid.unwrap()),
            Scenario::ReparentHumanoidIn => {
                world.set_parent(humanoid.unwrap(), Some(model)).unwrap()
            }
            Scenario::ReparentHumanoidOut => world
                .set_parent(humanoid.unwrap(), Some(other_model))
                .unwrap(),
            Scenario::OrdinaryControl | Scenario::CharacterControl => {}
        }
    }
    // The runtime's resync decision happens at the end of this normal tick.
    assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    // Match the existing public kinematic-body/anchored-sensor contact regression:
    // the sensor stays fixed while a solid target approaches it in short increments.
    // Position is excluded from PartShape, so this does not force a shape resync.
    for step in 1..=100 {
        game.world()
            .borrow_mut()
            .set(
                target,
                "Position",
                Value::Vector3(Vec3::new(40.0 - step as f64 * 0.1, 10.0, 0.0)),
            )
            .unwrap();
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    }
    let lines = output.0.borrow();
    assert!(
        !lines.iter().any(|line| line.starts_with("error:")),
        "script errors invalidate the observation fixture: {lines:?}"
    );
    lines.iter().filter(|line| *line == TOUCH).count()
}

#[test]
fn ordinary_sensor_control_reports_exactly_one_runtime_touch() {
    assert_eq!(observed_touches(Scenario::OrdinaryControl), 1);
}

#[test]
fn existing_character_control_suppresses_physics_touches() {
    assert_eq!(observed_touches(Scenario::CharacterControl), 0);
}

#[test]
fn adding_only_a_humanoid_suppresses_later_character_part_physics_touches() {
    assert_eq!(observed_touches(Scenario::CreateHumanoid), 0);
}

#[test]
fn removing_only_a_humanoid_restores_ordinary_part_physics_touches() {
    assert_eq!(observed_touches(Scenario::RemoveHumanoid), 1);
}

#[test]
fn reparenting_a_humanoid_in_suppresses_character_part_physics_touches() {
    assert_eq!(observed_touches(Scenario::ReparentHumanoidIn), 0);
}

#[test]
fn reparenting_a_humanoid_out_restores_ordinary_part_physics_touches() {
    assert_eq!(observed_touches(Scenario::ReparentHumanoidOut), 1);
}

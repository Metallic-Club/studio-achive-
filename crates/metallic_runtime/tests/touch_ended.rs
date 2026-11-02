#![cfg(feature = "presentation")]
//! `BasePart.TouchEnded` between two ordinary parts, through the public runtime frame path:
//! when two parts touch and separate, each part fires `Touched` once and `TouchEnded` once.

use metallic_core::{InstanceId, Value, Vec3, World};
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

fn part(world: &mut World, name: &str, x: f64, solid: bool) -> InstanceId {
    let workspace = world.root("Workspace").unwrap();
    let id = world.create("Part", Some(workspace)).unwrap();
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

fn lines(output: &Captured) -> Vec<String> {
    output.0.borrow().clone()
}

fn count(output: &Captured, line: &str) -> usize {
    output.0.borrow().iter().filter(|l| *l == line).count()
}

#[test]
fn touch_ended_fires_once_on_each_part_when_they_separate() {
    let mut world = World::new_with_services();
    part(&mut world, "Sensor", 30.0, false);
    let target = part(&mut world, "Target", 40.0, true);
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                r#"
for _, name in {"Sensor", "Target"} do
    local part = workspace[name]
    part.Touched:Connect(function(hit) print("touched " .. name .. " by " .. hit.Name) end)
    part.TouchEnded:Connect(function(hit) print("ended " .. name .. " by " .. hit.Name) end)
end
print("ready")
"#
                .to_owned(),
            ),
        )
        .unwrap();
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("touch-ended-fixture".to_owned(), world, output.clone());
    game.start().unwrap();
    for _ in 0..2 {
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    }
    assert_eq!(count(&output, "ready"), 1, "{:?}", lines(&output));

    let move_target = |game: &mut Game, x: f64| {
        game.world()
            .borrow_mut()
            .set(target, "Position", Value::Vector3(Vec3::new(x, 10.0, 0.0)))
            .unwrap();
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    };
    // The solid target approaches the sensor in short steps until they touch...
    for step in 1..=100 {
        move_target(&mut game, 40.0 - f64::from(step) * 0.1);
    }
    assert_eq!(count(&output, "touched Sensor by Target"), 1);
    assert_eq!(count(&output, "touched Target by Sensor"), 1);
    assert_eq!(
        count(&output, "ended Sensor by Target"),
        0,
        "still touching"
    );
    assert_eq!(
        count(&output, "ended Target by Sensor"),
        0,
        "still touching"
    );
    // ...and leaves again.
    for step in 1..=100 {
        move_target(&mut game, 30.0 + f64::from(step) * 0.2);
    }
    for (name, hit) in [("Sensor", "Target"), ("Target", "Sensor")] {
        assert_eq!(count(&output, &format!("touched {name} by {hit}")), 1);
        assert_eq!(
            count(&output, &format!("ended {name} by {hit}")),
            1,
            "{:?}",
            lines(&output)
        );
    }
    assert!(
        !lines(&output).iter().any(|l| l.starts_with("error")),
        "{:?}",
        lines(&output)
    );
}

#[test]
fn destroying_a_touching_part_fires_touch_ended_on_the_other() {
    let mut world = World::new_with_services();
    part(&mut world, "Sensor", 0.0, false);
    let target = part(&mut world, "Target", 10.0, true);
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                r#"
local sensor = workspace.Sensor
sensor.Touched:Connect(function(hit) print("touched by " .. hit.Name) end)
sensor.TouchEnded:Connect(function(hit) print("ended by " .. typeof(hit)) end)
"#
                .to_owned(),
            ),
        )
        .unwrap();
    let output = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("touch-ended-destroy".to_owned(), world, output.clone());
    game.start().unwrap();
    // Two resting anchored parts never pair; the anchored target moves in until it touches.
    for step in 1..=90 {
        game.world()
            .borrow_mut()
            .set(
                target,
                "Position",
                Value::Vector3(Vec3::new(10.0 - f64::from(step) * 0.1, 10.0, 0.0)),
            )
            .unwrap();
        game.frame(&FrameInput::default(), TICK);
    }
    assert_eq!(
        count(&output, "touched by Target"),
        1,
        "{:?}",
        lines(&output)
    );
    game.world().borrow_mut().destroy(target);
    for _ in 0..3 {
        game.frame(&FrameInput::default(), TICK);
    }
    assert_eq!(
        count(&output, "ended by Instance"),
        1,
        "{:?}",
        lines(&output)
    );
    assert_eq!(
        count(&output, "touched by Target"),
        1,
        "{:?}",
        lines(&output)
    );
}

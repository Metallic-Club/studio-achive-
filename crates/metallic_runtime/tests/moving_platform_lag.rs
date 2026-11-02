#![cfg(feature = "presentation")]
//! #374 / #273: a platform a `Heartbeat` script moves every tick carries its rider at most one
//! tick behind where the script put it (it used to be two), in the M2 tick order.

use metallic_core::{Value, Vec3, World};
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::OutputSink;
use std::rc::Rc;

struct Quiet;

impl OutputSink for Quiet {
    fn print(&self, _: &str) {}
    fn warn(&self, line: &str) {
        panic!("script warning: {line}");
    }
    fn error(&self, line: &str) {
        panic!("script error: {line}");
    }
}

const SPEED: f64 = 4.0;

#[test]
fn heartbeat_mover_carries_its_rider_within_one_tick() {
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let platform = world.create("Part", Some(workspace)).unwrap();
    for (name, value) in [
        ("Name", Value::String("Mover".into())),
        ("Anchored", Value::Bool(true)),
        ("Position", Value::Vector3(Vec3::new(0.0, -1.0, 0.0))),
        ("Size", Value::Vector3(Vec3::new(40.0, 2.0, 40.0))),
    ] {
        world.set(platform, name, value).unwrap();
    }
    let script = world
        .create("Script", world.root("ServerScriptService"))
        .unwrap();
    // Waits one second so the character has landed, then slides at SPEED studs per second.
    let source = format!(
        r#"
local mover = workspace.Mover
local t = 0
local go = false
task.delay(1, function() go = true end)
game:GetService("RunService").Heartbeat:Connect(function(dt)
    if go then t += dt end
    mover.Position = Vector3.new({SPEED} * t, -1, 0)
end)
"#
    );
    world.set(script, "Source", Value::String(source)).unwrap();
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("moving-platform-lag".into(), world, Rc::new(Quiet));
    game.start().unwrap();
    let mut offsets = Vec::new();
    for _ in 0..240 {
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
        let root = game.root_position().unwrap();
        let Value::Vector3(drawn) = game.world().borrow().get(platform, "Position").unwrap() else {
            panic!("platform position");
        };
        offsets.push(root.x - drawn.x);
    }
    // The rider started at x = 0 on the platform's centre; the last second is steady motion.
    let one_tick = SPEED * TICK;
    for (tick, offset) in offsets.iter().enumerate().skip(180) {
        assert!(
            offset.abs() <= one_tick + 1e-3,
            "tick {tick}: rider {:.4} stud from its spot, one tick is {one_tick:.4}",
            -offset
        );
    }
    let moved = game.root_position().unwrap().x;
    assert!(moved > 3.0, "the rider was carried ({moved} studs)");
}

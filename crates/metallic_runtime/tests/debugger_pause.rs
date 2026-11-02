#![cfg(feature = "presentation")]
//! A game paused in the script debugger holds its simulation still.

use std::cell::RefCell;
use std::rc::Rc;

use metallic_core::{Value, Vec3, World};
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::{Breakpoint, OutputSink};

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

#[test]
fn paused_game_does_not_advance_physics() {
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let crate_part = world.create("Part", Some(workspace)).unwrap();
    for (property, value) in [
        ("Name", Value::String("Crate".into())),
        ("Position", Value::Vector3(Vec3::new(0.0, 80.0, 0.0))),
        ("Size", Value::Vector3(Vec3::new(2.0, 2.0, 2.0))),
        ("Anchored", Value::Bool(false)),
    ] {
        world.set(crate_part, property, value).unwrap();
    }
    let sss = world.root("ServerScriptService").unwrap();
    let script = world.create("Script", Some(sss)).unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                "game:GetService('RunService').Heartbeat:Connect(function()\n    print('beat')\nend)\n"
                    .into(),
            ),
        )
        .unwrap();
    let out = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("Pause".into(), world, out.clone());
    game.start().unwrap();
    let height = |game: &Game| match game.world().borrow().get(crate_part, "Position") {
        Ok(Value::Vector3(p)) => p.y,
        other => panic!("no position: {other:?}"),
    };
    for _ in 0..10 {
        game.frame(&FrameInput::default(), TICK);
    }
    let falling = height(&game);
    assert!(falling < 80.0, "the crate falls while running");

    game.debugger().set_breakpoint(script, 2, Breakpoint::Plain);
    game.frame(&FrameInput::default(), TICK);
    assert!(game.is_paused());
    let (y, time, beats) = (height(&game), game.time(), out.0.borrow().len());
    for _ in 0..30 {
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 0);
    }
    assert_eq!(height(&game), y, "physics holds still while paused");
    assert_eq!(game.time(), time);
    assert_eq!(out.0.borrow().len(), beats, "no script ran while paused");

    game.debugger().clear_breakpoint(script, 2);
    game.debugger().resume();
    assert!(!game.is_paused());
    for _ in 0..10 {
        assert_eq!(game.frame(&FrameInput::default(), TICK).ticks, 1);
    }
    assert!(height(&game) < y, "the crate falls again after Continue");
}

#[test]
fn a_breakpoint_mid_frame_stops_the_remaining_ticks() {
    let mut world = World::new_with_services();
    let sss = world.root("ServerScriptService").unwrap();
    let script = world.create("Script", Some(sss)).unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                "local n = 0\ngame:GetService('RunService').Heartbeat:Connect(function()\n    n = n + 1\nend)\n"
                    .into(),
            ),
        )
        .unwrap();
    let out = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("Pause".into(), world, out);
    game.start().unwrap();
    game.frame(&FrameInput::default(), TICK);
    game.debugger().set_breakpoint(script, 3, Breakpoint::Plain);
    // A long frame owes several ticks; the first one hits the breakpoint and the rest wait.
    let time = game.time();
    let out = game.frame(&FrameInput::default(), TICK * 6.0);
    assert!(game.is_paused());
    assert_eq!(out.ticks, 1, "no tick runs after the one that paused");
    assert!((game.time() - time - TICK).abs() < 1e-9);
    game.debugger().clear_breakpoint(script, 3);
    game.debugger().resume();
    assert_eq!(
        game.frame(&FrameInput::default(), TICK).ticks,
        1,
        "the paused frame's leftover time is not replayed as a burst"
    );
}

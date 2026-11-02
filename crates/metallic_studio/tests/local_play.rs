//! Nonvisual Studio play uses the real Game and keeps the authored world independent.

use metallic_core::{InstanceId, Value, Vec3, World, save_scene};
use metallic_runtime::{FrameInput, TICK};
use metallic_studio::commands::Command;
use metallic_studio::editor::EditorState;
use metallic_studio::output::{Level, OutputLog, Tag};
use metallic_studio::play::{LocalPlaySession, PLAY_TAG, PlayController, PlaySession, PlayStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

fn scene(source: &str) -> (World, InstanceId) {
    metallic_script::use_test_budgets_on_this_thread();
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let part = world.create("Part", Some(workspace)).unwrap();
    world
        .set(part, "Name", Value::String("Probe".into()))
        .unwrap();
    world.set(part, "Anchored", Value::Bool(true)).unwrap();
    let scripts = world.root("ServerScriptService").unwrap();
    add_script(&mut world, scripts, "ProbeScript", "Script", source);
    (world, part)
}

fn add_script(world: &mut World, parent: InstanceId, name: &str, class: &str, source: &str) {
    let script = world.create(class, Some(parent)).unwrap();
    world
        .set(script, "Name", Value::String(name.into()))
        .unwrap();
    world
        .set(script, "Source", Value::String(source.into()))
        .unwrap();
}

fn count(log: &mut OutputLog, text: &str) -> u32 {
    log.drain();
    log.lines()
        .filter(|line| line.text == text)
        .map(|line| line.repeats)
        .sum()
}

fn frames(session: &mut LocalPlaySession, count: usize) -> u32 {
    (0..count)
        .map(|_| session.frame(&FrameInput::default(), TICK).unwrap().ticks)
        .sum()
}

#[test]
fn play_does_not_change_edit_data_history_cursor_or_signals() {
    let dir = tempfile::tempdir().unwrap();
    let mut log = OutputLog::new(64);
    let (world, part) = scene("workspace.Probe.Position = Vector3.new(21, 22, 23)");
    let mut editor = EditorState::new(
        world,
        dir.path(),
        &dir.path().join("main.mscene"),
        "Authored",
        log.sink(),
    );
    editor
        .apply(Command::set(
            part,
            "Transparency",
            Value::Number(0.0),
            Value::Number(0.25),
        ))
        .unwrap();
    editor.selection.set([part]);
    let fired = Arc::new(AtomicUsize::new(0));
    let observed = fired.clone();
    editor
        .world
        .property_changed(part, "Position")
        .connect(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
        });
    let cursor = editor.world.change_cursor();
    let history = (
        editor.history.len(),
        editor.history.revision(),
        editor.history.is_dirty(),
    );
    let selection = editor.selection.ids().to_vec();
    let before = dir.path().join("before.mscene");
    let after = dir.path().join("after.mscene");
    save_scene(&editor.world, &before).unwrap();

    let mut session = LocalPlaySession::start("Authored", &editor.world, log.sink()).unwrap();
    assert_eq!(
        session.inspect_world(|world| world.get(part, "Position").unwrap()),
        Some(Value::Vector3(Vec3::new(21.0, 22.0, 23.0)))
    );
    frames(&mut session, 2);
    session.stop();
    save_scene(&editor.world, &after).unwrap();
    assert_eq!(
        std::fs::read(before).unwrap(),
        std::fs::read(after).unwrap()
    );
    assert_eq!(editor.world.change_cursor(), cursor);
    assert_eq!(
        (
            editor.history.len(),
            editor.history.revision(),
            editor.history.is_dirty()
        ),
        history
    );
    assert_eq!(editor.selection.ids(), selection);
    assert_eq!(fired.load(Ordering::SeqCst), 0);
    editor
        .world
        .set(part, "Position", Value::Vector3(Vec3::new(1.0, 2.0, 3.0)))
        .unwrap();
    assert_eq!(
        fired.load(Ordering::SeqCst),
        1,
        "the original subscription remains connected"
    );
    editor.undo();
    assert_eq!(
        editor.world.get(part, "Transparency").unwrap(),
        Value::Number(0.0)
    );
    log.drain();
    assert!(!log.lines().any(|line| line.level == Level::Error));
}

#[test]
fn clean_snapshot_excludes_parked_history_instances() {
    let dir = tempfile::tempdir().unwrap();
    let log = OutputLog::new(64);
    let (world, part) = scene("");
    let mut editor = EditorState::new(
        world,
        dir.path(),
        &dir.path().join("main.mscene"),
        "Authored",
        log.sink(),
    );
    editor.apply(Command::delete(&[part])).unwrap();
    assert!(editor.world.is_alive(part));
    let mut session =
        LocalPlaySession::start("Authored", &editor.clean_world(), log.sink()).unwrap();
    assert_eq!(
        session.inspect_world(|world| world.is_alive(part)),
        Some(false)
    );
    session.stop();
    editor.undo();
    assert!(editor.world.is_alive(part));
    assert!(editor.is_live(part));
}

const PENDING: &str = "\
print('scheduled')\n\
task.delay(0.5, function() print('old delay') end)\n\
task.spawn(function() task.wait(0.5); print('old wait') end)\n\
game:GetService('RunService').Heartbeat:Connect(function() print('old beat') end)";

fn stop_or_drop_pending(drop_session: bool) {
    let mut log = OutputLog::new(512);
    let (mut world, _) = scene(PENDING);
    let mut old = LocalPlaySession::start("Old", &world, log.sink()).unwrap();
    let ticks = frames(&mut old, 2);
    assert_eq!(count(&mut log, "scheduled"), 1);
    assert_eq!(count(&mut log, "old beat"), ticks);
    assert_eq!(count(&mut log, "old delay"), 0);
    assert_eq!(count(&mut log, "old wait"), 0);
    if drop_session {
        drop(old);
    } else {
        old.stop();
        old.stop();
        assert!(!old.is_running());
        assert!(old.inspect_world(|_| ()).is_none());
        assert!(old.frame(&FrameInput::default(), 1.0).is_none());
        assert!(matches!(old.status(), PlayStatus::Ended(_)));
    }
    let scripts = world.root("ServerScriptService").unwrap();
    let script = world.find_first_child(scripts, "ProbeScript").unwrap();
    world
        .set(
            script,
            "Source",
            Value::String(
                "game:GetService('RunService').Heartbeat:Connect(function() print('new beat') end)"
                    .into(),
            ),
        )
        .unwrap();
    let mut fresh = LocalPlaySession::start("Fresh", &world, log.sink()).unwrap();
    let new_ticks = frames(&mut fresh, 65);
    assert_eq!(count(&mut log, "new beat"), new_ticks);
    assert_eq!(count(&mut log, "old beat"), ticks);
    assert_eq!(count(&mut log, "old delay"), 0);
    assert_eq!(count(&mut log, "old wait"), 0);
}

#[test]
fn stop_with_pending_delay_and_wait_fires_nothing_after_stop() {
    stop_or_drop_pending(false);
}

#[test]
fn drop_with_pending_delay_and_wait_fires_nothing_in_new_session() {
    stop_or_drop_pending(true);
}

#[test]
fn play_twice_connects_handlers_once_and_creates_one_player() {
    let mut log = OutputLog::new(64);
    let (world, _) =
        scene("game:GetService('RunService').Heartbeat:Connect(function() print('beat') end)");
    let mut expected = 0;
    for _ in 0..2 {
        let mut session = LocalPlaySession::start("Fresh", &world, log.sink()).unwrap();
        assert_eq!(
            session.inspect_world(|world| world
                .children(world.root("Players").unwrap())
                .iter()
                .filter(|&&id| world.is_a(id, "Player"))
                .count()),
            Some(1)
        );
        expected += session
            .frame(&FrameInput::default(), TICK / 2.0)
            .unwrap()
            .ticks;
        expected += frames(&mut session, 4);
        assert_eq!(count(&mut log, "beat"), expected);
        session.stop();
    }
    assert!(world.children(world.root("Players").unwrap()).is_empty());
    assert!(
        log.lines()
            .filter(|line| line.text == "beat")
            .any(|line| line.repeats > 1)
    );
}

#[test]
fn explicit_frame_forwards_evaluated_input_to_the_real_game() {
    fn root_position(world: &World) -> Vec3 {
        let player = world
            .find_first_child(world.root("Players").unwrap(), "Player1")
            .unwrap();
        let Value::Ref(Some(character)) = world.get(player, "Character").unwrap() else {
            panic!("Game creates a local Character");
        };
        let root = world
            .find_first_child(character, "HumanoidRootPart")
            .unwrap();
        let Value::Vector3(position) = world.get(root, "Position").unwrap() else {
            panic!("character root has a Position");
        };
        position
    }

    let log = OutputLog::new(32);
    let (world, _) = scene("");
    let mut session = LocalPlaySession::start("Input", &world, log.sink()).unwrap();
    let before = session.inspect_world(root_position).unwrap();
    let output = session
        .frame(
            &FrameInput {
                movement: [1.0, 0.0],
                ..FrameInput::default()
            },
            TICK * 4.0,
        )
        .unwrap();
    assert_eq!(output.ticks, 4);
    let after = session.inspect_world(root_position).unwrap();
    assert!((after.x - before.x).abs() + (after.z - before.z).abs() > 0.001);
}

#[test]
fn combined_output_preserves_server_client_and_error_text_without_false_attribution() {
    let mut log = OutputLog::new(64);
    let (mut world, _) = scene("print('server line'); warn('warning line'); error('boom')");
    let starter = world
        .find_first_child(world.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
        .unwrap();
    add_script(
        &mut world,
        starter,
        "ClientProbe",
        "LocalScript",
        "print('client line')",
    );
    let mut session = LocalPlaySession::start("Output", &world, log.sink()).unwrap();
    frames(&mut session, 1);
    log.drain();
    let lines: Vec<_> = log.lines().collect();
    for text in ["server line", "client line", "warning line"] {
        assert!(lines.iter().any(|line| line.text == text));
    }
    assert!(lines.iter().any(|line| {
        line.level == Level::Error
            && line
                .text
                .contains("ServerScriptService.ProbeScript:1: boom")
    }));
    assert!(
        lines
            .iter()
            .all(|line| line.tag == Tag::Other(Arc::from(PLAY_TAG)))
    );
}

#[test]
fn failed_start_and_trait_adapter_keep_lifecycle_explicit() {
    let mut log = OutputLog::new(64);
    let (mut broken, _) = scene(
        "print('before startup failure'); task.delay(0.5, function() print('failed late') end)",
    );
    broken.destroy(broken.root("Workspace").unwrap());
    let result = LocalPlaySession::start("Broken", &broken, log.sink());
    assert!(result.is_err());
    assert_eq!(
        count(&mut log, "before startup failure"),
        1,
        "failure occurs after script initialization"
    );
    let (world, _) = scene(
        "game:GetService('RunService').Heartbeat:Connect(function() print('trait beat') end)",
    );
    let mut session: Box<dyn PlaySession> =
        Box::new(LocalPlaySession::start("Adapter", &world, log.sink()).unwrap());
    assert!(matches!(session.status(), PlayStatus::Running(_)));
    session.update(TICK);
    assert_eq!(count(&mut log, "trait beat"), 1);
    session.stop().unwrap();
    assert_eq!(count(&mut log, "failed late"), 0);
    assert_eq!(
        PlayController::new().modes().count(),
        0,
        "the foundation registers no mode"
    );
}

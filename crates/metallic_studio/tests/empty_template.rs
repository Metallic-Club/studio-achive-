//! The Empty template: a Workspace with one SpawnLocation, default lighting and nothing else.

use std::path::PathBuf;
use std::rc::Rc;

use metallic_core::{InstanceId, Value, World, load_scene};
use metallic_runtime::{FrameInput, Game, StdoutSink, TICK};
use metallic_studio::templates::{TemplateKind, create_from_template, list_templates};

fn repo_templates() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

/// Every instance in `ids` and below as `depth class`, depth first.
fn tree(world: &World, ids: &[InstanceId], depth: usize, out: &mut Vec<String>) {
    for &id in ids {
        let class = world.class_name(id).expect("class");
        out.push(format!("{depth} {class}"));
        tree(world, world.children(id), depth + 1, out);
    }
}

#[test]
fn empty_is_listed_as_an_empty_world() {
    let empty = list_templates(&repo_templates())
        .into_iter()
        .find(|t| t.id == "empty")
        .expect("templates/empty is listed");
    assert_eq!(empty.name, "Empty");
    assert_eq!(empty.kind, TemplateKind::Empty);
    assert!(empty.preview_path.is_file(), "it has a preview image");
}

#[test]
fn empty_holds_one_spawn_and_the_services_only() {
    let world = load_scene(&repo_templates().join("empty").join("main.mscene")).expect("load");
    let mut found = Vec::new();
    tree(&world, world.roots(), 0, &mut found);
    assert_eq!(
        found,
        [
            "0 Workspace",
            "1 SpawnLocation",
            "0 Players",
            "0 ReplicatedStorage",
            "0 ServerScriptService",
            "0 StarterPlayer",
            "1 StarterPlayerScripts",
            "0 StarterGui",
            "0 Lighting",
            "0 RunService",
            // Every world has it; the scene file does not list it.
            "0 ServerStorage",
        ]
    );
    let spawn = world.children(world.root("Workspace").expect("Workspace"))[0];
    assert!(
        world.roots().iter().all(|&r| world.is_builtin(r)) && !world.is_builtin(spawn),
        "only the spawn is the template's own"
    );
}

#[test]
fn a_game_created_from_empty_plays_on_the_spawn() {
    let out = tempfile::tempdir().expect("tmp");
    let target = out.path().join("My Empty");
    let info = list_templates(&repo_templates())
        .into_iter()
        .find(|t| t.id == "empty")
        .expect("templates/empty");
    create_from_template(&info, &target, "My Empty").expect("create");
    assert!(
        !target.join("preview.png").exists(),
        "the preview stays out"
    );

    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::open(&target, None, Rc::new(StdoutSink)).expect("open");
    game.start().expect("start");
    for _ in 0..(2.0 / TICK).round() as usize {
        game.frame(&FrameInput::default(), TICK);
    }
    let world = game.world();
    let w = world.borrow();
    let character = game.character().expect("character spawned");
    let root = w
        .find_first_child(character, "HumanoidRootPart")
        .expect("root part");
    let y = match w.get(root, "Position") {
        Ok(Value::Vector3(v)) => v.y,
        other => panic!("Position: {other:?}"),
    };
    assert!(
        (1.0..10.0).contains(&y),
        "character stands on the spawn, y = {y}"
    );
}

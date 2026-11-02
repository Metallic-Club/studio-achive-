//! The built-in Baseplate template: the repo folder, its compiled-in copy, and that both play.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use metallic_core::Value;
use metallic_runtime::{FrameInput, Game, StdoutSink, TICK};
use metallic_studio::templates::{
    BASEPLATE_ID, PROJECT_FILE, TEMPLATE_FILE, builtin_baseplate, create_from_template,
    list_templates, with_builtin,
};

fn repo_templates() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

/// Opens the project with the Player's code path, plays two idle seconds and returns the
/// character root's height.
fn play_two_seconds(project: &Path) -> f64 {
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::open(project, None, Rc::new(StdoutSink)).expect("open the project");
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
    match w.get(root, "Position") {
        Ok(Value::Vector3(v)) => v.y,
        other => panic!("Position: {other:?}"),
    }
}

#[test]
fn embedded_baseplate_is_byte_identical_to_the_repo_folder() {
    let dir = repo_templates().join("baseplate");
    let info = builtin_baseplate().expect("built-in Baseplate parses");
    let files = info.embedded.expect("compiled-in files");
    let mut embedded: Vec<&str> = files.iter().map(|f| f.path).collect();
    embedded.sort_unstable();
    let mut on_disk: Vec<String> = fs::read_dir(&dir)
        .expect("templates/baseplate")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .filter(|name| name != "preview.png")
        .collect();
    on_disk.sort_unstable();
    assert_eq!(embedded, on_disk);
    for file in files {
        let bytes = fs::read(dir.join(file.path)).expect("read");
        assert!(
            bytes == file.bytes,
            "{} differs from the repo copy",
            file.path
        );
    }
}

#[test]
fn disk_templates_list_baseplate_then_empty_and_parkour() {
    let found = list_templates(&repo_templates());
    let ids: Vec<&str> = found.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(&ids[..3], [BASEPLATE_ID, "empty", "parkour"]);
    assert!(found[0].embedded.is_none(), "the folder copy wins");

    let merged = with_builtin(found.clone());
    assert_eq!(merged, found, "no second Baseplate when the folder has one");

    let builtin = builtin_baseplate().expect("built-in");
    assert_eq!(
        (&builtin.name, &builtin.description, builtin.order),
        (&found[0].name, &found[0].description, found[0].order)
    );
}

#[test]
fn without_a_templates_folder_baseplate_is_still_offered() {
    let missing = tempfile::tempdir().expect("tmp");
    let found = with_builtin(list_templates(&missing.path().join("templates")));
    let ids: Vec<&str> = found.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids, [BASEPLATE_ID]);
    assert_eq!(found[0].name, "Baseplate");
}

#[test]
fn baseplate_template_loads_and_plays() {
    let y = play_two_seconds(&repo_templates().join("baseplate"));
    assert!(
        (1.0..10.0).contains(&y),
        "character stands on the spawn, y = {y}"
    );
}

#[test]
fn a_game_created_from_the_embedded_baseplate_plays() {
    let out = tempfile::tempdir().expect("tmp");
    let target = out.path().join("My Game");
    let info = builtin_baseplate().expect("built-in");
    create_from_template(&info, &target, "My Game").expect("create");
    assert!(target.join("main.mscene").is_file());
    assert!(
        !target.join(TEMPLATE_FILE).exists(),
        "catalog file stays out"
    );
    let project = fs::read_to_string(target.join(PROJECT_FILE)).expect("metallic.toml");
    let table: toml::Table = project.parse().expect("toml");
    assert_eq!(table["name"].as_str(), Some("My Game"));
    assert_eq!(table["main_scene"].as_str(), Some("main.mscene"));
    let y = play_two_seconds(&target);
    assert!(
        (1.0..10.0).contains(&y),
        "character stands on the spawn, y = {y}"
    );
}

#[test]
fn embedded_create_refuses_a_busy_folder_and_writes_nothing() {
    let out = tempfile::tempdir().expect("tmp");
    fs::write(out.path().join("keep.txt"), "x").expect("write");
    let info = builtin_baseplate().expect("built-in");
    assert!(create_from_template(&info, out.path(), "Game").is_err());
    let names: Vec<_> = fs::read_dir(out.path()).expect("dir").collect();
    assert_eq!(names.len(), 1);
}

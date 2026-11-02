#![cfg(feature = "presentation")]
//! The Parkour template and the game loop that plays it.
use metallic_core::{Color3, InstanceId, Value, Vec3, World, load_scene};
use metallic_physics::HumanoidState;
use metallic_runtime::{FrameInput, Game, TICK};
use metallic_script::OutputSink;
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

fn template() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/parkour")
}

/// Collects script output so tests can assert on it.
#[derive(Default)]
struct Captured(RefCell<Vec<String>>);

impl OutputSink for Captured {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.to_string());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warning: {line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error: {line}"));
    }
}

fn descendants(w: &World, id: InstanceId, out: &mut Vec<InstanceId>) {
    for &c in w.children(id) {
        out.push(c);
        descendants(w, c, out);
    }
}

fn all(w: &World) -> Vec<InstanceId> {
    let mut out = Vec::new();
    for &r in w.roots() {
        out.push(r);
        descendants(w, r, &mut out);
    }
    out
}

fn vec3(w: &World, id: InstanceId, prop: &str) -> Vec3 {
    match w.get(id, prop) {
        Ok(Value::Vector3(v)) => v,
        other => panic!("{prop}: {other:?}"),
    }
}

fn start(output: Rc<Captured>) -> Game {
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::open(&template(), None, output).expect("load Parkour");
    game.start().expect("start");
    game
}

fn root_of(game: &Game) -> InstanceId {
    let world = game.world();
    let w = world.borrow();
    let character = game.character().expect("character");
    w.find_first_child(character, "HumanoidRootPart")
        .expect("root part")
}

fn run(game: &mut Game, input: FrameInput, seconds: f64) {
    for _ in 0..(seconds / TICK).round() as usize {
        game.frame(&input, TICK);
    }
}

#[test]
fn parkour_template_has_the_required_content() {
    let w = load_scene(&template().join("main.mscene")).expect("main scene");
    let ids = all(&w);
    let parts = ids.iter().filter(|&&i| w.is_a(i, "BasePart")).count();
    assert!((150..=300).contains(&parts), "{parts} parts");
    let spawns = ids.iter().filter(|&&i| w.is_a(i, "SpawnLocation")).count();
    assert_eq!(spawns, 1);
    let mut orders: Vec<f64> = ids
        .iter()
        .filter(|&&i| w.is_a(i, "Checkpoint"))
        .map(|&i| match w.get(i, "Order") {
            Ok(Value::Number(n)) => n,
            other => panic!("{other:?}"),
        })
        .collect();
    orders.sort_by(f64::total_cmp);
    assert_eq!(orders, vec![1.0, 2.0, 3.0]);

    let ws = w.root("Workspace").unwrap();
    let finish = w.find_first_child(ws, "Finish").expect("Finish part");
    assert!(w.children(finish).iter().any(|&c| w.is_a(c, "Script")));
    let storage = w.root("ReplicatedStorage").unwrap();
    for remote in ["Finished", "CheckpointReached"] {
        let r = w.find_first_child(storage, remote).expect(remote);
        assert!(w.is_a(r, "RemoteEvent"));
    }

    // The player's controls plate is the only controls row: the HUD's one text is the banner
    // (#480: a text row of the template's own sat under the plate).
    let gui = w.root("StarterGui").unwrap();
    let mut gui_ids = Vec::new();
    descendants(&w, gui, &mut gui_ids);
    let texts: Vec<String> = gui_ids
        .iter()
        .filter(|&&i| w.is_a(i, "TextLabel"))
        .map(|&i| w.full_name(i))
        .collect();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].ends_with("Banner.Message"), "{texts:?}");

    let lava = w.find_first_child(ws, "Lava").expect("Lava folder");
    let lava_parts: Vec<InstanceId> = w
        .children(lava)
        .iter()
        .copied()
        .filter(|&c| w.is_a(c, "BasePart"))
        .collect();
    assert!(lava_parts.len() >= 5);
    for p in lava_parts {
        assert_eq!(
            w.get(p, "Color").unwrap(),
            Value::Color3(Color3::new(196.0, 80.0, 30.0))
        );
        assert_eq!(
            w.get(p, "Material").unwrap(),
            Value::String("SmoothPlastic".into())
        );
    }

    let scripts: Vec<InstanceId> = ids
        .iter()
        .copied()
        .filter(|&i| w.is_a(i, "Script"))
        .collect();
    assert!(scripts.len() >= 6, "{} scripts", scripts.len());
    for s in scripts {
        let Ok(Value::String(src)) = w.get(s, "Source") else {
            panic!("no source")
        };
        let first = src.lines().next().unwrap_or("");
        assert!(
            first.starts_with("-- ") && first.len() > 12,
            "{} must start with a one-line comment, got {first:?}",
            w.full_name(s)
        );
    }
}

#[test]
fn every_one_stud_slab_has_its_faces_on_the_grid() {
    // Studio's grid rule (spec 2.2) snaps a part's bounding-box min corner, so a 1-stud slab is
    // on the 1-stud grid when its bottom (and so its top) is a whole stud. Walkable tops of the
    // course, its lava and the moving platforms are whole studs (#372).
    let w = load_scene(&template().join("main.mscene")).expect("load");
    let mut slabs = 0;
    for id in all(&w) {
        if !w.is_a(id, "BasePart") || w.get(id, "Size").is_err() {
            continue;
        }
        let size = vec3(&w, id, "Size");
        let turned = vec3(&w, id, "Orientation");
        if (size.y - 1.0).abs() > 1e-9 || turned != Vec3::new(0.0, 0.0, 0.0) {
            continue;
        }
        slabs += 1;
        let bottom = vec3(&w, id, "Position").y - 0.5;
        let name = w.get(id, "Name").ok();
        assert!(
            (bottom - bottom.round()).abs() < 1e-9,
            "{name:?}: bottom face at y = {bottom}"
        );
    }
    assert!(slabs > 60, "{slabs} slabs checked");
}

#[test]
fn walking_forward_moves_along_the_course_and_running_is_faster() {
    let out = Rc::new(Captured::default());
    let mut game = start(out.clone());
    run(&mut game, FrameInput::default(), 0.5);
    let root = root_of(&game);
    let world = game.world();
    let z0 = vec3(&world.borrow(), root, "Position").z;
    let walk = FrameInput {
        movement: [0.0, 1.0],
        ..FrameInput::default()
    };
    run(&mut game, walk, 0.5);
    let z1 = vec3(&world.borrow(), root, "Position").z;
    run(&mut game, FrameInput { run: true, ..walk }, 0.5);
    let z2 = vec3(&world.borrow(), root, "Position").z;
    let walked = z0 - z1;
    let ran = z1 - z2;
    assert!(
        (7.0..9.0).contains(&walked),
        "walked {walked} studs in 0.5 s"
    );
    assert!((11.0..13.0).contains(&ran), "ran {ran} studs in 0.5 s");
    let humanoid = world
        .borrow()
        .find_first_child(game.character().unwrap(), "Humanoid")
        .unwrap();
    assert_eq!(
        world.borrow().get(humanoid, "WalkSpeed").unwrap(),
        Value::Number(16.0),
        "scripts never see the run boost"
    );
    let errors: Vec<String> = out
        .0
        .borrow()
        .iter()
        .filter(|l| l.contains("error"))
        .cloned()
        .collect();
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn touching_lava_knocks_the_character_out_and_it_respawns() {
    let mut game = start(Rc::new(Captured::default()));
    run(&mut game, FrameInput::default(), 0.2);
    let world = game.world();
    let lava = {
        let w = world.borrow();
        let ws = w.root("Workspace").unwrap();
        let folder = w.find_first_child(ws, "Lava").unwrap();
        w.find_first_child(folder, "Lava").unwrap()
    };
    let first = game.character().unwrap();
    let root = root_of(&game);
    let on_lava = vec3(&world.borrow(), lava, "Position") + Vec3::new(0.0, 3.5, 0.0);
    world
        .borrow_mut()
        .set(root, "Position", Value::Vector3(on_lava))
        .unwrap();
    let out = game.frame(&FrameInput::default(), TICK);
    assert_ne!(out.state, Some(HumanoidState::Dead), "dies one tick later");
    run(&mut game, FrameInput::default(), 0.2);
    let out = game.frame(&FrameInput::default(), TICK);
    assert_eq!(out.state, Some(HumanoidState::Dead));
    run(&mut game, FrameInput::default(), 5.5);
    let second = game.character().unwrap();
    assert_ne!(first, second, "a new character spawned");
    let root = root_of(&game);
    let y = vec3(&world.borrow(), root, "Position").y;
    assert!((y - 3.4).abs() < 0.2, "respawned on the spawn pad, y = {y}");
}

#[test]
fn a_lava_part_added_while_running_knocks_the_character_out() {
    let mut game = start(Rc::new(Captured::default()));
    run(&mut game, FrameInput::default(), 0.2);
    let world = game.world();
    let spot = Vec3::new(10.0, 0.25, -6.0);
    {
        let mut w = world.borrow_mut();
        let ws = w.root("Workspace").unwrap();
        let folder = w.find_first_child(ws, "Lava").unwrap();
        let lava = w.create("Part", Some(folder)).unwrap();
        w.set(lava, "Anchored", Value::Bool(true)).unwrap();
        w.set(lava, "Size", Value::Vector3(Vec3::new(4.0, 0.5, 4.0)))
            .unwrap();
        w.set(lava, "Position", Value::Vector3(spot)).unwrap();
    }
    run(&mut game, FrameInput::default(), 0.2);
    let root = root_of(&game);
    world
        .borrow_mut()
        .set(
            root,
            "Position",
            Value::Vector3(spot + Vec3::new(0.0, 3.3, 0.0)),
        )
        .unwrap();
    run(&mut game, FrameInput::default(), 0.3);
    let out = game.frame(&FrameInput::default(), TICK);
    assert_eq!(out.state, Some(HumanoidState::Dead));
}

#[test]
fn parts_created_by_scripts_while_running_get_physics() {
    let mut w = World::new_with_services();
    let ws = w.root("Workspace").unwrap();
    let spawn = w.create("SpawnLocation", Some(ws)).unwrap();
    w.set(spawn, "Anchored", Value::Bool(true)).unwrap();
    w.set(spawn, "CanCollide", Value::Bool(false)).unwrap();
    w.set(spawn, "Position", Value::Vector3(Vec3::new(0.0, 50.0, 0.0)))
        .unwrap();
    let s = w
        .create("Script", Some(w.root("ServerScriptService").unwrap()))
        .unwrap();
    w.set(
        s,
        "Source",
        Value::String(
            "-- Builds a floor under the spawn a moment after the game starts.\n\
             task.wait(0.2)\n\
             local floor = Instance.new(\"Part\")\n\
             floor.Anchored = true\n\
             floor.Size = Vector3.new(40, 2, 40)\n\
             floor.Position = Vector3.new(0, 20, 0)\n\
             floor.Parent = workspace\n"
                .into(),
        ),
    )
    .unwrap();
    let out = Rc::new(Captured::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("Test".into(), w, out.clone());
    game.start().unwrap();
    run(&mut game, FrameInput::default(), 2.0);
    let root = root_of(&game);
    let y = vec3(&game.world().borrow(), root, "Position").y;
    assert!(
        (y - 24.0).abs() < 0.1,
        "stands on the scripted floor: y = {y}"
    );
    assert!(out.0.borrow().is_empty(), "{:?}", out.0.borrow());
}

#[test]
fn headless_runs_are_deterministic() {
    let trace = || {
        let mut game = start(Rc::new(Captured::default()));
        let input = FrameInput {
            movement: [0.3, 1.0],
            jump: true,
            ..FrameInput::default()
        };
        run(&mut game, input, 3.0);
        let root = root_of(&game);
        vec3(&game.world().borrow(), root, "Position")
    };
    assert_eq!(trace(), trace());
}

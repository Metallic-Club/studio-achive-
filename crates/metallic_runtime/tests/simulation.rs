//! CPU simulation contracts; this target also runs without presentation features.
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_physics::spawn_character;
use metallic_runtime::{MoveInput, PlayerInput, Simulation, TICK};
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

fn part(
    world: &mut World,
    parent: InstanceId,
    name: &str,
    position: Vec3,
    size: Vec3,
    anchored: bool,
) -> InstanceId {
    let id = world.create("Part", Some(parent)).unwrap();
    for (property, value) in [
        ("Name", Value::String(name.into())),
        ("Position", Value::Vector3(position)),
        ("Size", Value::Vector3(size)),
        ("Anchored", Value::Bool(anchored)),
    ] {
        world.set(id, property, value).unwrap();
    }
    id
}
fn script(world: &mut World, class: &str, parent: InstanceId, source: &str) {
    let id = world.create(class, Some(parent)).unwrap();
    world
        .set(id, "Source", Value::String(source.into()))
        .unwrap();
}
fn position(world: &World, id: InstanceId) -> Vec3 {
    match world.get(id, "Position").unwrap() {
        Value::Vector3(value) => value,
        _ => panic!("not a position"),
    }
}
fn character(world: &World, player: InstanceId) -> InstanceId {
    match world.get(player, "Character").unwrap() {
        Value::Ref(Some(value)) => value,
        _ => panic!("no character"),
    }
}
fn root(world: &World, player: InstanceId) -> InstanceId {
    world
        .find_first_child(character(world, player), "HumanoidRootPart")
        .unwrap()
}
fn floor(world: &mut World) {
    let ws = world.root("Workspace").unwrap();
    part(
        world,
        ws,
        "Floor",
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(1000.0, 1.0, 1000.0),
        true,
    );
}
fn simulation(world: World) -> (Simulation, Rc<Capture>) {
    let output = Rc::new(Capture::default());
    metallic_script::use_test_budgets_on_this_thread();
    let mut simulation = Simulation::from_world("CPU fixture".into(), world, output.clone());
    simulation.start().unwrap();
    (simulation, output)
}

#[test]
fn server_only_runs_physics_heartbeat_waits_and_no_synthetic_client() {
    let mut world = World::new_with_services();
    floor(&mut world);
    let ws = world.root("Workspace").unwrap();
    let body = part(
        &mut world,
        ws,
        "Body",
        Vec3::new(20.0, 20.0, 0.0),
        Vec3::new(2.0, 2.0, 2.0),
        false,
    );
    let server = world.root("ServerScriptService").unwrap();
    script(
        &mut world,
        "Script",
        server,
        r#"
assert(game:GetService('Players').LocalPlayer==nil)
assert(#game:GetService('Players'):GetChildren()==0)
game:GetService('Players').PlayerAdded:Connect(function() error('synthetic join') end)
print('started')
local beats=0
game:GetService('RunService').Heartbeat:Connect(function(dt)
    assert(math.abs(dt-1/60)<1e-12)
    beats+=1
    if beats==60 then print('sixty-heartbeats') end
end)
assert(task.wait(0.25)>=0.25)
print('waited')
"#,
    );
    let starter = world
        .find_first_child(world.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
        .unwrap();
    script(&mut world, "LocalScript", starter, "error('client ran')");
    let (mut simulation, output) = simulation(world);
    simulation.start().unwrap();
    for _ in 0..120 {
        assert!(simulation.tick_once(&[]));
    }
    assert_eq!(simulation.tick_count(), 120);
    assert_eq!(
        *output.0.borrow(),
        ["started", "waited", "sixty-heartbeats"]
    );
    let shared = simulation.world();
    let world = shared.borrow();
    assert!(position(&world, body).y < 10.0);
    assert!(world.children(world.root("Players").unwrap()).is_empty());
}

#[test]
fn fixed_ticks_match_at_different_frame_rates() {
    for rate in [30, 60, 144] {
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let marker = part(
            &mut world,
            ws,
            "Marker",
            Vec3::default(),
            Vec3::new(2.0, 2.0, 2.0),
            true,
        );
        let server = world.root("ServerScriptService").unwrap();
        script(
            &mut world,
            "Script",
            server,
            "game:GetService('RunService').Heartbeat:Connect(function() workspace.Marker.Position+=Vector3.new(1,0,0) end)",
        );
        let (mut simulation, output) = simulation(world);
        for _ in 0..rate * 2 {
            simulation.advance(1.0 / f64::from(rate), &[]);
        }
        assert_eq!(simulation.tick_count(), 120, "{rate}");
        assert!((simulation.time() - 2.0).abs() < 1e-10);
        assert_eq!(position(&simulation.world().borrow(), marker).x, 120.0);
        assert!(output.0.borrow().is_empty());
    }
}

#[test]
fn invalid_clamped_residual_and_borrowed_time_has_no_partial_ticks() {
    let (mut simulation, _) = simulation(World::new_with_services());
    for dt in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0, 0.0] {
        assert_eq!(simulation.advance(dt, &[]), 0);
    }
    assert_eq!(simulation.advance(TICK / 2.0, &[]), 0);
    assert_eq!(simulation.advance(TICK / 2.0, &[]), 1);
    let shared = simulation.world();
    let held = shared.borrow_mut();
    let cursor = held.change_cursor();
    assert_eq!(simulation.advance(TICK, &[]), 0);
    assert!(!simulation.tick_once(&[]));
    assert_eq!(simulation.tick_count(), 1);
    assert_eq!(held.change_cursor(), cursor);
    drop(held);
    assert_eq!(simulation.advance(0.0, &[]), 1);
    assert_eq!(simulation.advance(0.5, &[]), 15);
    assert_eq!(simulation.tick_count(), 17);
    simulation.stop();
    assert_eq!(simulation.advance(1.0, &[]), 0);
    assert!(!simulation.tick_once(&[]));
    simulation.start().unwrap();
    assert_eq!(simulation.time(), 0.0);
    assert_eq!(simulation.tick_count(), 0);
    assert_eq!(simulation.advance(0.0, &[]), 0);
}

#[test]
fn player_input_is_world_local_unique_per_tick_and_follows_respawn() {
    let mut world = World::new_with_services();
    floor(&mut world);
    let players = world.root("Players").unwrap();
    let one = world.create("Player", Some(players)).unwrap();
    world.set(one, "Name", Value::String("One".into())).unwrap();
    let two = world.create("Player", Some(players)).unwrap();
    world.set(two, "Name", Value::String("Two".into())).unwrap();
    // Keep collision resolution out of this input/respawn contract. Both Players have
    // distinct explicit respawn points, so the isolation remains valid after death too.
    for (player, x) in [(one, 0.0), (two, 100.0)] {
        let spawn = world
            .create("SpawnLocation", world.root("Workspace"))
            .unwrap();
        for (property, value) in [
            ("Position", Value::Vector3(Vec3::new(x, -0.5, 0.0))),
            ("Size", Value::Vector3(Vec3::new(4.0, 1.0, 4.0))),
            ("Anchored", Value::Bool(true)),
            ("CanCollide", Value::Bool(false)),
        ] {
            world.set(spawn, property, value).unwrap();
        }
        world
            .set(player, "RespawnLocation", Value::Ref(Some(spawn)))
            .unwrap();
    }
    let old = spawn_character(&mut world, one).unwrap();
    spawn_character(&mut world, two).unwrap();
    let (mut simulation, output) = simulation(world);
    let input = PlayerInput {
        player: one,
        input: MoveInput {
            dir: Vec3::new(1.0, 0.0, 0.0),
            ..MoveInput::default()
        },
    };
    let first = PlayerInput {
        player: one,
        input: MoveInput {
            dir: Vec3::new(-1.0, 0.0, 0.0),
            ..MoveInput::default()
        },
    };
    for _ in 0..60 {
        simulation.tick_once(&[first, input]);
    }
    {
        let shared = simulation.world();
        let world = shared.borrow();
        let moved = position(&world, root(&world, one));
        let idle = position(&world, root(&world, two));
        assert!(
            (moved.x - 16.0).abs() < 0.1,
            "moved={moved:?}, idle={idle:?}"
        );
        // Contact correction is floating point; idle means bounded horizontal drift,
        // rather than a bit-exact pose after repeated capsule/floor queries.
        assert!(
            (idle.x - 100.0).abs() < 1e-3 && idle.z.abs() < 1e-3,
            "idle={idle:?}"
        );
    }
    let shared = simulation.world();
    let humanoid = shared.borrow().find_first_child(old, "Humanoid").unwrap();
    shared
        .borrow_mut()
        .set(humanoid, "Health", Value::Number(0.0))
        .unwrap();
    for _ in 0..330 {
        simulation.tick_once(&[input]);
    }
    let replacement = character(&shared.borrow(), one);
    assert_ne!(replacement, old);
    let before = position(&shared.borrow(), root(&shared.borrow(), one)).x;
    for _ in 0..60 {
        simulation.tick_once(&[input]);
    }
    assert!(
        (position(&shared.borrow(), root(&shared.borrow(), one)).x - before - 16.0).abs() < 0.1
    );
    assert!(output.0.borrow().is_empty(), "{:?}", output.0.borrow());
}

#[test]
fn real_touches_follow_humanoid_membership_changes_through_the_shared_kernel() {
    for (initial, change, expected) in [
        (false, None, 1),
        (true, None, 0),
        (false, Some(true), 0),
        (true, Some(false), 1),
    ] {
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let model = world.create("Model", Some(ws)).unwrap();
        world
            .set(model, "Name", Value::String("NPC".into()))
            .unwrap();
        let sensor = part(
            &mut world,
            model,
            "Sensor",
            Vec3::default(),
            Vec3::new(2.0, 2.0, 2.0),
            true,
        );
        world.set(sensor, "CanCollide", Value::Bool(false)).unwrap();
        part(
            &mut world,
            ws,
            "Target",
            Vec3::new(-8.0, 0.0, 0.0),
            Vec3::new(2.0, 2.0, 2.0),
            true,
        );
        let humanoid = initial.then(|| world.create("Humanoid", Some(model)).unwrap());
        let server = world.root("ServerScriptService").unwrap();
        script(
            &mut world,
            "Script",
            server,
            "print('ready') workspace.NPC.Sensor.Touched:Connect(function(hit) if hit.Name=='Target' then print('touch') end end) game:GetService('RunService').Heartbeat:Connect(function() workspace.Target.Position+=Vector3.new(0.1,0,0) end)",
        );
        let (mut simulation, output) = simulation(world);
        if let Some(add) = change {
            if add {
                simulation
                    .world()
                    .borrow_mut()
                    .create("Humanoid", Some(model))
                    .unwrap();
            } else {
                simulation.world().borrow_mut().destroy(humanoid.unwrap());
            }
        }
        for _ in 0..100 {
            simulation.tick_once(&[]);
        }
        let lines = output.0.borrow();
        assert_eq!(lines.first().map(String::as_str), Some("ready"));
        assert_eq!(
            lines.iter().filter(|s| s.as_str() == "touch").count(),
            expected,
            "initial={initial} change={change:?}: {lines:?}"
        );
        assert!(
            lines.iter().all(|line| line == "ready" || line == "touch"),
            "{lines:?}"
        );
    }
}

#[test]
fn stopped_server_tasks_and_connections_do_not_escape_into_restart() {
    let mut world = World::new_with_services();
    let server = world.root("ServerScriptService").unwrap();
    script(
        &mut world,
        "Script",
        server,
        "print('start') task.delay(1,function() print('delayed') end)",
    );
    let (mut simulation, output) = simulation(world);
    simulation.tick_once(&[]);
    simulation.stop();
    for _ in 0..120 {
        assert!(!simulation.tick_once(&[]));
    }
    assert_eq!(*output.0.borrow(), ["start"]);
    simulation.start().unwrap();
    for _ in 0..65 {
        simulation.tick_once(&[]);
    }
    assert_eq!(*output.0.borrow(), ["start", "start", "delayed"]);
}

#[cfg(feature = "presentation")]
#[test]
fn combined_game_and_server_use_the_same_world_space_movement_kernel() {
    use metallic_runtime::{FrameInput, Game};
    let mut base = World::new_with_services();
    floor(&mut base);
    let mut server_world = base.deep_clone();
    let player = server_world
        .create("Player", server_world.root("Players"))
        .unwrap();
    server_world
        .set(player, "Name", Value::String("Player1".into()))
        .unwrap();
    spawn_character(&mut server_world, player).unwrap();
    let (mut server, output) = simulation(server_world);
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::from_world("Combined".into(), base, Rc::new(Capture::default()));
    game.start().unwrap();
    for _ in 0..120 {
        game.frame(
            &FrameInput {
                movement: [1.0, 0.0],
                ..FrameInput::default()
            },
            TICK,
        );
        server.advance(
            TICK,
            &[PlayerInput {
                player,
                input: MoveInput {
                    dir: Vec3::new(1.0, 0.0, 0.0),
                    ..MoveInput::default()
                },
            }],
        );
    }
    let shared = server.world();
    let world = shared.borrow();
    assert!(
        (game.root_position().unwrap() - position(&world, root(&world, player))).magnitude() < 1e-3
    );
    assert_eq!(game.time(), server.time());
    assert!(output.0.borrow().is_empty());
}

/// Game (presentation) and a server-mode Simulation stepped side by side from identical worlds
/// must keep identical worlds: the Simulation's tick is Game's tick minus rendering. The scene
/// exercises what main added to Game's tick: a script-moved platform the character rides
/// (#256, #382), a spinner, limb touch volumes from CPU bone poses (#370), a loose part, a death
/// and the controller's respawn (#343, #346), and a script-destroyed character that respawns
/// through the gone-without-dying timer (#273, #346), and part-part TouchEnded (#417).
#[cfg(feature = "presentation")]
mod game_parity {
    use super::*;
    use metallic_core::classes;
    use metallic_runtime::{CameraController, FrameInput, Game};
    use std::collections::{HashMap, HashSet};

    const TICKS: usize = 1200;

    struct Fnv(u64);
    impl Fnv {
        fn eat(&mut self, bytes: &[u8]) {
            for &b in bytes {
                self.0 ^= u64::from(b);
                self.0 = self.0.wrapping_mul(0x0100_0000_01b3);
            }
        }
    }

    /// FNV-1a over the whole tree in pre-order: class, parent position and every registry
    /// property. A Ref hashes as its target's tree position, so the worlds' ids may differ.
    fn world_hash(world: &World) -> u64 {
        let mut order = Vec::new();
        let mut stack: Vec<InstanceId> = world.roots().iter().rev().copied().collect();
        while let Some(id) = stack.pop() {
            order.push(id);
            stack.extend(world.children(id).iter().rev().copied());
        }
        let index: HashMap<InstanceId, u64> = order
            .iter()
            .enumerate()
            .map(|(i, &id)| (id, i as u64))
            .collect();
        let mut h = Fnv(0xcbf2_9ce4_8422_2325);
        for &id in &order {
            let class = world.class_name(id).unwrap();
            h.eat(class.as_bytes());
            h.eat(
                &world
                    .parent(id)
                    .map_or(u64::MAX, |p| index[&p])
                    .to_le_bytes(),
            );
            for def in classes::all_properties(class) {
                h.eat(def.name.as_bytes());
                // A Camera is per-client state: the combined game publishes the view it draws,
                // a server has no view.
                if class == "Camera" && def.repl == classes::Repl::ServerOnly {
                    continue;
                }
                match world.get(id, def.name) {
                    Ok(Value::Ref(target)) => {
                        let at = target
                            .map_or(u64::MAX, |t| index.get(&t).copied().unwrap_or(u64::MAX - 1));
                        h.eat(&at.to_le_bytes());
                    }
                    // Debug prints every f64 in its exact shortest round-trip form.
                    Ok(value) => h.eat(format!("{value:?}").as_bytes()),
                    Err(_) => h.eat(&[0xff]),
                }
            }
        }
        h.0
    }

    /// The shared scene. Scripts only change existing instances, so instance order is the same
    /// whether the character spawns before (server) or after (Game) the scripts start.
    fn scene() -> World {
        let mut world = World::new_with_services();
        floor(&mut world);
        let ws = world.root("Workspace").unwrap();
        // Both runtimes add a `Camera` when the scene has none; having it already keeps the
        // creation order (and so the ids) the same on both sides of this comparison.
        let camera = world.create("Camera", Some(ws)).unwrap();
        world
            .set(camera, "Name", Value::String("Camera".into()))
            .unwrap();
        let at = |x, y, z| Vec3::new(x, y, z);
        part(
            &mut world,
            ws,
            "Mover",
            at(0.0, 0.5, -14.0),
            at(8.0, 1.0, 8.0),
            true,
        );
        part(
            &mut world,
            ws,
            "Spinner",
            at(12.0, 0.5, -6.0),
            at(6.0, 1.0, 6.0),
            true,
        );
        let pad = part(
            &mut world,
            ws,
            "Pad",
            at(0.0, 0.6, -6.0),
            at(4.0, 0.2, 4.0),
            true,
        );
        world.set(pad, "CanCollide", Value::Bool(false)).unwrap();
        part(
            &mut world,
            ws,
            "Crate",
            at(3.0, 6.0, -3.0),
            at(2.0, 2.0, 2.0),
            false,
        );
        // Part-part touches that start and end (#417): a script sweeps a sensor through a post.
        let sweeper = part(
            &mut world,
            ws,
            "Sweeper",
            at(30.0, 10.0, 0.0),
            at(2.0, 2.0, 2.0),
            true,
        );
        world
            .set(sweeper, "CanCollide", Value::Bool(false))
            .unwrap();
        part(
            &mut world,
            ws,
            "Post",
            at(36.0, 10.0, 0.0),
            at(2.0, 2.0, 2.0),
            true,
        );
        let sss = world.root("ServerScriptService").unwrap();
        script(
            &mut world,
            "Script",
            sss,
            r#"
local mover, spinner, pad = workspace.Mover, workspace.Spinner, workspace.Pad
local t, touches = 0, 0
local sweeper = workspace.Sweeper
sweeper.Touched:Connect(function(hit) print("sweep touch " .. hit.Name) end)
sweeper.TouchEnded:Connect(function(hit) print("sweep end " .. hit.Name) end)
pad.Touched:Connect(function(hit)
    touches += 1
    pad.Transparency = (touches % 100) / 100
    print("pad " .. hit.Name)
end)
game:GetService("RunService").Heartbeat:Connect(function(dt)
    t += dt
    mover.Position = Vector3.new(math.sin(t) * 4, 0.5, -14)
    spinner.Orientation = Vector3.new(0, (t * 90) % 360, 0)
    sweeper.Position = Vector3.new(36 + math.sin(t * 2) * 6, 10, 0)
end)
task.delay(5, function()
    game:GetService("Players").Player1.Character.Humanoid.Health = 0
    print("killed")
end)
task.delay(12, function()
    game:GetService("Players").Player1.Character:Destroy()
    print("destroyed")
end)
"#,
        );
        world
    }

    /// Movement for tick `i`: walk forward onto the mover, jump, ride, then strafe and run.
    fn frame_input(i: usize) -> FrameInput {
        let (movement, jump, run) = match i {
            0..=40 => ([0.0, 1.0], false, false),
            41..=44 => ([0.0, 1.0], true, false),
            45..=200 => ([0.0, 0.0], false, false),
            201..=400 => ([0.7, 0.7], false, true),
            401..=420 => ([-1.0, 0.0], true, false),
            _ => ([0.3, -1.0], i % 90 < 3, i % 200 > 100),
        };
        FrameInput {
            movement,
            jump,
            run,
            ..FrameInput::default()
        }
    }

    #[test]
    fn game_and_server_simulation_keep_identical_world_hashes() {
        let game_out = Rc::new(Capture::default());
        metallic_script::use_test_budgets_on_this_thread();
        let mut game = Game::from_world("Parity".into(), scene(), game_out.clone());
        game.start().unwrap();

        let mut server_world = scene();
        let players = server_world.root("Players").unwrap();
        let player = server_world.create("Player", Some(players)).unwrap();
        server_world
            .set(player, "Name", Value::String("Player1".into()))
            .unwrap();
        server_world.create("PlayerGui", Some(player)).unwrap();
        spawn_character(&mut server_world, player).unwrap();
        let (mut server, server_out) = simulation(server_world);
        let shared = server.world();
        assert_eq!(
            world_hash(&game.world().borrow()),
            world_hash(&shared.borrow()),
            "worlds differ after start"
        );

        // Game turns camera-relative movement into world space with its camera; do the same.
        let mut camera = CameraController::new();
        let mut characters = HashSet::new();
        for i in 0..TICKS {
            let input = frame_input(i);
            camera.apply_input(&input);
            let sample = PlayerInput {
                player,
                input: MoveInput {
                    dir: camera.move_direction(input.movement),
                    jump: input.jump,
                    run: input.run,
                },
            };
            assert_eq!(game.frame(&input, TICK).ticks, 1, "tick {i}");
            assert_eq!(server.advance(TICK, &[sample]), 1, "tick {i}");
            let game_hash = world_hash(&game.world().borrow());
            assert_eq!(
                game_hash,
                world_hash(&shared.borrow()),
                "world hashes differ after tick {i}"
            );
            if let Ok(Value::Ref(Some(model))) = shared.borrow().get(player, "Character") {
                characters.insert(model);
            }
        }
        assert_eq!(game.time(), server.time());
        assert_eq!(server.tick_count(), TICKS as u64);
        let (g, s) = (game_out.0.borrow(), server_out.0.borrow());
        assert_eq!(*g, *s, "script output differs");
        assert!(g.iter().any(|l| l.starts_with("pad ")), "no touch: {g:?}");
        assert!(g.iter().any(|l| l == "killed"), "{g:?}");
        let ends = g.iter().filter(|l| *l == "sweep end Post").count();
        let starts = g.iter().filter(|l| *l == "sweep touch Post").count();
        assert!(
            ends >= 2 && starts >= ends,
            "TouchEnded: {starts} touches, {ends} ends"
        );
        assert!(g.iter().any(|l| l == "destroyed"), "{g:?}");
        assert!(!g.iter().any(|l| l.starts_with("error:")), "{g:?}");
        // The death and the destroy both respawned: three Models over the run, the last alive.
        assert_eq!(characters.len(), 3, "characters seen: {characters:?}");
        let world = shared.borrow();
        let last = character(&world, player);
        assert!(world.is_alive(last) && world.find_first_child(last, "HumanoidRootPart").is_some());
    }
}

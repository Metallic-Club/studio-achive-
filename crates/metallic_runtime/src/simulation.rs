//! CPU fixed-step orchestration shared by the server simulation and presentation Game.
use crate::body_pose::CharacterBodies;
use crate::physics_sync::PhysicsSyncCache;
use crate::{ProjectConfig, RuntimeError};
use metallic_core::{InstanceId, Value, World, load_scene};
use metallic_physics::{
    CharacterController, CharacterEvent, HumanoidState, PhysicsWorld, RESPAWN_DELAY,
    spawn_character,
};
pub use metallic_physics::{MoveInput, RUN_MULTIPLIER};
use metallic_script::{LOCAL_PLAYER_NAME, OutputSink, ScriptHost};
use std::{cell::RefCell, collections::HashMap, path::Path, rc::Rc};

/// Simulation ticks per second (physics, characters and scripts).
pub const TICK_RATE: f64 = 60.0;
/// Length of one simulation tick in seconds.
pub const TICK: f64 = 1.0 / TICK_RATE;
/// Longest frame time [`Simulation::advance`] simulates; a longer stall (a dragged window, a breakpoint)
/// is dropped instead of being caught up in a burst.
pub const MAX_FRAME_DT: f64 = 0.25;
/// Name of the character part passed as `hit` when a character touches a part.
const ROOT_PART: &str = "HumanoidRootPart";

/// Script output to the terminal: prints to stdout, warnings and errors to stderr.
pub struct StdoutSink;

impl OutputSink for StdoutSink {
    fn print(&self, line: &str) {
        println!("{line}");
    }
    fn warn(&self, line: &str) {
        eprintln!("warning: {line}");
    }
    fn error(&self, line: &str) {
        eprintln!("error: {line}");
    }
}

/// One trusted, world-local Player's world-space movement wish for a simulation tick.
/// This is not a backend user ID, admission token or network message. Repeated samples
/// for a player use the last value; missing samples mean idle.
/// The caller supplies finite direction components with horizontal length at most one;
/// this API does not validate hostile commands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerInput {
    /// A Player InstanceId belonging to this simulation's World.
    pub player: InstanceId,
    /// World-space movement, jump and run wishes.
    pub input: MoveInput,
}

/// World, physics, characters and scripts without camera, GUI or rendering.
/// CPU skeletal animation drives limb touch volumes independently of the drawn avatar.
/// Public constructors select server-only scripts and never create a synthetic player.
/// The default presentation Game uses a private Combined constructor over this same kernel.
/// The trusted World must give each Player a distinct live Character reference: sharing
/// one character between Players would step that character more than once per tick.
pub struct Simulation {
    pub(crate) name: String,
    pub(crate) world: Rc<RefCell<World>>,
    pub(crate) physics: PhysicsWorld,
    pub(crate) characters: CharacterController,
    pub(crate) scripts: ScriptHost,
    pub(crate) player: Option<InstanceId>,
    pub(crate) character: Option<InstanceId>,
    pub(crate) time: f64,
    accumulator: f64,
    physics_sync: PhysicsSyncCache,
    pub(crate) running: bool,
    combined: bool,
    ticks: u64,
    bodies: CharacterBodies,
    gui_copies: Vec<InstanceId>,
    character_gone: f64,
    /// Server mode: the gone-without-dying respawn timer of each Player that has had a
    /// Character (the same timer Combined mode keeps in `character_gone`).
    server_gone: HashMap<InstanceId, f64>,
}

impl Simulation {
    /// Loads a trusted project's main scene with terminal output and server-only scripts.
    /// Errors are reported through the existing RuntimeError variants.
    pub fn load(project_dir: &Path) -> Result<Self, RuntimeError> {
        Self::open(project_dir, None, Rc::new(StdoutSink))
    }

    /// Loads a trusted project's main scene or the given project-relative scene.
    /// This does not sandbox filesystem paths or admit players.
    pub fn open(
        project_dir: &Path,
        scene: Option<&Path>,
        output: Rc<dyn OutputSink>,
    ) -> Result<Self, RuntimeError> {
        Self::open_mode(project_dir, scene, output, false)
    }

    #[cfg(feature = "presentation")]
    pub(crate) fn open_combined(
        project_dir: &Path,
        scene: Option<&Path>,
        output: Rc<dyn OutputSink>,
    ) -> Result<Self, RuntimeError> {
        Self::open_mode(project_dir, scene, output, true)
    }

    fn open_mode(
        project_dir: &Path,
        scene: Option<&Path>,
        output: Rc<dyn OutputSink>,
        combined: bool,
    ) -> Result<Self, RuntimeError> {
        let config = ProjectConfig::read(project_dir)?;
        let path = project_dir.join(scene.unwrap_or(&config.main_scene));
        if !path.is_file() {
            return Err(RuntimeError::MissingScene(path));
        }
        let world = load_scene(&path).map_err(|source| RuntimeError::Scene { path, source })?;
        Ok(Self::with_mode(config.name, world, output, combined))
    }

    /// Creates a stopped server-only simulation over an already loaded World.
    /// Existing instances are retained; this constructor supplies no player admission API.
    pub fn from_world(name: String, world: World, output: Rc<dyn OutputSink>) -> Self {
        Self::with_mode(name, world, output, false)
    }

    #[cfg(feature = "presentation")]
    pub(crate) fn combined(name: String, world: World, output: Rc<dyn OutputSink>) -> Self {
        Self::with_mode(name, world, output, true)
    }

    fn with_mode(name: String, world: World, output: Rc<dyn OutputSink>, combined: bool) -> Self {
        let world = Rc::new(RefCell::new(world));
        let scripts = if combined {
            ScriptHost::new(world.clone(), output)
        } else {
            ScriptHost::new_server(world.clone(), output)
        };
        Self {
            name,
            world,
            physics: PhysicsWorld::new(),
            characters: CharacterController::new(),
            scripts,
            player: None,
            character: None,
            time: 0.0,
            accumulator: 0.0,
            physics_sync: PhysicsSyncCache::default(),
            running: false,
            combined,
            ticks: 0,
            bodies: CharacterBodies::default(),
            gui_copies: Vec::new(),
            character_gone: 0.0,
            server_gone: HashMap::new(),
        }
    }

    /// Project name.
    pub fn name(&self) -> &str {
        &self.name
    }
    /// World shared with the scripts. Release borrows before advancing the simulation.
    pub fn world(&self) -> Rc<RefCell<World>> {
        self.world.clone()
    }
    /// Simulated seconds since the latest successful start.
    pub fn time(&self) -> f64 {
        self.time
    }
    /// Completed fixed ticks since the latest successful start.
    pub fn tick_count(&self) -> u64 {
        self.ticks
    }
    /// Whether the simulation is running.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// Starts scripts and synchronizes physics for queries. Server mode creates no local player.
    /// Calling start while running is a no-op. Stop/start retains the existing World state.
    /// Returns RuntimeError::Start when VM or Combined character setup fails.
    pub fn start(&mut self) -> Result<(), RuntimeError> {
        if self.running {
            return Ok(());
        }
        self.ensure_camera();
        // Startup scripts run immediately and may remove an existing Character before
        // the first tick can observe it. Remember only already-spawned server Players.
        let spawned_players = if self.combined {
            Vec::new()
        } else {
            let world = self
                .world
                .try_borrow()
                .map_err(|_| RuntimeError::Start("the world is busy".into()))?;
            world.root("Players").map_or_else(Vec::new, |players| {
                world
                    .children(players)
                    .iter()
                    .copied()
                    .filter(|&player| {
                        world.is_a(player, "Player")
                            && matches!(world.get_ref(player, "Character"), Ok(Value::Ref(Some(_))))
                    })
                    .collect()
            })
        };
        self.scripts
            .start()
            .map_err(|e| RuntimeError::Start(e.message))?;
        if self.combined {
            let (player, model) = match self.spawn_local_player() {
                Ok(spawned) => spawned,
                Err(error) => {
                    self.scripts.stop();
                    return Err(error);
                }
            };
            self.player = Some(player);
            self.character = Some(model);
        }
        self.sync_physics_now();
        if let (Some(player), Some(model)) = (self.player, self.character) {
            self.scripts.character_added(player, model);
        }
        self.time = 0.0;
        self.accumulator = 0.0;
        self.ticks = 0;
        self.character_gone = 0.0;
        self.server_gone.clear();
        self.server_gone
            .extend(spawned_players.into_iter().map(|player| (player, 0.0)));
        self.running = true;
        Ok(())
    }

    /// Makes sure the Workspace has the `Camera` that `workspace.CurrentCamera` returns.
    fn ensure_camera(&mut self) {
        let Ok(mut world) = self.world.try_borrow_mut() else {
            return;
        };
        let Some(workspace) = world.root("Workspace") else {
            return;
        };
        let has = world
            .children(workspace)
            .iter()
            .any(|&c| world.class_name(c) == Some("Camera"));
        if !has && let Ok(camera) = world.create("Camera", Some(workspace)) {
            let _ = world.set(camera, "Name", Value::String("Camera".into()));
        }
    }

    /// Finds `Player1` (created by the script host), gives it the StarterGui and a character.
    fn spawn_local_player(&mut self) -> Result<(InstanceId, InstanceId), RuntimeError> {
        let mut world = self
            .world
            .try_borrow_mut()
            .map_err(|_| RuntimeError::Start("the world is busy".into()))?;
        let player = world
            .root("Players")
            .and_then(|p| world.find_first_child(p, LOCAL_PLAYER_NAME))
            .ok_or_else(|| RuntimeError::Start("no local player".into()))?;
        for old in self.gui_copies.drain(..) {
            world.destroy(old);
        }
        self.gui_copies = copy_starter_gui(&mut world, player);
        let model = spawn_character(&mut world, player)
            .map_err(|e| RuntimeError::Start(format!("cannot spawn the character: {e}")))?;
        Ok((player, model))
    }

    /// Stops scripts and releases their tasks/connections, retaining the last World state.
    pub fn stop(&mut self) {
        self.scripts.stop();
        self.running = false;
    }

    /// Advances by elapsed seconds, preserving the existing 60 Hz accumulator and 0.25 s cap.
    /// Negative/non-finite elapsed time is zero. Returns the number of completed ticks.
    /// A borrowed World retains pending time for retry and causes no partial tick.
    /// Server mode steps each valid Player's current Character once, defaulting to idle.
    /// No ticks run while the script debugger has the game paused, and a tick that hits a
    /// breakpoint ends the frame.
    pub fn advance(&mut self, dt: f64, inputs: &[PlayerInput]) -> u32 {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_FRAME_DT)
        } else {
            0.0
        };
        let mut ticks = 0;
        // Paused in the script debugger: physics, characters and scripts hold still and no time
        // accumulates. A host without a debugger answers with one load (`ScriptHost::is_paused`).
        if self.running && !self.scripts.is_paused() {
            self.accumulator = (self.accumulator + dt).min(MAX_FRAME_DT + TICK);
            while self.accumulator + 1e-9 >= TICK {
                if !self.tick(inputs) {
                    break;
                }
                self.accumulator -= TICK;
                ticks += 1;
                if self.scripts.is_paused() {
                    // A breakpoint hit during this tick: the simulation stops right here, and
                    // the frame's remaining time is dropped rather than run on Continue.
                    self.accumulator = 0.0;
                    break;
                }
            }
        }
        self.scripts.end_frame();
        ticks
    }

    /// Executes exactly one fixed tick, without consuming buffered fractional frame time.
    /// Returns false while stopped, paused in the debugger or while the World is borrowed. This
    /// is useful for a deterministic trusted local runner; it is not a real-time capacity
    /// guarantee.
    pub fn tick_once(&mut self, inputs: &[PlayerInput]) -> bool {
        let completed = self.running && !self.scripts.is_paused() && self.tick(inputs);
        self.scripts.end_frame();
        completed
    }

    fn tick(&mut self, inputs: &[PlayerInput]) -> bool {
        // 2^64 ticks at 60 Hz is about 9.7 billion years; saturate rather than panic.
        let next_tick = self.ticks.saturating_add(1);
        let mut respawned = Vec::new();
        {
            let Ok(mut world) = self.world.try_borrow_mut() else {
                return false;
            };
            if self.combined {
                if let (Some(player), Some(character)) = (self.player, self.character) {
                    self.characters.step(
                        &mut world,
                        &self.physics,
                        character,
                        input_for(inputs, player),
                        TICK,
                    );
                    self.bodies
                        .animate(&mut self.characters, &world, character, TICK as f32);
                }
            } else if let Some(players) = world.root("Players") {
                // Controller steps may replace a Character under Workspace, but do not mutate
                // Players' child order. Each Player is visited once, regardless of input duplicates.
                let count = world.children(players).len();
                for index in 0..count {
                    let Some(&player) = world.children(players).get(index) else {
                        continue;
                    };
                    if !world.is_a(player, "Player") {
                        continue;
                    }
                    let character = match world.get_ref(player, "Character") {
                        Ok(Value::Ref(Some(character))) => *character,
                        _ => continue,
                    };
                    self.characters.step(
                        &mut world,
                        &self.physics,
                        character,
                        input_for(inputs, player),
                        TICK,
                    );
                    self.bodies
                        .animate(&mut self.characters, &world, character, TICK as f32);
                }
            }
            self.physics.step(&mut world, TICK);
            if self.combined {
                if let (Some(player), Some(character)) = (
                    self.player,
                    respawn_if_gone(
                        &mut world,
                        &self.characters,
                        self.player,
                        &mut self.character_gone,
                    ),
                ) {
                    respawned.push((player, character));
                }
            } else {
                respawned =
                    respawn_server_players(&mut world, &self.characters, &mut self.server_gone);
            }
        }
        for (player, character) in respawned {
            if Some(player) == self.player {
                self.character = Some(character);
            }
            self.sync_physics_now();
            self.scripts.character_added(player, character);
        }
        self.feed_events();
        self.time += TICK;
        self.ticks = next_tick;
        self.scripts.step(self.time, TICK);
        self.sync_physics_if_changed();
        true
    }

    /// Hands this tick's character and physics events to the script host.
    fn feed_events(&mut self) {
        for event in self.characters.drain_events() {
            match event {
                CharacterEvent::Touched { part, hit, .. } => {
                    self.scripts.touched(part, hit);
                    self.scripts.touched(hit, part);
                }
                CharacterEvent::TouchEnded { part, hit, .. } => {
                    self.scripts.touch_ended(part, hit);
                    self.scripts.touch_ended(hit, part);
                }
                CharacterEvent::Died { humanoid, .. } => self.scripts.humanoid_died(humanoid),
                CharacterEvent::Respawned {
                    player, character, ..
                } => {
                    let alive = self.world.try_borrow().is_ok_and(|w| w.is_alive(character));
                    if !alive {
                        continue;
                    }
                    if Some(player) == self.player {
                        self.character = Some(character);
                    }
                    self.sync_physics_now();
                    self.scripts.character_added(player, character);
                }
                CharacterEvent::CheckpointReached { .. } => {}
            }
        }
        for (a, b) in self.physics.drain_touched() {
            self.scripts.touched(a, b);
            self.scripts.touched(b, a);
        }
        // Drained every tick (#417): undrained part-part TouchEnded pairs would pile up on a server.
        for (a, b) in self.physics.drain_touch_ended() {
            self.scripts.touch_ended(a, b);
            self.scripts.touch_ended(b, a);
        }
    }

    /// Re-syncs changed part signatures and character membership; pose-only changes need no sync.
    fn sync_physics_if_changed(&mut self) {
        if let Ok(world) = self.world.try_borrow() {
            self.physics_sync
                .synchronize(&mut self.physics, &world, false);
        }
    }

    /// Syncs the physics with the world right away.
    fn sync_physics_now(&mut self) {
        let Ok(world) = self.world.try_borrow() else {
            return;
        };
        self.physics_sync
            .synchronize(&mut self.physics, &world, true);
    }
}

fn input_for(inputs: &[PlayerInput], player: InstanceId) -> MoveInput {
    inputs
        .iter()
        .rev()
        .find(|sample| sample.player == player)
        .map_or_else(MoveInput::default, |sample| sample.input)
}

/// The one respawn timer for a character that is gone without dying (#273): a Model a script
/// destroyed, took out of the Workspace or stripped of its `HumanoidRootPart`. `gone` counts
/// the seconds; after [`RESPAWN_DELAY`] a new character spawns.
///
/// The player's `Character` property is read each tick (never a cached id), so a respawn that
/// already happened (the controller's after a death, or a script's) cancels this timer, and a
/// dead character is left to the controller's own timer: one character ever spawns.
fn respawn_if_gone(
    world: &mut World,
    controller: &CharacterController,
    player: Option<InstanceId>,
    gone: &mut f64,
) -> Option<InstanceId> {
    let player = player.filter(|&p| world.is_alive(p))?;
    let character = match world.get(player, "Character") {
        Ok(Value::Ref(c)) => c,
        _ => None,
    };
    let present = character.is_some_and(|c| {
        world.is_alive(c)
            && in_workspace(world, c)
            && world.find_first_child(c, ROOT_PART).is_some()
    });
    // The controller respawns a dead character it can still step (Humanoid and root present).
    let dying = character.is_some_and(|c| {
        world.is_alive(c)
            && controller.state(c) == Some(HumanoidState::Dead)
            && world.find_first_child(c, "Humanoid").is_some()
            && world.find_first_child(c, ROOT_PART).is_some()
    });
    if present || dying {
        *gone = 0.0;
        return None;
    }
    *gone += TICK;
    if *gone < RESPAWN_DELAY {
        return None;
    }
    *gone = 0.0;
    spawn_character(world, player).ok()
}

/// Server mode's [`respawn_if_gone`]: the same timer for every Player under `Players`, visited
/// in child order. Only Players that have had a Character are tracked, so a Player a server has
/// not spawned yet gets no character from here. Timers of Players that are gone are dropped.
fn respawn_server_players(
    world: &mut World,
    controller: &CharacterController,
    timers: &mut HashMap<InstanceId, f64>,
) -> Vec<(InstanceId, InstanceId)> {
    let mut respawned = Vec::new();
    let Some(players) = world.root("Players") else {
        timers.clear();
        return respawned;
    };
    let count = world.children(players).len();
    for index in 0..count {
        let Some(&player) = world.children(players).get(index) else {
            continue;
        };
        if !world.is_a(player, "Player") {
            continue;
        }
        let has_character = matches!(world.get_ref(player, "Character"), Ok(Value::Ref(Some(_))));
        if !has_character && !timers.contains_key(&player) {
            continue;
        }
        let gone = timers.entry(player).or_insert(0.0);
        if let Some(character) = respawn_if_gone(world, controller, Some(player), gone) {
            respawned.push((player, character));
        }
    }
    timers.retain(|&player, _| world.is_alive(player) && world.parent(player) == Some(players));
    respawned
}

/// True when `id` is a descendant of the Workspace.
fn in_workspace(world: &World, id: InstanceId) -> bool {
    let Some(ws) = world.root("Workspace") else {
        return false;
    };
    let mut at = world.parent(id);
    while let Some(p) = at {
        if p == ws {
            return true;
        }
        at = world.parent(p);
    }
    false
}

/// Copies every child of `StarterGui` into the player's `PlayerGui` and returns the copies.
fn copy_starter_gui(world: &mut World, player: InstanceId) -> Vec<InstanceId> {
    let mut copies = Vec::new();
    let Some(starter) = world.root("StarterGui") else {
        return copies;
    };
    let Some(gui) = world
        .children(player)
        .iter()
        .copied()
        .find(|&c| world.is_a(c, "PlayerGui"))
    else {
        return copies;
    };
    for child in world.children(starter).to_vec() {
        if let Ok(copy) = world.clone_subtree(child) {
            if world.set_parent(copy, Some(gui)).is_ok() {
                copies.push(copy);
            } else {
                world.destroy(copy);
            }
        }
    }
    copies
}

#[cfg(test)]
mod compatibility_tests {
    use super::*;
    use metallic_core::Vec3;
    use metallic_physics::BodyPart;

    struct Quiet;
    impl OutputSink for Quiet {
        fn print(&self, _: &str) {}
        fn warn(&self, line: &str) {
            panic!("{line}");
        }
        fn error(&self, line: &str) {
            panic!("{line}");
        }
    }

    #[test]
    fn server_ticks_animate_each_players_body_without_presentation() {
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let floor = world.create("Part", Some(ws)).unwrap();
        world
            .set(
                floor,
                "Size",
                Value::Vector3(Vec3::new(1000.0, 1.0, 1000.0)),
            )
            .unwrap();
        world
            .set(floor, "Position", Value::Vector3(Vec3::new(0.0, -0.5, 0.0)))
            .unwrap();
        world.set(floor, "Anchored", Value::Bool(true)).unwrap();
        let players = world.root("Players").unwrap();
        let mut ids = Vec::new();
        for x in [0.0, 100.0] {
            let player = world.create("Player", Some(players)).unwrap();
            let character = spawn_character(&mut world, player).unwrap();
            let root = world.find_first_child(character, ROOT_PART).unwrap();
            world
                .set(root, "Position", Value::Vector3(Vec3::new(x, 3.0, 0.0)))
                .unwrap();
            ids.push((player, character));
        }
        metallic_script::use_test_budgets_on_this_thread();
        let mut sim = Simulation::from_world("body animation".into(), world, Rc::new(Quiet));
        sim.start().unwrap();
        let inputs: Vec<_> = ids
            .iter()
            .map(|&(player, _)| PlayerInput {
                player,
                input: MoveInput {
                    dir: Vec3::new(0.0, 0.0, -1.0),
                    run: true,
                    ..MoveInput::default()
                },
            })
            .collect();
        let mut lo = [f64::MAX; 2];
        let mut hi = [f64::MIN; 2];
        for _ in 0..120 {
            assert!(sim.tick_once(&inputs));
            for (i, &(_, character)) in ids.iter().enumerate() {
                let pose = sim.characters.body_pose(character);
                assert!(pose.is_finite());
                let hand = pose.parts[BodyPart::RightHand.index()].position.z;
                lo[i] = lo[i].min(hand);
                hi[i] = hi[i].max(hand);
            }
        }
        for i in 0..2 {
            assert!(
                hi[i] - lo[i] > 0.8,
                "player {i}: running hand must swing, {:?}",
                (lo[i], hi[i])
            );
        }
        assert_eq!(sim.world.borrow().children(players).len(), 2);
    }
}

//! [`Game`]: one running game and its frame loop.
use crate::{MAX_FRAME_DT, PlayerInput, RuntimeError, Simulation, StdoutSink};
const ROOT_PART: &str = "HumanoidRootPart";
use crate::avatar::{Avatar, default_model};
use crate::body_pose::locomotion_input;
use crate::camera_controller::{CameraController, FOCUS_HEIGHT};
use crate::gui_input::{GuiInput, PointerId, PointerPhase};
use crate::input::{
    ActionEvent, ActionEventKind, ActionId, ActionValue, DeviceKind, FrameInput, InputRuntime,
};
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_physics::{HumanoidState, MoveInput};
use metallic_render::glam::Vec3 as GVec3;
use metallic_render::{Camera, Renderer};
use metallic_script::{
    InputActionEvent, InputDevice, InputEventKind, InputValue, OutputSink, TouchButtonInfo,
};
use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

/// What a frame produced, for the app to draw.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameOutput {
    /// The camera to render this frame with.
    pub camera: Camera,
    /// Simulation ticks run during this frame (0 when the display is faster than [`crate::TICK_RATE`]).
    pub ticks: u32,
    /// The local character's state, if it has one.
    pub state: Option<HumanoidState>,
}

/// One running game: the world, its physics, characters, scripts and camera.
///
/// The world is shared with the script host as `Rc<RefCell<World>>`; the game never holds a
/// borrow of it while scripts run.
pub struct Game {
    simulation: Simulation,
    camera: CameraController,
    last_camera: Camera,
    /// Where the camera looks; kept still while the character is dead.
    focus: Vec3,
    /// The local character's skinned avatar and the character Model it belongs to.
    avatar: Option<(InstanceId, Avatar)>,
    /// `InputRuntime::device_changes` when the scripts were last told the last-used device.
    input_device_changes: Option<u64>,
    /// Points at the top of the screen that ScreenGuis leave free (see [`Game::set_gui_inset`]).
    gui_inset: f32,
    gui_input: GuiInput,
}

impl Game {
    /// Loads the project in `project_dir` (its `metallic.toml` and main scene). Script output goes
    /// to the terminal ([`StdoutSink`]).
    ///
    /// Errors: [`RuntimeError::Config`] for a bad `metallic.toml`, [`RuntimeError::MissingScene`]
    /// when the main scene file does not exist, [`RuntimeError::Scene`] when it does not load.
    pub fn load(project_dir: &Path) -> Result<Game, RuntimeError> {
        Game::open(project_dir, None, Rc::new(StdoutSink))
    }

    /// Like [`Game::load`], but plays `scene` (relative to the project folder) instead of the
    /// main scene when given, and sends script output to `output`.
    pub fn open(
        project_dir: &Path,
        scene: Option<&Path>,
        output: Rc<dyn OutputSink>,
    ) -> Result<Game, RuntimeError> {
        Ok(Self::from_simulation(Simulation::open_combined(
            project_dir,
            scene,
            output,
        )?))
    }

    /// A game over an already loaded world (Studio's Play passes a copy of the edited world).
    pub fn from_world(name: String, world: World, output: Rc<dyn OutputSink>) -> Game {
        Self::from_simulation(Simulation::combined(name, world, output))
    }

    fn from_simulation(simulation: Simulation) -> Self {
        Self {
            simulation,
            camera: CameraController::new(),
            last_camera: Camera::default(),
            focus: Vec3::default(),
            avatar: None,
            input_device_changes: None,
            gui_inset: 0.0,
            gui_input: GuiInput::new(),
        }
    }

    /// The game's name from `metallic.toml`.
    pub fn name(&self) -> &str {
        &self.simulation.name
    }

    /// The world, shared with the scripts. Do not keep a borrow across [`Game::frame`].
    pub fn world(&self) -> Rc<RefCell<World>> {
        self.simulation.world.clone()
    }

    /// The local player (`Players.Player1`) once started.
    pub fn player(&self) -> Option<InstanceId> {
        self.simulation.player
    }

    /// The local player's current character Model.
    pub fn character(&self) -> Option<InstanceId> {
        self.simulation.character
    }

    /// Simulated seconds since [`Game::start`].
    pub fn time(&self) -> f64 {
        self.simulation.time
    }

    /// The camera of the last frame.
    pub fn camera(&self) -> Camera {
        self.last_camera
    }

    /// True between [`Game::start`] and [`Game::stop`].
    pub fn is_running(&self) -> bool {
        self.simulation.running
    }

    /// The script debugger (breakpoints, pause, step, inspection). While it has the game
    /// paused, [`Game::frame`] runs no ticks.
    pub fn debugger(&mut self) -> metallic_script::Debugger<'_> {
        self.simulation.scripts.debugger()
    }

    /// True while the script debugger has the game paused.
    pub fn is_paused(&self) -> bool {
        self.simulation.scripts.is_paused()
    }

    /// Starts the game: runs the scripts (which creates the local player `Player1`), copies the
    /// contents of `StarterGui` into the player's `PlayerGui` (once per start; a restart replaces
    /// the previous copies, while retaining runtime-created GUI),
    /// spawns the character on the first `SpawnLocation` and fires `CharacterAdded`.
    /// Calling it again while running does nothing.
    ///
    /// Errors: [`RuntimeError::Start`] if the script VMs or the character cannot be created.
    pub fn start(&mut self) -> Result<(), RuntimeError> {
        if self.simulation.running {
            return Ok(());
        }
        self.simulation.start()?;
        self.focus = self.root_position().unwrap_or_default();
        self.gui_input.reset();
        Ok(())
    }

    /// Keeps the `Camera` instance and the drawn camera in step. While its `CameraType` is
    /// `Custom` the game's camera is published to the instance (so scripts read where the
    /// camera is); with `Scriptable` or `Fixed` the instance's `CFrame` and `FieldOfView` are
    /// the camera. The drawn camera has no roll: its up is always +Y.
    fn sync_camera(&mut self) {
        let world = Rc::clone(&self.simulation.world);
        let Ok(mut world) = world.try_borrow_mut() else {
            return;
        };
        let Some(workspace) = world.root("Workspace") else {
            return;
        };
        let Some(camera) = world.children(workspace).iter().copied().find(|&c| {
            world.class_name(c) == Some("Camera")
                && matches!(world.get_ref(c, "Name"), Ok(Value::String(n)) if n == "Camera")
        }) else {
            return;
        };
        let scripted =
            !matches!(world.get_ref(camera, "CameraType"), Ok(Value::String(t)) if t == "Custom");
        if scripted {
            let pose = match world.get_ref(camera, "CFrame") {
                Ok(Value::CFrame(cf)) if cf.is_finite() => **cf,
                _ => return,
            };
            let fov = match world.get_ref(camera, "FieldOfView") {
                Ok(Value::Number(n)) => *n as f32,
                _ => self.last_camera.fov_deg,
            };
            let look = pose.look_vector();
            // Straight up or down would have no horizontal direction to draw with: lean a hair.
            let look = if look.x.abs() + look.z.abs() < 1e-6 {
                Vec3::new(look.x + 1e-4, look.y, look.z)
            } else {
                look
            };
            let at = pose.position + look;
            self.last_camera = Camera {
                position: GVec3::new(
                    pose.position.x as f32,
                    pose.position.y as f32,
                    pose.position.z as f32,
                ),
                look_at: GVec3::new(at.x as f32, at.y as f32, at.z as f32),
                fov_deg: fov,
            };
        } else {
            let eye = Vec3::new(
                f64::from(self.last_camera.position.x),
                f64::from(self.last_camera.position.y),
                f64::from(self.last_camera.position.z),
            );
            let at = Vec3::new(
                f64::from(self.last_camera.look_at.x),
                f64::from(self.last_camera.look_at.y),
                f64::from(self.last_camera.look_at.z),
            );
            let pose = metallic_core::CFrame::look_at(eye, at, Vec3::new(0.0, 1.0, 0.0));
            let _ = world.set(camera, "CFrame", Value::CFrame(Box::new(pose)));
            let _ = world.set(
                camera,
                "FieldOfView",
                Value::Number(f64::from(self.last_camera.fov_deg)),
            );
        }
    }

    /// Reports the size of the viewport the game is drawn in (points), for the scripts'
    /// `Camera.ViewportSize` and the projection methods. Call it when the window resizes.
    pub fn set_viewport_size(&mut self, width: f32, height: f32) {
        self.simulation
            .scripts
            .set_viewport_size(f64::from(width), f64::from(height));
    }

    /// Stops the scripts, retaining the last World state.
    pub fn stop(&mut self) {
        self.simulation.stop();
        self.gui_input.reset();
    }

    /// Evaluates the player's input for one frame and connects it to the scripts; call it before
    /// [`Game::frame`] and pass the result to it. `now` is the input clock (seconds), `dt` the
    /// frame time.
    ///
    /// In order: the runtime follows scene changes (a `BindAction` creates actions) and the
    /// answers of handlers from the last frame are applied; the raw events are evaluated; the
    /// events and values of every `InputAction` instance go to the scripts; the client scripts
    /// handle at most [`metallic_script::MAX_ACTION_EVENTS_PER_STEP`] events now, under the
    /// frame's 0.25 s handler budget; `Sink` answers of `ContextActionService` handlers are
    /// applied at once (spec 3.7), so a sunk key never reaches the contexts below it, the
    /// default character controls included. A `Pass` answer, or a handler that has not answered
    /// (over budget, or yielded), lets the key through. Allocates nothing once the buffers have
    /// grown (the script values handlers receive aside).
    pub fn input_frame(&mut self, rt: &mut InputRuntime, now: f64, dt: f32) -> FrameInput {
        if let Ok(world) = self.simulation.world.try_borrow() {
            rt.sync(&world);
        }
        self.apply_script_sinks(rt, now);
        rt.evaluate(now, dt);
        self.feed_input(rt);
        if self.simulation.running {
            self.simulation
                .scripts
                .dispatch_input_now(self.simulation.time);
        }
        self.apply_script_sinks(rt, now);
        // Release edges of actions that a sink just blocked.
        self.feed_input(rt);
        rt.frame_input()
    }

    fn apply_script_sinks(&mut self, rt: &mut InputRuntime, now: f64) {
        self.simulation.scripts.drain_bound_results(|r| {
            if let Some(action) = rt_action(rt, r.action) {
                rt.set_script_sink(action, r.sink, now);
            }
        });
    }

    /// Sends the runtime's events and the values of its scene actions to the scripts.
    fn feed_input(&mut self, rt: &mut InputRuntime) {
        while let Some(ActionEvent {
            action,
            kind,
            value,
            time,
        }) = rt.pop_event()
        {
            let Some(instance) = rt.instance(action) else {
                continue;
            };
            let kind = match kind {
                ActionEventKind::Pressed => InputEventKind::Pressed,
                ActionEventKind::Released => InputEventKind::Released,
                ActionEventKind::Triggered => InputEventKind::Triggered,
                ActionEventKind::StateChanged => InputEventKind::StateChanged,
            };
            self.simulation
                .scripts
                .input_action_event(InputActionEvent {
                    action: instance,
                    kind,
                    value: input_value(value),
                    time,
                });
        }
        for i in 0..rt.action_count() {
            let id = ActionId(i as u16);
            if let Some(instance) = rt.instance(id) {
                self.simulation.scripts.set_action_state(
                    instance,
                    input_value(rt.state(id)),
                    rt.hold_progress(id),
                );
            }
        }
        if self.input_device_changes != Some(rt.device_changes()) {
            self.input_device_changes = Some(rt.device_changes());
            let (device, last) = match rt.last_device() {
                DeviceKind::KeyboardMouse => (InputDevice::KeyboardMouse, "Keyboard"),
                DeviceKind::Gamepad => (InputDevice::Gamepad, "Gamepad1"),
                DeviceKind::Touch => (InputDevice::Touch, "Touch"),
            };
            self.simulation.scripts.set_preferred_input(device);
            self.simulation.scripts.set_last_input_type(last);
        }
    }

    /// Advances the game by a frame of `dt` seconds and returns the camera to draw it with.
    ///
    /// Camera orbit and zoom apply every frame. The simulation runs in fixed ticks of [`crate::TICK`]
    /// (an accumulator carries the remainder; `dt` is clamped to [`MAX_FRAME_DT`], a negative or
    /// non-finite `dt` counts as 0). Each tick, in order:
    /// 1. character controller (movement from `input`, death, respawn, checkpoints, touches),
    ///    then the character's CPU body animation, whose pose the touch volumes use next tick;
    /// 2. physics step (movers, loose parts); then a character that was destroyed or taken out
    ///    of the Workspace respawns after [`metallic_physics::RESPAWN_DELAY`];
    /// 3. events fed to the scripts: part-part touches both ways, character touches as
    ///    `part.Touched(hit)` and `part.TouchEnded(hit)` with the touching body part (`LeftHand`,
    ///    ...) or the `HumanoidRootPart`, `Humanoid.Died`, `CharacterAdded` on respawn;
    /// 4. script step (inputs, `RunService.Heartbeat`, resumed waits);
    /// 5. physics re-sync when scripts created, destroyed or reshaped parts (change journal).
    ///
    /// No ticks run while the script debugger has the game paused ([`Game::is_paused`]).
    ///
    /// Then the skinned avatar follows the character (pose, smoothed facing, animation over `dt`)
    /// and the camera follows too. (The touch volumes follow the tick-rate body animation of
    /// step 1, which the avatar's animation matches.) Draw with [`Game::render`] and [`crate::gui::paint`].
    pub fn frame(&mut self, input: &FrameInput, dt: f64) -> FrameOutput {
        let dt = if dt.is_finite() {
            dt.clamp(0.0, MAX_FRAME_DT)
        } else {
            0.0
        };
        self.camera.apply_input(input);
        for event in self.gui_input.drain_events() {
            self.simulation.scripts.gui_event(event);
        }
        let sample = self.simulation.player.map(|player| PlayerInput {
            player,
            input: MoveInput {
                dir: self.camera.move_direction(input.movement),
                jump: input.jump,
                run: input.run,
            },
        });
        let ticks = self.simulation.advance(dt, sample.as_slice());
        let state = self
            .simulation
            .character
            .and_then(|c| self.simulation.characters.state(c));
        if !self.simulation.scripts.is_paused() {
            // The avatar's pose holds still while paused, like the simulation.
            self.update_avatar(dt as f32);
        }
        if state != Some(HumanoidState::Dead)
            && let Some(root) = self.root_position()
        {
            self.focus = root + Vec3::new(0.0, FOCUS_HEIGHT, 0.0);
        }
        let ignore = self.character_parts();
        self.last_camera =
            self.camera
                .camera(self.focus, &self.simulation.physics, &ignore, dt as f32);
        self.sync_camera();
        FrameOutput {
            camera: self.last_camera,
            ticks,
            state,
        }
    }

    /// Brings `renderer` up to date with the world and draws the frame into `target` (`size` in
    /// pixels) with the camera of the last [`Game::frame`].
    pub fn render(&self, renderer: &mut Renderer, target: &wgpu::TextureView, size: (u32, u32)) {
        // The size drawn is the viewport the scripts' camera methods work in.
        self.simulation
            .scripts
            .set_viewport_size(f64::from(size.0), f64::from(size.1));
        self.render_from(renderer, target, size, &self.last_camera);
    }

    /// Like [`Game::render`] but from `camera` (overview shots, Studio's editor camera).
    pub fn render_from(
        &self,
        renderer: &mut Renderer,
        target: &wgpu::TextureView,
        size: (u32, u32),
        camera: &Camera,
    ) {
        self.sync_renderer(renderer);
        renderer.render(target, camera, size);
    }

    /// Brings `renderer` up to date with the world and the avatar without drawing (for
    /// [`Renderer::capture`]).
    pub fn sync_renderer(&self, renderer: &mut Renderer) {
        if let Ok(world) = self.simulation.world.try_borrow() {
            renderer.sync(&world);
        }
        match &self.avatar {
            Some((_, avatar)) => renderer.set_skinned(&[avatar.instance()]),
            None => renderer.set_skinned(&[]),
        }
    }

    /// Tells the scripts where the touch overlay's buttons are, for
    /// `ContextActionService:GetButton` (see
    /// [`TouchControls::button_infos`](crate::input::touch::TouchControls::button_infos)).
    pub fn set_touch_buttons(&mut self, buttons: &[TouchButtonInfo]) {
        self.simulation.scripts.set_touch_buttons(buttons);
    }

    /// Sets `UserInputService.TouchEnabled`, `KeyboardEnabled` and `GamepadEnabled`.
    pub fn set_input_devices(&mut self, touch: bool, keyboard: bool, gamepad: bool) {
        self.simulation
            .scripts
            .set_input_devices(touch, keyboard, gamepad);
    }

    /// Sets the GUI inset: the top `points` of the screen belong to the Player's own controls
    /// (the touch overlay's menu button, spec 7.2), so every ScreenGui is laid out in the screen
    /// below it, like Roblox's default `IgnoreGuiInset = false`. Zero when nothing is there.
    pub fn set_gui_inset(&mut self, points: f32) {
        self.gui_inset = if points.is_finite() {
            points.max(0.0)
        } else {
            0.0
        };
        self.simulation
            .scripts
            .set_gui_inset(f64::from(self.gui_inset));
    }

    /// The GUI inset in points.
    pub fn gui_inset(&self) -> f32 {
        self.gui_inset
    }

    /// Draws the local player's GUI with `painter`, shaded by hover and press (see [`crate::gui`]).
    pub fn paint_gui(&self, painter: &egui::Painter) {
        if let Ok(world) = self.simulation.world.try_borrow() {
            let input = &self.gui_input;
            crate::gui::paint_shaded(&world, painter, self.gui_inset, &|id| input.shade(id));
        }
    }

    /// Gives a pointer event to the GUI before the game's input: the left mouse button and
    /// position, or a finger, at `at` GUI points (window pixels over the scale factor) on a
    /// `screen` of width x height points. Returns true when the GUI took it; the caller must
    /// then not pass it (nor that press's moves and release) to the [`InputRuntime`], so a
    /// click on a button never also drives the camera or a jump. The events reach the client
    /// scripts in the next [`Game::frame`]'s first tick. Does nothing (false) while stopped.
    pub fn gui_pointer(
        &mut self,
        pointer: PointerId,
        phase: PointerPhase,
        at: [f32; 2],
        screen: [f32; 2],
    ) -> bool {
        if !self.simulation.running {
            return false;
        }
        let Ok(world) = self.simulation.world.try_borrow() else {
            return false;
        };
        self.gui_input
            .pointer(&world, screen, self.gui_inset, pointer, phase, at)
    }

    /// The mouse left the window: the GUI elements under it get `MouseLeave`.
    pub fn gui_mouse_left(&mut self, at: [f32; 2]) {
        if self.simulation.running {
            self.gui_input.mouse_left(at);
        }
    }

    /// Moves and animates the local character's avatar; a new character (after a respawn) gets a
    /// fresh one.
    fn update_avatar(&mut self, dt: f32) {
        let (Some(character), Some((root, yaw))) = (self.simulation.character, self.root_pose())
        else {
            self.avatar = None;
            return;
        };
        let root = GVec3::new(root.x as f32, root.y as f32, root.z as f32);
        let yaw = (yaw as f32).to_radians();
        if self.avatar.as_ref().map(|(c, _)| *c) != Some(character) {
            self.avatar = default_model().map(|m| (character, Avatar::new(m, root, yaw)));
        }
        let Some((_, avatar)) = self.avatar.as_mut() else {
            return;
        };
        let input = locomotion_input(&self.simulation.characters, character);
        avatar.update(root, yaw, input, dt);
    }

    /// The character's `HumanoidRootPart` position and facing (`Orientation.Y`, degrees).
    fn root_pose(&self) -> Option<(Vec3, f64)> {
        let world = self.simulation.world.try_borrow().ok()?;
        let root = world.find_first_child(self.simulation.character?, ROOT_PART)?;
        match (world.get(root, "Position"), world.get(root, "Orientation")) {
            (Ok(Value::Vector3(p)), Ok(Value::Vector3(o))) => Some((p, o.y)),
            _ => None,
        }
    }

    /// The local character's `HumanoidRootPart` position, if it has one.
    pub fn root_position(&self) -> Option<Vec3> {
        let world = self.simulation.world.try_borrow().ok()?;
        let root = world.find_first_child(self.simulation.character?, ROOT_PART)?;
        match world.get(root, "Position") {
            Ok(Value::Vector3(v)) => Some(v),
            _ => None,
        }
    }

    /// The character's parts (the camera ray ignores them).
    fn character_parts(&self) -> Vec<InstanceId> {
        let (Some(model), Ok(world)) = (
            self.simulation.character,
            self.simulation.world.try_borrow(),
        ) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        let mut stack = vec![model];
        while let Some(id) = stack.pop() {
            if world.is_a(id, "BasePart") {
                out.push(id);
            }
            stack.extend_from_slice(world.children(id));
        }
        out
    }
}

/// The runtime action of an `InputAction` instance.
fn rt_action(rt: &InputRuntime, instance: InstanceId) -> Option<ActionId> {
    rt.action_for_instance(instance)
}

fn input_value(v: ActionValue) -> InputValue {
    match v {
        ActionValue::Bool(b) => InputValue::Bool(b),
        ActionValue::Axis1(x) => InputValue::Axis1(x),
        ActionValue::Axis2(x) => InputValue::Axis2(x),
        ActionValue::Axis3(x) => InputValue::Axis3(x),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{TICK, locomotion::state};
    use metallic_physics::RESPAWN_DELAY;

    struct Silent;
    impl OutputSink for Silent {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    /// A started game on a flat baseplate whose top is at y = 0.
    fn game() -> Game {
        let mut w = World::new_with_services();
        let ws = w.root("Workspace").unwrap();
        let floor = w.create("Part", Some(ws)).unwrap();
        w.set(floor, "Size", Value::Vector3(Vec3::new(400.0, 1.0, 400.0)))
            .unwrap();
        w.set(floor, "Position", Value::Vector3(Vec3::new(0.0, -0.5, 0.0)))
            .unwrap();
        w.set(floor, "Anchored", Value::Bool(true)).unwrap();
        metallic_script::use_test_budgets_on_this_thread();
        let mut g = Game::from_world("test".into(), w, Rc::new(Silent));
        g.start().unwrap();
        g
    }

    fn run(g: &mut Game, input: FrameInput, seconds: f64) {
        for _ in 0..(seconds / TICK).round() as usize {
            g.frame(&input, TICK);
        }
    }

    fn avatar(g: &Game) -> &Avatar {
        &g.avatar.as_ref().expect("the character has an avatar").1
    }

    #[test]
    fn the_avatar_stands_on_the_floor_and_follows_the_character() {
        let mut g = game();
        run(&mut g, FrameInput::default(), 0.5);
        let a = avatar(&g);
        assert!(a.feet().y.abs() < 0.05, "feet at {}", a.feet().y);
        assert_eq!(a.locomotion().state(), state::GROUND);
        // Walk forward (-Z): the avatar moves with the root.
        let forward = FrameInput {
            movement: [0.0, 1.0],
            ..FrameInput::default()
        };
        run(&mut g, forward, 1.0);
        let root = g.root_position().unwrap();
        let feet = avatar(&g).feet();
        assert!((f64::from(feet.z) - root.z).abs() < 1e-3);
        assert!(root.z < -10.0, "walked {root:?}");
    }

    #[test]
    fn the_avatar_turns_smoothly_toward_the_move_direction() {
        let mut g = game();
        run(&mut g, FrameInput::default(), 0.2);
        assert!(avatar(&g).yaw().abs() < 1e-4);
        // Walk right (+X): the physics faces yaw -90 degrees at once; the avatar turns over a
        // few frames.
        let right = FrameInput {
            movement: [1.0, 0.0],
            ..FrameInput::default()
        };
        g.frame(&right, TICK);
        let turned = |g: &Game| {
            let y = avatar(g).yaw();
            (y - std::f32::consts::TAU).abs().min(y.abs())
        };
        let first = turned(&g);
        assert!(
            first > 0.05 && first < 1.0,
            "first frame turned {first} rad"
        );
        run(&mut g, right, 0.3);
        assert!(
            (turned(&g) - std::f32::consts::FRAC_PI_2).abs() < 0.02,
            "turned {}",
            turned(&g)
        );
    }

    #[test]
    fn jumping_plays_jump_then_lands() {
        let mut g = game();
        run(&mut g, FrameInput::default(), 0.3);
        let jump = FrameInput {
            jump: true,
            ..FrameInput::default()
        };
        g.frame(&jump, TICK);
        assert_eq!(avatar(&g).locomotion().state(), state::JUMP);
        // Apex after 0.25 s, back down after about 0.5 s.
        let mut seen: Vec<String> = Vec::new();
        for _ in 0..60 {
            g.frame(&FrameInput::default(), TICK);
            let s = avatar(&g).locomotion().state().to_owned();
            if seen.last() != Some(&s) {
                seen.push(s);
            }
        }
        assert_eq!(seen, [state::JUMP, state::FALL, state::LAND]);
        run(&mut g, FrameInput::default(), 0.6);
        assert_eq!(avatar(&g).locomotion().state(), state::GROUND);
    }
    /// #273: a Died handler that takes the body out of the Workspace (without destroying it)
    /// gets exactly one new character, and `Player.Character` is that one.
    #[test]
    fn a_body_removed_on_death_respawns_once() {
        let mut g = game();
        run(&mut g, FrameInput::default(), 0.2);
        let player = g.player().unwrap();
        let old = g.character().unwrap();
        {
            let mut w = g.simulation.world.borrow_mut();
            let humanoid = w.find_first_child(old, "Humanoid").unwrap();
            w.set(humanoid, "Health", Value::Number(0.0)).unwrap();
        }
        g.frame(&FrameInput::default(), TICK);
        // What the Died handler does.
        g.simulation
            .world
            .borrow_mut()
            .set_parent(old, None)
            .unwrap();
        run(&mut g, FrameInput::default(), RESPAWN_DELAY + 1.0);
        let w = g.simulation.world.borrow();
        let ws = w.root("Workspace").unwrap();
        let models: Vec<_> = w
            .children(ws)
            .iter()
            .copied()
            .filter(|&c| w.is_a(c, "Model"))
            .collect();
        assert_eq!(models.len(), 1, "one character: {models:?}");
        assert_eq!(g.character(), Some(models[0]));
        assert_eq!(
            w.get(player, "Character").unwrap(),
            Value::Ref(Some(models[0]))
        );
    }

    fn screen_guis(g: &Game) -> usize {
        let w = g.simulation.world.borrow();
        let Some(gui) = crate::gui::local_player_gui(&w) else {
            return 0;
        };
        w.children(gui)
            .iter()
            .filter(|&&c| w.is_a(c, "ScreenGui"))
            .count()
    }

    /// #273: stop then start again gives one copy of the StarterGui, not two.
    #[test]
    fn a_restart_does_not_duplicate_the_hud() {
        let mut w = World::new_with_services();
        let starter = w.root("StarterGui").unwrap();
        let hud = w.create("ScreenGui", Some(starter)).unwrap();
        w.set(hud, "Name", Value::String("HUD".into())).unwrap();
        metallic_script::use_test_budgets_on_this_thread();
        let mut g = Game::from_world("test".into(), w, Rc::new(Silent));
        g.start().unwrap();
        assert_eq!(screen_guis(&g), 1);
        g.stop();
        g.start().unwrap();
        assert_eq!(screen_guis(&g), 1, "one HUD after a restart");
        // A GUI a script made at run time is not ours to remove.
        {
            let mut w = g.simulation.world.borrow_mut();
            let gui = crate::gui::local_player_gui(&w).unwrap();
            w.create("ScreenGui", Some(gui)).unwrap();
        }
        g.stop();
        g.start().unwrap();
        assert_eq!(screen_guis(&g), 2);
    }

    /// #273: a script that destroys the character gets a new one after the respawn delay.
    #[test]
    fn a_destroyed_character_respawns_after_the_delay() {
        let mut g = game();
        run(&mut g, FrameInput::default(), 0.2);
        let old = g.character().unwrap();
        g.simulation.world.borrow_mut().destroy(old);
        run(&mut g, FrameInput::default(), RESPAWN_DELAY - 0.5);
        assert_eq!(g.character(), Some(old), "not before the delay");
        run(&mut g, FrameInput::default(), 1.0);
        let new = g.character().unwrap();
        assert_ne!(new, old);
        assert!(g.simulation.world.borrow().is_alive(new));
        assert!(g.root_position().is_some());
        let player = g.player().unwrap();
        assert_eq!(
            g.simulation
                .world
                .borrow()
                .get(player, "Character")
                .unwrap(),
            Value::Ref(Some(new))
        );
        // Removed from the Workspace without being destroyed counts too.
        g.simulation
            .world
            .borrow_mut()
            .set_parent(new, None)
            .unwrap();
        run(&mut g, FrameInput::default(), RESPAWN_DELAY + 0.5);
        assert_ne!(g.character(), Some(new));
    }
}

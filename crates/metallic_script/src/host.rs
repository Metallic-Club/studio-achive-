//! [`ScriptHost`]: runs the game's scripts and feeds events (Touched, deaths, new characters,
//! remotes) to them. The default host has server/client VMs over one shared `World`;
//! [`ScriptHost::new_server`] starts only the server side without a synthetic local player.
//! [`ScriptHost::with_role`] also selects a Client VM that waits for explicit local binding.
//!
//! The runtime (task 8) owns the host and calls [`ScriptHost::step`] every frame. Inputs
//! that come from other systems (physics, character controller) are queued with
//! [`ScriptHost::touched`], [`ScriptHost::humanoid_died`] and
//! [`ScriptHost::character_added`] and reach the scripts on the next `step`.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use metallic_core::{ChangeCursor, Changes, InstanceId, Value as CoreValue, World};
use mlua::Value;

use crate::api::{
    self, ApiState, BoundResult, GuiEvent, HostShared, InputActionEvent, InputDevice, InputValue,
    MAX_GUI_EVENTS_PER_STEP, RemoteTarget, TouchButtonInfo,
};
use crate::debug::{Breakpoint, Debugger};
use crate::vm::{
    Context, OutputSink, ScriptError, ScriptVm, initial_resume_budget, initial_step_budget_sec,
};

/// Name of the one local player created by the default combined [`ScriptHost::start`].
pub const LOCAL_PLAYER_NAME: &str = "Player1";

/// Which script VMs a host creates. This is a local execution policy, not authentication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScriptRole {
    /// Legacy shared-world server/client execution with a synthetic local player.
    Combined,
    /// Server scripts only, with players supplied by the caller.
    Server,
    /// Client scripts only, waiting for an explicitly bound local player.
    Client,
}

/// One VM together with the API state that drives it.
pub(crate) struct Side {
    pub(crate) vm: ScriptVm,
    pub(crate) state: Rc<ApiState>,
}

/// A script that was started: where it runs.
pub(crate) struct RunningScript {
    context: Context,
}

/// The owner id a script's threads and connections carry (never 0).
pub(crate) fn owner_id(id: InstanceId) -> u64 {
    id.to_bits() + 1
}

/// An input from another system, delivered on the next `step`.
enum HostEvent {
    Touched {
        part: InstanceId,
        hit: InstanceId,
    },
    TouchEnded {
        part: InstanceId,
        hit: InstanceId,
    },
    Died {
        humanoid: InstanceId,
    },
    CharacterAdded {
        player: InstanceId,
        model: InstanceId,
    },
    PlayerAdded {
        player: InstanceId,
    },
}

/// Runs a world's scripts in the fixed [`ScriptRole`] selected at construction.
pub struct ScriptHost {
    pub(crate) world: Rc<RefCell<World>>,
    pub(crate) output: Rc<dyn OutputSink>,
    shared: Rc<HostShared>,
    server: Option<Side>,
    client: Option<Side>,
    role: ScriptRole,
    /// Explicitly registered players, plus the Combined host's synthetic local player.
    players: HashSet<InstanceId>,
    /// Last client eligibility scope; Character changes need more than script journal IDs.
    client_scope: Option<(InstanceId, Option<InstanceId>)>,
    inputs: Vec<HostEvent>,
    /// Scripts that were started and not stopped since (each runs once per `start`).
    pub(crate) running: HashMap<InstanceId, RunningScript>,
    /// Where the world's change journal was when scripts were last looked for.
    cursor: Option<ChangeCursor>,
    /// Resume budget of both VMs (see [`ScriptVm::set_resume_budget`]).
    resume_budget: Duration,
    /// Step budget of both VMs (see [`ScriptVm::set_step_budget_sec`]).
    step_budget_sec: f64,
    /// `dispatch_input_now` already delivered this frame's action events.
    input_dispatched: bool,
    /// Debugger breakpoints by script and line; they apply to every start.
    pub(crate) breakpoints: BTreeMap<InstanceId, BTreeMap<u32, Breakpoint>>,
    /// The debugger pauses at errors no `pcall` handles.
    pub(crate) break_on_error: bool,
    /// Paused by [`Debugger::pause`] while no script was running.
    pub(crate) manual_pause: bool,
    /// Set once [`Self::debugger`] is first used: until then nothing can pause, and
    /// [`Self::is_paused`] is one load (a server or test host never asks for a debugger).
    debugging: bool,
    /// Scripts found while paused (or after a start paused): started once the game runs.
    pending_starts: Vec<(InstanceId, Context, String, String)>,
    /// GUI events for the client scripts, delivered by the next step.
    gui_events: Vec<GuiEvent>,
}

impl ScriptHost {
    /// Creates a combined server/client host that is not running yet.
    /// Output of all scripts goes to `output`; [`Self::start`] creates the local player.
    pub fn new(world: Rc<RefCell<World>>, output: Rc<dyn OutputSink>) -> ScriptHost {
        Self::with_role(world, output, ScriptRole::Combined)
    }

    /// Creates a stopped host with a fixed execution role.
    /// Client hosts create no instances and wait for [`Self::set_local_player`] before
    /// running LocalScripts. Player registration is separate from local binding.
    pub fn with_role(
        world: Rc<RefCell<World>>,
        output: Rc<dyn OutputSink>,
        role: ScriptRole,
    ) -> ScriptHost {
        ScriptHost {
            world,
            output,
            shared: Rc::new(HostShared::default()),
            server: None,
            client: None,
            role,
            players: HashSet::new(),
            client_scope: None,
            inputs: Vec::new(),
            running: HashMap::new(),
            cursor: None,
            resume_budget: initial_resume_budget(),
            step_budget_sec: initial_step_budget_sec(),
            input_dispatched: false,
            breakpoints: BTreeMap::new(),
            break_on_error: false,
            manual_pause: false,
            debugging: false,
            pending_starts: Vec::new(),
            gui_events: Vec::new(),
        }
    }

    /// Creates a fixed server-only host that is not running yet.
    ///
    /// [`Self::start`] creates only the server VM and runs eligible `Script`s. It creates no
    /// local player, PlayerGui or synthetic PlayerAdded event; existing world instances remain.
    /// `LocalScript`s do not run, including those created or moved while the host is running.
    /// Events, waits, budgets and stop/restart use the same host lifecycle as [`Self::new`].
    ///
    /// This constructor adds no networking, player admission or dedicated-server process.
    /// Client-targeted remotes have no receiver and are discarded on the next delivery step.
    pub fn new_server(world: Rc<RefCell<World>>, output: Rc<dyn OutputSink>) -> ScriptHost {
        Self::with_role(world, output, ScriptRole::Server)
    }

    /// The fixed execution role selected at construction.
    pub fn role(&self) -> ScriptRole {
        self.role
    }

    /// Registers an existing, live Player directly under the trusted Players service.
    /// Returns false for an already registered player. PlayerAdded is delivered on the
    /// next step (or after the next start); GetPlayers includes it when that event fires.
    /// No Player, GUI or Character is created, and no backend identity is interpreted.
    ///
    /// # Errors
    /// Returns an error for a busy World, stale/non-Player handle or invalid parent.
    pub fn add_player(&mut self, player: InstanceId) -> Result<bool, ScriptError> {
        self.validate_player(player)?;
        if !self.players.insert(player) {
            return Ok(false);
        }
        if self.is_running() {
            self.inputs.push(HostEvent::PlayerAdded { player });
        }
        Ok(true)
    }

    /// Removes a registered player from this host without destroying World instances.
    /// Returns false for an unknown/already removed handle. A pending PlayerAdded is
    /// delivered before PlayerRemoving in an active session; the player remains in
    /// GetPlayers during immediate Removing dispatch. Deferred handlers observe later roster
    /// state. A stale registered handle is forgotten without callbacks.
    ///
    /// The caller destroys the Player/Character after this call and before the next step.
    /// Signal callbacks use the existing budgets; yielded/deferred handlers are not promised
    /// to finish before destruction. Local client-owned work stops when its binding leaves.
    ///
    /// # Errors
    /// A busy World returns an error without changing registration or pending events.
    pub fn remove_player(&mut self, player: InstanceId) -> Result<bool, ScriptError> {
        if !self.players.contains(&player) {
            return Ok(false);
        }
        let players = {
            let world = self
                .world
                .try_borrow()
                .map_err(|_| host_error("the world is busy"))?;
            valid_player(&world, player)
                .then(|| world.root("Players"))
                .flatten()
        };
        self.prune_client_scripts()?;
        self.inputs.retain(|event| {
            !matches!(event,
            HostEvent::PlayerAdded { player: p } | HostEvent::CharacterAdded { player: p, .. }
                if *p == player)
        });
        if self.is_running()
            && let Some(players) = players
        {
            let _dispatch_budgets: Vec<_> = self
                .sides()
                .into_iter()
                .map(|side| side.state.dispatch_budget())
                .collect();
            self.deliver_player_added(player);
            self.fire_instance_event(players, "PlayerRemoving", Some(player));
        }
        self.players.remove(&player);
        self.shared.joined.borrow_mut().remove(&player);
        if self.shared.local_player.get() == Some(player) {
            self.shared.local_player.set(None);
            self.stop_client_scripts();
            self.client_scope = None;
            self.cursor = None;
            self.shared.input.clear();
            self.gui_events.clear();
            self.shared.remotes.borrow_mut().clear();
            self.shared.queued.set([(0, 0); 2]);
            self.input_dispatched = false;
        }
        Ok(true)
    }

    /// Binds a Client host to an existing live Player; returns false for the same binding.
    /// Eligible LocalScripts start on the next step, or during start when bound beforehand.
    /// Rebinding cancels the old client scripts, tasks, connections and queued remotes, action input and GUI events.
    /// It does not register a PlayerAdded event; use [`Self::add_player`] for roster changes.
    ///
    /// # Errors
    /// Rejects non-Client roles, busy Worlds and stale/non-Player/improperly parented handles.
    pub fn set_local_player(&mut self, player: InstanceId) -> Result<bool, ScriptError> {
        if self.role != ScriptRole::Client {
            return Err(host_error(
                "only a Client host accepts a local-player binding",
            ));
        }
        self.validate_player(player)?;
        if self.shared.local_player.get() == Some(player) {
            return Ok(false);
        }
        self.stop_client_scripts();
        self.shared.local_player.set(Some(player));
        self.client_scope = None;
        self.cursor = None;
        self.shared.remotes.borrow_mut().clear();
        self.shared.queued.set([(0, 0); 2]);
        self.shared.input.clear();
        self.gui_events.clear();
        self.input_dispatched = false;
        Ok(true)
    }

    fn validate_player(&self, player: InstanceId) -> Result<(), ScriptError> {
        let world = self
            .world
            .try_borrow()
            .map_err(|_| host_error("the world is busy"))?;
        if !valid_player(&world, player) {
            return Err(host_error("expected a live Player directly under Players"));
        }
        Ok(())
    }

    /// Sets the resume budget of active VMs, now and after the next `start`
    /// ([`crate::RESUME_BUDGET`] by default).
    pub fn set_resume_budget(&mut self, budget: Duration) {
        self.resume_budget = budget;
        for side in [&self.server, &self.client].into_iter().flatten() {
            side.vm.set_resume_budget(budget);
        }
    }

    /// Sets the step budget of active VMs, now and after the next `start`
    /// ([`crate::STEP_BUDGET_SEC`] by default).
    pub fn set_step_budget_sec(&mut self, seconds: f64) {
        self.step_budget_sec = seconds;
        for side in [&self.server, &self.client].into_iter().flatten() {
            side.vm.set_step_budget_sec(seconds);
        }
    }

    /// Reports the size of the viewport the game is drawn in, in pixels, to the camera
    /// methods (`ViewportSize`, `ViewportPointToRay`, `WorldToViewportPoint`, ...). Until it is
    /// called they assume 1280 x 720. Non-positive or non-finite sizes are ignored.
    pub fn set_viewport_size(&self, width: f64, height: f64) {
        if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            self.shared.viewport.set([width, height]);
        }
    }

    /// Reports the GUI inset (the strip at the top that ScreenGuis leave free) in pixels, for
    /// `ScreenPointToRay` and `WorldToScreenPoint`.
    pub fn set_gui_inset(&self, pixels: f64) {
        self.shared.gui_inset.set(if pixels.is_finite() {
            pixels.max(0.0)
        } else {
            0.0
        });
    }
    /// Makes a step count as out of time after `limit` handler calls (`None`: only the clock
    /// decides, the default). A throttle for hosts that want to spread a burst of events, and
    /// the hook the tests use to make a step run out of time on any machine: handlers past the
    /// limit are deferred, and lifecycle events (tag and attribute signals) wait in the log for
    /// the next step.
    pub fn set_step_handler_limit(&mut self, limit: Option<usize>) {
        self.shared.step_handler_limit.set(limit);
    }

    /// True between `start` and `stop`.
    pub fn is_running(&self) -> bool {
        self.server.is_some() || self.client.is_some()
    }

    /// The script debugger: breakpoints, pause and step, and the paused frames.
    pub fn debugger(&mut self) -> Debugger<'_> {
        self.debugging = true;
        Debugger { host: self }
    }

    /// True while the debugger has the game paused: [`Self::step`] then runs nothing and
    /// the runtime stops physics and characters too.
    pub fn is_paused(&self) -> bool {
        // No allocation: the runtime asks every tick.
        if !self.debugging {
            return false;
        }
        self.manual_pause
            || self
                .server
                .iter()
                .chain(self.client.iter())
                .any(|side| side.vm.sched().debug.is_paused())
    }

    /// In the default combined host, creates local player `Player1` (UserId 0, with a
    /// `PlayerGui`), starts both VMs and runs every enabled script in tree order: `Script`s
    /// under Workspace and ServerScriptService in the server VM, `LocalScript`s under StarterPlayerScripts and
    /// a `PlayerGui` in the client VM. A script that errors is reported to the output sink
    /// and the others still run. `PlayerAdded` fires for `Player1` on the first `step`.
    /// A [`Self::new_server`] host starts only the server side, without a synthetic player or
    /// synthetic PlayerAdded, and excludes client scripts before recording them as running.
    /// Client hosts start only the client VM; LocalScripts wait for an explicit valid binding.
    /// Registered live players receive one queued PlayerAdded in World child order per session.
    ///
    /// Scripts cloned into those places later (for example StarterGui contents copied
    /// into the `PlayerGui` when the character spawns) start on the next `step`.
    /// Calling `start` while running does nothing.
    ///
    /// # Errors
    /// Returns an error if a combined host has no `Players` service or a VM cannot be set up.
    pub fn start(&mut self) -> Result<(), ScriptError> {
        if self.is_running() {
            return Ok(());
        }
        let player = match self.role {
            ScriptRole::Combined => Some(self.ensure_local_player()?),
            ScriptRole::Server => None,
            ScriptRole::Client => {
                let world = self
                    .world
                    .try_borrow()
                    .map_err(|_| host_error("the world is busy"))?;
                self.shared
                    .local_player
                    .get()
                    .filter(|&p| valid_player(&world, p))
            }
        };
        if self.role == ScriptRole::Combined
            && let Some(player) = player
        {
            self.players.insert(player);
        }
        let joined = {
            let world = self
                .world
                .try_borrow()
                .map_err(|_| host_error("the world is busy"))?;
            self.players.retain(|&p| valid_player(&world, p));
            world.root("Players").map_or_else(Vec::new, |root| {
                world
                    .children(root)
                    .iter()
                    .copied()
                    .filter(|p| self.players.contains(p))
                    .collect()
            })
        };
        self.shared.local_player.set(player);
        self.shared.joined.borrow_mut().clear();
        self.shared.remotes.borrow_mut().clear();
        self.shared.queued.set([(0, 0); 2]);
        self.shared.input.clear();
        self.input_dispatched = false;
        self.inputs = joined
            .into_iter()
            .map(|player| HostEvent::PlayerAdded { player })
            .collect();
        self.running.clear();
        self.cursor = None;
        self.client_scope = None;

        let server = (self.role != ScriptRole::Client)
            .then(|| self.make_side(Context::Server))
            .transpose()?;
        let client = (self.role != ScriptRole::Server)
            .then(|| self.make_side(Context::Client))
            .transpose()?;
        self.server = server;
        self.client = client;
        self.sync_scripts();
        Ok(())
    }

    /// Advances the scripts to time `now` (seconds). In this order: starts scripts that
    /// appeared since the last step, delivers the queued remotes and inputs and the signals core
    /// code fired through [`World::fire`], fires the property-changed signals (`Changed`,
    /// `GetPropertyChangedSignal`) for changes since the last step, fires
    /// `RunService.Heartbeat(dt)`, then resumes `WaitForChild` and `task.wait` threads.
    /// Does nothing before `start`.
    /// Client hosts postpone the whole step if the World cannot be read for ownership
    /// validation, preserving queued work and any held frame deadline for a later retry.
    pub fn step(&mut self, now: f64, dt: f64) {
        if !self.is_running() || self.is_paused() {
            return;
        }
        if self.role == ScriptRole::Client && self.world.try_borrow().is_err() {
            return;
        }
        for side in self.sides() {
            side.state.now.set(now);
            side.state.begin_step();
        }
        self.sync_scripts();
        for side in self.sides() {
            side.state.run_deferred();
        }
        if self.is_paused() {
            return;
        }
        // Remotes first: one fired by a handler in this step waits for the next one.
        self.deliver_remotes();
        self.deliver_inputs();
        self.deliver_input_actions();
        if self.is_paused() {
            return;
        }
        for side in self.sides() {
            side.state.deliver_attribute_changes();
            side.state.deliver_tag_changes();
        }
        // Every reader of the attribute log is past what it delivered: drop those entries.
        let cursors: Vec<_> = self
            .sides()
            .iter()
            .map(|s| s.state.attribute_cursor())
            .collect();
        let tag_cursors: Vec<_> = self.sides().iter().map(|s| s.state.tag_cursor()).collect();
        if let Ok(mut world) = self.world.try_borrow_mut() {
            world.trim_attribute_log(&cursors);
            world.trim_tag_log(&tag_cursors);
        }
        self.deliver_gui_events();
        // Property changes since the last step, Rust and script writes alike. Changes made by
        // these handlers (or by Heartbeat below) fire on the next step.
        for side in self.sides() {
            side.state.deliver_property_changes();
        }

        let run_service = self
            .world
            .try_borrow()
            .ok()
            .and_then(|w| w.root("RunService"));
        if let Some(run_service) = run_service {
            for side in self.sides() {
                if let Some(key) = side.state.signal_key(run_service, "Heartbeat") {
                    side.state.fire(key, &|_| Ok(vec![Value::Number(dt)]));
                }
            }
        }
        for side in self.sides() {
            side.state.wake_waiters();
            side.state.prune_hub();
            side.state.end_step();
        }
        if self.is_paused() {
            return;
        }
        for side in self.sides_mut() {
            side.vm.step(now);
        }
    }

    /// Shuts active VMs down, dropping every connection and sleeping thread. `start` may be
    /// called again afterwards; scripts then run again from the beginning.
    /// Retains registered players and Client binding, validating them against the World on
    /// restart. It never destroys Player/Character instances or binds replacements by name.
    pub fn stop(&mut self) {
        for side in [self.server.take(), self.client.take()]
            .into_iter()
            .flatten()
        {
            side.state.unpublish_signals();
            side.vm.shutdown();
        }
        self.inputs.clear();
        self.shared.remotes.borrow_mut().clear();
        self.shared.queued.set([(0, 0); 2]);
        self.shared.joined.borrow_mut().clear();
        self.shared.input.clear();
        self.input_dispatched = false;
        self.running.clear();
        self.cursor = None;
        self.manual_pause = false;
        self.pending_starts.clear();
        self.client_scope = None;
        self.gui_events.clear();
    }

    /// Queues an event of an `InputAction` instance for the client scripts: the next `step`
    /// delivers `Pressed`, `Released`, `Triggered` or `StateChanged` and the handlers of
    /// `ContextActionService`, oldest event time first, at most
    /// [`MAX_ACTION_EVENTS_PER_STEP`](crate::MAX_ACTION_EVENTS_PER_STEP) per step.
    /// Server-only hosts have no receiver and discard these events at the next step.
    pub fn input_action_event(&mut self, event: InputActionEvent) {
        self.shared.input.push_event(event);
    }

    /// Sets what `InputAction:GetState()` and `GetHoldProgress()` return from now on (the
    /// values as of the start of the frame). `hold_progress` is 0 to 1.
    pub fn set_action_state(&mut self, action: InstanceId, value: InputValue, hold_progress: f32) {
        let mut states = self.shared.input.states.borrow_mut();
        // The host reports every action every frame, so dropping the map loses nothing.
        if states.len() >= 4096 && !states.contains_key(&action) {
            states.clear();
        }
        states.insert(action, (value, hold_progress));
    }

    /// Sets `UserInputService.TouchEnabled`, `KeyboardEnabled` and `GamepadEnabled`.
    pub fn set_input_devices(&mut self, touch: bool, keyboard: bool, gamepad: bool) {
        self.shared.input.set_devices(touch, keyboard, gamepad);
    }

    /// Sets the touch overlay's buttons that `ContextActionService:GetButton` returns. Call it
    /// when the overlay places its buttons again or shows or hides; unchanged lists cost a
    /// comparison only.
    pub fn set_touch_buttons(&mut self, buttons: &[TouchButtonInfo]) {
        let mut mine = self.shared.input.touch_buttons.borrow_mut();
        if mine.as_slice() != buttons {
            mine.clear();
            mine.extend_from_slice(buttons);
        }
    }

    /// Sets `UserInputService.PreferredInput`.
    pub fn set_preferred_input(&mut self, device: InputDevice) {
        self.shared.input.set_preferred(device);
    }

    /// Sets what `UserInputService:GetLastInputType()` returns, by the name of an
    /// `Enum.UserInputType` item. When it changes, `LastInputTypeChanged` fires on the next
    /// `step`. Returns false for an unknown name.
    pub fn set_last_input_type(&mut self, name: &str) -> bool {
        self.shared.input.set_last_input_type(name)
    }

    /// Takes what the `ContextActionService` handlers answered since the last call.
    pub fn take_bound_results(&mut self) -> Vec<BoundResult> {
        std::mem::take(&mut *self.shared.input.results.borrow_mut())
    }

    /// Like [`ScriptHost::take_bound_results`], but hands each result to `f` and keeps the
    /// buffer, so a frame loop allocates nothing.
    pub fn drain_bound_results(&mut self, mut f: impl FnMut(BoundResult)) {
        for r in self.shared.input.results.borrow_mut().drain(..) {
            f(r);
        }
    }

    /// How many queued action events were dropped because the queue was full.
    pub fn dropped_action_events(&self) -> u64 {
        self.shared.input.dropped.get()
    }

    /// Queues `part.Touched(hit)` for the next `step`.
    pub fn touched(&mut self, part: InstanceId, hit: InstanceId) {
        self.inputs.push(HostEvent::Touched { part, hit });
    }

    /// Queues `part.TouchEnded(hit)` for the next `step`. It fires while `part` exists, also
    /// when `hit` has been destroyed by then.
    pub fn touch_ended(&mut self, part: InstanceId, hit: InstanceId) {
        self.inputs.push(HostEvent::TouchEnded { part, hit });
    }

    /// Queues `Humanoid.Died` for the next `step`.
    pub fn humanoid_died(&mut self, humanoid: InstanceId) {
        self.inputs.push(HostEvent::Died { humanoid });
    }

    /// Sets `Player.Character` to `model` and queues `Player.CharacterAdded(model)` for
    /// the next `step`.
    pub fn character_added(&mut self, player: InstanceId, model: InstanceId) {
        if let Ok(mut world) = self.world.try_borrow_mut() {
            // Fails only for a destroyed player, whose event is dropped on delivery.
            let _ = world.set(player, "Character", CoreValue::Ref(Some(model)));
        }
        self.inputs
            .push(HostEvent::CharacterAdded { player, model });
    }

    /// How many times signal handler arguments were built so far (both VMs). A test hook
    /// that shows the dispatch cost stays bounded when handlers cannot keep up.
    #[doc(hidden)]
    pub fn dispatch_args_built(&self) -> u64 {
        self.sides().iter().map(|s| s.state.args_built()).sum()
    }

    /// How many threads wait in `signal:Wait()` right now (both VMs). A test hook that shows
    /// stopped scripts and destroyed instances leave no waiter behind.
    #[doc(hidden)]
    pub fn signal_waiters(&self) -> usize {
        self.sides().iter().map(|s| s.state.signal_waiters()).sum()
    }

    /// How many world-journal entries property-changed delivery has read so far (both VMs).
    /// A test hook: it stays 0 while no script listens to any instance's `Changed`.
    #[doc(hidden)]
    pub fn property_changes_scanned(&self) -> u64 {
        self.sides()
            .iter()
            .map(|s| s.state.property_changes_scanned())
            .sum()
    }

    pub(crate) fn sides(&self) -> Vec<&Side> {
        self.server.iter().chain(self.client.iter()).collect()
    }

    fn sides_mut(&mut self) -> Vec<&mut Side> {
        self.server
            .iter_mut()
            .chain(self.client.iter_mut())
            .collect()
    }

    fn make_side(&self, context: Context) -> Result<Side, ScriptError> {
        let vm = ScriptVm::with_output(context, self.output.clone())?;
        vm.set_resume_budget(self.resume_budget);
        vm.set_step_budget_sec(self.step_budget_sec);
        let state = api::install(
            &vm,
            self.world.clone(),
            self.shared.clone(),
            self.output.clone(),
        )?;
        let side = Side { vm, state };
        self.push_breakpoints(&side, None);
        Ok(side)
    }

    fn side_for(&mut self, context: Context) -> Option<&mut Side> {
        match context {
            Context::Server => self.server.as_mut(),
            Context::Client => self.client.as_mut(),
        }
    }

    fn runnable_context(&self, world: &World, id: InstanceId) -> Option<Context> {
        match self.role {
            ScriptRole::Client => client_script_context(world, id, self.shared.local_player.get()?),
            ScriptRole::Server => script_context(world, id).filter(|c| *c == Context::Server),
            ScriptRole::Combined => script_context(world, id).filter(|context| {
                *context == Context::Server || self.shared.local_player.get().is_some()
            }),
        }
    }

    fn stop_client_scripts(&mut self) {
        let ids: Vec<_> = self
            .running
            .iter()
            .filter_map(|(&id, running)| (running.context == Context::Client).then_some(id))
            .collect();
        for id in ids {
            self.running.remove(&id);
            if let Some(client) = &self.client {
                client.state.stop_owner(owner_id(id));
            }
        }
    }

    /// Stops invalid Client owners before public immediate dispatch paths. It neither
    /// starts scripts nor advances the journal cursor: the next normal step must still
    /// observe every new script/change. Owner cancellation does not call Lua handlers.
    fn prune_client_scripts(&mut self) -> Result<(), ScriptError> {
        if self.role != ScriptRole::Client || self.client.is_none() {
            return Ok(());
        }
        self.refresh_client_scope();
        let stops: Vec<_> = {
            let world = self
                .world
                .try_borrow()
                .map_err(|_| host_error("the world is busy"))?;
            let changed = self
                .cursor
                .and_then(|cursor| match world.changes_since(cursor).0 {
                    Changes::Ids(ids) => Some(ids),
                    Changes::Overflow => None,
                });
            let candidates = changed.unwrap_or_else(|| self.running.keys().copied().collect());
            candidates
                .into_iter()
                .filter(|id| {
                    self.running
                        .get(id)
                        .is_some_and(|script| script.context == Context::Client)
                        && self.runnable_context(&world, *id) != Some(Context::Client)
                })
                .collect()
        };
        for id in stops {
            self.running.remove(&id);
            if let Some(client) = &self.client {
                client.state.stop_owner(owner_id(id));
            }
        }
        Ok(())
    }

    fn refresh_client_scope(&mut self) {
        if self.role != ScriptRole::Client {
            return;
        }
        let scope = {
            let Ok(world) = self.world.try_borrow() else {
                return;
            };
            self.shared
                .local_player
                .get()
                .filter(|&p| valid_player(&world, p))
                .map(|p| {
                    let character = match world.get_ref(p, "Character") {
                        Ok(CoreValue::Ref(Some(c))) if world.is_a(*c, "Model") => Some(*c),
                        _ => None,
                    };
                    (p, character)
                })
        };
        let invalid_binding = scope.is_none() && self.shared.local_player.get().is_some();
        if scope != self.client_scope || invalid_binding {
            if scope.is_none() {
                if let Some(player) = self.shared.local_player.get() {
                    self.players.remove(&player);
                    self.shared.joined.borrow_mut().remove(&player);
                    self.inputs.retain(|event| !matches!(event,
                        HostEvent::PlayerAdded { player: p } | HostEvent::CharacterAdded { player: p, .. }
                            if *p == player));
                    self.shared.remotes.borrow_mut().clear();
                    self.shared.queued.set([(0, 0); 2]);
                    self.shared.input.clear();
                    self.gui_events.clear();
                    self.input_dispatched = false;
                }
                self.shared.local_player.set(None);
            }
            self.client_scope = scope;
            self.cursor = None;
        }
    }

    /// Finds or creates `Players.Player1` and its `PlayerGui`.
    fn ensure_local_player(&self) -> Result<InstanceId, ScriptError> {
        let failed = |what: &str| ScriptError {
            message: format!("cannot create the local player: {what}"),
        };
        let mut world = self
            .world
            .try_borrow_mut()
            .map_err(|_| failed("the world is busy"))?;
        let players = world
            .root("Players")
            .ok_or_else(|| failed("the world has no Players service"))?;
        let existing = world.find_first_child(players, LOCAL_PLAYER_NAME);
        let player = match existing {
            Some(p) if world.is_a(p, "Player") => p,
            _ => {
                let p = world
                    .create("Player", Some(players))
                    .map_err(|e| failed(&e.to_string()))?;
                world
                    .set(p, "Name", CoreValue::String(LOCAL_PLAYER_NAME.to_string()))
                    .map_err(|e| failed(&e.to_string()))?;
                p
            }
        };
        let has_gui = world
            .children(player)
            .iter()
            .any(|&c| world.is_a(c, "PlayerGui"));
        if !has_gui {
            world
                .create("PlayerGui", Some(player))
                .map_err(|e| failed(&e.to_string()))?;
        }
        Ok(player)
    }

    /// Starts scripts that exist now and are not running, and stops running scripts that
    /// were destroyed, disabled or moved out of a place where scripts run. Looks at the whole
    /// tree the first time and only at changed instances afterwards (the world's journal).
    fn sync_scripts(&mut self) {
        self.refresh_client_scope();
        let (starts, stops) = {
            let Ok(world) = self.world.try_borrow() else {
                return;
            };
            let changed = match self.cursor {
                Some(cursor) => match world.changes_since(cursor).0 {
                    Changes::Ids(ids) => Some(ids),
                    Changes::Overflow => None,
                },
                None => None,
            };
            self.cursor = Some(world.change_cursor());
            let (candidates, stop_candidates): (Vec<InstanceId>, Vec<InstanceId>) = match changed {
                Some(ids) => {
                    let running = ids
                        .iter()
                        .copied()
                        .filter(|id| self.running.contains_key(id))
                        .collect();
                    (ids, running)
                }
                None => {
                    let mut all = Vec::new();
                    for &root in world.roots() {
                        collect_tree(&world, root, &mut all);
                    }
                    (all, self.running.keys().copied().collect())
                }
            };
            let stops: Vec<InstanceId> = stop_candidates
                .into_iter()
                .filter(|&id| self.runnable_context(&world, id).is_none())
                .collect();
            let mut starts = Vec::new();
            for id in candidates {
                if self.running.contains_key(&id) {
                    continue;
                }
                if let Some(context) = self.runnable_context(&world, id) {
                    starts.push((id, context, world.full_name(id), source_of(&world, id)));
                }
            }
            (starts, stops)
        };
        for id in stops {
            let Some(script) = self.running.remove(&id) else {
                continue;
            };
            if let Some(side) = self.side_for(script.context) {
                side.state.stop_owner(owner_id(id));
            }
        }
        let mut starts = starts;
        if !self.pending_starts.is_empty() {
            let mut all = std::mem::take(&mut self.pending_starts);
            all.extend(starts);
            starts = all;
        }
        for (id, context, chunk_name, source) in starts {
            if self.running.contains_key(&id) {
                continue;
            }
            if self.is_paused() {
                // A script start paused in the debugger: the rest start once it runs.
                self.pending_starts.push((id, context, chunk_name, source));
                continue;
            }
            self.running.insert(id, RunningScript { context });
            let Some(side) = self.side_for(context) else {
                continue;
            };
            let result = side
                .state
                .wrap(side.vm.lua(), id)
                .map_err(|e| ScriptError {
                    message: crate::vm::format_error(&e),
                })
                .and_then(|script| {
                    side.vm.run_script_owned(
                        &chunk_name,
                        &source,
                        vec![("script", Value::UserData(script))],
                        owner_id(id),
                    )
                });
            if let Err(e) = result {
                self.output.error(&e.message);
            }
        }
    }

    /// Queues an event of a GUI element for the client scripts; the next `step` fires it, in
    /// report order. At most [`MAX_GUI_EVENTS_PER_STEP`] wait at once (more are dropped), and
    /// server-only hosts discard them. Client binding changes discard pending GUI events;
    /// a no-op binding retains valid queued input.
    pub fn gui_event(&mut self, event: GuiEvent) {
        if self.role != ScriptRole::Server && self.gui_events.len() < MAX_GUI_EVENTS_PER_STEP {
            self.gui_events.push(event);
        }
    }

    fn deliver_gui_events(&mut self) {
        if let Some(client) = &self.client {
            for event in &self.gui_events {
                client.state.deliver_gui_event(event);
            }
        }
        // Keeps the capacity: no allocation per step once grown.
        self.gui_events.clear();
    }

    fn deliver_inputs(&mut self) {
        for input in std::mem::take(&mut self.inputs) {
            match input {
                HostEvent::Touched { part, hit } => {
                    self.fire_instance_event(part, "Touched", Some(hit));
                }
                HostEvent::TouchEnded { part, hit } => {
                    // The touching part may be gone already (its character was destroyed or
                    // replaced before scripts ran): the surviving part still hears the end,
                    // with the destroyed part as `hit`.
                    self.fire_touch_ended(part, hit);
                }
                HostEvent::Died { humanoid } => {
                    self.fire_instance_event(humanoid, "Died", None);
                }
                HostEvent::CharacterAdded { player, model } => {
                    self.fire_instance_event(player, "CharacterAdded", Some(model));
                }
                HostEvent::PlayerAdded { player } => {
                    self.deliver_player_added(player);
                }
            }
        }
        self.deliver_world_signals();
    }

    /// Delivers what core code fired through [`World::fire`] since the last step, to every VM.
    fn deliver_world_signals(&mut self) {
        let (queued, dropped) = match self.world.try_borrow_mut() {
            Ok(mut world) => {
                let signals = world.signals_mut();
                (signals.take(), signals.take_dropped())
            }
            Err(_) => return,
        };
        if dropped > 0 {
            self.output.warn(&format!(
                "{dropped} signal fires were dropped: too many between two steps"
            ));
        }
        for signal in &queued {
            for side in self.sides() {
                side.state.deliver_signal(signal);
            }
        }
    }

    fn deliver_player_added(&self, player: InstanceId) {
        let players = self.world.try_borrow().ok().and_then(|world| {
            (self.players.contains(&player) && valid_player(&world, player))
                .then(|| world.root("Players"))
                .flatten()
        });
        if let Some(players) = players {
            let first = self.shared.joined.borrow_mut().insert(player);
            if first {
                self.fire_instance_event(players, "PlayerAdded", Some(player));
            }
        }
    }

    /// Fires the signal `event` of `source` with an optional instance argument in active VMs.
    /// Dropped when either instance no longer exists, or the class of `source` has no such signal.
    fn fire_instance_event(&self, source: InstanceId, event: &str, arg: Option<InstanceId>) {
        let alive = self
            .world
            .try_borrow()
            .is_ok_and(|w| w.is_alive(source) && arg.is_none_or(|a| w.is_alive(a)));
        if !alive {
            return;
        }
        for side in self.sides() {
            let state = &side.state;
            let Some(key) = state.signal_key(source, event) else {
                continue;
            };
            state.fire(key, &|lua| match arg {
                Some(id) => Ok(vec![Value::UserData(state.wrap(lua, id)?)]),
                None => Ok(Vec::new()),
            });
        }
    }

    /// Fires `part.TouchEnded(hit)` in active VMs while `part` exists, even when `hit` was
    /// destroyed (scripts get the destroyed instance; reading its members errors).
    fn fire_touch_ended(&self, part: InstanceId, hit: InstanceId) {
        let alive = self.world.try_borrow().is_ok_and(|w| w.is_alive(part));
        if !alive {
            return;
        }
        for side in self.sides() {
            let state = &side.state;
            let Some(key) = state.signal_key(part, "TouchEnded") else {
                continue;
            };
            state.fire(key, &|lua| Ok(vec![Value::UserData(state.wrap(lua, hit)?)]));
        }
    }

    fn deliver_input_actions(&mut self) {
        if self.role == ScriptRole::Server {
            self.shared.input.events.borrow_mut().clear();
        }
        let changes = std::mem::take(&mut *self.shared.input.type_changes.borrow_mut());
        if !changes.is_empty() {
            for side in self.sides() {
                side.state.deliver_input_type_changes(&changes);
            }
        }
        if !self.input_dispatched
            && let Some(client) = &self.client
        {
            client.state.deliver_action_events();
        }
    }

    /// Delivers the queued action events to the client scripts now (at most
    /// [`MAX_ACTION_EVENTS_PER_STEP`](crate::MAX_ACTION_EVENTS_PER_STEP)), so the answers of
    /// `ContextActionService` handlers are known before the character reads its input. The
    /// `step` of the same frame then delivers no more action events and continues the time
    /// budget started here; call [`ScriptHost::end_frame`] when the frame is done.
    /// Client hosts first prune owners that lost eligibility, without starting new scripts
    /// or consuming journal changes. A borrowed World postpones Client execution until its
    /// scope can be validated; existing queued work is retained.
    pub fn dispatch_input_now(&mut self, now: f64) {
        if self.is_paused() {
            return;
        }
        if self.prune_client_scripts().is_err() {
            // Without a readable World the Client scope cannot be validated. Leave
            // queued work for a later dispatch/step instead of running stale owners.
            return;
        }
        let Some(client) = &self.client else { return };
        client.state.now.set(now);
        client.state.restart_step();
        client.state.run_deferred();
        client.state.deliver_action_events();
        client.state.hold_step_deadline();
        self.input_dispatched = true;
    }

    /// Ends a frame that used [`ScriptHost::dispatch_input_now`].
    pub fn end_frame(&mut self) {
        self.input_dispatched = false;
    }

    fn deliver_remotes(&mut self) {
        let queued = std::mem::take(&mut *self.shared.remotes.borrow_mut());
        self.shared.queued.set([(0, 0); 2]);
        for message in queued {
            let side = match message.target {
                RemoteTarget::Server { .. } => self.server.as_ref(),
                RemoteTarget::Client { player } => {
                    let local = self.shared.local_player.get();
                    if player.is_some() && player != local {
                        continue;
                    }
                    self.client.as_ref()
                }
            };
            if let Some(side) = side {
                side.state.deliver_remote(&message);
            }
        }
    }
}

fn host_error(message: &str) -> ScriptError {
    ScriptError {
        message: format!("script host: {message}"),
    }
}

fn valid_player(world: &World, player: InstanceId) -> bool {
    world.is_a(player, "Player")
        && world.root("Players").is_some_and(|players| {
            world.is_builtin(players) && world.parent(player) == Some(players)
        })
}

/// A Client's script must be in its own GUI/scripts/Character, or the existing starter
/// template path. Inspect every ancestor so an outer private boundary cannot be bypassed.
fn client_script_context(world: &World, id: InstanceId, player: InstanceId) -> Option<Context> {
    if !valid_player(world, player)
        || world.class_name(id) != Some("LocalScript")
        || !matches!(world.get_ref(id, "Enabled"), Ok(CoreValue::Bool(true)))
    {
        return None;
    }
    let character = match world.get_ref(player, "Character") {
        Ok(CoreValue::Ref(Some(c))) if world.is_a(*c, "Model") => Some(*c),
        _ => None,
    };
    let mut owned = false;
    let mut starter = false;
    let mut current = world.parent(id);
    while let Some(node) = current {
        let class = world.class_name(node)?;
        if matches!(class, "ServerStorage" | "ServerScriptService")
            || (class == "Player" && node != player)
        {
            return None;
        }
        if matches!(class, "PlayerGui" | "PlayerScripts") {
            let mut ancestor = world.parent(node);
            let mut owner = None;
            while let Some(parent) = ancestor {
                if world.is_a(parent, "Player") {
                    owner = Some(parent);
                    break;
                }
                ancestor = world.parent(parent);
            }
            if owner != Some(player) {
                return None;
            }
            owned = true;
        }
        owned |= character == Some(node);
        starter |= class == "StarterPlayerScripts";
        current = world.parent(node);
        if current.is_none() {
            return (world.is_builtin(node)
                && ((owned && matches!(class, "Workspace" | "Players"))
                    || (starter && class == "StarterPlayer")))
                .then_some(Context::Client);
        }
    }
    None
}

/// All instances below `id` (and `id`), parents first, in tree order.
fn collect_tree(world: &World, id: InstanceId, out: &mut Vec<InstanceId>) {
    out.push(id);
    for &child in world.children(id) {
        collect_tree(world, child, out);
    }
}

fn source_of(world: &World, id: InstanceId) -> String {
    match world.get(id, "Source") {
        Ok(CoreValue::String(s)) => s,
        _ => String::new(),
    }
}

/// Which VM runs `id`, if it is an enabled script in a place where scripts run.
fn script_context(world: &World, id: InstanceId) -> Option<Context> {
    let class = world.class_name(id)?;
    if !matches!(class, "Script" | "LocalScript") {
        return None;
    }
    if !matches!(world.get(id, "Enabled"), Ok(CoreValue::Bool(true))) {
        return None;
    }
    let mut cur = world.parent(id);
    match class {
        "Script" => {
            while let Some(a) = cur {
                if matches!(
                    world.class_name(a),
                    Some("Workspace" | "ServerScriptService")
                ) {
                    return Some(Context::Server);
                }
                cur = world.parent(a);
            }
            None
        }
        "LocalScript" => {
            while let Some(a) = cur {
                if matches!(
                    world.class_name(a),
                    Some("StarterPlayerScripts" | "PlayerGui")
                ) {
                    return Some(Context::Client);
                }
                cur = world.parent(a);
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod role_tests {
    use super::*;

    struct Quiet;
    impl OutputSink for Quiet {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    #[test]
    fn server_role_has_one_vm_and_never_records_client_scripts_as_running() {
        let mut world = World::new_with_services();
        let starter = world
            .find_first_child(world.root("StarterPlayer").unwrap(), "StarterPlayerScripts")
            .unwrap();
        let local = world.create("LocalScript", Some(starter)).unwrap();
        let replicated = world.root("ReplicatedStorage").unwrap();
        let action = world.create("InputAction", Some(replicated)).unwrap();
        let remote = world.create("RemoteEvent", Some(replicated)).unwrap();
        world
            .set(remote, "Name", CoreValue::String("Notify".into()))
            .unwrap();
        let server = world
            .create("Script", world.root("ServerScriptService"))
            .unwrap();
        world
            .set(
                server,
                "Source",
                CoreValue::String(
                    "game:GetService('ReplicatedStorage').Notify:FireAllClients('ignored')".into(),
                ),
            )
            .unwrap();
        let world = Rc::new(RefCell::new(world));
        let mut host = ScriptHost::new_server(world.clone(), Rc::new(Quiet));
        host.start().unwrap();
        assert!(host.server.is_some() && host.client.is_none());
        assert!(host.shared.local_player.get().is_none());
        assert!(host.inputs.is_empty());
        assert_eq!(host.running.len(), 1);
        assert!(host.running.contains_key(&server));
        assert!(!host.running.contains_key(&local));
        assert_eq!(host.shared.remotes.borrow().len(), 1);
        host.input_action_event(InputActionEvent {
            action,
            kind: crate::InputEventKind::Pressed,
            value: InputValue::Bool(true),
            time: 0.0,
        });
        assert_eq!(host.shared.input.events.borrow().len(), 1);
        host.dispatch_input_now(0.0);
        host.step(0.1, 0.1);
        assert!(host.shared.remotes.borrow().is_empty());
        assert_eq!(host.shared.queued.get(), [(0, 0); 2]);
        assert!(host.shared.input.events.borrow().is_empty());

        let late = world
            .borrow_mut()
            .create("LocalScript", Some(starter))
            .unwrap();
        host.step(0.2, 0.1);
        world
            .borrow_mut()
            .set(late, "Enabled", CoreValue::Bool(false))
            .unwrap();
        host.step(0.3, 0.1);
        world
            .borrow_mut()
            .set(late, "Enabled", CoreValue::Bool(true))
            .unwrap();
        host.step(0.4, 0.1);
        assert_eq!(host.running.len(), 1);
        assert!(!host.running.contains_key(&late));
        assert!(host.server.is_some() && host.client.is_none());

        host.stop();
        assert!(host.server.is_none() && host.client.is_none());
        assert!(host.running.is_empty());
        assert!(host.shared.remotes.borrow().is_empty());
    }
}

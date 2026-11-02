//! Own a disposable local Game without sharing the authored world's state, and the Play mode
//! that starts one.

use std::rc::Rc;
use std::sync::Arc;

use eframe::egui;
use metallic_core::World;
use metallic_render::Renderer;
use metallic_runtime::input::{ControlsHint, RawEvent};
use metallic_runtime::{FrameInput, FrameOutput, Game, InputRuntime, ProjectConfig, RuntimeError};

use super::{PlayMode, PlaySession, PlayStart, PlayStatus, PlayView};
use crate::output::{OutputSink, ScriptOutput, Tag};

/// The Output tag of every line a local Play session's scripts write (server and client
/// combined: the runtime has one sink for both VMs).
pub const PLAY_TAG: &str = "play";

/// Play (F5): runs the game in this process on a copy of the edit world, client and server
/// combined like the Player, drawn in the Studio viewport. Action and mode id `play.start`.
#[derive(Debug, Default, Clone, Copy)]
pub struct LocalPlayMode;

impl PlayMode for LocalPlayMode {
    fn id(&self) -> &'static str {
        "play.start"
    }

    fn label(&self) -> String {
        "Play".into()
    }

    /// Starts a [`LocalPlaySession`]; the game is named after `metallic.toml` (or "Play").
    fn start(&mut self, start: PlayStart<'_>) -> Result<Box<dyn PlaySession>, String> {
        let name =
            ProjectConfig::read(start.project_dir).map_or_else(|_| "Play".into(), |c| c.name);
        LocalPlaySession::start(name, start.world, start.output)
            .map(|s| Box::new(s) as Box<dyn PlaySession>)
            .map_err(|e| e.to_string())
    }
}

// Also guards partial initialization: Game::start may fail after creating VMs.
struct ActiveGame(Game);

impl Drop for ActiveGame {
    fn drop(&mut self) {
        self.0.stop();
    }
}

/// A fresh local Game over an independent snapshot of the authored world.
///
/// Stop and Drop shut down the real VMs and discard the play world. Starting again means
/// constructing a new session, rather than restarting its mutated Game. No mode or action is
/// registered automatically. This foundation supplies no viewport or input-event wiring.
///
/// Copying costs O(world). The existing [`super::PlayStart`] seam already supplies a clean copy
/// with parked history nodes removed; starting from that borrowed copy incurs a second copy.
/// Callers with an [`crate::editor::EditorState`] should supply its `clean_world()` snapshot.
/// Startup's StarterGui copying still uses the independently tracked recursive `clone_subtree`
/// implementation (#92/#275); this wrapper does not establish arbitrary-depth startup safety.
///
/// The session owns its [`InputRuntime`] (built from the snapshot's input contexts) and input
/// clock, so raw events pushed through [`PlayView::push_input`] are evaluated like the Player
/// evaluates them; dropping the session drops both.
pub struct LocalPlaySession {
    game: Option<ActiveGame>,
    input: InputRuntime,
    /// The controls plate, drawn over the game (from this session's `input`, on its clock).
    hint: ControlsHint,
    /// Seconds of input clock since start (the sum of update frame times).
    clock: f64,
}

impl LocalPlaySession {
    /// Copies `snapshot` and starts a fresh Game without reading a scene from disk.
    ///
    /// The Game guard exists before fallible initialization. Errors retain their runtime type;
    /// any partially started VMs are stopped on error or unwinding. Panics are not recovered.
    /// Both server and client messages use the combined [`PLAY_TAG`] tag because the runtime
    /// supplies one output sink for both VMs. Their text and severity are preserved.
    pub fn start(
        name: impl Into<String>,
        snapshot: &World,
        output: OutputSink,
    ) -> Result<Self, RuntimeError> {
        let output = Rc::new(ScriptOutput {
            sink: output,
            tag: Tag::Other(Arc::from(PLAY_TAG)),
        });
        let input = InputRuntime::from_world(snapshot);
        let mut game = ActiveGame(Game::from_world(name.into(), snapshot.deep_clone(), output));
        game.0.start()?;
        Ok(Self {
            game: Some(game),
            input,
            hint: ControlsHint::new(),
            clock: 0.0,
        })
    }

    /// True until this session has been stopped.
    pub fn is_running(&self) -> bool {
        self.game.as_ref().is_some_and(|game| game.0.is_running())
    }

    /// Advances one caller frame with already evaluated input; returns None after Stop.
    ///
    /// This uses Game's existing fixed ticks and elapsed-time clamp. The PlaySession `update`
    /// adapter instead evaluates the raw events queued with [`PlayView::push_input`] (none:
    /// neutral input); use either that adapter or this method once per caller frame, not both.
    pub fn frame(&mut self, input: &FrameInput, dt: f64) -> Option<FrameOutput> {
        self.game.as_mut().map(|game| game.0.frame(input, dt))
    }

    /// Lends the play world read-only for the duration of `inspect`.
    ///
    /// Returns None after Stop or if the world cannot currently be borrowed. No mutable World,
    /// Game or shared world handle is exposed, and a borrowed world reference cannot escape.
    pub fn inspect_world<R>(&self, inspect: impl FnOnce(&World) -> R) -> Option<R> {
        let world = self.game.as_ref()?.0.world();
        let world = world.try_borrow().ok()?;
        Some(inspect(&world))
    }

    /// Stops VMs and releases the play Game. Repeated calls do nothing.
    pub fn stop(&mut self) {
        drop(self.game.take());
    }
}

impl PlaySession for LocalPlaySession {
    /// Evaluates the queued raw input on the session's clock, then advances one frame.
    fn update(&mut self, dt: f64) {
        let Some(game) = self.game.as_mut() else {
            return;
        };
        let dt = if dt.is_finite() { dt.max(0.0) } else { 0.0 };
        self.clock += dt;
        let input = game.0.input_frame(&mut self.input, self.clock, dt as f32);
        game.0.frame(&input, dt);
        self.hint.update(&self.input, self.clock);
    }

    fn status(&self) -> PlayStatus {
        if self.is_running() {
            PlayStatus::Running("Playing".into())
        } else {
            PlayStatus::Ended("Stopped".into())
        }
    }

    fn stop(mut self: Box<Self>) -> Result<(), String> {
        LocalPlaySession::stop(&mut self);
        Ok(())
    }

    fn view(&self) -> Option<&dyn PlayView> {
        Some(self)
    }

    fn view_mut(&mut self) -> Option<&mut dyn PlayView> {
        Some(self)
    }
}

impl PlayView for LocalPlaySession {
    fn input_time(&self) -> f64 {
        self.clock
    }

    fn push_input(&mut self, event: RawEvent) {
        self.input.push(event);
    }

    fn release_input(&mut self) {
        self.input.release_all(self.clock);
    }

    fn render(&self, renderer: &mut Renderer, target: &wgpu::TextureView, size: (u32, u32)) {
        if let Some(game) = &self.game {
            game.0.render(renderer, target, size);
        }
    }

    fn paint_gui(&self, painter: &egui::Painter) {
        if let Some(game) = &self.game {
            game.0.paint_gui(painter);
            self.hint.paint(painter, self.clock);
        }
    }

    fn inspect_world(&self, inspect: &mut dyn FnMut(&World)) {
        let _ = self.inspect_world(|world| inspect(world));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::OutputLog;
    use metallic_runtime::input::DeviceKind;

    fn pending_session() -> LocalPlaySession {
        let mut world = World::new_with_services();
        let parent = world.root("ServerScriptService").unwrap();
        let script = world.create("Script", Some(parent)).unwrap();
        world
            .set(
                script,
                "Source",
                metallic_core::Value::String(
                    "task.delay(10, function() print('late') end); \
                     game:GetService('RunService').Heartbeat:Connect(function() end)"
                        .into(),
                ),
            )
            .unwrap();
        let log = OutputLog::new(8);
        metallic_script::use_test_budgets_on_this_thread();
        LocalPlaySession::start("Ownership", &world, log.sink()).unwrap()
    }

    #[test]
    fn stop_and_drop_release_pending_vm_world_ownership() {
        for explicit_stop in [false, true] {
            let mut session = pending_session();
            let world = session.game.as_ref().unwrap().0.world();
            let weak = Rc::downgrade(&world);
            drop(world);
            if explicit_stop {
                session.stop();
                assert!(weak.upgrade().is_none());
            }
            drop(session);
            assert!(weak.upgrade().is_none());
        }
    }

    #[test]
    fn caller_unwind_drops_the_guard_without_a_recovery_contract() {
        let session = pending_session();
        let world = session.game.as_ref().unwrap().0.world();
        let weak = Rc::downgrade(&world);
        drop(world);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _session = session;
            panic!("test caller unwind");
        }));
        assert!(result.is_err());
        assert!(weak.upgrade().is_none());
    }

    fn parkour() -> World {
        let scene = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../templates/parkour/main.mscene");
        metallic_core::load_scene(&scene).expect("Parkour scene")
    }

    /// True when `session` paints the controls plate now.
    fn paints_plate(session: &LocalPlaySession) -> bool {
        let ctx = egui::Context::default();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            PlayView::paint_gui(session, ui.painter());
        });
        out.textures_delta.clear();
        out.shapes
            .iter()
            .any(|c| matches!(&c.shape, egui::Shape::Text(t) if t.galley.text() == "Jump"))
    }

    /// At the start, gone after 8 s, back after a glyph change, never after Stop (#494).
    #[test]
    fn controls_plate_follows_the_session_clock_and_glyph_changes() {
        let log = OutputLog::new(8);
        let mut session = LocalPlaySession::start("Parkour", &parkour(), log.sink()).unwrap();
        PlaySession::update(&mut session, metallic_runtime::TICK);
        assert_eq!(session.hint.opacity(session.clock), Some(1.0));
        assert!(paints_plate(&session), "at the start");

        PlaySession::update(&mut session, 7.0);
        assert!(paints_plate(&session), "still there before 8 s");
        PlaySession::update(&mut session, 1.5);
        assert!(!paints_plate(&session), "gone after 8 s");
        PlaySession::update(&mut session, metallic_runtime::TICK);
        assert!(!paints_plate(&session), "and it stays gone");

        session.input.set_forced_device(Some(DeviceKind::Gamepad));
        PlaySession::update(&mut session, metallic_runtime::TICK);
        assert!(paints_plate(&session), "back after a glyph change");
        assert_eq!(session.hint.rows()[0].0.text, "Left Stick");
        PlaySession::update(&mut session, 8.5);
        assert!(!paints_plate(&session), "and gone again");

        session
            .input
            .set_forced_device(Some(DeviceKind::KeyboardMouse));
        PlaySession::update(&mut session, metallic_runtime::TICK);
        assert!(paints_plate(&session));
        session.stop();
        assert!(!paints_plate(&session), "hidden when not playing");
    }

    /// Studio's plate has the rows the Player's has: both come from an `InputRuntime` of the same
    /// scene (the Player without overrides, as `--no-overrides` runs).
    #[test]
    fn parkour_rows_are_the_players_rows() {
        let world = parkour();
        let mut player = ControlsHint::new();
        player.update(&InputRuntime::from_world(&world), 0.0);
        let log = OutputLog::new(8);
        let mut session = LocalPlaySession::start("Parkour", &world, log.sink()).unwrap();
        PlaySession::update(&mut session, metallic_runtime::TICK);
        assert_eq!(session.hint.rows(), player.rows());
        let rows: Vec<(&str, &str)> = session
            .hint
            .rows()
            .iter()
            .map(|(g, word)| (g.text.as_str(), *word))
            .collect();
        assert_eq!(
            rows,
            [
                ("W A S D", "Move"),
                ("Space", "Jump"),
                ("Left Shift", "Run"),
                ("Right Click", "Camera"),
                ("Esc", "Menu"),
            ]
        );
    }
}

//! Play as a mode switch.
//!
//! # Seam: adding a play mode
//!
//! A [`PlayMode`] is one way to run the game from the editor: Play (#11), Team Test with N
//! players (multiplayer task A12), and later Play Here. Modes register with the
//! [`PlayController`]; the toolbar shows one entry per mode next to the Stop button, and only one
//! session runs at a time. A mode has three hooks:
//!
//! - **start**: [`PlayMode::start`] gets a [`PlayStart`] (a copy of the edit world without parked
//!   instances, the project folder and an [`OutputSink`]) and returns a running [`PlaySession`].
//!   The edit world is never handed out mutably, so Stop always returns to the exact edit state.
//! - **status**: [`PlaySession::status`] is polled each frame for the toolbar and status bar
//!   ("Running · 3 players"); [`PlaySession::update`] gets the frame time.
//! - **stop**: [`PlaySession::stop`] ends the session and releases its processes or threads.
//! - **view** (optional): a session whose game runs inside Studio returns a [`PlayView`] from
//!   [`PlaySession::view`]; the viewport then draws that game with its camera and GUI, sends it
//!   keyboard and mouse input, and Explorer shows its world read-only. Sessions in other
//!   processes (Team Test) return `None` and keep their own windows.
//!
//! Listeners added with [`PlayController::on_event`] hear [`PlayEvent`]s (started, stopped,
//! failed), for example to clear Output on Play or to switch the Explorer to the play world.
//! While a session runs, [`PlayController::is_playing`] is true and editing commands are disabled.
//!
//! [`LocalPlayMode`] is Play (#11, F5): a [`LocalPlaySession`] runs the real Game in this
//! process, client and server combined like the Player, and is its own [`PlayView`]. Over the game
//! it draws the Player's controls plate ([`metallic_runtime::input::ControlsHint`]).
//! [`input::PlayInput`] turns the editor's egui input into the game's raw events.
pub mod input;
mod local;

pub use local::{LocalPlayMode, LocalPlaySession, PLAY_TAG};

use std::borrow::Cow;
use std::path::Path;

use eframe::egui;
use metallic_core::World;
use metallic_render::Renderer;
use metallic_runtime::input::RawEvent;

use crate::output::{Level, OutputSink, Tag};

/// What a mode gets when it starts.
pub struct PlayStart<'a> {
    /// The edit world as saved (parked instances removed); the mode copies what it needs.
    pub world: &'a World,
    /// The game folder.
    pub project_dir: &'a Path,
    /// Where the session's log lines go.
    pub output: OutputSink,
}

/// Status of a running session, as shown in the toolbar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayStatus {
    /// Still starting (processes launching, world loading).
    Starting,
    /// Running; the text is shown in the status bar ("Playing", "3 players"). Borrowed text
    /// costs nothing to poll each frame.
    Running(Cow<'static, str>),
    /// The session ended on its own (a crash or the game closed); the text says why.
    Ended(Cow<'static, str>),
}

/// One way to run the game.
pub trait PlayMode {
    /// Stable id, also the action id of its toolbar entry (e.g. `play.start`, `play.team_test`).
    fn id(&self) -> &'static str;
    /// Toolbar label ("Play", "Team Test").
    fn label(&self) -> String;
    /// Starts a session.
    fn start(&mut self, start: PlayStart<'_>) -> Result<Box<dyn PlaySession>, String>;
}

/// A running session.
pub trait PlaySession {
    /// Called once per editor frame with the frame time in seconds.
    fn update(&mut self, _dt: f64) {}
    /// Current status.
    fn status(&self) -> PlayStatus;
    /// Ends the session.
    fn stop(self: Box<Self>) -> Result<(), String>;
    /// The in-Studio game, for a session drawn in the viewport (`None` by default).
    fn view(&self) -> Option<&dyn PlayView> {
        None
    }
    /// The in-Studio game, mutably (to send it input).
    fn view_mut(&mut self) -> Option<&mut dyn PlayView> {
        None
    }
}

/// A game that runs inside Studio: the viewport draws it and feeds it input.
///
/// Input is queued with [`PlayView::push_input`] and used by the next [`PlaySession::update`],
/// so a frame is: push this frame's events, update, render.
pub trait PlayView {
    /// The input clock in seconds; raw events are stamped with it.
    fn input_time(&self) -> f64;
    /// Queues one raw input event (keyboard or mouse) for the next update.
    fn push_input(&mut self, event: RawEvent);
    /// Releases every held control at once (the viewport lost the keyboard or the window lost
    /// focus), so no key stays stuck down.
    fn release_input(&mut self);
    /// Brings `renderer` up to date with the play world and draws it into `target` (`size` in
    /// pixels) with the game camera.
    fn render(&self, renderer: &mut Renderer, target: &wgpu::TextureView, size: (u32, u32));
    /// Paints the local player's ScreenGuis and the controls plate into `painter`'s clip rectangle
    /// (the viewport). It only paints, so it takes no input.
    fn paint_gui(&self, painter: &egui::Painter);
    /// Lends the play world read-only for the duration of `inspect` (Explorer's play tree). Does
    /// nothing after Stop or while the world is borrowed.
    fn inspect_world(&self, inspect: &mut dyn FnMut(&World));
}

/// Something that happened to play state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayEvent {
    /// A mode started.
    Started {
        /// Mode id.
        mode: &'static str,
    },
    /// A session stopped (by Stop or on its own).
    Stopped {
        /// Mode id.
        mode: &'static str,
    },
    /// A mode failed to start or stop.
    Failed {
        /// Mode id.
        mode: &'static str,
        /// What went wrong.
        reason: String,
    },
}

type Listener = Box<dyn FnMut(&PlayEvent)>;

/// The registered modes and the running session, if any.
#[derive(Default)]
pub struct PlayController {
    modes: Vec<Box<dyn PlayMode>>,
    running: Option<(usize, Box<dyn PlaySession>)>,
    listeners: Vec<Listener>,
}

impl PlayController {
    /// No modes yet.
    pub fn new() -> PlayController {
        PlayController::default()
    }

    /// Adds a mode (shown in registration order).
    pub fn register(&mut self, mode: Box<dyn PlayMode>) {
        self.modes.push(mode);
    }

    /// Ids and labels of the modes.
    pub fn modes(&self) -> impl Iterator<Item = (&'static str, String)> + '_ {
        self.modes.iter().map(|m| (m.id(), m.label()))
    }

    /// Adds a listener for play events.
    pub fn on_event(&mut self, listener: impl FnMut(&PlayEvent) + 'static) {
        self.listeners.push(Box::new(listener));
    }

    fn emit(&mut self, event: PlayEvent) {
        for l in &mut self.listeners {
            l(&event);
        }
    }

    /// True while a session runs.
    pub fn is_playing(&self) -> bool {
        self.running.is_some()
    }

    /// Id of the running mode.
    pub fn running_mode(&self) -> Option<&'static str> {
        self.running.as_ref().map(|(i, _)| self.modes[*i].id())
    }

    /// Status of the running session.
    pub fn status(&self) -> Option<PlayStatus> {
        self.running.as_ref().map(|(_, s)| s.status())
    }

    /// The running session's in-Studio game, if it has one.
    pub fn view(&self) -> Option<&dyn PlayView> {
        self.running.as_ref().and_then(|(_, s)| s.view())
    }

    /// The running session's in-Studio game, mutably.
    pub fn view_mut(&mut self) -> Option<&mut dyn PlayView> {
        self.running.as_mut().and_then(|(_, s)| s.view_mut())
    }

    /// True when a mode with `id` is registered.
    pub fn has_mode(&self, id: &str) -> bool {
        self.modes.iter().any(|m| m.id() == id)
    }

    /// Starts the mode `id`. Does nothing (returns false) when a session already runs or the mode
    /// is unknown. Failures go to Output and to listeners.
    pub fn start(&mut self, id: &str, start: PlayStart<'_>) -> bool {
        if self.running.is_some() {
            return false;
        }
        let Some(index) = self.modes.iter().position(|m| m.id() == id) else {
            return false;
        };
        let mode = self.modes[index].id();
        let output = start.output.clone();
        match self.modes[index].start(start) {
            Ok(session) => {
                self.running = Some((index, session));
                self.emit(PlayEvent::Started { mode });
                true
            }
            Err(reason) => {
                output.push(
                    Tag::Studio,
                    Level::Error,
                    format!("Could not start: {reason}"),
                );
                self.emit(PlayEvent::Failed { mode, reason });
                false
            }
        }
    }

    /// Stops the running session, if any.
    pub fn stop(&mut self) {
        if let Some((index, session)) = self.running.take() {
            let mode = self.modes[index].id();
            match session.stop() {
                Ok(()) => self.emit(PlayEvent::Stopped { mode }),
                Err(reason) => self.emit(PlayEvent::Failed { mode, reason }),
            }
        }
    }

    /// Per-frame update; ends a session that reports [`PlayStatus::Ended`].
    pub fn update(&mut self, dt: f64) {
        let ended = match &mut self.running {
            Some((_, s)) => {
                s.update(dt);
                matches!(s.status(), PlayStatus::Ended(_))
            }
            None => false,
        };
        if ended {
            self.stop();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Fake;
    struct FakeSession;
    impl PlayMode for Fake {
        fn id(&self) -> &'static str {
            "play.fake"
        }
        fn label(&self) -> String {
            "Fake".into()
        }
        fn start(&mut self, s: PlayStart<'_>) -> Result<Box<dyn PlaySession>, String> {
            s.output.push(Tag::Server, Level::Info, "started");
            Ok(Box::new(FakeSession))
        }
    }
    impl PlaySession for FakeSession {
        fn status(&self) -> PlayStatus {
            PlayStatus::Running("2 players".into())
        }
        fn stop(self: Box<Self>) -> Result<(), String> {
            Ok(())
        }
    }

    #[test]
    fn modes_start_stop_and_notify() {
        let mut c = PlayController::new();
        c.register(Box::new(Fake));
        let events = Rc::new(RefCell::new(Vec::new()));
        let e2 = events.clone();
        c.on_event(move |e| e2.borrow_mut().push(e.clone()));
        let world = World::new_with_services();
        let log = crate::output::OutputLog::new(10);
        let start = || PlayStart {
            world: &world,
            project_dir: Path::new("."),
            output: log.sink(),
        };
        assert!(c.start("play.fake", start()));
        assert!(!c.start("play.fake", start()), "only one session at a time");
        assert_eq!(c.status(), Some(PlayStatus::Running("2 players".into())));
        c.stop();
        assert!(!c.is_playing());
        assert_eq!(
            *events.borrow(),
            vec![
                PlayEvent::Started { mode: "play.fake" },
                PlayEvent::Stopped { mode: "play.fake" }
            ]
        );
    }
}

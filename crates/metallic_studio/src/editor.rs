//! The editing model of one open game: world, selection, history, tool settings, Output and Play.
//!
//! [`EditorState`] holds no UI. Panels ([`crate::explorer`], [`crate::properties`],
//! [`crate::viewport`], [`crate::output`]) read it and change it only through its methods, which
//! route every edit through [`History`](crate::commands::History). Building tools (#213 onward)
//! use the same entry points:
//!
//! - [`EditorState::apply`] for a one-shot edit (Insert, Delete, a typed property value).
//! - [`EditorState::record`] after a live-preview gesture that already wrote the world (gizmo
//!   drags, Properties scrubbing): one gesture, one undo step.
//! - [`EditorState::selection`] for what to act on; [`EditorState::tool`],
//!   [`EditorState::snapping`] and [`EditorState::space`] for the header settings.
//! - [`EditorState::preview`]: the gesture overlay (#213). Tools write live values through it;
//!   [`EditorState::finish_drag`] / [`EditorState::cancel_drag`] end a gizmo or body drag as one
//!   step or none; [`EditorState::amend_drag`] changes the last drag (editable delta).
//! - [`EditorState::gizmo_pivot`] and [`EditorState::pivot_mode`]: where transforms happen
//!   (spec 4.4); [`EditorState::rotate_selection`], [`EditorState::drop_selection`] and
//!   [`EditorState::snap_selection_to_grid`] are the one-shot building commands.
//!
//! After any history change the selection drops ids that are no longer in the tree, and the undo
//! name flashes in the status bar for 2 s.
//!
//! **One gesture flag.** [`EditorState::gesture_active`] is true while the preview overlay holds
//! values or a UI part reports a [`Gesture`] in progress (a viewport drag, a Properties scrub, a
//! brush stroke, a lasso, the array handle). Save, Play, Undo/Redo, [`EditorState::apply`] and
//! every tool commit ([`EditorState::edit_refusal`]) refuse while it is set, so a half-finished
//! gesture is never saved, played or interleaved with another step.
//!
//! **Play locks editing.** [`EditorState::new`] registers [`LocalPlayMode`] (Play, F5). While a
//! session runs, [`EditorState::apply`], [`EditorState::record`], undo and redo refuse and leave
//! the edit world, its history and the selection as they were; the session runs on its own copy,
//! so [`EditorState::stop_play`] returns to exactly the edit state Play started from.
//!
//! **Keeping Play edits** (#130). Stop then offers what the game changed
//! ([`EditorState::play_review`], [`crate::play::review`]); [`EditorState::keep_play_changes`]
//! applies the rows the creator ticked as one undo step. Nothing else reaches the edit world.
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use std::collections::HashSet;

use metallic_core::{InstanceId, Value, Vec3, World, load_scene, save_scene};
use metallic_render::glam::{DQuat, DVec3};

use crate::StudioError;
use crate::commands::{Command, CommandError, History, PropName, count};
use crate::gizmo::GizmoDrag;
use crate::output::{Level, OutputSink, Tag};
use crate::picking::PickCache;
use crate::pivot::{
    Pivot, PivotWrite, Rigid, has_custom_pivot, models_with_set_pivots, pivot_writes, reset_writes,
    to_cframe, world_pivot,
};
use crate::play::review::{PlayReview, compare};
use crate::play::{LocalPlayMode, PlayController, PlayStart};
use crate::preview::PreviewOverlay;
use crate::selection::{Selection, is_detached};
use crate::snapping::grid_shift;
use crate::transform::Aabb;
use crate::transform::{
    PartTransform, Ray, from_d, orientation_to_quat, quat_to_orientation, snap,
};

/// How long an undo or save note stays in the status bar.
pub const NOTE_TIME: Duration = Duration::from_secs(2);

/// Classes offered by Insert (Explorer right-click, the Insert menu and the palettes), from the
/// registry: see [`metallic_core::classes::Registry::insertable`].
pub fn insertable() -> &'static [&'static str] {
    static NAMES: std::sync::LazyLock<Vec<&'static str>> = std::sync::LazyLock::new(|| {
        metallic_core::classes::registry()
            .insertable()
            .iter()
            .map(|class| class.name)
            .collect()
    });
    &NAMES
}

/// `response` with the description of `class` as its tooltip, when it has one.
pub fn with_class_tooltip(response: eframe::egui::Response, class: &str) -> eframe::egui::Response {
    match metallic_core::classes::class_def(class) {
        Some(def) if !def.description.is_empty() => response.on_hover_text(def.description),
        _ => response,
    }
}

/// The active viewport tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tool {
    /// Click to select, drag on empty space for a box.
    Select,
    /// Move arrows and plane squares.
    #[default]
    Move,
    /// Rotate rings.
    Rotate,
    /// Scale face handles and the uniform corner.
    Scale,
    /// Move arrows, rotate rings and scale handles together.
    Transform,
}

impl Tool {
    /// Name in the header.
    pub fn label(self) -> &'static str {
        match self {
            Tool::Select => "Select",
            Tool::Move => "Move",
            Tool::Rotate => "Rotate",
            Tool::Scale => "Scale",
            Tool::Transform => "Transform",
        }
    }
}

/// Move increments offered by `[` and `]` (spec 2.2).
pub const MOVE_STEPS: &[f64] = &[0.05, 0.1, 0.2, 0.25, 0.5, 1.0, 2.0, 4.0, 8.0, 16.0];
/// Rotate increments offered by Shift+`[` and Shift+`]`.
pub const ROTATE_STEPS: &[f64] = &[1.0, 5.0, 11.25, 15.0, 22.5, 30.0, 45.0, 90.0];

/// Snapping settings (header chips, spec 3.1 and 5).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapping {
    /// Grid chip: moves, sizes and turns snap to the increments (Ctrl inverts during a drag).
    pub grid: bool,
    /// Surface chip: a body drag follows the surface under the cursor.
    pub surface: bool,
    /// Vertex chip: snap to corners (V forces it for one drag).
    pub vertex: bool,
    /// Edge chip: snap to edge midpoints, face centres and points on edges.
    pub edge: bool,
    /// Soft snap: with every chip off, still pull to a vertex or edge midpoint within 6 px.
    pub soft: bool,
    /// Align to surface: a body drag turns the selection's up axis to the surface normal.
    pub align: bool,
    /// Collisions chip: a drag stops where a moved part would overlap another (off by default).
    pub collisions: bool,
    /// Move and scale increment in studs.
    pub move_step: f64,
    /// Rotate increment in degrees.
    pub rotate_step: f64,
}

impl Default for Snapping {
    fn default() -> Self {
        Snapping {
            grid: true,
            surface: true,
            vertex: false,
            edge: false,
            soft: true,
            align: false,
            collisions: false,
            move_step: 1.0,
            rotate_step: 15.0,
        }
    }
}

/// Steps through a list of increments: the next smaller (`up` false) or larger one.
pub fn next_step(steps: &[f64], current: f64, up: bool) -> f64 {
    if up {
        steps
            .iter()
            .copied()
            .find(|&s| s > current + 1e-9)
            .unwrap_or(current)
    } else {
        steps
            .iter()
            .rev()
            .copied()
            .find(|&s| s < current - 1e-9)
            .unwrap_or(current)
    }
}

/// Where transforms happen (header Pivot / Centre, Z).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PivotMode {
    /// The active object's pivot.
    #[default]
    Pivot,
    /// The centre of the selection's bounding box.
    Centre,
}

/// Gizmo axes: world or the active part's own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Space {
    /// World axes.
    #[default]
    World,
    /// The active part's axes.
    Local,
}

/// A one-off request from a command to the UI (the UI owns the camera and text fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiRequest {
    /// Frame the selection in the viewport (F).
    FrameSelection,
    /// Frame everything (Home).
    FrameAll,
    /// Start renaming the active item in Explorer (F2).
    Rename(InstanceId),
    /// Open a script in the editor tab.
    OpenScript(InstanceId),
    /// Run a registered action (context menus).
    RunAction(&'static str),
    /// Start the play mode with this id, after the UI commits open script tabs and ends any
    /// live-preview gesture (toolbar Play, F5).
    StartPlay(&'static str),
    /// Open the Settings dialog (Ctrl+,).
    OpenSettings,
    /// Close the game, asking first when it has unsaved changes (Ctrl+W, like File > Close game).
    CloseGame,
    /// Open the command palette with this text typed ("" for Ctrl+K, ">" for Ctrl+Shift+P).
    OpenPalette(&'static str),
    /// Open the Insert palette at the cursor (Shift+A over the viewport).
    InsertAtCursor,
}

/// Everything about one open game except the UI.
pub struct EditorState {
    /// The edit world.
    pub world: World,
    /// Selected instances.
    pub selection: Selection,
    /// Undo and redo.
    pub history: History,
    /// The game folder.
    pub project_dir: PathBuf,
    /// The scene file being edited.
    pub scene_path: PathBuf,
    /// Game name (title bar).
    pub name: String,
    /// Active tool.
    pub tool: Tool,
    /// Snapping settings.
    pub snapping: Snapping,
    /// Gizmo space.
    pub space: Space,
    /// Pivot or selection centre.
    pub pivot_mode: PivotMode,
    /// Pivot edit mode (P): the gizmo moves the pivot only.
    pub pivot_edit: bool,
    /// Live values of the gesture in progress (#213).
    pub preview: PreviewOverlay,
    /// Every part's box for picking and snapping; the viewport keeps it in sync.
    pub picks: PickCache,
    /// The camera's right axis (Ctrl+T tilts about the nearest world axis to it).
    pub view_right: DVec3,
    /// Output handle.
    pub output: OutputSink,
    /// Play modes (Play is registered) and the running session.
    pub play: PlayController,
    /// The offer after Stop: what Play changed (#130).
    play_review: Option<PlayReview>,
    /// When false, nothing is written to disk (screenshot mode).
    pub can_save: bool,
    /// Where Insert puts a new part: the surface under the cursor, updated by the viewport.
    pub insert_point: Vec3,
    requests: Vec<UiRequest>,
    note: Option<(String, Instant)>,
    gestures: [bool; Gesture::COUNT],
}

/// A UI part that holds a gesture in progress (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gesture {
    /// A gizmo, body, box or pivot drag in the viewport.
    Viewport,
    /// A Properties scrub.
    Properties,
    /// A building tool: brush stroke, lasso or paint trail, array handle drag.
    Tool,
}

impl Gesture {
    const COUNT: usize = 3;
}

impl EditorState {
    /// Wraps a world.
    pub fn new(
        world: World,
        project_dir: &Path,
        scene_path: &Path,
        name: &str,
        output: OutputSink,
    ) -> EditorState {
        // Loading logged every attribute and tag; nothing reads the edit world's logs.
        let mut world = world;
        world.clear_change_logs();
        let mut play = PlayController::new();
        play.register(Box::new(LocalPlayMode));
        EditorState {
            world,
            selection: Selection::new(),
            history: History::new(),
            project_dir: project_dir.to_path_buf(),
            scene_path: scene_path.to_path_buf(),
            name: name.to_string(),
            tool: Tool::default(),
            snapping: Snapping::default(),
            space: Space::default(),
            pivot_mode: PivotMode::default(),
            pivot_edit: false,
            preview: PreviewOverlay::new(),
            picks: PickCache::new(),
            view_right: DVec3::X,
            output,
            play,
            play_review: None,
            can_save: true,
            insert_point: Vec3::new(0.0, 0.5, 0.0),
            requests: Vec::new(),
            note: None,
            gestures: [false; Gesture::COUNT],
        }
    }

    /// Opens a game folder: reads `metallic.toml` and its main scene.
    pub fn open(project_dir: &Path, output: OutputSink) -> Result<EditorState, StudioError> {
        let config =
            metallic_runtime::ProjectConfig::read(project_dir).map_err(|e| StudioError::Scene {
                path: project_dir.to_path_buf(),
                message: e.to_string(),
            })?;
        let scene_path = project_dir.join(&config.main_scene);
        let world = load_scene(&scene_path).map_err(|e| StudioError::Scene {
            path: scene_path.clone(),
            message: e.to_string(),
        })?;
        output.push(
            Tag::Studio,
            Level::Info,
            format!("Opened {} ({})", config.name, config.main_scene.display()),
        );
        Ok(EditorState::new(
            world,
            project_dir,
            &scene_path,
            &config.name,
            output,
        ))
    }

    /// Reports whether `part` holds a gesture now. The part calls this when the gesture starts
    /// and ends (or once a frame with its current state).
    pub fn set_gesture(&mut self, part: Gesture, on: bool) {
        self.gestures[part as usize] = on;
    }

    /// True while a gesture is in progress anywhere (see the module docs).
    pub fn gesture_active(&self) -> bool {
        self.preview.is_active() || self.gestures.iter().any(|g| *g)
    }

    /// Why an edit cannot start now: the game is playing, or a gesture is open (its values are
    /// in the world but not in History yet, so another step now would undo out of order).
    /// `None` when edits may run. Every edit entry point checks this.
    pub fn edit_refusal(&self) -> Option<&'static str> {
        if self.play.is_playing() {
            Some("edit while playing")
        } else if self.gesture_active() {
            Some("edit during a drag")
        } else {
            None
        }
    }

    /// Runs a command as one undo step.
    ///
    /// While playing it refuses with [`CommandError::Nothing`] and changes nothing.
    pub fn apply(&mut self, cmd: Command) -> Result<(), CommandError> {
        if let Some(why) = self.edit_refusal() {
            if self.play.is_playing() {
                self.note("Stop the game to edit".into());
            }
            return Err(CommandError::Nothing(why));
        }
        let result = self.history.apply(cmd, &mut self.world).map(|_| ());
        if let Err(e) = &result {
            self.output.push(Tag::Studio, Level::Warning, e.to_string());
        }
        self.after_history_change();
        result
    }

    /// Pushes an already-applied command (end of a live-preview gesture). Ignored while playing.
    pub fn record(&mut self, cmd: Command) {
        if self.play.is_playing() {
            // Edits are refused while playing; a gesture cannot have started then.
            return;
        }
        self.history.record(cmd, &mut self.world);
        self.after_history_change();
    }

    /// Undo; shows the step name in the status bar.
    pub fn undo(&mut self) {
        if self.edit_refusal().is_some() {
            return;
        }
        if let Some(name) = self.history.undo(&mut self.world) {
            self.note(format!("Undo {name}"));
        }
        self.after_history_change();
    }

    /// Redo.
    pub fn redo(&mut self) {
        if self.edit_refusal().is_some() {
            return;
        }
        if let Some(name) = self.history.redo(&mut self.world) {
            self.note(format!("Redo {name}"));
        }
        self.after_history_change();
    }

    fn after_history_change(&mut self) {
        // No script VM reads the edit world's attribute and tag logs (Play runs a copy), so
        // they would only grow: drop what the edit just logged.
        self.world.clear_change_logs();
        self.selection.retain_live(&self.world, &self.history);
    }

    /// True when there are unsaved edits.
    pub fn is_dirty(&self) -> bool {
        self.history.is_dirty()
    }

    /// True when `id` is in the tree (not destroyed, not parked by undo/delete).
    pub fn is_live(&self, id: InstanceId) -> bool {
        self.world.is_alive(id) && !is_detached(&self.world, &self.history, id)
    }

    /// Roots shown in Explorer (services and other non-parked roots).
    pub fn visible_roots(&self) -> Vec<InstanceId> {
        self.world
            .roots()
            .iter()
            .copied()
            .filter(|&r| !self.history.is_parked(r))
            .collect()
    }

    /// A copy of the world without parked instances (what Save writes and Play runs).
    pub fn clean_world(&self) -> World {
        let mut w = self.world.deep_clone();
        for id in self.history.parked() {
            w.destroy(id);
        }
        w
    }

    /// Writes the scene file and marks the history clean.
    pub fn save(&mut self) -> Result<(), StudioError> {
        if self.gesture_active() {
            // The world holds preview values that are not edits yet.
            return Err(StudioError::Scene {
                path: self.scene_path.clone(),
                message: "finish or cancel the drag first".into(),
            });
        }
        if !self.can_save {
            self.note("Saving is off in this window".into());
            return Ok(());
        }
        let world = self.clean_world();
        save_scene(&world, &self.scene_path).map_err(|e| StudioError::Scene {
            path: self.scene_path.clone(),
            message: e.to_string(),
        })?;
        self.history.mark_saved();
        self.note("Saved".into());
        self.output.push(
            Tag::Studio,
            Level::Info,
            format!("Saved {}", file_name(&self.scene_path)),
        );
        Ok(())
    }

    /// Shows `text` in the status bar for [`NOTE_TIME`].
    pub fn note(&mut self, text: String) {
        self.note = Some((text, Instant::now()));
    }

    /// The current status note, if still fresh.
    pub fn current_note(&self) -> Option<&str> {
        self.note
            .as_ref()
            .filter(|(_, at)| at.elapsed() < NOTE_TIME)
            .map(|(t, _)| t.as_str())
    }

    /// Queues a request for the UI.
    pub fn request(&mut self, r: UiRequest) {
        self.requests.push(r);
    }

    /// Takes queued UI requests.
    pub fn take_requests(&mut self) -> Vec<UiRequest> {
        std::mem::take(&mut self.requests)
    }

    /// Inserts `class` under `parent` (Workspace when `None`); a part lands at `position`.
    /// Selects the new instance.
    pub fn insert(
        &mut self,
        class: &str,
        parent: Option<InstanceId>,
        position: Option<Vec3>,
    ) -> Option<InstanceId> {
        let parent = parent.or_else(|| self.world.root("Workspace"))?;
        let mut props = Vec::new();
        if metallic_core::classes::is_subclass(class, "BasePart") {
            if let Some(p) = position {
                props.push(("Position".to_string(), Value::Vector3(p)));
            }
            props.push(("Anchored".to_string(), Value::Bool(true)));
        }
        let cmd = Command::Create {
            class: class.to_string(),
            parent,
            props,
            id: None,
        };
        self.apply(cmd).ok()?;
        let id = self.history.peek_undo()?.created().first().copied()?;
        self.selection.select_only(id);
        Some(id)
    }

    /// Deletes the selection (Delete key).
    pub fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let cmd = Command::delete(self.selection.ids());
        let _ = self.apply(cmd);
    }

    /// Duplicates the selection, placing the copies on top of it, and selects the copies.
    pub fn duplicate_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let parts = self.selection.parts(&self.world);
        let height = Aabb::of_parts(&self.world, &parts).map_or(0.0, |b| b.size().y);
        let cmd = Command::Duplicate {
            sources: self.selection.ids().to_vec(),
            offset: Vec3::new(0.0, height, 0.0),
            copies: Vec::new(),
        };
        if self.apply(cmd).is_ok()
            && let Some(done) = self.history.peek_undo()
        {
            let copies = done.created();
            self.selection.set(copies);
        }
    }

    /// Groups the selection into a new Model and selects it.
    pub fn group_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let cmd = Command::Group {
            items: self
                .selection
                .ids()
                .iter()
                .map(|&id| crate::commands::Placement {
                    id,
                    parent: None,
                    index: 0,
                })
                .collect(),
            model: None,
        };
        if self.apply(cmd).is_ok()
            && let Some(m) = self
                .history
                .peek_undo()
                .and_then(|c| c.created().first().copied())
        {
            self.selection.select_only(m);
        }
    }

    /// Moves `ids` under `parent` (Explorer drag and drop).
    pub fn reparent(&mut self, ids: &[InstanceId], parent: InstanceId) {
        let from = ids
            .iter()
            .map(|&id| crate::commands::Placement {
                id,
                parent: None,
                index: 0,
            })
            .collect();
        let _ = self.apply(Command::Reparent { from, to: parent });
    }

    /// Sets one property on several instances as one undo step.
    pub fn set_property(&mut self, ids: &[InstanceId], prop: &str, value: Value) {
        let mut commands: Vec<Command> = ids
            .iter()
            .filter_map(|&id| {
                let old = self.world.get(id, prop).ok()?;
                (old != value).then(|| Command::set(id, prop, old, value.clone()))
            })
            .collect();
        let cmd = match commands.len() {
            0 => return,
            1 => commands.remove(0),
            n => Command::Batch {
                name: format!("Set {prop} of {n} objects"),
                commands,
            },
        };
        let _ = self.apply(cmd);
    }

    /// Starts the play mode `id` with a clean copy of the world (parked instances removed).
    /// Returns true when it runs. A mode that fails to start reports why in Output and Studio
    /// stays in edit mode. Selection and history are not touched. Refused (false) while a
    /// gesture is open: its preview values are not edits.
    pub fn start_play(&mut self, id: &str) -> bool {
        if self.gesture_active() {
            self.note("Finish or cancel the drag first".into());
            return false;
        }
        let world = self.clean_world();
        let output = self.output.clone();
        let dir = self.project_dir.clone();
        let started = self.play.start(
            id,
            PlayStart {
                world: &world,
                project_dir: &dir,
                output,
            },
        );
        if started {
            self.play_review = None;
            self.output.studio("Play started on a copy of the world");
        } else if self.play.has_mode(id) && !self.play.is_playing() {
            self.note("Play could not start; see Output".into());
        }
        started
    }

    /// Stops the running session (Stop, Shift+F5, Esc): drops it with its scripts, waits and
    /// handlers, and returns to the edit world exactly as Play found it, at once and with no
    /// prompt. When a game that ran in this process changed anything, [`EditorState::play_review`]
    /// then offers those changes; a session in other processes (Team Test) has no world to compare.
    pub fn stop_play(&mut self) {
        if self.play.is_playing() {
            let review = self.play_changes();
            self.play.stop();
            self.play_review = review;
            self.output.studio("Play stopped; back to the edit world");
        }
    }

    /// Compares the running game's world with the edit world (once, at Stop).
    fn play_changes(&self) -> Option<PlayReview> {
        let view = self.play.view()?;
        let roots = self.visible_roots();
        let mut group = None;
        view.inspect_world(&mut |play| group = Some(compare(&self.world, &roots, play)));
        PlayReview::new(vec![group?], self.history.revision())
    }

    /// The offer after Stop, while it stands: starting Play, dismissing it, keeping changes or
    /// the first edit of the level (Undo and Redo count) ends it.
    pub fn play_review(&self) -> Option<&PlayReview> {
        let revision = self.history.revision();
        self.play_review.as_ref().filter(|r| r.revision == revision)
    }

    /// The offer after Stop, mutably (Keep boxes, list open or closed).
    pub fn play_review_mut(&mut self) -> Option<&mut PlayReview> {
        let revision = self.history.revision();
        self.play_review.as_mut().filter(|r| r.revision == revision)
    }

    /// Ends an offer the level has moved on from: its values were taken against the level as Play
    /// left it, so after an edit they could undo that edit. The UI calls this once a frame.
    pub fn sync_play_review(&mut self) {
        if self.play_review.is_some() && self.play_review().is_none() {
            self.play_review = None;
            self.note("The level changed, so the changes from Play were dropped".into());
        }
    }

    /// Opens or closes the list of changes.
    pub fn toggle_play_review(&mut self) {
        if let Some(review) = self.play_review_mut() {
            review.open = !review.open;
        }
    }

    /// Drops the offer: nothing from Play is kept.
    pub fn dismiss_play_review(&mut self) {
        self.play_review = None;
    }

    /// Applies the changes ticked Keep as one undo step and ends the offer; the others are gone.
    /// Returns how many rows were kept. With none ticked, or during a gesture (the world holds
    /// preview values), nothing happens and the offer stays.
    pub fn keep_play_changes(&mut self) -> usize {
        self.sync_play_review();
        let picked = self.play_review().map_or(0, PlayReview::picked);
        if picked == 0 {
            return 0;
        }
        if self.edit_refusal().is_some() {
            self.note("Finish or cancel the drag first".into());
            return 0;
        }
        let Some(review) = self.play_review.take() else {
            return 0;
        };
        let (commands, failed) = review.keep(&mut self.history, &mut self.world);
        for why in failed {
            let text = format!("Could not keep {why}");
            self.output.push(Tag::Studio, Level::Warning, text);
        }
        if commands.is_empty() {
            self.note("Nothing could be kept; see Output".into());
            self.after_history_change();
            return 0;
        }
        let kept = count(picked, "change");
        self.record(Command::Batch {
            name: format!("Keep {kept} from Play"),
            commands,
        });
        self.note(format!("Kept {kept} from Play"));
        picked
    }

    /// The bounding box of the selection's parts.
    pub fn selection_bounds(&self) -> Option<Aabb> {
        Aabb::of_parts(&self.world, &self.selection.parts(&self.world))
    }

    /// The pivot `id` moves about (part frame times offset, Model pivot, or box centre).
    pub fn object_pivot(&self, id: InstanceId) -> Option<Pivot> {
        world_pivot(&self.world, id)
    }

    /// Where the gizmo sits: the active object's pivot (Pivot mode) or the selection's box
    /// centre with the active object's axes (Centre mode). In pivot edit mode, always the active
    /// object's pivot.
    pub fn gizmo_pivot(&self) -> Option<Pivot> {
        let active = self.selection.active()?;
        let pivot = self.object_pivot(active);
        if self.pivot_edit || self.pivot_mode == PivotMode::Pivot {
            return pivot;
        }
        let centre = self.selection_bounds()?.centre();
        Some(Pivot {
            position: centre,
            rotation: pivot.map_or(DQuat::IDENTITY, |p| p.rotation),
        })
    }

    /// Models in the selection (or under it) whose `WorldPivot` is set: it moves with them.
    pub fn models_with_pivots(&self) -> Vec<(InstanceId, Pivot)> {
        models_with_set_pivots(&self.world, self.selection.ids())
    }

    /// Ends a drag: records its one step (or nothing) and drops what an unused duplicate made.
    /// True when a step was recorded.
    pub fn finish_drag(&mut self, drag: GizmoDrag) -> bool {
        let (record, abandon) = drag.finish(&mut self.preview);
        if let Some(c) = abandon {
            self.history.abandon(c, &mut self.world);
        }
        let done = match record {
            Some(c) if !self.play.is_playing() => {
                self.record(c);
                true
            }
            _ => false,
        };
        self.after_history_change();
        done
    }

    /// Cancels a drag (Esc): the world is as before it started.
    pub fn cancel_drag(&mut self, drag: GizmoDrag) {
        if let Some(c) = drag.cancel(&mut self.world, &mut self.preview) {
            self.history.abandon(c, &mut self.world);
        }
        self.after_history_change();
    }

    /// Changes the last drag's amount (the editable delta after release, spec 4.5): the newest
    /// step is replaced, not added to. `drag` is the finished drag as it was before
    /// [`EditorState::finish_drag`]. False when the drag has no single amount.
    pub fn amend_drag(&mut self, drag: &GizmoDrag, value: f64) -> bool {
        if self.play.is_playing() || !drag.accepts_typing() {
            return false;
        }
        if self.history.take_last(&mut self.world).is_none() {
            return false;
        }
        let mut d = drag.clone();
        if let Some(p) = d.pending_mut()
            && self.history.apply_pending(p, &mut self.world).is_err()
        {
            self.after_history_change();
            return false;
        }
        d.set_amount(value);
        d.apply(&mut self.world, &mut self.preview);
        self.finish_drag(d)
    }

    /// Writes new transforms for parts as one step named `name` (Position and Size, and
    /// Orientation when given). Model pivots in the selection follow `motion` when given.
    fn commit_transforms(
        &mut self,
        name: String,
        changes: &[(InstanceId, PartTransform, Option<Vec3>)],
        motion: Option<Rigid>,
    ) -> bool {
        // An open gesture owns the overlay; committing now would take its values with it.
        if self.play.is_playing() || self.preview.is_active() {
            return false;
        }
        let names: [PropName; 3] = ["Position".into(), "Orientation".into(), "Size".into()];
        for (id, t, orientation) in changes {
            let _ = self.preview.set(
                &mut self.world,
                *id,
                &names[0],
                Value::Vector3(from_d(t.position)),
            );
            if let Some(o) = orientation {
                let _ = self
                    .preview
                    .set(&mut self.world, *id, &names[1], Value::Vector3(*o));
            }
            let _ = self.preview.set(
                &mut self.world,
                *id,
                &names[2],
                Value::Vector3(from_d(t.size)),
            );
        }
        if let Some(m) = motion {
            let world_pivot: PropName = "WorldPivot".into();
            for (id, p) in self.models_with_pivots() {
                let value = Value::CFrame(Box::new(to_cframe(&m.frame(&p))));
                let _ = self.preview.set(&mut self.world, id, &world_pivot, value);
            }
        }
        let Some(cmd) = self.preview.commit(name) else {
            return false;
        };
        self.record(cmd);
        true
    }

    /// Writes pivot property changes as one step named `name`.
    fn commit_writes(&mut self, name: String, writes: Vec<PivotWrite>) -> bool {
        if self.play.is_playing() || self.preview.is_active() || writes.is_empty() {
            return false;
        }
        for (id, prop, value) in writes {
            let prop: PropName = prop.into();
            let _ = self.preview.set(&mut self.world, id, &prop, value);
        }
        match self.preview.commit(name) {
            Some(cmd) => {
                self.record(cmd);
                true
            }
            None => false,
        }
    }

    /// Moves and turns every selected part by `motion` as one step.
    pub fn transform_selection(&mut self, motion: Rigid, verb: &str) -> bool {
        let parts = self.selection.parts(&self.world);
        let turns = !motion.rotation.abs_diff_eq(DQuat::IDENTITY, 1e-12);
        let changes: Vec<_> = parts
            .iter()
            .filter_map(|&id| {
                let mut t = PartTransform::read(&self.world, id)?;
                t.position = motion.point(t.position);
                t.rotation = (motion.rotation * t.rotation).normalize();
                Some((id, t, turns.then(|| quat_to_orientation(t.rotation))))
            })
            .collect();
        let name = format!("{verb} {}", count(changes.len(), "part"));
        self.commit_transforms(name, &changes, Some(motion))
    }

    /// Turns the selection by `degrees` about world `axis` through the gizmo pivot (Ctrl+R,
    /// Ctrl+T). One step.
    pub fn rotate_selection(&mut self, axis: DVec3, degrees: f64, verb: &str) -> bool {
        let Some(pivot) = self.gizmo_pivot() else {
            return false;
        };
        let motion = Rigid {
            rotation: DQuat::from_axis_angle(axis.normalize_or(DVec3::Y), degrees.to_radians()),
            about: pivot.position,
            translation: DVec3::ZERO,
        };
        self.transform_selection(motion, verb)
    }

    /// The world axis nearest the camera's right (Ctrl+T tilts about it).
    pub fn tilt_axis(&self) -> DVec3 {
        let r = self.view_right;
        let i = (0..3)
            .max_by(|&a, &b| r[a].abs().total_cmp(&r[b].abs()))
            .unwrap_or(0);
        DVec3::AXES[i] * r[i].signum()
    }

    /// Drops the selection straight down onto the first surface below it (End). One step.
    pub fn drop_selection(&mut self) -> bool {
        self.picks.sync(&self.world);
        let parts = self.selection.parts(&self.world);
        let set: HashSet<InstanceId> = parts.iter().copied().collect();
        let skip = |id: InstanceId| set.contains(&id);
        let mut gap = f64::INFINITY;
        for &id in &parts {
            let Some(t) = PartTransform::read(&self.world, id) else {
                continue;
            };
            let corners = t.corners();
            let bottom = corners.iter().map(|c| c.y).fold(f64::INFINITY, f64::min);
            let centre = DVec3::new(t.position.x, bottom, t.position.z);
            for c in corners.iter().chain(std::iter::once(&centre)) {
                let ray = Ray {
                    origin: *c,
                    dir: DVec3::NEG_Y,
                };
                if let Some((hit, _)) = self.picks.surface(&ray, &skip) {
                    gap = gap.min(hit.distance);
                }
            }
        }
        if !gap.is_finite() {
            self.note("Nothing below the selection".into());
            return false;
        }
        if gap < 1e-9 {
            return false;
        }
        self.transform_selection(Rigid::translate(DVec3::new(0.0, -gap, 0.0)), "Drop")
    }

    /// True when a selected part is off the grid (its reference point, spec 2.2, or its turn).
    pub fn off_grid(&self) -> bool {
        let step = self.snapping.move_step;
        let turn = self.snapping.rotate_step;
        self.selection
            .parts(&self.world)
            .iter()
            .take(256)
            .filter_map(|&id| PartTransform::read(&self.world, id).map(|t| (id, t)))
            .any(|(id, t)| {
                let shift = grid_snap_shift(&t, step);
                let o = match self.world.get_ref(id, "Orientation") {
                    Ok(Value::Vector3(v)) => *v,
                    _ => Vec3::new(0.0, 0.0, 0.0),
                };
                shift.length() > 1e-6
                    || [o.x, o.y, o.z]
                        .iter()
                        .any(|&a| (snap(a, turn) - a).abs() > 1e-6)
            })
    }

    /// Moves each selected part's reference point to the grid and rounds its turn to the rotate
    /// increment (Ctrl+Shift+Home, spec 4.6). One step.
    pub fn snap_selection_to_grid(&mut self) -> bool {
        let step = self.snapping.move_step;
        let turn = self.snapping.rotate_step;
        let parts = self.selection.parts(&self.world);
        let changes: Vec<_> = parts
            .iter()
            .filter_map(|&id| {
                let mut t = PartTransform::read(&self.world, id)?;
                let o = match self.world.get_ref(id, "Orientation") {
                    Ok(Value::Vector3(v)) => *v,
                    _ => return None,
                };
                let rounded = Vec3::new(snap(o.x, turn), snap(o.y, turn), snap(o.z, turn));
                t.rotation = orientation_to_quat(rounded);
                t.position += grid_snap_shift(&t, step);
                Some((id, t, (rounded != o).then_some(rounded)))
            })
            .collect();
        let name = format!("Snap {} to grid", count(changes.len(), "part"));
        self.commit_transforms(name, &changes, None)
    }

    /// Sets the pivot of `id` to the world frame `pivot` (one step of `PivotOffset` or
    /// `WorldPivot` writes).
    pub fn set_pivot(&mut self, id: InstanceId, pivot: Pivot) -> bool {
        let writes = pivot_writes(&self.world, id, &pivot);
        let name = format!("Edit pivot of {}", self.display_name(id));
        self.commit_writes(name, writes)
    }

    /// Restores the default pivot of the active object (Reset pivot). One step.
    pub fn reset_pivot(&mut self) -> bool {
        let Some(id) = self.selection.active() else {
            return false;
        };
        if !has_custom_pivot(&self.world, id) {
            return false;
        }
        let writes = reset_writes(&self.world, id);
        let name = format!("Reset pivot of {}", self.display_name(id));
        self.commit_writes(name, writes)
    }

    /// Halves or doubles the move increment (`[`, `]`) through the offered steps.
    pub fn step_move_increment(&mut self, up: bool) {
        self.snapping.move_step = next_step(MOVE_STEPS, self.snapping.move_step, up);
        self.note(format!("Move increment {} stud", self.snapping.move_step));
    }

    /// Steps the rotate increment down or up (Shift+`[`, Shift+`]`).
    pub fn step_rotate_increment(&mut self, up: bool) {
        self.snapping.rotate_step = next_step(ROTATE_STEPS, self.snapping.rotate_step, up);
        self.note(format!("Rotate increment {}°", self.snapping.rotate_step));
    }

    /// Display name of an instance (`Name`, or its class).
    pub fn display_name(&self, id: InstanceId) -> String {
        match self.world.get_ref(id, "Name") {
            Ok(Value::String(s)) => s.clone(),
            _ => self.world.class_name(id).unwrap_or("?").to_string(),
        }
    }
}

/// The shift that puts a part's bounding-box min corner on the world grid on each axis (spec 2.2):
/// its faces then sit on grid lines.
pub fn grid_snap_shift(t: &PartTransform, step: f64) -> DVec3 {
    let corners = t.corners();
    let mut out = DVec3::ZERO;
    for axis in 0..3 {
        let lo = corners
            .iter()
            .map(|c| c[axis])
            .fold(f64::INFINITY, f64::min);
        out[axis] = grid_shift(lo, step);
    }
    out
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

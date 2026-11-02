//! The editor window layout: menu bar, toolbar, Explorer (left), Properties (right), Output
//! (bottom), the scene/script tabs in the centre and the status bar.
//!
//! [`EditorUi`] owns the panels, the [`ActionRegistry`](crate::actions::ActionRegistry) and the
//! [`Keymap`]; [`EditorState`] owns the data. Keys are dispatched once per frame through the
//! keymap to actions: global keys always (except while typing in a text field, where only Save
//! and Play keys work), viewport keys only while the pointer is over the 3D view and the right
//! button is up (so WASD flies the camera while it is held).
//!
//! The building tools ([`EditorTools`]), the Input Actions dock ([`InputActionsDock`]) and the
//! colour popover ([`ColourDock`]) live in their own modules; this file only registers them,
//! gives them their place in the layout and calls them once per frame.
//!
//! **While playing** Studio's keys are dispatched first each frame (Global and Play contexts:
//! Esc and Shift+F5 stop; a disabled action leaves its key alone), then the rest of the keyboard,
//! Tab, Space and Enter included, and the mouse over the viewport go to the game
//! ([`PlayInput`]), then the session advances and the viewport draws it. #367's status bar shows
//! "Playing" in `success`, the viewport has a `success` frame, Explorer shows the play world read-only,
//! and Properties, Insert, the script editor and every editing action are off until Stop.
use eframe::egui::{self, Frame, Margin, RichText};
use eframe::egui_wgpu;

use crate::actions::{ActionRegistry, ToolbarGroup, default_actions};
use crate::context_bar::ContextBar;
use crate::editor::{EditorState, Gesture, UiRequest, insertable, with_class_tooltip};
use crate::editor_colour::ColourDock;
use crate::editor_input_actions::InputActionsDock;
use crate::editor_tools::{EditorTools, ViewContext};
use crate::explorer::{Explorer, PlayTree};
use crate::hints::{HintEvent, Hints};
use crate::keymap::{KeyContext, Keymap};
use crate::output::OutputPanel;
use crate::palette::{Choice, CommandPalette};
use crate::panel;
use crate::play::input::PlayInput;
use crate::properties::Properties;
use crate::script_editor::ScriptTabs;
use crate::style_picker::StylePicker;
use crate::theme;
use crate::tokens::ui_tokens;
use crate::toolbar::{Toolbar, ToolbarLayout};
use crate::viewport::Viewport;
use crate::viewport_header::ViewportHeader;
use crate::widgets::Plate;

/// What the editor asks the app shell to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorExit {
    /// Keep editing.
    Stay,
    /// Close the game and go back to the start screen.
    Close,
    /// Quit Studio (the window's close button or Alt+F4, after the unsaved-changes prompt).
    Quit,
}

/// What the unsaved-changes prompt is guarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Leave {
    /// File > Close game.
    Close,
    /// Closing the window.
    Quit,
}

/// The part whose Color the step `c` changed, when it changed exactly one part's Color.
fn recoloured_one(c: &crate::commands::Command) -> Option<metallic_core::InstanceId> {
    use crate::commands::Command;
    match c {
        Command::SetProperty { id, prop, .. } if prop == "Color" => Some(*id),
        Command::SetProperties { changes, .. } => match changes.as_slice() {
            [(id, prop, _, _)] if &**prop == "Color" => Some(*id),
            _ => None,
        },
        Command::Batch { commands, .. } if commands.len() == 1 => recoloured_one(&commands[0]),
        _ => None,
    }
}

/// The editor window for one open game.
pub struct EditorUi {
    /// Data and edit entry points.
    pub state: EditorState,
    /// Actions run by toolbar, menus and keys.
    pub actions: ActionRegistry,
    /// Shortcuts.
    pub keymap: Keymap,
    /// Output panel (owns the log the state's sink feeds).
    pub output: OutputPanel,
    /// 3D view.
    pub viewport: Viewport,
    /// Building tools (align, mirror, array, brush, selection modes, measure).
    pub tools: EditorTools,
    /// The Input Actions panel, docked on the right when open.
    pub input_actions: InputActionsDock,
    /// The colour popover for Color3 properties.
    pub colour: ColourDock,
    toolbar: Toolbar,
    explorer: Explorer,
    /// The strip over the viewport (spec 3.1).
    pub header: ViewportHeader,
    /// The floating toolbar over the selection (spec 3.2).
    pub context_bar: ContextBar,
    /// The building style picker (L, Properties > Style).
    pub style_picker: StylePicker,
    /// First-time hints (spec 14.1); in memory until the app loads the saved ones.
    pub hints: Hints,
    /// Selection and history revisions the hints last looked at.
    hint_seen: (u64, u64),
    /// Properties panel.
    pub properties: Properties,
    scripts: ScriptTabs,
    pressed: Vec<&'static str>,
    /// Open while the unsaved-changes prompt shows.
    confirm: Option<Leave>,
    /// Why the last Save from the prompt failed (shown in the prompt).
    save_error: Option<String>,
    /// The creator asked for Settings; the app shows the dialog.
    settings_requested: bool,
    /// Keyboard and mouse state of the running game's input.
    play_input: PlayInput,
    /// Explorer's read-only tree of the play world.
    play_tree: PlayTree,
    /// Ctrl+K command palette and Shift+A Insert palette.
    pub palette: CommandPalette,
    /// The last question asked of Litha from the palette, for the Litha panel to take.
    litha_question: Option<String>,
}

impl EditorUi {
    /// Builds the editor around `state`, whose Output sink must come from `output`.
    pub fn new(state: EditorState, output: OutputPanel) -> EditorUi {
        let mut actions = default_actions();
        let tools = EditorTools::new(&mut actions);
        let input_actions = InputActionsDock::new(&state.world, &mut actions);
        let colour = ColourDock::new(&state, &mut actions);
        let style_picker = StylePicker::new(&mut actions);
        let mut ui = EditorUi {
            state,
            actions,
            style_picker,
            keymap: Keymap::metallic(),
            output,
            viewport: Viewport::new(),
            tools,
            input_actions,
            colour,
            toolbar: Toolbar::new(),
            explorer: Explorer::new(),
            properties: Properties::new(),
            header: ViewportHeader::new(),
            context_bar: ContextBar::new(),
            hints: Hints::in_memory(),
            hint_seen: (u64::MAX, 0),
            scripts: ScriptTabs::new(),
            pressed: Vec::new(),
            confirm: None,
            save_error: None,
            settings_requested: false,
            play_input: PlayInput::new(),
            play_tree: PlayTree::new(),
            palette: CommandPalette::new(),
            litha_question: None,
        };
        ui.viewport.frame_all(&ui.state);
        ui.sync_hold_keys();
        ui
    }

    /// Where last frame's toolbar put its groups and which collapsed into the "»" menu.
    pub fn toolbar_layout(&self) -> &ToolbarLayout {
        self.toolbar.layout()
    }

    /// Copies the held keys (V, Space) from the keymap into the viewport.
    fn sync_hold_keys(&mut self) {
        if let Some(c) = self.keymap.chord("hold.vertex") {
            self.viewport.holds.vertex = c.key;
        }
        if let Some(c) = self.keymap.chord("hold.summon") {
            self.viewport.holds.summon = c.key;
        }
    }

    /// Drag-context keys (Esc, X/Y/Z, R/T, Ctrl+D) go to the drag before the viewport runs.
    fn drag_keys(&mut self, ctx: &egui::Context) {
        if !self.viewport.is_dragging() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let mut pressed = std::mem::take(&mut self.pressed);
        self.keymap.pressed(ctx, &[KeyContext::Drag], &mut pressed);
        for &id in &pressed {
            self.viewport.drag_action(id, &mut self.state);
        }
        self.pressed = pressed;
    }

    /// Opens a game folder in a new editor.
    pub fn open(path: &std::path::Path) -> Result<EditorUi, crate::StudioError> {
        let output = OutputPanel::new();
        let state = EditorState::open(path, output.log.sink())?;
        Ok(EditorUi::new(state, output))
    }

    /// The editor on a template folder for screenshots: one part selected, nothing is written to
    /// disk.
    pub fn demo(template: &std::path::Path) -> Option<EditorUi> {
        let mut editor = EditorUi::open(template).ok()?;
        editor.state.can_save = false;
        let ws = editor.state.world.root("Workspace")?;
        let course = editor.state.world.find_first_child(ws, "Course")?;
        let part = editor.state.world.children(course).get(2).copied()?;
        editor.state.selection.select_only(part);
        editor.viewport.frame_selection(&editor.state);
        let back = -editor.viewport.camera.forward();
        editor.viewport.camera.dolly(back, 6.0);
        // Show the contextual toolbar at once (no pointer to wait for in a capture).
        editor.context_bar.arm(editor.state.selection.revision());
        Some(editor)
    }

    /// The window is asked to close (close button, Alt+F4). Returns true when it may close now;
    /// with unsaved changes it opens the Save / Don't save / Cancel prompt instead and returns
    /// false, and a later frame returns [`EditorExit::Quit`] if the user agrees. A running game
    /// stops first, so it never keeps running behind the prompt.
    pub fn request_quit(&mut self) -> bool {
        self.state.stop_play();
        // Unsaved script text counts: put the open tabs into the world first.
        self.scripts.flush(&mut self.state);
        if !self.state.is_dirty() {
            return true;
        }
        self.confirm = Some(Leave::Quit);
        false
    }

    /// True once after the creator asked for Settings (Ctrl+, or File > Settings); the app shows
    /// the dialog over the editor.
    pub fn take_settings_request(&mut self) -> bool {
        std::mem::take(&mut self.settings_requested)
    }

    /// True while the unsaved-changes prompt is open.
    pub fn is_confirming(&self) -> bool {
        self.confirm.is_some()
    }

    /// Why the last Save from the prompt failed, if it did.
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Saves now (script buffers first). On failure the error goes to Output and is returned.
    pub fn save(&mut self) -> Result<(), String> {
        self.scripts.flush(&mut self.state);
        self.state.save().map_err(|e| {
            let text = e.to_string();
            self.state.output.push(
                crate::output::Tag::Studio,
                crate::output::Level::Error,
                text.clone(),
            );
            text
        })
    }

    /// Runs action `id`, flushing script buffers first for Save.
    pub fn run_action(&mut self, id: &str) {
        if id == "file.save" {
            self.scripts.flush(&mut self.state);
        }
        if id == "edit.duplicate" {
            self.hint(HintEvent::Duplicated);
        }
        if id == "view.focus_context_bar" {
            self.context_bar.focus();
            return;
        }
        if id == "output.clear" {
            self.output.log.clear();
            return;
        }
        self.actions.run(id, &mut self.state);
    }

    /// The building tools' keys, before the contextual toolbar and the hint card see Esc:
    /// [`KeyContext::Popover`] while a tool popover is open, [`KeyContext::Tool`] while the
    /// brush or Measure is on over the viewport. A key is claimed only when its action is enabled
    /// (the open popover or tool handles it); any other key stays for [`EditorUi::keys`]. During a
    /// tool gesture only `tool.cancel` (Esc) runs; during any other gesture nothing does.
    fn tool_keys(&mut self, ctx: &egui::Context, viewport_hovered: bool, right_down: bool) {
        // A focused contextual-toolbar button does not count: the popovers' Esc comes first.
        let focused = ctx.egui_wants_keyboard_input() && !self.context_bar.has_focus();
        if self.state.play.is_playing() || right_down || focused || self.viewport.is_editing_delta()
        {
            return;
        }
        let mut contexts = [KeyContext::Popover; 2];
        let mut active = 0;
        if self.tools.popover_keys() {
            active = 1;
        }
        if self.tools.tool_keys(viewport_hovered) {
            contexts[active] = KeyContext::Tool;
            active += 1;
        }
        if active == 0 {
            return;
        }
        let tool_gesture = self.tools.gesture();
        if self.state.gesture_active() && !tool_gesture {
            return;
        }
        let mut pressed = std::mem::take(&mut self.pressed);
        let (actions, state) = (&self.actions, &self.state);
        self.keymap
            .pressed_where(ctx, &contexts[..active], &mut pressed, |id| {
                (!tool_gesture || id == "tool.cancel")
                    && actions.get(id).is_some_and(|a| a.is_enabled(state))
            });
        if pressed
            .iter()
            .any(|a| *a == "popover.close" || *a == "tool.cancel")
        {
            // The tool took Esc: the contextual toolbar keeps its focus for the next one.
            self.context_bar.keep_focus();
        }
        for &id in &pressed {
            self.run_action(id);
        }
        self.pressed = pressed;
    }

    /// True when the hint card must wait: a viewport drag or flight, Play, or any gesture
    /// ([`EditorState::gesture_active`]: preview, Properties scrub, brush stroke, lasso, array
    /// handle).
    fn hints_blocked(&self) -> bool {
        self.viewport.is_busy() || self.state.play.is_playing() || self.state.gesture_active()
    }

    fn keys(&mut self, ctx: &egui::Context, viewport_hovered: bool, right_down: bool) {
        let playing = self.state.play.is_playing();
        // While playing, a focused button must not keep Esc from stopping: only text counts.
        let typing = if playing {
            ctx.text_edit_focused()
        } else {
            ctx.egui_wants_keyboard_input()
                || self.viewport.is_editing_delta()
                || self.context_bar.has_focus()
        };
        // A gesture (preview overlay, viewport drag, Properties scrub, brush stroke, lasso, array
        // handle) is open: no action runs until it ends. Esc cancels it where it lives; a tool
        // gesture's Esc is `tool.cancel` (EditorUi::tool_keys, earlier in the frame).
        let busy = !playing && self.state.gesture_active();
        let fly = right_down;
        // At most Global and Viewport (or Global and Play): no allocation. The tools' Popover
        // and Tool contexts ran already (EditorUi::tool_keys).
        let mut contexts = [KeyContext::Global; 2];
        let mut active = 0;
        let mut push = |c| {
            contexts[active] = c;
            active += 1;
        };
        if !typing {
            if playing {
                push(KeyContext::Global);
                // With the unsaved-changes prompt open, Esc belongs to the prompt.
                if self.confirm.is_none() {
                    push(KeyContext::Play);
                }
            } else {
                push(KeyContext::Global);
                if viewport_hovered && !fly && !busy {
                    push(KeyContext::Viewport);
                }
            }
        }
        let contexts = &contexts[..active];
        let mut pressed = std::mem::take(&mut self.pressed);
        if busy {
            pressed.clear();
        } else if typing {
            // Only Save and Play keys work while a text field has focus (Ctrl+W must not close the game).
            self.keymap
                .pressed(ctx, &[KeyContext::Global], &mut pressed);
            pressed.retain(|a| *a == "file.save" || a.starts_with("play."));
        } else if playing {
            // Studio keeps F5, Shift+F5, Esc and Ctrl+S; a disabled action leaves its key to the
            // game.
            let (actions, state) = (&self.actions, &self.state);
            self.keymap
                .pressed_where(ctx, contexts, &mut pressed, |id| {
                    id.starts_with("play.") || actions.get(id).is_some_and(|a| a.is_enabled(state))
                });
        } else {
            self.keymap.pressed(ctx, contexts, &mut pressed);
        }
        for &id in &pressed {
            self.run_action(id);
        }
        self.pressed = pressed;
    }

    fn hint(&mut self, event: HintEvent) {
        let blocked = self.hints_blocked();
        self.hints.note(&event, std::time::Instant::now(), blocked);
    }

    /// Turns what happened this frame into hint events (selection, drags, recolouring).
    fn watch_hints(&mut self) {
        let revision = self.state.selection.revision();
        if revision != self.hint_seen.0 {
            self.hint_seen.0 = revision;
            let parts = !self.state.selection.parts(&self.state.world).is_empty();
            self.hint(HintEvent::Selected { parts });
        }
        if let Some(d) = self.viewport.take_finished() {
            self.hint(HintEvent::DragEnded {
                axis: d.axis,
                body: d.body,
                snapping_off: d.snapping_off,
                off_grid: d.off_grid,
                near_corner: d.near_corner,
            });
            self.hint(HintEvent::MovedSelection);
            if let Some((id, y)) = d.one {
                self.hint(HintEvent::MovedOne { id, y });
            }
        }
        let history = self.state.history.revision();
        if history != self.hint_seen.1 {
            self.hint_seen.1 = history;
            if let Some(id) = self.state.history.peek_undo().and_then(recoloured_one) {
                self.hint(HintEvent::RecolouredOne(id));
            }
        }
    }

    /// Starts play mode `id` on what is on screen: open script tabs are committed and a gizmo or
    /// Properties gesture ends first. On success the Scene tab shows and the game gets the
    /// keyboard; on failure Output says why and editing goes on.
    fn start_play(&mut self, ctx: &egui::Context, id: &'static str) {
        self.scripts.flush(&mut self.state);
        self.colour.close(&mut self.state);
        self.style_picker.close(&mut self.state);
        self.properties.finish_gesture(&mut self.state);
        self.viewport.cancel_drag(&mut self.state);
        self.tools.cancel_gestures();
        for g in [Gesture::Viewport, Gesture::Properties, Gesture::Tool] {
            self.state.set_gesture(g, false);
        }
        self.palette.close();
        if self.state.start_play(id) {
            self.scripts.show_scene();
            self.play_input = PlayInput::new();
            self.play_tree = PlayTree::new();
            ctx.memory_mut(|m| {
                if let Some(focused) = m.focused() {
                    m.surrender_focus(focused);
                }
            });
        }
    }

    /// Call from the app's `raw_input_hook`, before egui sees the frame's input. While the game
    /// has the keyboard, Tab, Space and Enter are taken out here, so egui can neither move focus
    /// to a Studio widget nor press the focused one; the game gets them through [`PlayInput`].
    pub fn raw_input_hook(&mut self, raw: &mut egui::RawInput) {
        if self.state.play.view().is_some() && self.confirm.is_none() {
            self.play_input.hold_game_keys(raw);
        }
    }

    /// File > Close game and Ctrl+W: closes at once, or asks first when there are unsaved changes.
    /// A running game stops first (Ctrl+W works while playing).
    fn close_game(&mut self) -> Option<EditorExit> {
        self.state.stop_play();
        self.scripts.flush(&mut self.state);
        if self.state.is_dirty() {
            self.confirm = Some(Leave::Close);
            self.save_error = None;
            None
        } else {
            Some(EditorExit::Close)
        }
    }

    /// Opens the command palette (Ctrl+K). The title bar's search field calls this too.
    pub fn open_command_palette(&mut self) {
        self.open_command_palette_with("");
    }

    /// Opens the command palette with `text` typed (">" lists commands only). Not while playing
    /// or during a gesture.
    pub fn open_command_palette_with(&mut self, text: &str) {
        // Not mid-gesture either (a drag, a gizmo, a Properties scrub or a tool): the palette's
        // choices are edits, and an edit now would land inside the open gesture.
        if self.state.play.is_playing() || self.viewport.is_busy() || self.state.gesture_active() {
            return;
        }
        self.palette.open(text, &self.state.project_dir);
    }

    /// The question the creator last asked Litha from the palette (for the Litha panel).
    pub fn take_litha_question(&mut self) -> Option<String> {
        self.litha_question.take()
    }

    /// Carries out what was picked in the palette.
    fn apply_palette(&mut self, ctx: &egui::Context, choice: Choice) {
        match choice {
            Choice::Select(ids) => self.state.selection.set(ids),
            Choice::Run(id) => self.run_action(id),
            Choice::Insert { class, parent, at } => {
                self.state.insert(class, parent, Some(at));
            }
            Choice::Asset(rel) => {
                ctx.copy_text(rel.clone());
                self.state.note(format!("Copied the path {rel}"));
            }
            Choice::Learn(url) => ctx.open_url(egui::OpenUrl::new_tab(url)),
            Choice::AskLitha(question) => {
                self.state
                    .note("Litha arrives in milestone 7; your question is kept for it".into());
                self.litha_question = Some(question);
            }
        }
    }

    fn requests(&mut self, ctx: &egui::Context) -> Option<EditorExit> {
        let mut exit = None;
        for r in self.state.take_requests() {
            match r {
                UiRequest::FrameSelection | UiRequest::FrameAll => {
                    self.viewport.handle(r, &self.state)
                }
                UiRequest::Rename(id) => self.explorer.rename(id, &self.state),
                UiRequest::OpenScript(id) => self.scripts.open(id, &self.state),
                UiRequest::RunAction(id) => self.run_action(id),
                UiRequest::OpenSettings => self.settings_requested = true,
                UiRequest::StartPlay(id) => self.start_play(ctx, id),
                UiRequest::CloseGame => exit = exit.or(self.close_game()),
                UiRequest::OpenPalette(text) => self.open_command_palette_with(text),
                UiRequest::InsertAtCursor => {
                    let pos = ctx
                        .input(|i| i.pointer.latest_pos())
                        .unwrap_or_else(|| self.viewport.rect().center());
                    let parent = crate::palette::insert_parent(&self.state);
                    self.palette
                        .open_insert(pos, self.state.insert_point, parent);
                }
            }
        }
        exit
    }

    /// Draws one frame. `render_state` is eframe's wgpu state (None without a GPU).
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        render_state: Option<&egui_wgpu::RenderState>,
    ) -> EditorExit {
        let ctx = ui.ctx().clone();
        let dt = f64::from(ctx.input(|i| i.stable_dt));
        if self.confirm.is_some()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            // Esc answers the unsaved-changes prompt (Cancel); it never stops Play then.
            self.confirm = None;
            self.save_error = None;
        }
        let played = self.state.play.is_playing();
        let mut early = None;
        if played {
            // Studio keys first (Esc, Shift+F5, Ctrl+S); what is left belongs to the game.
            self.keys(&ctx, false, false);
            early = self.requests(&ctx);
        }
        let (rect, hovered) = (self.viewport.rect(), self.viewport.play_hovered());
        let prompt = self.confirm.is_some();
        if let Some(view) = self.state.play.view_mut() {
            if prompt {
                self.play_input.release(view);
            } else {
                self.play_input.feed(&ctx, view, rect, hovered);
            }
        }
        self.state.play.update(dt);
        let playing = self.state.play.is_playing();
        let mut exit = early.unwrap_or(EditorExit::Stay);

        let menu = egui::Panel::top("menu")
            .frame(
                Frame::new()
                    .fill(theme::bg_deep())
                    .inner_margin(Margin::symmetric(8, 2)),
            )
            .show(ui, |ui| {
                egui::MenuBar::new().ui(ui, |ui| {
                    ui.menu_button("File", |ui| {
                        if ui
                            .add(
                                egui::Button::new("Save")
                                    .shortcut_text(self.keymap.label("file.save")),
                            )
                            .clicked()
                        {
                            self.run_action("file.save");
                            ui.close();
                        }
                        self.actions.menu_items(
                            ui,
                            &["settings.open"],
                            &mut self.state,
                            &self.keymap,
                        );
                        if ui
                            .add(
                                egui::Button::new("Close game")
                                    .shortcut_text(self.keymap.label("file.close")),
                            )
                            .clicked()
                        {
                            if let Some(done) = self.close_game() {
                                exit = done;
                            }
                            ui.close();
                        }
                    });
                    ui.menu_button("Edit", |ui| {
                        let undo = self.state.history.undo_name(&self.state.world);
                        let redo = self.state.history.redo_name(&self.state.world);
                        let u = undo.map_or("Undo".to_string(), |n| format!("Undo {n}"));
                        let r = redo.map_or("Redo".to_string(), |n| format!("Redo {n}"));
                        if let Some(a) = self.actions.get("edit.undo") {
                            let enabled = a.is_enabled(&self.state);
                            if theme::add_enabled(
                                ui,
                                enabled,
                                egui::Button::new(u).shortcut_text(self.keymap.label("edit.undo")),
                            )
                            .clicked()
                            {
                                self.run_action("edit.undo");
                                ui.close();
                            }
                        }
                        if let Some(a) = self.actions.get("edit.redo") {
                            let enabled = a.is_enabled(&self.state);
                            if theme::add_enabled(
                                ui,
                                enabled,
                                egui::Button::new(r).shortcut_text(self.keymap.label("edit.redo")),
                            )
                            .clicked()
                            {
                                self.run_action("edit.redo");
                                ui.close();
                            }
                        }
                        ui.separator();
                        self.actions.menu_items(
                            ui,
                            &[
                                "edit.duplicate",
                                "edit.delete",
                                "edit.group",
                                "edit.rename",
                                "edit.select_all",
                            ],
                            &mut self.state,
                            &self.keymap,
                        );
                        ui.separator();
                        self.actions.menu_items(
                            ui,
                            &[
                                "build.rotate90",
                                "build.tilt90",
                                "build.drop",
                                "build.snap_to_grid",
                                "pivot.reset",
                            ],
                            &mut self.state,
                            &self.keymap,
                        );
                        ui.separator();
                        self.actions.menu_items(
                            ui,
                            &[
                                "select.similar",
                                "select.mode_box",
                                "select.mode_lasso",
                                "select.mode_paint",
                                "colour.open",
                            ],
                            &mut self.state,
                            &self.keymap,
                        );
                    });
                    ui.menu_button("View", |ui| {
                        self.actions.menu_items(
                            ui,
                            &[
                                "view.frame_selection",
                                "view.frame_all",
                                "snap.soft",
                                crate::editor_input_actions::TOGGLE_ID,
                            ],
                            &mut self.state,
                            &self.keymap,
                        );
                        ui.separator();
                        ui.checkbox(&mut self.context_bar.enabled, "Contextual toolbar");
                    });
                    ui.menu_button("Build", |ui| {
                        self.actions.menu_items(
                            ui,
                            &[
                                "build.align",
                                "build.distribute",
                                "build.mirror",
                                "build.mirror_copy",
                                "build.array",
                                "tool.brush",
                                "tool.measure",
                            ],
                            &mut self.state,
                            &self.keymap,
                        );
                        ui.separator();
                        self.tools.mirror_axis_menu(ui);
                    });
                    ui.menu_button("Insert", |ui| {
                        for &class in insertable() {
                            let button = theme::add_enabled(ui, !playing, egui::Button::new(class));
                            if with_class_tooltip(button, class).clicked() {
                                let parent = self
                                    .state
                                    .selection
                                    .active()
                                    .filter(|&a| !self.state.world.is_a(a, "BasePart"));
                                let at = self.state.insert_point;
                                self.state.insert(class, parent, Some(at));
                                ui.close();
                            }
                        }
                    });
                    ui.menu_button("Test", |ui| {
                        let ids: Vec<&'static str> = self
                            .actions
                            .iter()
                            .filter(|a| a.group == ToolbarGroup::Play)
                            .map(|a| a.id)
                            .collect();
                        self.actions
                            .menu_items(ui, &ids, &mut self.state, &self.keymap);
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let dirty = self.state.is_dirty();
                        ui.label(
                            RichText::new(self.state.name.clone()).font(theme::fonts::strong()),
                        );
                        if dirty {
                            ui.label(RichText::new("•").color(theme::warning()))
                                .on_hover_text("Unsaved changes");
                        }
                    });
                });
            });
        // The command palette hangs from the menu bar, over the whole toolbar.
        self.palette.hang_from(menu.response.rect.bottom());

        egui::Panel::top("toolbar")
            .exact_size(theme::CONTROL_HEIGHT_LG)
            .frame(
                Frame::new()
                    .fill(theme::bg_panel())
                    .inner_margin(Margin::symmetric(8, 4)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.spacing_mut().interact_size.y = 28.0;
                    // Edit, Play, View and Build (Tools, Settings and Snap are in the viewport
                    // header). Groups that do not fit collapse into the "»" menu (crate::toolbar).
                    self.toolbar
                        .show(ui, &mut self.actions, &mut self.state, &self.keymap);
                });
            });

        egui::Panel::bottom("status")
            .exact_size(theme::STATUS_HEIGHT)
            .frame(
                Frame::new()
                    .fill(theme::bg_deep())
                    .inner_margin(Margin::symmetric(12, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let dim = theme::text_3();
                    let small = |t: String| RichText::new(t).font(theme::fonts::small());
                    ui.label(small(self.state.tool.label().to_string()).color(theme::text_2()));
                    ui.label(small("·".into()).color(dim));
                    let (play, colour) = match self.state.play.status() {
                        None => ("Editing".into(), theme::text_2()),
                        Some(crate::play::PlayStatus::Starting) => {
                            ("Starting…".into(), theme::success())
                        }
                        Some(crate::play::PlayStatus::Running(t)) => (t, theme::success()),
                        Some(crate::play::PlayStatus::Ended(t)) => (t, theme::text_2()),
                    };
                    ui.label(small(play.into_owned()).color(colour));
                    if let Some(note) = self.state.current_note() {
                        ui.label(small("·".into()).color(dim));
                        ui.label(small(note.to_string()).color(theme::text_1()));
                        ctx.request_repaint_after(crate::editor::NOTE_TIME);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (save, colour) = if self.state.is_dirty() {
                            ("Unsaved changes", theme::warning())
                        } else if self.state.can_save {
                            ("Saved", theme::text_3())
                        } else {
                            ("Not saved to disk", theme::text_3())
                        };
                        ui.label(small(save.to_string()).color(colour));
                        let n = self.state.selection.len();
                        let mut text = selection_label(&self.state);
                        ui.label(small("·".into()).color(dim));
                        if n > 0
                            && let Some(b) = self.state.selection_bounds()
                        {
                            let s = b.size();
                            text.push_str(&format!(
                                " · {} x {} x {}",
                                fmt(s.x),
                                fmt(s.y),
                                fmt(s.z)
                            ));
                        }
                        ui.label(small(text).monospace().color(theme::text_2()));
                        // The drag readout (spec 3.3 and 4.5): the amount, editable for a few
                        // seconds after release (accent), and what the drag snapped to.
                        if let Some(st) = self.viewport.status(&self.state) {
                            ui.label(small("·".into()).color(dim));
                            if let Some(snap) = st.snap {
                                ui.label(small(snap).color(dim));
                            }
                            let colour = if st.editing {
                                theme::accent()
                            } else {
                                theme::text_1()
                            };
                            ui.label(small(st.text).monospace().color(colour));
                        }
                    });
                });
            });

        egui::Panel::left("explorer")
            .default_size(280.0)
            .min_size(220.0)
            .resizable(true)
            .frame(panel::frame())
            .show(ui, |ui| {
                panel::show(
                    ui,
                    "Explorer",
                    |_| {},
                    |ui| {
                        if let Some(view) = self.state.play.view() {
                            playing_banner(ui, "Play world, read-only. Stop returns to editing.");
                            let tree = &mut self.play_tree;
                            view.inspect_world(&mut |world| tree.ui(ui, world));
                        } else if playing {
                            playing_banner(ui, "Playing. Stop to edit.");
                            ui.add_enabled_ui(false, |ui| self.explorer.ui(ui, &mut self.state));
                        } else {
                            self.explorer.ui(ui, &mut self.state);
                        }
                    },
                );
            });
        egui::Panel::right("properties")
            .default_size(304.0)
            .min_size(260.0)
            .resizable(true)
            .frame(panel::frame())
            .show(ui, |ui| {
                panel::show(
                    ui,
                    "Properties",
                    |_| {},
                    |ui| {
                        if playing {
                            playing_banner(ui, "Read-only while playing. Stop to edit.");
                            ui.add_enabled_ui(false, |ui| self.properties.ui(ui, &mut self.state));
                        } else {
                            // The owner may edit during its own gesture; the flag is its state
                            // after it ran (see EditorState::gesture_active).
                            self.state.set_gesture(Gesture::Properties, false);
                            self.properties.ui(ui, &mut self.state);
                            self.state
                                .set_gesture(Gesture::Properties, self.properties.is_busy());
                        }
                    },
                );
            });
        if let Some((prop, at)) = self.properties.take_colour_request() {
            let pos = at.left_top() - egui::vec2(crate::colour::popover::WIDTH + 12.0, 120.0);
            self.colour.open(&self.state, &prop, pos);
        }
        if self.input_actions.is_open() {
            egui::Panel::right("input_actions")
                .default_size(560.0)
                .min_size(420.0)
                .resizable(true)
                .frame(panel::frame())
                .show(ui, |ui| self.input_actions.ui(ui, &mut self.state));
        } else {
            self.input_actions.follow_history(&mut self.state);
        }
        egui::Panel::bottom("output")
            .default_size(200.0)
            .min_size(120.0)
            .resizable(true)
            .frame(panel::frame())
            .show(ui, |ui| {
                panel::show(ui, "Output", |_| {}, |ui| self.output.ui(ui));
            });

        self.drag_keys(&ctx);
        let mut viewport_hovered = false;
        egui::CentralPanel::default()
            .frame(Frame::new().fill(theme::bg_deep()))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 0.0;
                self.scripts.prune(&self.state);
                let scene = self.scripts.tab_strip(ui, &mut self.state);
                if scene {
                    self.header
                        .ui(ui, &mut self.actions, &mut self.state, &self.keymap);
                    let before = ui.available_rect_before_wrap();
                    self.viewport.capture_pointer = self.tools.captures_pointer(&self.state);
                    self.viewport.capture_wheel = self.tools.captures_wheel();
                    // Each gesture owner may edit during its own gesture; the flag is its
                    // state after it ran (see EditorState::gesture_active).
                    self.state.set_gesture(Gesture::Viewport, false);
                    self.viewport.ui(ui, &mut self.state, render_state);
                    self.state
                        .set_gesture(Gesture::Viewport, self.viewport.is_dragging());
                    viewport_hovered = ui.rect_contains_pointer(before);
                    // The tools read the picker while they edit the state; it is put back after.
                    let picks = std::mem::take(&mut self.state.picks);
                    let cx = ViewContext {
                        rect: self.viewport.rect(),
                        view: self.viewport.view(),
                        picks: &picks,
                        pointer: ui.ctx().pointer_latest_pos().filter(|_| viewport_hovered),
                        hovered: self.viewport.hovered(),
                    };
                    self.state.set_gesture(Gesture::Tool, false);
                    self.tools.viewport(ui, &mut self.state, &cx);
                    self.state.set_gesture(Gesture::Tool, self.tools.gesture());
                    self.state.picks = picks;
                } else {
                    self.scripts.editor(ui, &mut self.state);
                }
            });

        // The colour popover first: it is the topmost and takes its Esc before anything below.
        self.colour
            .show(&ctx, &mut self.state, self.viewport.hovered());
        // Then the style picker: its Esc, arrows, Space and Enter come before the tools' keys.
        self.style_picker
            .show(&ctx, &mut self.state, self.viewport.rect(), render_state);
        self.tools.ui(&ctx, &mut self.state);
        // Then the tool popovers' and the brush/measure keys (Esc, Enter, letters, [ ]), before
        // the contextual toolbar and the hint card can take Esc.
        let right_down = ctx.input(|i| i.pointer.secondary_down());
        if !played {
            self.tool_keys(&ctx, viewport_hovered, right_down);
        }

        let top = self.viewport.rect().top();
        // Hidden while a building tool has the pointer (brush, measure, lasso, paint, array).
        self.context_bar.suppressed = self.tools.captures_pointer(&self.state);
        self.context_bar.ui(
            &ctx,
            &mut self.state,
            &self.viewport,
            &mut self.actions,
            &self.keymap,
            top,
        );

        self.watch_hints();
        let blocked = self.hints_blocked() || self.palette.is_open();
        self.hints.ui(&ctx, self.viewport.rect(), blocked);

        // Last of the overlays, so it draws over the context bar and hints.
        if let Some(choice) = self
            .palette
            .show(&ctx, &self.state, &self.actions, &self.keymap)
        {
            self.apply_palette(&ctx, choice);
        }
        if !played {
            let right_down = ctx.input(|i| i.pointer.secondary_down());
            self.keys(&ctx, viewport_hovered, right_down);
            self.state.set_gesture(Gesture::Tool, self.tools.gesture());
        }
        if let Some(done) = self.requests(&ctx) {
            exit = done;
        }

        if let Some(leave) = self.confirm {
            let done = match leave {
                Leave::Close => EditorExit::Close,
                Leave::Quit => EditorExit::Quit,
            };
            egui::Window::new("Save changes?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(&ctx, |ui| {
                    ui.label(format!("{} has unsaved changes.", self.state.name));
                    if let Some(err) = &self.save_error {
                        ui.label(
                            RichText::new(format!("Could not save: {err}"))
                                .color(ui_tokens().error),
                        );
                    }
                    ui.horizontal(|ui| {
                        if ui.button("Save").clicked() {
                            match self.save() {
                                Ok(()) => {
                                    self.confirm = None;
                                    self.save_error = None;
                                    exit = done;
                                }
                                // Stay open and show why; the game is not closed unsaved.
                                Err(e) => self.save_error = Some(e),
                            }
                        }
                        if ui.button("Don't save").clicked() {
                            self.confirm = None;
                            self.save_error = None;
                            exit = done;
                        }
                        if ui.button("Cancel").clicked() {
                            self.confirm = None;
                            self.save_error = None;
                        }
                    });
                });
        }
        exit
    }
}

/// A full-width note at the top of a panel while playing.
fn playing_banner(ui: &mut egui::Ui, text: &str) {
    let width = ui.available_width();
    let galley = ui.painter().layout(
        text.to_string(),
        ui_tokens().small(),
        theme::success(),
        width - 16.0,
    );
    let pad = egui::vec2(8.0, 4.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, galley.size().y + pad.y * 2.0),
        egui::Sense::hover(),
    );
    Plate::panel()
        .fill(theme::success_bg())
        .paint(ui.painter(), rect);
    ui.painter()
        .galley(rect.min + pad, galley, theme::success());
    ui.add_space(4.0);
}

fn fmt(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r}")
    }
}

/// The status bar's selection text: the selected instance's name, "N selected" for several,
/// "Nothing selected" for none. (It never names the part under the pointer.)
fn selection_label(state: &EditorState) -> String {
    match state.selection.len() {
        0 => "Nothing selected".to_string(),
        1 => state
            .selection
            .active()
            .map_or_else(|| "1 selected".to_string(), |id| state.display_name(id)),
        n => format!("{n} selected"),
    }
}

#[cfg(test)]
mod tests {
    use super::selection_label;
    use crate::editor::EditorState;
    use crate::output::OutputPanel;
    use metallic_core::World;

    #[test]
    fn status_names_the_selection_not_the_hover() {
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let a = world.create("Part", Some(ws)).unwrap();
        let b = world.create("Part", Some(ws)).unwrap();
        world
            .set(a, "Name", metallic_core::Value::String("Jump1".to_string()))
            .unwrap();
        let output = OutputPanel::new();
        let mut state = EditorState::new(
            world,
            std::path::Path::new("."),
            std::path::Path::new("unused.mscene"),
            "Test",
            output.log.sink(),
        );
        assert_eq!(selection_label(&state), "Nothing selected");
        state.selection.select_only(a);
        assert_eq!(selection_label(&state), "Jump1");
        state.selection.select_only(a);
        state.selection.toggle(b);
        assert_eq!(selection_label(&state), "2 selected");
    }
}

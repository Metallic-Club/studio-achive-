//! The building tools in the editor: the pure cores of [`crate::tools`] wired to the toolbar,
//! the keymap, the viewport and the undo history.
//!
//! | Action | Key | What it does |
//! |---|---|---|
//! | `tool.select` | Q | Select tool; Q again cycles Box, Lasso, Paint ([`select`]) |
//! | `tool.brush` | B | Scatter brush ([`brush`]): drag paints, Shift-drag erases, `[` `]` radius |
//! | `tool.measure` | Shift+M | Measure ([`measure`]): click A, click B; Esc clears; Alt-hover gaps |
//! | `build.align`, `build.distribute` | A | Align and distribute popover ([`arrange`]) |
//! | `build.mirror`, `build.mirror_copy` | Ctrl+M, Ctrl+Shift+M | Mirror across the pivot plane |
//! | `build.array` | Ctrl+Shift+D | Array popover: linear (with the drag handle), radial, grid, path |
//! | `select.similar` | Shift+G | Select similar popover, then C M L S K N T P |
//!
//! Every operation reads parts through [`crate::tools::geom::snapshot`] (directly or through
//! [`crate::tools::arrange::read_objects`]) and commits its result as one undo step
//! ([`commit`]): a `Batch` of property changes plus the copies it made, already applied and then
//! recorded, so undo puts the world back exactly. Bad input and caps (too many copies, too few
//! objects) go to Output and the status bar; nothing here panics on user input.
//!
//! Tool state lives in one [`ToolState`] shared by the action handlers and [`EditorTools`].
//! While a tool owns the pointer (brush, measure, lasso, paint, array path points or the array
//! handle) the editor sets [`crate::viewport::Viewport::capture_pointer`] and the tool paints its
//! overlay over the viewport ([`EditorTools::viewport`]).
//!
//! **Keys.** The tools read no keys themselves. Their keys are keymap rows in two contexts the
//! editor checks first ([`EditorTools::popover_keys`], [`EditorTools::tool_keys`]): with a
//! popover open, Esc closes it (`popover.close`), Enter makes the array (`array.make`) and the
//! Select similar letters run (`similar.*`); with the brush or Measure on over the viewport, Esc
//! cancels the stroke or clears (`tool.cancel`) and `[` `]` change the radius
//! (`brush.radius_*`). So text fields and right-mouse fly keep their keys, and Esc reaches the
//! topmost thing first: the colour popover, then a building popover, then the tool, then the
//! selection.
//!
//! **Gestures.** A brush stroke, a lasso or paint trail and an array handle drag are gestures
//! ([`EditorTools::gesture`], reported as [`crate::editor::Gesture::Tool`]): Save, Play, Undo and
//! other edits wait for them. A stroke is cancelled (nothing was written yet) by Esc, a tool
//! change, Play, or the window losing focus.

pub mod arrange;
pub mod brush;
pub mod commit;
pub mod measure;
mod overlay;
pub mod select;

use std::cell::RefCell;
use std::rc::Rc;

use eframe::egui::{self, Pos2, Rect};
use metallic_core::InstanceId;

use crate::actions::{Action, ActionRegistry, ToolbarGroup};
use crate::camera::ViewProjection;
use crate::editor::{EditorState, Tool};
use crate::icons::Icon;
use crate::output::{Level, Tag};
use crate::picking::PickCache;
use crate::tools::measure::MeasureTool;
use crate::tools::select::SelectMode;
use crate::tools::select::SimilarKey;

/// A tool that replaces the gizmo tools while it is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modal {
    /// The scatter brush (B).
    Brush,
    /// Measure (Shift+M).
    Measure,
}

/// Which building popover is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Popover {
    /// Align and distribute (A).
    Align,
    /// Array (Ctrl+Shift+D).
    Array,
    /// Select similar (Shift+G).
    Similar,
}

/// Everything the building tools remember between frames.
#[derive(Default)]
pub struct ToolState {
    /// Brush or Measure, when on.
    pub modal: Option<Modal>,
    /// The Select tool's mode.
    pub select_mode: SelectMode,
    /// The open popover.
    pub popover: Option<Popover>,
    /// Align, distribute, mirror and array settings.
    pub arrange: arrange::ArrangeSettings,
    /// The brush set, settings and stroke.
    pub brush: brush::BrushState,
    /// Measurements on screen.
    pub measure: MeasureTool,
    /// The lasso or paint-select trail.
    pub select: select::Gesture,
}

/// The view the overlay tools work in this frame.
pub struct ViewContext<'a> {
    /// The viewport's rectangle on screen.
    pub rect: Rect,
    /// The camera's projection (screen points relative to `rect.min`).
    pub view: ViewProjection,
    /// The part picker.
    pub picks: &'a PickCache,
    /// The pointer, when it is over the viewport and no window covers it.
    pub pointer: Option<Pos2>,
    /// The part under the pointer.
    pub hovered: Option<InstanceId>,
}

impl ViewContext<'_> {
    /// `p` relative to the viewport, as the projection wants it.
    pub fn local(&self, p: Pos2) -> (f64, f64) {
        (
            f64::from(p.x - self.rect.min.x),
            f64::from(p.y - self.rect.min.y),
        )
    }

    /// Where world point `p` is on screen.
    pub fn project(&self, p: glam::DVec3) -> Option<Pos2> {
        self.view
            .project(p)
            .map(|(x, y)| self.rect.min + egui::vec2(x as f32, y as f32))
    }

    /// The ray under screen point `p`.
    pub fn ray(&self, p: Pos2) -> crate::transform::Ray {
        let (x, y) = self.local(p);
        self.view.ray(x, y)
    }
}

/// Shows a problem in Output and the status bar (never a panic).
pub fn report(state: &mut EditorState, text: impl Into<String>) {
    let text = text.into();
    state.output.push(Tag::Studio, Level::Warning, text.clone());
    state.note(text);
}

/// The building tools of one editor window.
pub struct EditorTools {
    shared: Rc<RefCell<ToolState>>,
}

fn editing(s: &EditorState) -> bool {
    s.edit_refusal().is_none()
}

impl ToolState {
    /// Leaves Brush and Measure: the stroke in progress is dropped, measurements cleared.
    fn leave_modal(&mut self) {
        self.modal = None;
        self.measure.clear();
        self.brush.cancel_stroke();
    }

    /// Closes the open popover (and stops picking path points or dragging the handle).
    fn close_popover(&mut self) {
        self.popover = None;
        self.arrange.picking_points = false;
        self.arrange.cancel_handle();
    }

    /// Esc for the brush and Measure: cancel the stroke, else clear, else leave the tool.
    fn cancel(&mut self, s: &mut EditorState) {
        if self.brush.cancel_stroke() {
            s.note("Brush stroke cancelled".into());
        } else if self.select.is_active() {
            self.select.cancel();
        } else if self.modal == Some(Modal::Measure)
            && (self.measure.measurements().len() > 0 || self.measure.pending().is_some())
        {
            self.measure.clear();
        } else {
            self.leave_modal();
        }
    }
}

fn selected(n: usize) -> impl Fn(&EditorState) -> bool {
    move |s: &EditorState| editing(s) && s.selection.len() >= n
}

impl EditorTools {
    /// Registers the building actions in `actions` (replacing the plain tool actions so a tool
    /// change also leaves Brush and Measure).
    pub fn new(actions: &mut ActionRegistry) -> EditorTools {
        let shared = Rc::new(RefCell::new(ToolState::default()));
        let tools = EditorTools { shared };
        tools.register(actions);
        tools
    }

    /// The shared tool state (for tests and the menus).
    pub fn state(&self) -> std::cell::RefMut<'_, ToolState> {
        self.shared.borrow_mut()
    }

    fn register(&self, r: &mut ActionRegistry) {
        let sh = &self.shared;
        // Select: Q again cycles its mode.
        let (a, b) = (sh.clone(), sh.clone());
        r.register(
            Action::new(
                "tool.select",
                "Select",
                Icon::Select,
                ToolbarGroup::Tools,
                move |s| {
                    let mut t = a.borrow_mut();
                    if s.tool == Tool::Select && t.modal.is_none() {
                        t.select_mode = t.select_mode.next();
                        s.note(format!("Select: {}", t.select_mode.label()));
                    }
                    t.leave_modal();
                    t.select.cancel();
                    s.tool = Tool::Select;
                },
            )
            .checked(move |s| s.tool == Tool::Select && b.borrow().modal.is_none()),
        );
        for (id, label, icon, tool) in [
            ("tool.move", "Move", Icon::Move, Tool::Move),
            ("tool.rotate", "Rotate", Icon::Rotate, Tool::Rotate),
            ("tool.scale", "Scale", Icon::Scale, Tool::Scale),
            (
                "tool.transform",
                "Transform",
                Icon::Transform,
                Tool::Transform,
            ),
        ] {
            let (a, b) = (sh.clone(), sh.clone());
            r.register(
                Action::new(id, label, icon, ToolbarGroup::Tools, move |s| {
                    let mut t = a.borrow_mut();
                    t.leave_modal();
                    t.select.cancel();
                    s.tool = tool;
                })
                .checked(move |s| s.tool == tool && b.borrow().modal.is_none()),
            );
        }
        for (id, label, icon, modal) in [
            ("tool.brush", "Brush", Icon::Brush, Modal::Brush),
            ("tool.measure", "Measure", Icon::Measure, Modal::Measure),
        ] {
            let (a, b) = (sh.clone(), sh.clone());
            r.register(
                // Icon buttons with the building actions: the labelled Tools group is full.
                Action::new(id, label, icon, ToolbarGroup::Build, move |s| {
                    let mut t = a.borrow_mut();
                    if t.modal == Some(modal) {
                        t.leave_modal();
                    } else {
                        t.leave_modal();
                        t.modal = Some(modal);
                        if modal == Modal::Brush && t.brush.set.is_none() {
                            brush::use_selection(&mut t.brush, s);
                        }
                    }
                })
                .enabled(editing)
                .checked(move |_| b.borrow().modal == Some(modal)),
            );
        }
        for (mode, id, label, icon) in [
            (
                SelectMode::Box,
                "select.mode_box",
                "Box select",
                Icon::SelectBox,
            ),
            (
                SelectMode::Lasso,
                "select.mode_lasso",
                "Lasso select",
                Icon::SelectLasso,
            ),
            (
                SelectMode::Paint,
                "select.mode_paint",
                "Paint select",
                Icon::SelectPaint,
            ),
        ] {
            let (a, b) = (sh.clone(), sh.clone());
            r.register(
                Action::new(id, label, icon, ToolbarGroup::Hidden, move |s| {
                    let mut t = a.borrow_mut();
                    t.leave_modal();
                    t.select.cancel();
                    t.select_mode = mode;
                    s.tool = Tool::Select;
                })
                .checked(move |s| s.tool == Tool::Select && b.borrow().select_mode == mode),
            );
        }
        let popover = |id: &'static str, label: &str, icon, group, which, need: usize| {
            let a = sh.clone();
            Action::new(id, label, icon, group, move |_| {
                let mut t = a.borrow_mut();
                t.popover = if t.popover == Some(which) {
                    None
                } else {
                    Some(which)
                };
            })
            .enabled(selected(need))
        };
        r.register(popover(
            "build.align",
            "Align",
            Icon::Align,
            ToolbarGroup::Build,
            Popover::Align,
            2,
        ));
        r.register(popover(
            "build.distribute",
            "Distribute",
            Icon::Distribute,
            ToolbarGroup::Hidden,
            Popover::Align,
            3,
        ));
        for (id, label, icon, copy) in [
            ("build.mirror", "Mirror", Icon::Mirror, false),
            ("build.mirror_copy", "Mirror copy", Icon::MirrorCopy, true),
        ] {
            let a = sh.clone();
            let group = if copy {
                ToolbarGroup::Hidden
            } else {
                ToolbarGroup::Build
            };
            r.register(
                Action::new(id, label, icon, group, move |s| {
                    let axis = a.borrow().arrange.mirror_axis;
                    arrange::run_mirror(s, axis, copy);
                })
                .enabled(selected(1)),
            );
        }
        r.register(popover(
            "build.array",
            "Array",
            Icon::Array,
            ToolbarGroup::Build,
            Popover::Array,
            1,
        ));
        r.register(popover(
            "select.similar",
            "Select similar",
            Icon::SelectSimilar,
            ToolbarGroup::Build,
            Popover::Similar,
            1,
        ));
        self.register_keys(r);
    }

    /// The keymap rows of the popovers and the brush/measure tool (see the module docs).
    fn register_keys(&self, r: &mut ActionRegistry) {
        let sh = &self.shared;
        let hidden = |id, label, f: Box<dyn FnMut(&mut EditorState)>| {
            let mut f = f;
            Action::new(id, label, Icon::Keyboard, ToolbarGroup::Hidden, move |s| {
                f(s)
            })
        };
        let a = sh.clone();
        r.register(hidden(
            "popover.close",
            "Close popover",
            Box::new(move |_| a.borrow_mut().close_popover()),
        ));
        let (a, b) = (sh.clone(), sh.clone());
        r.register(
            hidden(
                "array.make",
                "Make the array",
                Box::new(move |s| {
                    let mut t = a.borrow_mut();
                    if arrange::run_array(s, &mut t.arrange) {
                        t.close_popover();
                    }
                }),
            )
            .enabled(move |s| editing(s) && b.borrow().popover == Some(Popover::Array)),
        );
        for (id, key) in select::SIMILAR_ACTIONS {
            let (a, b) = (sh.clone(), sh.clone());
            r.register(
                hidden(
                    id,
                    similar_label(key),
                    Box::new(move |s| {
                        select::run_similar(s, key);
                        a.borrow_mut().popover = None;
                    }),
                )
                .enabled(move |s| editing(s) && b.borrow().popover == Some(Popover::Similar)),
            );
        }
        let a = sh.clone();
        r.register(hidden(
            "tool.cancel",
            "Cancel the tool",
            Box::new(move |s| a.borrow_mut().cancel(s)),
        ));
        for (id, label, bigger) in [
            ("brush.radius_down", "Smaller brush", false),
            ("brush.radius_up", "Bigger brush", true),
        ] {
            let (a, b) = (sh.clone(), sh.clone());
            r.register(
                hidden(
                    id,
                    label,
                    Box::new(move |_| a.borrow_mut().brush.step_radius(bigger)),
                )
                .enabled(move |_| b.borrow().modal == Some(Modal::Brush)),
            );
        }
    }

    /// True while a building popover is open: the editor checks
    /// [`crate::keymap::KeyContext::Popover`] first.
    pub fn popover_keys(&self) -> bool {
        self.shared.borrow().popover.is_some()
    }

    /// True when [`crate::keymap::KeyContext::Tool`] is live: the brush or Measure is on and the
    /// pointer is over the viewport, or a tool gesture is held.
    pub fn tool_keys(&self, viewport_hovered: bool) -> bool {
        let t = self.shared.borrow();
        (t.modal.is_some() && viewport_hovered) || gesture_of(&t)
    }

    /// Ends every tool gesture without its change (Play starting): the brush stroke is dropped,
    /// the trail cleared, the array handle put back.
    pub fn cancel_gestures(&self) {
        let mut t = self.shared.borrow_mut();
        t.brush.cancel_stroke();
        t.select.cancel();
        t.arrange.cancel_handle();
    }

    /// True while a tool gesture is held: a brush stroke, a lasso or paint trail, an array
    /// handle drag.
    pub fn gesture(&self) -> bool {
        gesture_of(&self.shared.borrow())
    }

    /// The Build menu's "Mirror axis" choice (remembered for Ctrl+M and Ctrl+Shift+M).
    pub fn mirror_axis_menu(&self, ui: &mut egui::Ui) {
        use crate::tools::arrange::Axis;
        let mut t = self.shared.borrow_mut();
        ui.horizontal(|ui| {
            ui.label("Mirror axis");
            for (i, a) in Axis::ALL.into_iter().enumerate() {
                let text = egui::RichText::new(["X", "Y", "Z"][i])
                    .color(crate::tokens::ui_tokens().axis[i]);
                if ui
                    .selectable_label(t.arrange.mirror_axis == a, text)
                    .clicked()
                {
                    t.arrange.mirror_axis = a;
                }
            }
        });
    }

    /// True when a tool owns left clicks and drags in the viewport this frame.
    pub fn captures_pointer(&self, state: &EditorState) -> bool {
        if state.play.is_playing() {
            return false;
        }
        let t = self.shared.borrow();
        t.modal.is_some()
            || (state.tool == Tool::Select && t.select_mode != SelectMode::Box)
            || (t.popover == Some(Popover::Array) && t.arrange.wants_pointer())
    }

    /// True when a tool owns the wheel in the viewport this frame (over the array's drag handle).
    pub fn captures_wheel(&self) -> bool {
        let t = self.shared.borrow();
        t.popover == Some(Popover::Array) && t.arrange.wheel_on_handle()
    }

    /// Draws the open popover and the brush header (call once per frame, outside the panels).
    pub fn ui(&mut self, ctx: &egui::Context, state: &mut EditorState) {
        let mut t = self.shared.borrow_mut();
        if state.play.is_playing() {
            t.close_popover();
            t.leave_modal();
            t.select.cancel();
            return;
        }
        let t = &mut *t;
        match t.popover {
            Some(Popover::Align) => {
                arrange::align_window(ctx, state, &mut t.arrange, &mut t.popover)
            }
            Some(Popover::Array) => {
                arrange::array_window(ctx, state, &mut t.arrange, &mut t.popover)
            }
            Some(Popover::Similar) => select::similar_window(ctx, state, &mut t.popover),
            None => {}
        }
        if t.modal == Some(Modal::Brush) {
            brush::header_window(ctx, state, &mut t.brush, &mut t.modal);
        }
    }

    /// Handles the pointer for the active tool and paints its overlay over the viewport. Call
    /// right after the viewport drew, in the same `Ui`.
    pub fn viewport(&mut self, ui: &mut egui::Ui, state: &mut EditorState, cx: &ViewContext<'_>) {
        if state.play.is_playing() {
            return;
        }
        let painter = ui.painter_at(cx.rect);
        let mut t = self.shared.borrow_mut();
        let t = &mut *t;
        // A gesture whose tool is no longer active, or a window that lost focus, ends without
        // effect: nothing stays pending to commit later.
        let focused = ui.input(|i| i.focused);
        if !focused || t.modal != Some(Modal::Brush) {
            t.brush.cancel_stroke();
        }
        let lasso =
            t.modal.is_none() && state.tool == Tool::Select && t.select_mode != SelectMode::Box;
        if !focused || !lasso {
            t.select.cancel();
        }
        if !focused || t.popover != Some(Popover::Array) {
            t.arrange.cancel_handle();
        }
        match t.modal {
            Some(Modal::Brush) => brush::viewport(ui, state, cx, &painter, &mut t.brush),
            Some(Modal::Measure) => measure::viewport(ui, state, cx, &painter, &mut t.measure),
            None if lasso => {
                select::viewport(ui, state, cx, &painter, t.select_mode, &mut t.select);
            }
            None => {}
        }
        measure::gap_readout(ui, state, cx, &painter);
        if t.popover == Some(Popover::Array) {
            arrange::array_viewport(ui, state, cx, &painter, &mut t.arrange);
        }
    }
}

fn gesture_of(t: &ToolState) -> bool {
    t.brush.has_stroke() || t.select.is_active() || t.arrange.handle_active()
}

fn similar_label(key: SimilarKey) -> &'static str {
    match key {
        SimilarKey::Color => "Select similar: colour",
        SimilarKey::Material => "Select similar: material",
        SimilarKey::Style => "Select similar: look",
        SimilarKey::Size => "Select similar: size",
        SimilarKey::Class => "Select similar: class",
        SimilarKey::Name => "Select similar: name",
        SimilarKey::Tags => "Select similar: tags",
        SimilarKey::Parent => "Select similar: parent",
    }
}

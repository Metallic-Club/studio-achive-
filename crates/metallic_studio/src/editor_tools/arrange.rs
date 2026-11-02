//! Align, distribute, mirror and array in the editor (spec 6.1, 6.2, 7, 8.2).
//!
//! - **Align popover** (A): "Relative to" Active object / Selection bounds / Grid, then Min,
//!   Centre, Max per axis; a click aligns at once (one undo step) and keeps the popover open.
//!   Distribute X, Y, Z by centres or gaps, with an optional exact gap; needs 3 objects.
//! - **Mirror** (Ctrl+M in place, Ctrl+Shift+M copy) across the plane through the selection's
//!   pivot, normal to the remembered axis (Build menu).
//! - **Array popover** (Ctrl+Shift+D): Linear, Radial, Grid and Path. Ghost copies preview at
//!   60% while fields change; Linear has the drag handle past the last ghost (drag along the
//!   axis to add or remove copies); Path takes points clicked in the viewport. "Make N copies"
//!   (or Enter) commits one undo step, grouped under a Model "Array" when Group result is on.
//!
//! Each operation runs on [`read_objects`] (built on `tools::geom::snapshot`) and commits
//! through [`commit::commit_plan`]. Errors (too few objects, the copy cap, bad fields) show in
//! the popover and in Output.
use eframe::egui::{self, Pos2, Stroke};
use glam::{DMat3, DVec3};

use super::commit;
use super::{Popover, ViewContext, overlay, report};
use crate::editor::EditorState;
use crate::theme;
use crate::tools::arrange::array::{
    AngleField, MAX_COPIES, count_from_drag, default_linear_offset, drag_along, handle_position,
    linear_step, parse_angle, parse_count,
};
use crate::tools::arrange::scene::{selection_bounds_in, selection_pivot};
use crate::tools::arrange::{
    AlignOptions, Axis, DistributeMode, DistributeOptions, Edge, GridOptions, LinearOptions,
    MirrorOptions, MirrorPlane, OffsetIs, PathFill, PathOptions, Plan, RadialCentre, RadialOptions,
    RelativeTo, Space, align, along_path, distribute, grid, linear, mirror, read_objects,
};
use crate::tools::geom::rotation_from_orientation_degrees;

/// Most ghost parts drawn by the array preview.
const MAX_GHOSTS: usize = 2000;

/// Which array the popover makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArrayMode {
    /// Copies along an offset.
    #[default]
    Linear,
    /// Copies around an axis.
    Radial,
    /// Copies on a 3D grid.
    Grid,
    /// Copies along a curve through clicked points.
    Path,
}

/// The options of the array being edited.
#[derive(Debug, Clone, PartialEq)]
pub enum ArrayRequest {
    /// Linear.
    Linear(LinearOptions),
    /// Radial.
    Radial(RadialOptions),
    /// Grid.
    Grid(GridOptions),
    /// Path.
    Path(PathOptions),
}

/// What "Relative to" is set to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Relative {
    /// The active object.
    #[default]
    Active,
    /// The selection's bounds.
    Selection,
    /// The grid.
    Grid,
}

struct HandleDrag {
    start_count: usize,
    origin: DVec3,
    from: DVec3,
    step: DVec3,
}

/// What the cached array preview was computed for: selection, world and fields.
type PreviewKey = (u64, metallic_core::ChangeCursor, ArrayRequest);

/// Align, distribute, mirror and array settings, kept while the editor is open.
pub struct ArrangeSettings {
    /// Align reference.
    pub relative: Relative,
    /// Grid step for "Relative to Grid".
    pub grid_step: f64,
    /// World or the active object's axes.
    pub space: Space,
    /// Distribute by centres or gaps.
    pub distribute_mode: DistributeMode,
    /// Distribute gap field; empty keeps the span.
    pub gap_text: String,
    /// Mirror plane normal.
    pub mirror_axis: Axis,
    /// Array mode.
    pub mode: ArrayMode,
    /// Linear options.
    pub linear: LinearOptions,
    /// Radial options.
    pub radial: RadialOptions,
    /// Grid options.
    pub grid: GridOptions,
    /// Path options (points clicked in the viewport).
    pub path: PathOptions,
    /// Count field text (accepts `5x`).
    pub count_text: String,
    /// Radial angle text (accepts `/7`).
    pub angle_text: String,
    /// While on, viewport clicks add path points.
    pub picking_points: bool,
    /// What the linear default offset was taken from: selection revision and last duplicate offset.
    linear_for: Option<(u64, Option<DVec3>)>,
    handle: Option<HandleDrag>,
    handle_hover: bool,
    /// Where the wheel last changed the count on the handle. Each notch moves the ring away from
    /// the pointer, so the wheel stays with the array until the pointer moves.
    wheel_latch: Option<Pos2>,
    preview: Option<(PreviewKey, Result<Plan, String>)>,
    /// The selection's pivot and size for the drag handle, cached per selection and world.
    handle_basis: Option<(BasisKey, Option<(DVec3, DVec3)>)>,
}

/// What the cached handle basis was read for: selection revision and world change cursor.
type BasisKey = (u64, metallic_core::ChangeCursor);

impl Default for ArrangeSettings {
    fn default() -> Self {
        ArrangeSettings {
            relative: Relative::Active,
            grid_step: 1.0,
            space: Space::World,
            distribute_mode: DistributeMode::Centres,
            gap_text: String::new(),
            mirror_axis: Axis::X,
            mode: ArrayMode::Linear,
            linear: LinearOptions {
                count: 5,
                offset: DVec3::new(4.0, 0.0, 0.0),
                offset_is: OffsetIs::Distance,
                group: true,
            },
            radial: RadialOptions {
                count: 8,
                angle: 360.0,
                axis: Axis::Y,
                centre: RadialCentre::Pivot,
                rotate_copies: true,
                group: true,
            },
            grid: GridOptions {
                counts: [3, 1, 3],
                spacing: DVec3::new(4.0, 4.0, 4.0),
                group: true,
            },
            path: PathOptions {
                points: Vec::new(),
                fill: PathFill::Count(5),
                align: true,
                up: Axis::Y,
                group: true,
            },
            count_text: "5".into(),
            angle_text: "360".into(),
            picking_points: false,
            linear_for: None,
            handle: None,
            handle_hover: false,
            wheel_latch: None,
            preview: None,
            handle_basis: None,
        }
    }
}

impl ArrangeSettings {
    /// True while the linear drag handle is held.
    pub fn handle_active(&self) -> bool {
        self.handle.is_some()
    }

    /// Ends a handle drag without its change: the count goes back to where the drag began.
    pub fn cancel_handle(&mut self) {
        if let Some(h) = self.handle.take() {
            self.linear.count = h.start_count;
            self.count_text = h.start_count.to_string();
        }
        self.handle_hover = false;
        self.wheel_latch = None;
    }

    /// The selection's pivot and size (cached until the selection or the world changes).
    fn basis(&mut self, state: &EditorState) -> Option<(DVec3, DVec3)> {
        let key = (state.selection.revision(), state.world.change_cursor());
        if let Some((k, b)) = &self.handle_basis
            && *k == key
        {
            return *b;
        }
        let b = read_objects(&state.world, state.selection.ids())
            .ok()
            .filter(|o| !o.is_empty())
            .map(|o| {
                (
                    selection_pivot(&o),
                    selection_bounds_in(&o, &DMat3::IDENTITY).size(),
                )
            });
        self.handle_basis = Some((key, b));
        b
    }

    /// True when the array popover needs viewport clicks (path points, the drag handle).
    pub fn wants_pointer(&self) -> bool {
        (self.mode == ArrayMode::Path && self.picking_points)
            || (self.mode == ArrayMode::Linear && (self.handle_hover || self.handle.is_some()))
    }

    /// True when the wheel belongs to the array: the pointer is over the linear drag handle.
    pub fn wheel_on_handle(&self) -> bool {
        self.mode == ArrayMode::Linear && (self.handle_hover || self.wheel_latch.is_some())
    }

    /// The array the fields describe.
    pub fn request(&self) -> ArrayRequest {
        match self.mode {
            ArrayMode::Linear => ArrayRequest::Linear(self.linear),
            ArrayMode::Radial => ArrayRequest::Radial(self.radial),
            ArrayMode::Grid => ArrayRequest::Grid(self.grid),
            ArrayMode::Path => ArrayRequest::Path(self.path.clone()),
        }
    }

    fn align_relative(&self) -> RelativeTo {
        match self.relative {
            Relative::Active => RelativeTo::Active,
            Relative::Selection => RelativeTo::Selection,
            Relative::Grid => RelativeTo::Grid(self.grid_step),
        }
    }

    /// The array preview for the current selection and fields (cached until either changes).
    fn preview(&mut self, state: &EditorState) -> &Result<Plan, String> {
        let key = (
            state.selection.revision(),
            state.world.change_cursor(),
            self.request(),
        );
        if self.preview.as_ref().is_none_or(|(k, _)| *k != key) {
            let plan = plan_array(state, &key.2);
            self.preview = Some((key, plan));
        }
        &self.preview.as_ref().expect("just set").1
    }
}

/// Plans `req` on the selection.
pub fn plan_array(state: &EditorState, req: &ArrayRequest) -> Result<Plan, String> {
    let objects = read_objects(&state.world, state.selection.ids()).map_err(|e| e.to_string())?;
    match req {
        ArrayRequest::Linear(o) => linear(&objects, o),
        ArrayRequest::Radial(o) => crate::tools::arrange::radial(&objects, o),
        ArrayRequest::Grid(o) => grid(&objects, o),
        ArrayRequest::Path(o) => along_path(&objects, o),
    }
    .map_err(|e| e.to_string())
}

/// Commits `plan`, or reports why it cannot.
fn commit_or_report(
    state: &mut EditorState,
    plan: Result<Plan, String>,
) -> Option<commit::Committed> {
    match plan.and_then(|p| commit::commit_plan(state, &p)) {
        Ok(done) => Some(done),
        Err(e) => {
            report(state, e);
            None
        }
    }
}

/// Aligns the selection on `axis` to `edge` (one undo step).
pub fn run_align(state: &mut EditorState, s: &ArrangeSettings, axis: Axis, edge: Edge) {
    let plan = read_objects(&state.world, state.selection.ids())
        .and_then(|o| {
            align(
                &o,
                &AlignOptions {
                    axis,
                    edge,
                    relative: s.align_relative(),
                    space: s.space,
                },
            )
        })
        .map_err(|e| format!("Align: {e}"));
    commit_or_report(state, plan);
}

/// Distributes the selection on `axis` (one undo step).
pub fn run_distribute(state: &mut EditorState, s: &ArrangeSettings, axis: Axis) {
    let spacing = match s.gap_text.trim() {
        "" => Ok(None),
        t => crate::expr::eval(t)
            .map(Some)
            .ok_or_else(|| format!("Distribute: \"{t}\" is not a number")),
    };
    let plan = spacing.and_then(|spacing| {
        read_objects(&state.world, state.selection.ids())
            .and_then(|o| {
                distribute(
                    &o,
                    &DistributeOptions {
                        axis,
                        mode: s.distribute_mode,
                        spacing,
                        space: s.space,
                    },
                )
            })
            .map_err(|e| format!("Distribute: {e}"))
    });
    commit_or_report(state, plan);
}

/// Mirrors the selection across the pivot plane normal to `axis`; a copy is selected after.
pub fn run_mirror(state: &mut EditorState, axis: Axis, copy: bool) {
    let plan = read_objects(&state.world, state.selection.ids())
        .and_then(|o| {
            mirror(
                &o,
                &MirrorOptions {
                    axis,
                    plane: MirrorPlane::Pivot,
                    copy,
                },
            )
        })
        .map_err(|e| format!("Mirror: {e}"));
    if let Some(done) = commit_or_report(state, plan)
        && copy
        && !done.copies.is_empty()
    {
        state.selection.set(done.copies);
    }
}

/// Makes the array the fields describe (one undo step) and closes the popover.
pub fn run_array(state: &mut EditorState, s: &mut ArrangeSettings) -> bool {
    let plan = plan_array(state, &s.request()).map_err(|e| format!("Array: {e}"));
    let done = commit_or_report(state, plan).is_some();
    if done {
        s.picking_points = false;
        s.handle = None;
    }
    done
}

fn axis_buttons(ui: &mut egui::Ui, value: &mut Axis) {
    for (i, a) in Axis::ALL.into_iter().enumerate() {
        let text = egui::RichText::new(["X", "Y", "Z"][i]).color(overlay::axis_colour(i));
        if ui.selectable_label(*value == a, text).clicked() {
            *value = a;
        }
    }
}

/// The Align and Distribute popover.
pub fn align_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    s: &mut ArrangeSettings,
    popover: &mut Option<Popover>,
) {
    let mut open = true;
    let n = state.selection.len();
    egui::Window::new("Align")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(Pos2::new(420.0, 90.0))
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.label("Relative to");
                crate::widgets::segmented(
                    ui,
                    &mut s.relative,
                    &[
                        (Relative::Active, "Active object"),
                        (Relative::Selection, "Selection"),
                        (Relative::Grid, "Grid"),
                    ],
                );
            });
            if s.relative == Relative::Grid {
                ui.horizontal(|ui| {
                    ui.label("Grid step");
                    ui.add(
                        egui::DragValue::new(&mut s.grid_step)
                            .range(0.05..=512.0)
                            .speed(0.05),
                    );
                });
            }
            ui.horizontal(|ui| {
                ui.label("Axes");
                crate::widgets::segmented(
                    ui,
                    &mut s.space,
                    &[(Space::World, "World"), (Space::Local, "Local")],
                );
            });
            ui.separator();
            let can_align = n >= 2 || (n >= 1 && s.relative == Relative::Grid);
            for (i, axis) in Axis::ALL.into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(["X", "Y", "Z"][i])
                            .color(overlay::axis_colour(i))
                            .strong(),
                    );
                    for (edge, label) in [
                        (Edge::Min, "Min"),
                        (Edge::Centre, "Centre"),
                        (Edge::Max, "Max"),
                    ] {
                        let b = theme::add_enabled(ui, can_align, egui::Button::new(label))
                            .on_disabled_hover_text("Select 2 or more");
                        if b.clicked() {
                            run_align(state, s, axis, edge);
                        }
                    }
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Distribute");
                crate::widgets::segmented(
                    ui,
                    &mut s.distribute_mode,
                    &[
                        (DistributeMode::Centres, "Centres"),
                        (DistributeMode::Gaps, "Gaps"),
                    ],
                );
            });
            ui.horizontal(|ui| {
                ui.label("Gap");
                ui.add(
                    egui::TextEdit::singleline(&mut s.gap_text)
                        .hint_text("keep span")
                        .desired_width(70.0),
                );
                for (i, axis) in Axis::ALL.into_iter().enumerate() {
                    let b = theme::add_enabled(
                        ui,
                        n >= 3,
                        egui::Button::new(format!("Distribute {}", ["X", "Y", "Z"][i])),
                    )
                    .on_disabled_hover_text("Select 3 or more");
                    if b.clicked() {
                        run_distribute(state, s, axis);
                    }
                }
            });
        });
    // Esc is the keymap's `popover.close` (KeyContext::Popover).
    if !open {
        *popover = None;
    }
}

/// The Array popover.
pub fn array_window(
    ctx: &egui::Context,
    state: &mut EditorState,
    s: &mut ArrangeSettings,
    popover: &mut Option<Popover>,
) {
    // A new selection, or a copy just moved, gets the default linear offset: the offset Ctrl+D
    // would repeat, else its size on X, side by side.
    let last_duplicate = state.last_duplicate_offset();
    let key = (state.selection.revision(), last_duplicate);
    if s.linear_for != Some(key) {
        s.linear_for = Some(key);
        if let Ok(objects) = read_objects(&state.world, state.selection.ids())
            && !objects.is_empty()
        {
            let size = selection_bounds_in(&objects, &DMat3::IDENTITY).size();
            s.linear.offset = default_linear_offset(last_duplicate, size, DVec3::X);
            s.linear.offset_is = OffsetIs::Distance;
            s.grid.spacing = size.max(DVec3::splat(0.05));
        }
    }
    let mut open = true;
    let mut make = false;
    egui::Window::new("Array")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(Pos2::new(420.0, 90.0))
        .show(ctx, |ui| {
            crate::widgets::segmented(
                ui,
                &mut s.mode,
                &[
                    (ArrayMode::Linear, "Linear"),
                    (ArrayMode::Radial, "Radial"),
                    (ArrayMode::Grid, "Grid"),
                    (ArrayMode::Path, "Path"),
                ],
            );
            ui.separator();
            match s.mode {
                ArrayMode::Linear => linear_fields(ui, s),
                ArrayMode::Radial => radial_fields(ui, s),
                ArrayMode::Grid => grid_fields(ui, s),
                ArrayMode::Path => path_fields(ui, s),
            }
            ui.separator();
            let group = match s.mode {
                ArrayMode::Linear => &mut s.linear.group,
                ArrayMode::Radial => &mut s.radial.group,
                ArrayMode::Grid => &mut s.grid.group,
                ArrayMode::Path => &mut s.path.group,
            };
            ui.checkbox(group, "Group result");
            // Only the count and the error text are needed; the cached plan is not copied.
            let summary = match s.preview(state) {
                Ok(p) => Ok(p.copies.len()),
                Err(e) => Err(e.clone()),
            };
            match &summary {
                Ok(n) => {
                    let n = *n;
                    if ui
                        .add(crate::widgets::Button::primary(&format!(
                            "Make {n} {}",
                            if n == 1 { "copy" } else { "copies" }
                        )))
                        .clicked()
                    {
                        make = true;
                    }
                }
                Err(e) => {
                    ui.label(egui::RichText::new(e).color(theme::warning()));
                }
            }
        });
    // Enter and Esc are the keymap's `array.make` and `popover.close` (KeyContext::Popover).
    if make && run_array(state, s) {
        *popover = None;
    }
    if !open {
        s.picking_points = false;
        s.cancel_handle();
        *popover = None;
    }
}

/// Ctrl+wheel over the viewport, or the wheel over the linear handle, while the array preview
/// shows: `notches` (up is +1) change the count by one each, clamped to the mode's range (linear
/// and path 1, radial 2, up to [`MAX_COPIES`]). Grid has three counts and ignores the wheel.
/// Returns whether the count changed.
/// Nothing is committed: the preview follows, and "Make N copies" is still the one undo step.
pub fn wheel_count(s: &mut ArrangeSettings, notches: i32) -> bool {
    if notches == 0 {
        return false;
    }
    let step = |n: usize, min: usize| -> usize {
        let next = n as i64 + i64::from(notches);
        next.clamp(min as i64, MAX_COPIES as i64) as usize
    };
    let (before, after) = match s.mode {
        ArrayMode::Linear => {
            let b = s.linear.count;
            s.linear.count = step(b, 1);
            (b, s.linear.count)
        }
        ArrayMode::Radial => {
            let b = s.radial.count;
            s.radial.count = step(b, 2);
            (b, s.radial.count)
        }
        ArrayMode::Path => match &mut s.path.fill {
            PathFill::Count(n) => {
                let b = *n;
                *n = step(b, 1);
                (b, *n)
            }
            _ => return false,
        },
        ArrayMode::Grid => return false,
    };
    if before == after {
        return false;
    }
    s.count_text = after.to_string();
    true
}

fn count_field(ui: &mut egui::Ui, s: &mut ArrangeSettings, label: &str) -> Option<usize> {
    let mut out = None;
    ui.horizontal(|ui| {
        ui.label(label);
        let r = ui.add(egui::TextEdit::singleline(&mut s.count_text).desired_width(60.0));
        if r.changed()
            && let Some(c) = parse_count(&s.count_text)
        {
            out = Some(c.count);
            if c.times && s.mode == ArrayMode::Linear {
                s.linear.offset_is = OffsetIs::Distance;
            }
        }
    });
    out
}

fn linear_fields(ui: &mut egui::Ui, s: &mut ArrangeSettings) {
    if let Some(c) = count_field(ui, s, "Count") {
        s.linear.count = c;
    }
    ui.horizontal(|ui| {
        ui.label("Offset");
        for (i, v) in [
            &mut s.linear.offset.x,
            &mut s.linear.offset.y,
            &mut s.linear.offset.z,
        ]
        .into_iter()
        .enumerate()
        {
            ui.label(egui::RichText::new(["X", "Y", "Z"][i]).color(overlay::axis_colour(i)));
            ui.add(egui::DragValue::new(v).speed(0.1).max_decimals(3));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Offset is");
        crate::widgets::segmented(
            ui,
            &mut s.linear.offset_is,
            &[(OffsetIs::Distance, "Distance"), (OffsetIs::Gap, "Gap")],
        );
    });
    ui.label(
        egui::RichText::new(
            "Drag or scroll the ring past the last ghost, or Ctrl+wheel, to add copies",
        )
        .small()
        .color(theme::text_3()),
    );
}

fn radial_fields(ui: &mut egui::Ui, s: &mut ArrangeSettings) {
    if let Some(c) = count_field(ui, s, "Count") {
        s.radial.count = c.max(2);
    }
    ui.horizontal(|ui| {
        ui.label("Angle");
        let r = ui.add(egui::TextEdit::singleline(&mut s.angle_text).desired_width(60.0));
        if r.changed() {
            match parse_angle(&s.angle_text) {
                Some(AngleField::Degrees(d)) => s.radial.angle = d,
                Some(AngleField::Divide(n)) => {
                    s.radial.count = n;
                    s.radial.angle = 360.0;
                    s.count_text = n.to_string();
                }
                None => {}
            }
        }
    });
    ui.horizontal(|ui| {
        ui.label("Axis");
        axis_buttons(ui, &mut s.radial.axis);
    });
    ui.horizontal(|ui| {
        ui.label("Centre");
        let mut c = matches!(s.radial.centre, RadialCentre::Active);
        crate::widgets::segmented(ui, &mut c, &[(false, "Pivot"), (true, "Active")]);
        s.radial.centre = if c {
            RadialCentre::Active
        } else {
            RadialCentre::Pivot
        };
    });
    ui.checkbox(&mut s.radial.rotate_copies, "Rotate copies");
}

fn grid_fields(ui: &mut egui::Ui, s: &mut ArrangeSettings) {
    ui.horizontal(|ui| {
        ui.label("Count");
        for (i, c) in s.grid.counts.iter_mut().enumerate() {
            ui.label(egui::RichText::new(["X", "Y", "Z"][i]).color(overlay::axis_colour(i)));
            ui.add(egui::DragValue::new(c).range(1..=1000));
        }
    });
    ui.horizontal(|ui| {
        ui.label("Spacing");
        for (i, v) in [
            &mut s.grid.spacing.x,
            &mut s.grid.spacing.y,
            &mut s.grid.spacing.z,
        ]
        .into_iter()
        .enumerate()
        {
            ui.label(egui::RichText::new(["X", "Y", "Z"][i]).color(overlay::axis_colour(i)));
            ui.add(egui::DragValue::new(v).speed(0.1).max_decimals(3));
        }
    });
}

fn path_fields(ui: &mut egui::Ui, s: &mut ArrangeSettings) {
    ui.horizontal(|ui| {
        ui.toggle_value(&mut s.picking_points, "Click points in the viewport");
        ui.label(format!("{} points", s.path.points.len()));
        if ui.button("Clear").clicked() {
            s.path.points.clear();
        }
    });
    ui.horizontal(|ui| {
        let mut by_count = matches!(s.path.fill, PathFill::Count(_));
        crate::widgets::segmented(ui, &mut by_count, &[(true, "Count"), (false, "Spacing")]);
        match (&mut s.path.fill, by_count) {
            (PathFill::Count(n), true) => {
                ui.add(egui::DragValue::new(n).range(1..=1000));
            }
            (PathFill::Spacing(d), false) => {
                ui.add(egui::DragValue::new(d).range(0.05..=4096.0).speed(0.1));
            }
            (fill, true) => *fill = PathFill::Count(5),
            (fill, false) => *fill = PathFill::Spacing(4.0),
        }
    });
    ui.horizontal(|ui| {
        ui.checkbox(&mut s.path.align, "Align to path");
        ui.label("Up");
        axis_buttons(ui, &mut s.path.up);
    });
}

/// The array preview, the linear drag handle and path point picking over the viewport.
pub fn array_viewport(
    ui: &egui::Ui,
    state: &mut EditorState,
    cx: &ViewContext<'_>,
    painter: &egui::Painter,
    s: &mut ArrangeSettings,
) {
    // Ghosts.
    if let Ok(plan) = s.preview(state) {
        for part in plan.copies.iter().flat_map(|c| &c.parts).take(MAX_GHOSTS) {
            let v = |p: metallic_core::Vec3| DVec3::new(p.x, p.y, p.z);
            overlay::ghost_box(
                painter,
                cx,
                v(part.position),
                rotation_from_orientation_degrees(v(part.orientation)),
                v(part.size),
            );
        }
    }
    let (pressed, down, released) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.pointer.primary_released(),
        )
    });
    // Ctrl+wheel anywhere changes the count, and so does the plain wheel over the handle. egui
    // turns Ctrl+wheel into zoom, and `captures_wheel` keeps the viewport's dolly off the handle
    // (from the same hover as here), so the camera does not move at the same time; while flying
    // the wheel stays the camera speed. Read the raw wheel events, one notch per event.
    s.wheel_latch = s
        .wheel_latch
        .filter(|at| cx.pointer.is_some_and(|p| (p - *at).length() <= 3.0));
    if cx.pointer.is_some() {
        let on_handle = s.wheel_on_handle() && !ui.input(|i| i.pointer.secondary_down());
        let notches: i32 = ui.input(|i| {
            i.events
                .iter()
                .filter_map(|e| match e {
                    egui::Event::MouseWheel {
                        delta, modifiers, ..
                    } if (modifiers.command || on_handle) && delta.y != 0.0 => {
                        Some(delta.y.signum() as i32)
                    }
                    _ => None,
                })
                .sum()
        });
        if wheel_count(s, notches) {
            if on_handle {
                s.wheel_latch = cx.pointer;
            }
            ui.ctx().request_repaint();
        }
    }
    match s.mode {
        ArrayMode::Linear => linear_handle(state, cx, painter, s, pressed, down),
        ArrayMode::Path => {
            if s.picking_points
                && released
                && let Some(p) = cx.pointer
            {
                let ray = cx.ray(p);
                let at = cx
                    .picks
                    .first(&ray, &[])
                    .map(|h| ray.at(h.distance))
                    .or_else(|| ray.plane_hit(DVec3::ZERO, DVec3::Y).map(|t| ray.at(t)));
                if let Some(at) = at {
                    s.path.points.push(at);
                }
            }
            let pts: Vec<Pos2> = s
                .path
                .points
                .iter()
                .filter_map(|p| cx.project(*p))
                .collect();
            if pts.len() > 1 {
                painter.add(egui::Shape::line(
                    pts.clone(),
                    Stroke::new(1.5, theme::accent()),
                ));
            }
            for p in pts {
                painter.circle_filled(p, 3.5, theme::accent());
            }
        }
        _ => {}
    }
}

fn linear_handle(
    state: &EditorState,
    cx: &ViewContext<'_>,
    painter: &egui::Painter,
    s: &mut ArrangeSettings,
    pressed: bool,
    down: bool,
) {
    s.handle_hover = false;
    let Some((pivot, size)) = s.basis(state) else {
        return;
    };
    let step = linear_step(size, s.linear.offset, s.linear.offset_is);
    if step.length() < 1e-9 {
        return;
    }
    let at = handle_position(pivot, step, s.linear.count);
    let Some(screen) = cx.project(at) else { return };
    s.handle_hover = cx.pointer.is_some_and(|p| (p - screen).length() <= 10.0);
    if let Some(h) = &s.handle {
        if !down {
            s.handle = None;
        } else if let Some(p) = cx.pointer {
            let dir = h.step.normalize();
            if let Some(t) = cx.ray(p).closest_on_line(h.origin, dir) {
                let along = drag_along(h.from, h.origin + dir * t, h.step);
                let count = count_from_drag(h.start_count, along, h.step);
                s.linear.count = count;
                s.count_text = count.to_string();
            }
        }
    } else if pressed && s.handle_hover {
        let dir = step.normalize();
        let from = cx
            .pointer
            .and_then(|p| cx.ray(p).closest_on_line(at, dir))
            .map_or(at, |t| at + dir * t);
        s.handle = Some(HandleDrag {
            start_count: s.linear.count,
            origin: at,
            from,
            step,
        });
    }
    let hot = s.handle_hover || s.handle.is_some();
    painter.circle_stroke(
        screen,
        10.0,
        Stroke::new(if hot { 2.5 } else { 1.5 }, theme::accent()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_wheel_steps_the_count_within_its_range() {
        let mut s = ArrangeSettings {
            mode: ArrayMode::Linear,
            ..ArrangeSettings::default()
        };
        s.linear.count = 1;
        assert!(!wheel_count(&mut s, -1), "linear stops at 1");
        assert!(wheel_count(&mut s, 3));
        assert_eq!((s.linear.count, s.count_text.as_str()), (4, "4"));
        s.linear.count = MAX_COPIES - 1;
        assert!(wheel_count(&mut s, 5));
        assert_eq!(s.linear.count, MAX_COPIES);
        s.mode = ArrayMode::Radial;
        s.radial.count = 3;
        assert!(wheel_count(&mut s, -4));
        assert_eq!(s.radial.count, 2, "radial keeps two");
        s.mode = ArrayMode::Path;
        s.path.fill = PathFill::Count(5);
        assert!(wheel_count(&mut s, 1));
        assert!(matches!(s.path.fill, PathFill::Count(6)));
        s.mode = ArrayMode::Grid;
        assert!(!wheel_count(&mut s, 1));
    }

    #[test]
    fn requests_follow_the_mode() {
        let mut s = ArrangeSettings::default();
        assert!(matches!(s.request(), ArrayRequest::Linear(_)));
        s.mode = ArrayMode::Path;
        assert!(!s.wants_pointer());
        s.picking_points = true;
        assert!(s.wants_pointer());
    }
}

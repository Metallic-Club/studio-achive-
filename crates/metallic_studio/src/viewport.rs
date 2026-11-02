//! The 3D viewport: draws the edit world with `metallic_render` into a texture egui shows, and
//! handles camera navigation, picking, selection and gizmo drags.
//!
//! - **Rendering** shares eframe's wgpu device. The world is drawn only when the world, the
//!   camera or the panel size changed, so an idle editor costs no GPU time (#124).
//! - **Selection**: click selects the part under the cursor, Model first (a second click on the
//!   same part selects the part itself); Ctrl-click toggles; Alt-click cycles through everything
//!   under the cursor; dragging on empty space draws a selection box.
//! - **Gizmos** ([`crate::gizmo`]): the active tool's handles sit at the selection's pivot
//!   ([`EditorState::gizmo_pivot`]). A drag previews live through the editor's preview overlay
//!   and records one undo step on release; Esc or a right click during the drag cancels it.
//!   Space held brings the gizmo to the cursor.
//! - **Body drags** (any tool): dragging a part moves the selection over the surface under the
//!   cursor ([`crate::snapping`]); V grabs by the nearest corner, Alt duplicates first, Ctrl+D
//!   stamps a copy, R / T turn it, X / Y / Z lock it ([`Viewport::drag_action`]).
//! - **Typed values**: digits during a drag set the amount exactly; after release the delta stays
//!   editable for [`EDIT_DELTA_TIME`] (click it or press Enter).
//! - **Pivot edit** (P): the gizmo moves the pivot; its 27 snap points show as dots, a click
//!   puts the pivot on one.
//! - **Tools that own the pointer** (brush, measure, lasso and paint select; see
//!   [`crate::editor_tools`]) set [`Viewport::capture_pointer`] before the frame: the viewport
//!   then still renders and navigates but leaves clicks and drags to the tool, which reads
//!   [`Viewport::view`] and [`EditorState::picks`] and paints its overlay on top.
//! - **Play**: while a session with a [`PlayView`](crate::play::PlayView) runs, the viewport draws
//!   that game every frame with the game camera, character and ScreenGuis instead of the edit
//!   world, framed by a thin `success` line. Picking, gizmos, body drags, the editable drag
//!   readout and the fly camera are off; the editor sends keyboard and mouse to the game
//!   ([`crate::play::input`]). Exposure adaptation restarts when Play starts and after Stop. The
//!   edit world is then drawn again from the editor camera, which Play never moved.
use std::sync::Arc;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, Pos2, Rect, Sense, Stroke, Vec2};
use eframe::egui_wgpu;
use metallic_core::InstanceId;
use metallic_render::glam::{DQuat, DVec3};
use metallic_render::{RenderContext, Renderer};

use crate::camera::{EditorCamera, ViewProjection};
use crate::commands::Command;
use crate::editor::{EditorState, Space, Tool, UiRequest};
use crate::gizmo::{self, DragInput, DragModifiers, GizmoDrag, GizmoFrame, Handle, Lock};
use crate::picking::Hit;
use crate::snapping::{self, SnapKind};
use crate::theme;
use crate::tokens::ui_tokens;
use crate::tools::select::{ScreenRect, SelectOp};
use crate::transform::{Aabb, from_d};

/// Pointer travel (points) before a press becomes a drag.
const DRAG_SLOP: f32 = 4.0;
/// Most selection outlines drawn (beyond that the status bar still counts them).
const MAX_OUTLINES: usize = 2000;
/// Colour format of the viewport texture (egui samples it like its own textures).
const TEXTURE_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

struct Gpu {
    renderer: Renderer,
    device: wgpu::Device,
    egui: Arc<egui::mutex::RwLock<egui_wgpu::Renderer>>,
    target: Option<(wgpu::Texture, wgpu::TextureView, (u32, u32))>,
    texture_id: Option<egui::TextureId>,
}

impl Gpu {
    fn new(rs: &egui_wgpu::RenderState) -> Gpu {
        let ctx =
            RenderContext::from_eframe(rs.device.clone(), rs.queue.clone(), rs.adapter.clone());
        Gpu {
            renderer: Renderer::new(&ctx, TEXTURE_FORMAT),
            device: rs.device.clone(),
            egui: rs.renderer.clone(),
            target: None,
            texture_id: None,
        }
    }

    fn ensure_target(&mut self, size: (u32, u32)) {
        if self.target.as_ref().is_some_and(|t| t.2 == size) {
            return;
        }
        let texture = self.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("studio viewport"),
            size: wgpu::Extent3d {
                width: size.0,
                height: size.1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TEXTURE_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut egui = self.egui.write();
        match self.texture_id {
            Some(id) => egui.update_egui_texture_from_wgpu_texture(
                &self.device,
                &view,
                wgpu::FilterMode::Linear,
                id,
            ),
            None => {
                self.texture_id = Some(egui.register_native_texture(
                    &self.device,
                    &view,
                    wgpu::FilterMode::Linear,
                ));
            }
        }
        drop(egui);
        self.target = Some((texture, view, size));
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Press {
    /// Left press on empty space, not yet a drag.
    Pending { at: Pos2 },
    /// Left press on a part: a click selects it, a drag moves the selection over surfaces.
    Body { at: Pos2, part: InstanceId },
    /// Box selection from `from`.
    Box { from: Pos2 },
    /// Alt held on press: a click cycles selection, a drag orbits the camera.
    Alt { at: Pos2 },
    /// Nothing (gizmo drag, camera move, or released).
    None,
}

/// Keys that act while held rather than on press (from the keymap's `hold.*` rows).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HoldKeys {
    /// Vertex and edge snapping for the drag (V).
    pub vertex: egui::Key,
    /// Brings the gizmo to the cursor (Space).
    pub summon: egui::Key,
}

impl Default for HoldKeys {
    fn default() -> Self {
        HoldKeys {
            vertex: egui::Key::V,
            summon: egui::Key::Space,
        }
    }
}

/// The last finished drag, editable for [`EDIT_DELTA_TIME`] (spec 4.5).
struct LastDrag {
    drag: GizmoDrag,
    at: Instant,
    /// History revision right after the drag; any other edit ends the offer.
    revision: u64,
    /// The text being typed into the delta field, once opened.
    editing: Option<String>,
}

/// What a drag that made an undo step did, for the first-time hints (spec 14.1).
#[derive(Debug, Clone, PartialEq)]
pub struct FinishedDrag {
    /// The axis of an axis-handle move.
    pub axis: Option<usize>,
    /// A body drag (not a handle).
    pub body: bool,
    /// Grid snapping was off.
    pub snapping_off: bool,
    /// The moved parts' box does not start on the move increment.
    pub off_grid: bool,
    /// A moved corner ended near (within 0.2 studs) but not on another part's corner.
    pub near_corner: bool,
    /// The one part moved, with its new height, when the drag moved a single part.
    pub one: Option<(InstanceId, f64)>,
}

/// How long the delta of the last drag stays editable.
pub const EDIT_DELTA_TIME: Duration = Duration::from_secs(5);

/// Viewport state.
pub struct Viewport {
    /// The editor camera.
    pub camera: EditorCamera,
    /// Hold keys (set from the keymap).
    pub holds: HoldKeys,
    gpu: Option<Gpu>,
    hovered_part: Option<InstanceId>,
    hovered_handle: Option<Handle>,
    hovered_dot: Option<DVec3>,
    drag: Option<GizmoDrag>,
    /// Selection before an Alt-drag duplicated it (restored when the copy is dropped).
    before_duplicate: Option<Vec<InstanceId>>,
    last_drag: Option<LastDrag>,
    press: Press,
    last_click: Option<InstanceId>,
    cycle: usize,
    hits: Vec<Hit>,
    rendered: Option<(metallic_core::ChangeCursor, EditorCamera, (u32, u32))>,
    gpu_missing_logged: bool,
    last_rect: Rect,
    /// Ray pick under the pointer, reused while pointer, world and camera are unchanged, so an
    /// idle pointer does not scan every part each frame.
    hover_pick: Option<(HoverKey, Option<Hit>)>,
    /// The pointer was over the viewport in the last play frame (input goes to the game).
    play_hovered: bool,
    /// The last frame drew a play session (the renderer holds the play world and avatar).
    showing_play: bool,
    delta_rect: Option<Rect>,
    /// Set by the host each frame: a building tool owns left clicks and drags (no selection,
    /// no gizmo).
    pub capture_pointer: bool,
    /// Set by the host each frame: a building tool owns the wheel (no dolly).
    pub capture_wheel: bool,
    /// The camera is being flown, panned or orbited this frame.
    navigating: bool,
    finished: Option<FinishedDrag>,
    /// Collisions for the current drag (built when it starts).
    guard: Option<crate::collisions::Guard>,
}

type HoverKey = (Pos2, metallic_core::ChangeCursor, EditorCamera, Rect);

impl Default for Viewport {
    fn default() -> Self {
        Viewport::new()
    }
}

/// What the status line shows for the viewport.
pub struct ViewportStatus {
    /// Drag value or the last drag's delta ("X +4.00").
    pub text: String,
    /// The snap target ("Vertex of Wall_03").
    pub snap: Option<String>,
    /// True while the delta field takes typing.
    pub editing: bool,
}

impl Viewport {
    /// A viewport with the default camera; the GPU side starts on the first frame.
    pub fn new() -> Viewport {
        Viewport {
            camera: EditorCamera::default(),
            holds: HoldKeys::default(),
            gpu: None,
            hovered_part: None,
            hovered_handle: None,
            hovered_dot: None,
            drag: None,
            before_duplicate: None,
            last_drag: None,
            press: Press::None,
            last_click: None,
            cycle: 0,
            hits: Vec::new(),
            rendered: None,
            gpu_missing_logged: false,
            last_rect: Rect::NOTHING,
            hover_pick: None,
            play_hovered: false,
            showing_play: false,
            delta_rect: None,
            capture_pointer: false,
            capture_wheel: false,
            navigating: false,
            finished: None,
            guard: None,
        }
    }

    /// The camera's projection for last frame's rectangle (screen points are relative to
    /// [`Viewport::rect`]'s top-left corner).
    pub fn view(&self) -> ViewProjection {
        let r = self.last_rect;
        self.camera
            .view((f64::from(r.width()), f64::from(r.height())))
    }

    /// True when the pointer was over the viewport in the last play frame.
    pub fn play_hovered(&self) -> bool {
        self.showing_play && self.play_hovered
    }

    /// Cancels a gizmo or body drag in progress, putting the parts back, and closes the editable
    /// readout of the last drag (Play starting: nothing may edit the world until Stop).
    pub fn cancel_drag(&mut self, state: &mut EditorState) {
        self.cancel(state);
        self.last_drag = None;
        self.delta_rect = None;
        self.press = Press::None;
    }

    /// The part under the cursor last frame.
    pub fn hovered(&self) -> Option<InstanceId> {
        self.hovered_part
    }

    /// The screen rectangle the viewport had last frame.
    pub fn rect(&self) -> Rect {
        self.last_rect
    }

    /// Where world point `p` appears on screen (last frame's rectangle and the current camera).
    pub fn screen_pos(&self, p: DVec3) -> Option<Pos2> {
        let r = self.last_rect;
        let view = self
            .camera
            .view((f64::from(r.width()), f64::from(r.height())));
        view.project(p)
            .map(|(x, y)| r.min + Vec2::new(x as f32, y as f32))
    }

    /// True while a gizmo or body drag is held.
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// The last drag that made an undo step, once (for the hints).
    pub fn take_finished(&mut self) -> Option<FinishedDrag> {
        self.finished.take()
    }

    /// True during a drag, a box selection or camera flight: floating UI (the contextual toolbar,
    /// hints) stays out of the way.
    pub fn is_busy(&self) -> bool {
        self.drag.is_some() || matches!(self.press, Press::Box { .. }) || self.navigating
    }

    /// True while the last drag's delta field takes typing.
    pub fn is_editing_delta(&self) -> bool {
        !self.showing_play && self.last_drag.as_ref().is_some_and(|l| l.editing.is_some())
    }

    /// Drag value and snap target for the status line.
    pub fn status(&self, state: &EditorState) -> Option<ViewportStatus> {
        if state.play.is_playing() {
            return None;
        }
        if let Some(d) = &self.drag {
            let snap = d.snap.and_then(|s| {
                let owner = s.owner?;
                Some(format!(
                    "{} of {}",
                    s.kind.label(),
                    state.display_name(owner)
                ))
            });
            return Some(ViewportStatus {
                text: d.label(),
                snap,
                editing: false,
            });
        }
        let last = self.live_last(state)?;
        Some(ViewportStatus {
            text: match &last.editing {
                Some(t) => format!("{}: {t}", last.drag.label()),
                None => format!("{} (Enter to change)", last.drag.label()),
            },
            snap: None,
            editing: last.editing.is_some(),
        })
    }

    fn live_last(&self, state: &EditorState) -> Option<&LastDrag> {
        self.last_drag.as_ref().filter(|l| {
            (l.editing.is_some() || l.at.elapsed() < EDIT_DELTA_TIME)
                && l.revision == state.history.revision()
        })
    }

    /// Frames the selection (or everything when it is empty).
    pub fn frame_selection(&mut self, state: &EditorState) {
        match state.selection_bounds() {
            Some(b) => self.camera.frame(b),
            None => self.frame_all(state),
        }
    }

    /// Frames every part in Workspace.
    pub fn frame_all(&mut self, state: &EditorState) {
        let parts = state
            .world
            .root("Workspace")
            .map(|ws| {
                let mut out = Vec::new();
                let mut seen = std::collections::HashSet::new();
                crate::selection::collect_parts(&state.world, ws, &mut out, &mut seen);
                out
            })
            .unwrap_or_default();
        if let Some(b) = Aabb::of_parts(&state.world, &parts) {
            self.camera.frame(b);
        }
    }

    /// Handles a UI request aimed at the viewport.
    pub fn handle(&mut self, request: UiRequest, state: &EditorState) {
        match request {
            UiRequest::FrameSelection => self.frame_selection(state),
            UiRequest::FrameAll => self.frame_all(state),
            _ => {}
        }
    }

    /// Runs a drag-context key action (`drag.*` keymap rows) on the drag in progress.
    pub fn drag_action(&mut self, id: &str, state: &mut EditorState) {
        let Some(drag) = &mut self.drag else { return };
        match id {
            "drag.cancel" => self.cancel(state),
            "drag.lock_x" => drag.lock(Lock::Axis(0)),
            "drag.lock_y" => drag.lock(Lock::Axis(1)),
            "drag.lock_z" => drag.lock(Lock::Axis(2)),
            "drag.plane_x" => drag.lock(Lock::Plane(0)),
            "drag.plane_y" => drag.lock(Lock::Plane(1)),
            "drag.plane_z" => drag.lock(Lock::Plane(2)),
            "drag.turn" => {
                let axis = drag.turn_axis();
                drag.turn(axis);
            }
            "drag.tilt" => drag.turn(state.tilt_axis()),
            "drag.stamp" => self.stamp(state),
            _ => {}
        }
    }

    fn cancel(&mut self, state: &mut EditorState) {
        if let Some(d) = self.drag.take() {
            state.cancel_drag(d);
            if let Some(sel) = self.before_duplicate.take() {
                state.selection.set(sel);
            }
            state.note("Drag cancelled".into());
        }
    }

    /// Ctrl+D during a body drag: leaves a copy where the selection is and keeps dragging. The
    /// move so far plus the copy are one step.
    fn stamp(&mut self, state: &mut EditorState) {
        let Some(drag) = self.drag.take() else { return };
        // Not a body drag, or an Alt-drag whose copies have not moved yet (they would be dropped
        // and the selection left on them): nothing to stamp.
        if drag.handle != Handle::Body || (drag.has_pending() && drag.is_unmoved()) {
            self.drag = Some(drag);
            return;
        }
        let parts: Vec<InstanceId> = drag.part_ids().collect();
        let grab = drag.grab_now();
        let frame = drag.frame();
        let ray = drag_ray(&self.camera, self.last_rect, grab);
        let (record, abandon) = drag.finish(&mut state.preview);
        if let Some(c) = abandon {
            state.history.abandon(c, &mut state.world);
            if let Some(sel) = self.before_duplicate.take() {
                state.selection.set(sel);
            }
            return;
        }
        let mut dup = Command::Duplicate {
            sources: parts.clone(),
            offset: metallic_core::Vec3::new(0.0, 0.0, 0.0),
            copies: Vec::new(),
        };
        if state
            .history
            .apply_pending(&mut dup, &mut state.world)
            .is_ok()
        {
            let commands = record.into_iter().chain(std::iter::once(dup)).collect();
            state.record(Command::Batch {
                name: "Stamp copy".into(),
                commands,
            });
        } else if let Some(r) = record {
            state.record(r);
        }
        // The Alt-drag's copies (if any) are recorded now; the selection stays on them.
        self.before_duplicate = None;
        let active = state.selection.active();
        self.drag = GizmoDrag::begin_body(&state.world, frame, &ray, &parts, grab, active);
    }

    /// Draws the viewport into `ui` and handles its input.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut EditorState,
        render_state: Option<&egui_wgpu::RenderState>,
    ) {
        if state.play.view().is_some() {
            self.play_ui(ui, state, render_state);
            return;
        }
        if self.showing_play {
            self.leave_play();
        }
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        self.last_rect = rect;
        state.picks.sync(&state.world);
        state.view_right = self.camera.right();
        let size = (f64::from(rect.width()), f64::from(rect.height()));

        self.navigate(ui, &response, rect);
        let view = self.camera.view(size);
        let ppp = ui.ctx().pixels_per_point();
        let px = (
            ((rect.width() * ppp).round() as u32).max(1),
            ((rect.height() * ppp).round() as u32).max(1),
        );

        self.render(state, render_state, px);
        let painter = ui.painter_at(rect);
        match self.gpu.as_ref().and_then(|g| g.texture_id) {
            Some(id) => {
                painter.image(
                    id,
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.rect_filled(rect, 0.0, ui_tokens().deep);
                painter.text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "The 3D view needs a GPU (wgpu)",
                    ui_tokens().body(),
                    ui_tokens().text_dim,
                );
            }
        }

        let pointer = response.hover_pos();
        let local = |p: Pos2| (f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y));
        // A tool takes the pointer only between gestures: a press or drag that began here
        // always gets its release, so it ends (and the keys it holds are freed).
        let capture = self.capture_pointer && self.drag.is_none() && self.press == Press::None;
        let summon = self.drag.is_none()
            && !capture
            && response.hovered()
            && ui.input(|i| i.key_down(self.holds.summon))
            && !ui.ctx().egui_wants_keyboard_input();
        let pivot = state.gizmo_pivot();
        let frame = if capture {
            None
        } else {
            self.gizmo_frame(state, &view, pivot, summon.then_some(pointer).flatten())
        };

        // Hover: pivot dots, gizmo handles, then parts.
        if self.drag.is_none() {
            self.hovered_dot = None;
            if state.pivot_edit
                && let (Some(p), Some(id)) = (pointer, state.selection.active())
                && let Some(points) = crate::pivot::candidates(&state.world, id)
            {
                let at = local(p);
                self.hovered_dot = points
                    .iter()
                    .filter_map(|&c| Some((c, view.project(c)?)))
                    .map(|(c, s)| (c, ((s.0 - at.0).powi(2) + (s.1 - at.1).powi(2)).sqrt()))
                    .filter(|&(_, d)| d <= DOT_HIT_PX)
                    .min_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(c, _)| c);
            }
            self.hovered_handle = match (frame, pointer) {
                (Some(f), Some(p)) => f.hit(&view, local(p)),
                _ => None,
            };
        }
        // One pick per pointer position, world change or camera move (not one per frame).
        let pick = pointer.map(|p| {
            let key = (p, state.world.change_cursor(), self.camera, rect);
            match &self.hover_pick {
                Some((k, hit)) if *k == key => *hit,
                _ => {
                    let (x, y) = local(p);
                    let hit = state.picks.first(&view.ray(x, y), &[]);
                    self.hover_pick = Some((key, hit));
                    hit
                }
            }
        });
        if self.drag.is_none() {
            self.hovered_part = match pick {
                Some(hit) if self.hovered_handle.is_none() => hit.map(|h| h.id),
                _ => None,
            };
        }
        if let (Some(p), Some(hit)) = (pointer, pick) {
            let (x, y) = local(p);
            let ray = view.ray(x, y);
            let at = hit.map_or(ray.at(20.0), |h| ray.at(h.distance) + DVec3::Y * 0.5);
            state.insert_point = from_d(at);
        }

        if !capture {
            self.delta_keys(ui, &response, state);
            self.pointer_buttons(ui, &response, rect, &view, frame, pivot, state);
        }

        // Outlines, snap feedback, then the gizmo on top.
        self.paint_outlines(&painter, rect.min, &view, state);
        let to = |p: DVec3| {
            view.project(p)
                .map(|(x, y)| rect.min + Vec2::new(x as f32, y as f32))
        };
        if let Some(d) = &self.drag {
            if let (Some(s), Some(surface)) = (d.snap, d.surface)
                && s.kind == SnapKind::Grid
            {
                snapping::paint_grid_patch(
                    &painter,
                    &to,
                    s.point,
                    &surface,
                    state.snapping.move_step,
                );
            }
            if let Some(s) = d.snap
                && let Some(at) = to(s.point)
            {
                snapping::paint_glyph(&painter, at, s.kind);
            }
        }
        if state.pivot_edit
            && self.drag.is_none()
            && let Some(id) = state.selection.active()
            && let Some(points) = crate::pivot::candidates(&state.world, id)
        {
            let t = ui_tokens();
            for c in points {
                let Some(at) = to(c) else { continue };
                let hovered = self.hovered_dot == Some(c);
                painter.circle(
                    at,
                    3.0,
                    if hovered { t.accent } else { t.icon },
                    Stroke::new(1.0, t.icon_hole),
                );
            }
        }
        if let Some(f) = frame {
            let shown = self.drag.as_ref().map_or(f, GizmoDrag::shown_frame);
            gizmo::paint(
                &painter,
                rect.min,
                &view,
                &shown,
                self.hovered_handle,
                self.drag.as_ref(),
                pointer,
            );
        }
        self.paint_delta(ui, &painter, &to, pivot, state);
        if let Press::Box { from } = self.press
            && let Some(p) = pointer
        {
            let r = Rect::from_two_pos(from, p);
            painter.rect_filled(r, 0.0, ui_tokens().accent.gamma_multiply(0.12));
            painter.rect_stroke(
                r,
                0.0,
                Stroke::new(1.0, ui_tokens().accent),
                egui::StrokeKind::Inside,
            );
        }
        // Camera speed note while flying.
        if response.ctx.input(|i| i.pointer.secondary_down()) && response.hovered() {
            painter.text(
                rect.left_bottom() + Vec2::new(12.0, -12.0),
                egui::Align2::LEFT_BOTTOM,
                format!("Camera speed {:.0} studs/s", self.camera.speed),
                ui_tokens().small(),
                ui_tokens().text_dim,
            );
        }
        if summon || self.drag.is_some() {
            ui.ctx().request_repaint();
        }
    }

    /// The last drag's delta next to the selection, clickable for 5 s after release.
    fn paint_delta(
        &mut self,
        ui: &egui::Ui,
        painter: &egui::Painter,
        to: &dyn Fn(DVec3) -> Option<Pos2>,
        pivot: Option<crate::pivot::Pivot>,
        state: &mut EditorState,
    ) {
        self.delta_rect = None;
        if self.drag.is_some() {
            return;
        }
        let Some(last) = self.live_last(state) else {
            self.last_drag = None;
            return;
        };
        let Some(at) = pivot.and_then(|p| to(p.position)) else {
            return;
        };
        let text = match &last.editing {
            Some(t) => format!("{}: {t}", last.drag.label()),
            None => last.drag.label(),
        };
        let editing = last.editing.is_some();
        let pos = at + Vec2::new(18.0, -34.0);
        gizmo::label_box(painter, pos, &text, editing);
        let galley = painter.layout_no_wrap(text, ui_tokens().mono(), ui_tokens().text);
        let r = Rect::from_min_size(pos, galley.size() + Vec2::new(10.0, 6.0));
        self.delta_rect = Some(r);
        let resp = ui.interact(
            r,
            egui::Id::NULL.with("viewport-last-delta"),
            Sense::click(),
        );
        if resp.clicked()
            && let Some(l) = &mut self.last_drag
        {
            l.editing = Some(String::new());
        }
        if !editing {
            ui.ctx().request_repaint_after(Duration::from_millis(250));
        }
    }

    /// Enter opens the last drag's delta field; typing then Enter changes that drag.
    fn delta_keys(&mut self, ui: &egui::Ui, response: &egui::Response, state: &mut EditorState) {
        if self.drag.is_some() || self.live_last(state).is_none() {
            return;
        }
        let ctx = ui.ctx();
        let Some(last) = &mut self.last_drag else {
            return;
        };
        if last.editing.is_none() {
            if response.hovered()
                && !ctx.egui_wants_keyboard_input()
                && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                last.editing = Some(String::new());
            }
            return;
        }
        let mut text = last.editing.take().unwrap_or_default();
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Text(t) = e {
                    text.extend(t.chars().filter(|c| expr_char(*c)));
                }
            }
        });
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
            text.pop();
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            return;
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            if let Some(v) = crate::expr::eval(&text)
                && state.amend_drag(&last.drag, v)
            {
                let mut d = last.drag.clone();
                d.set_amount(v);
                self.last_drag = Some(LastDrag {
                    drag: d,
                    at: Instant::now(),
                    revision: state.history.revision(),
                    editing: None,
                });
            }
            return;
        }
        text.truncate(crate::expr::MAX_LEN);
        last.editing = Some(text);
    }

    /// Draws the running game: every frame, from the game camera, GUI on top.
    fn play_ui(
        &mut self,
        ui: &mut egui::Ui,
        state: &EditorState,
        render_state: Option<&egui_wgpu::RenderState>,
    ) {
        let rect = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(rect, Sense::click_and_drag());
        self.last_rect = rect;
        self.play_hovered = response.hovered() || response.dragged();
        let first = !self.showing_play;
        self.showing_play = true;
        self.rendered = None;
        self.hovered_part = None;
        self.hovered_handle = None;
        let ppp = ui.ctx().pixels_per_point();
        let px = (
            ((rect.width() * ppp).round() as u32).max(1),
            ((rect.height() * ppp).round() as u32).max(1),
        );
        if self.ensure_gpu(state, render_state)
            && let Some(gpu) = &mut self.gpu
        {
            gpu.ensure_target(px);
            if first {
                // A camera cut: meter the game's first frame afresh.
                gpu.renderer.reset_exposure();
            }
            if let (Some(view), Some((_, target, _))) = (state.play.view(), &gpu.target) {
                view.render(&mut gpu.renderer, target, px);
            }
        }
        let painter = ui.painter_at(rect);
        match self.gpu.as_ref().and_then(|g| g.texture_id) {
            Some(id) => {
                painter.image(
                    id,
                    rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.rect_filled(rect, 0.0, ui_tokens().deep);
            }
        }
        if let Some(view) = state.play.view() {
            view.paint_gui(&painter);
        }
        painter.rect_stroke(
            rect.shrink(1.0),
            0.0,
            Stroke::new(2.0, theme::success()),
            egui::StrokeKind::Inside,
        );
        ui.ctx().request_repaint();
    }

    /// Back from Play: the renderer still holds the play world and the avatar.
    fn leave_play(&mut self) {
        if let Some(gpu) = &mut self.gpu {
            gpu.renderer.set_skinned(&[]);
            gpu.renderer.reset_exposure();
        }
        self.rendered = None;
        self.hover_pick = None;
        self.showing_play = false;
        self.play_hovered = false;
    }

    /// Creates the GPU side on first use; false (logged once) without a GPU.
    fn ensure_gpu(&mut self, state: &EditorState, rs: Option<&egui_wgpu::RenderState>) -> bool {
        if self.gpu.is_none() {
            match rs {
                Some(rs) => self.gpu = Some(Gpu::new(rs)),
                None => {
                    if !self.gpu_missing_logged {
                        state.output.studio("No GPU renderer: the 3D view is off");
                        self.gpu_missing_logged = true;
                    }
                    return false;
                }
            }
        }
        true
    }

    fn render(&mut self, state: &EditorState, rs: Option<&egui_wgpu::RenderState>, px: (u32, u32)) {
        if !self.ensure_gpu(state, rs) {
            return;
        }
        let Some(gpu) = &mut self.gpu else { return };
        let key = (state.world.change_cursor(), self.camera, px);
        if self.rendered == Some(key) {
            return;
        }
        gpu.ensure_target(px);
        gpu.renderer.sync(&state.world);
        if let Some((_, view, _)) = &gpu.target {
            gpu.renderer.render(view, &self.camera.to_render(), px);
        }
        self.rendered = Some(key);
    }

    fn navigate(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) {
        let ctx = ui.ctx().clone();
        let (delta, secondary, middle, alt, primary, scroll, dt) = ctx.input(|i| {
            (
                i.pointer.delta(),
                i.pointer.secondary_down(),
                i.pointer.middle_down(),
                i.modifiers.alt,
                i.pointer.primary_down(),
                i.smooth_scroll_delta.y,
                f64::from(i.stable_dt.min(0.1)),
            )
        });
        let over = response.hovered() || response.dragged() || response.is_pointer_button_down_on();
        self.navigating = false;
        if !over {
            return;
        }
        self.navigating = (secondary && self.drag.is_none())
            || middle
            || (alt && primary && self.drag.is_none() && matches!(self.press, Press::Alt { .. }));
        let (dx, dy) = (f64::from(delta.x), f64::from(delta.y));
        if secondary && self.drag.is_none() {
            self.camera.rotate(dx, dy);
            let key = |k| ctx.input(|i| i.key_down(k));
            let mut mv = DVec3::ZERO;
            if key(egui::Key::W) {
                mv.z += 1.0;
            }
            if key(egui::Key::S) {
                mv.z -= 1.0;
            }
            if key(egui::Key::D) {
                mv.x += 1.0;
            }
            if key(egui::Key::A) {
                mv.x -= 1.0;
            }
            if key(egui::Key::E) {
                mv.y += 1.0;
            }
            if key(egui::Key::Q) {
                mv.y -= 1.0;
            }
            let fast = ctx.input(|i| i.modifiers.shift);
            if mv != DVec3::ZERO {
                self.camera.fly(mv, dt, fast);
            }
            if scroll != 0.0 {
                self.camera.change_speed(f64::from(scroll) / 50.0);
            }
            ctx.request_repaint();
        } else if middle {
            self.camera.pan(dx, dy, f64::from(rect.height()));
        } else if alt && primary && self.drag.is_none() && matches!(self.press, Press::Alt { .. }) {
            self.camera.orbit(dx, dy);
        } else if scroll != 0.0 && response.hovered() && !self.capture_wheel {
            let toward = match response.hover_pos() {
                Some(p) => {
                    let view = self
                        .camera
                        .view((f64::from(rect.width()), f64::from(rect.height())));
                    view.ray(f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y))
                        .dir
                }
                None => self.camera.forward(),
            };
            self.camera.dolly(toward, f64::from(scroll) / 50.0);
        }
    }

    fn gizmo_frame(
        &self,
        state: &EditorState,
        view: &ViewProjection,
        pivot: Option<crate::pivot::Pivot>,
        summon_at: Option<Pos2>,
    ) -> Option<GizmoFrame> {
        if state.selection.is_empty() || state.play.is_playing() {
            return None;
        }
        let tool = if state.pivot_edit {
            if state.tool == Tool::Rotate {
                Tool::Rotate
            } else {
                Tool::Move
            }
        } else {
            state.tool
        };
        if tool == Tool::Select {
            return None;
        }
        let pivot = pivot?;
        let rotation = match (tool, state.space) {
            (Tool::Scale, _) | (_, Space::Local) => pivot.rotation,
            _ => DQuat::IDENTITY,
        };
        let mut origin = pivot.position;
        if let Some(p) = summon_at {
            // Same depth as the pivot, under the cursor.
            let r = self.last_rect;
            let ray = view.ray(f64::from(p.x - r.min.x), f64::from(p.y - r.min.y));
            let fwd = self.camera.forward();
            let along = ray.dir.dot(fwd);
            if along > 1e-6 {
                origin = ray.at((pivot.position - view.eye).dot(fwd) / along);
            }
        }
        Some(GizmoFrame::new(origin, rotation, tool, view))
    }

    #[allow(clippy::too_many_arguments)]
    fn pointer_buttons(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        rect: Rect,
        view: &ViewProjection,
        frame: Option<GizmoFrame>,
        pivot: Option<crate::pivot::Pivot>,
        state: &mut EditorState,
    ) {
        let ctx = ui.ctx().clone();
        let (pressed, released, secondary_pressed, mods, pos) = ctx.input(|i| {
            (
                i.pointer.primary_pressed(),
                i.pointer.primary_released(),
                i.pointer.secondary_pressed(),
                i.modifiers,
                i.pointer.interact_pos(),
            )
        });
        let local = |p: Pos2| (f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y));
        let vertex =
            ctx.input(|i| i.key_down(self.holds.vertex)) && !ctx.egui_wants_keyboard_input();

        if self.drag.is_some() {
            // Losing focus (Alt+Tab, another window) loses the pointer release too: the drag is
            // cancelled, as with Esc, so nothing half-done lands in History.
            let focus_lost = ctx.input(|i| {
                !i.focused
                    || i.events
                        .iter()
                        .any(|e| matches!(e, egui::Event::WindowFocused(false)))
            });
            if secondary_pressed || focus_lost {
                self.cancel(state);
                return;
            }
            self.drag_frame(&ctx, view, rect, pos, mods, vertex, released, state);
            return;
        }
        if state.play.is_playing() {
            self.press = Press::None;
        }

        if pressed && response.hovered() && !state.play.is_playing() {
            let Some(p) = pos else { return };
            let (x, y) = local(p);
            let ray = view.ray(x, y);
            if state.pivot_edit
                && !mods.alt
                && let (Some(dot), Some(id), Some(pv)) =
                    (self.hovered_dot, state.selection.active(), pivot)
            {
                state.set_pivot(
                    id,
                    crate::pivot::Pivot {
                        position: dot,
                        rotation: pv.rotation,
                    },
                );
                self.press = Press::None;
                return;
            }
            if let (Some(f), Some(h)) = (frame, self.hovered_handle) {
                self.begin_handle(state, f, h, &ray, pivot, mods.alt);
                self.press = Press::None;
                if self.drag.is_some() {
                    return;
                }
            }
            if let Some(part) = self.hovered_part
                && !state.pivot_edit
            {
                let in_selection = self.selected_target(state, part).is_some();
                if mods.alt && in_selection {
                    // Alt on the body: duplicate, then drag the copy at once.
                    if let Some(dup) = self.duplicate(state) {
                        let grab = self.grab_point(state, &ray, part, view, (x, y), vertex);
                        self.begin_body(state, &ray, grab, Some(dup));
                    }
                    self.press = Press::None;
                    return;
                }
                if !mods.alt && !mods.command {
                    self.press = Press::Body { at: p, part };
                    return;
                }
            }
            self.press = if mods.alt {
                Press::Alt { at: p }
            } else {
                Press::Pending { at: p }
            };
        }
        let down = ctx.input(|i| i.pointer.primary_down());
        match self.press {
            Press::Pending { at } => {
                if let Some(p) = pos
                    && (p - at).length() > DRAG_SLOP
                    && down
                {
                    self.press = Press::Box { from: at };
                } else if released {
                    self.click(state, mods.command, mods.shift);
                    self.press = Press::None;
                }
            }
            Press::Body { at, part } => {
                if let Some(p) = pos
                    && (p - at).length() > DRAG_SLOP
                    && down
                {
                    self.press = Press::None;
                    if self.selected_target(state, part).is_none() {
                        self.hovered_part = Some(part);
                        self.click(state, false, false);
                    }
                    let (x, y) = local(at);
                    let ray = view.ray(x, y);
                    let grab = self.grab_point(state, &ray, part, view, (x, y), vertex);
                    self.begin_body(state, &ray, grab, None);
                    if self.drag.is_some() {
                        self.drag_frame(&ctx, view, rect, pos, mods, vertex, false, state);
                    }
                } else if released {
                    self.click(state, false, mods.shift);
                    self.press = Press::None;
                }
            }
            Press::Box { from } => {
                if released {
                    if let Some(p) = pos {
                        let corner = |p: Pos2| {
                            let (x, y) = local(p);
                            glam::DVec2::new(x, y)
                        };
                        crate::editor_tools::select::finish_box(
                            state,
                            view,
                            ScreenRect::from_corners(corner(from), corner(p)),
                            mods.alt,
                            SelectOp::from_modifiers(mods.shift, mods.command),
                        );
                    }
                    self.press = Press::None;
                }
            }
            Press::Alt { at } => {
                if released {
                    // A click without travel cycles through everything under the cursor.
                    if let Some(p) = pos
                        && (p - at).length() <= DRAG_SLOP
                    {
                        let (x, y) = local(p);
                        state.picks.all(&view.ray(x, y), &mut self.hits);
                        if !self.hits.is_empty() {
                            self.cycle = (self.cycle + 1) % self.hits.len();
                            state.selection.select_only(self.hits[self.cycle].id);
                        }
                    }
                    self.press = Press::None;
                }
            }
            Press::None => {}
        }
    }

    /// One frame of the drag in progress: typing, pointer, live preview, release.
    #[allow(clippy::too_many_arguments)]
    fn drag_frame(
        &mut self,
        ctx: &egui::Context,
        view: &ViewProjection,
        rect: Rect,
        pos: Option<Pos2>,
        mods: egui::Modifiers,
        vertex: bool,
        released: bool,
        state: &mut EditorState,
    ) {
        let Some(mut drag) = self.drag.take() else {
            return;
        };
        // Typed values (spec 4.5).
        ctx.input(|i| {
            for e in &i.events {
                if let egui::Event::Text(t) = e {
                    for c in t.chars() {
                        drag.type_char(c);
                    }
                }
            }
        });
        if drag.typed().is_some() {
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
                drag.backspace();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
                && drag.commit_typed()
            {
                drag.apply(&mut state.world, &mut state.preview);
                self.end(drag, state);
                return;
            }
        }
        let released = released || !ctx.input(|i| i.pointer.primary_down());
        if released {
            drag.clear_typed();
        }
        if let Some(p) = pos {
            let (x, y) = (f64::from(p.x - rect.min.x), f64::from(p.y - rect.min.y));
            let input = DragInput {
                ray: view.ray(x, y),
                cursor: (x, y),
                view: Some(view),
                picks: Some(&state.picks),
                snapping: &state.snapping,
                mods: DragModifiers {
                    invert_snap: mods.command,
                    precision: mods.shift && !vertex,
                    alt: mods.alt,
                },
                vertex,
                onto_surface: vertex && mods.shift,
            };
            drag.update(&input);
            if state.snapping.collisions {
                if self
                    .guard
                    .as_ref()
                    .is_none_or(|g| g.serial() != drag.serial())
                {
                    self.guard = Some(crate::collisions::Guard::for_drag(&drag, &state.picks));
                }
                if let Some(guard) = &mut self.guard {
                    drag.limit(|moved| guard.blocked(moved));
                    if guard.overloaded() {
                        state.note(
                            "Collisions: too many parts nearby to check; the drag is held".into(),
                        );
                    }
                }
            }
            drag.apply(&mut state.world, &mut state.preview);
        }
        if released {
            self.end(drag, state);
        } else {
            self.drag = Some(drag);
        }
    }

    fn end(&mut self, drag: GizmoDrag, state: &mut EditorState) {
        self.guard = None;
        let keep = drag.accepts_typing().then(|| drag.clone());
        let summary = summarise(&drag, state);
        let recorded = state.finish_drag(drag);
        self.finished = recorded.then_some(summary);
        if !recorded && let Some(sel) = self.before_duplicate.take() {
            state.selection.set(sel);
        }
        self.before_duplicate = None;
        self.last_drag = match keep {
            Some(drag) if recorded => Some(LastDrag {
                drag,
                at: Instant::now(),
                revision: state.history.revision(),
                editing: None,
            }),
            _ => None,
        };
    }

    /// The selected item that `part` belongs to (itself, or a selected ancestor).
    fn selected_target(&self, state: &EditorState, part: InstanceId) -> Option<InstanceId> {
        let mut cur = Some(part);
        while let Some(c) = cur {
            if state.selection.contains(c) {
                return Some(c);
            }
            cur = state.world.parent(c);
        }
        None
    }

    /// Duplicates the selection for an Alt-drag (not yet an undo step) and selects the copies.
    fn duplicate(&mut self, state: &mut EditorState) -> Option<Command> {
        let mut cmd = Command::Duplicate {
            sources: state.selection.ids().to_vec(),
            offset: metallic_core::Vec3::new(0.0, 0.0, 0.0),
            copies: Vec::new(),
        };
        state
            .history
            .apply_pending(&mut cmd, &mut state.world)
            .ok()?;
        self.before_duplicate = Some(state.selection.ids().to_vec());
        state.selection.set(cmd.created());
        Some(cmd)
    }

    fn begin_handle(
        &mut self,
        state: &mut EditorState,
        f: GizmoFrame,
        h: Handle,
        ray: &crate::transform::Ray,
        pivot: Option<crate::pivot::Pivot>,
        alt: bool,
    ) {
        let screen_normal = -self.camera.forward();
        let about = pivot.map_or(f.origin, |p| p.position);
        if state.pivot_edit {
            let (Some(id), Some(pv)) = (state.selection.active(), pivot) else {
                return;
            };
            let Some(points) = crate::pivot::candidates(&state.world, id) else {
                return;
            };
            let name = state.display_name(id);
            self.drag = GizmoDrag::begin_pivot(f, h, ray, id, name, pv, points)
                .map(|d| d.with_screen_normal(screen_normal));
            return;
        }
        let duplicate = alt && matches!(h, Handle::Axis(_) | Handle::Plane(_) | Handle::Free);
        let pending = if duplicate {
            self.duplicate(state)
        } else {
            None
        };
        let parts = state.selection.parts(&state.world);
        let models = state.models_with_pivots();
        let drag = GizmoDrag::begin(&state.world, f, h, ray, &parts).map(|d| {
            let d = d
                .with_screen_normal(screen_normal)
                .with_about(about)
                .with_models(models);
            match pending.clone() {
                Some(p) => d.with_pending(p),
                None => d,
            }
        });
        if drag.is_none()
            && let Some(p) = pending
        {
            state.history.abandon(p, &mut state.world);
            if let Some(sel) = self.before_duplicate.take() {
                state.selection.set(sel);
            }
        }
        self.drag = drag;
    }

    fn begin_body(
        &mut self,
        state: &mut EditorState,
        ray: &crate::transform::Ray,
        grab: DVec3,
        pending: Option<Command>,
    ) {
        let parts = state.selection.parts(&state.world);
        let active = state
            .selection
            .active()
            .filter(|&a| state.world.is_a(a, "BasePart"));
        let frame = GizmoFrame {
            origin: grab,
            axes: [DVec3::X, DVec3::Y, DVec3::Z],
            length: 1.0,
            tool: state.tool,
        };
        let models = state.models_with_pivots();
        let drag = GizmoDrag::begin_body(&state.world, frame, ray, &parts, grab, active)
            .map(|d| d.with_models(models));
        match (drag, pending) {
            (Some(d), Some(p)) => self.drag = Some(d.with_pending(p)),
            (Some(d), None) => self.drag = Some(d),
            (None, Some(p)) => {
                state.history.abandon(p, &mut state.world);
                if let Some(sel) = self.before_duplicate.take() {
                    state.selection.set(sel);
                }
            }
            (None, None) => {}
        }
    }

    /// Where a body drag holds the selection: the point under the cursor on `part`, or with V
    /// the selection's corner nearest the cursor (spec 5.3).
    fn grab_point(
        &self,
        state: &EditorState,
        ray: &crate::transform::Ray,
        part: InstanceId,
        view: &ViewProjection,
        cursor: (f64, f64),
        vertex: bool,
    ) -> DVec3 {
        if vertex {
            let mut best: Option<(f64, DVec3)> = None;
            for id in state.selection.parts(&state.world) {
                let Some(t) = state.picks.transform(id) else {
                    continue;
                };
                for c in t.corners() {
                    if let Some(s) = view.project(c) {
                        let d = (s.0 - cursor.0).powi(2) + (s.1 - cursor.1).powi(2);
                        if best.is_none_or(|(b, _)| d < b) {
                            best = Some((d, c));
                        }
                    }
                }
            }
            if let Some((_, c)) = best {
                return c;
            }
        }
        state
            .picks
            .transform(part)
            .and_then(|t| t.ray_hit_normal(ray).map(|(d, _)| ray.at(d)))
            .unwrap_or_else(|| ray.at(10.0))
    }

    /// Click selection with Model-first rule.
    fn click(&mut self, state: &mut EditorState, ctrl: bool, shift: bool) {
        let Some(part) = self.hovered_part else {
            if !ctrl && !shift {
                state.selection.clear();
            }
            self.last_click = None;
            return;
        };
        let model = outermost_model(state, part);
        let target = match model {
            Some(m) if !(state.selection.ids() == [m] && self.last_click == Some(part)) => m,
            _ => part,
        };
        if ctrl {
            state.selection.toggle(target);
        } else if shift {
            state.selection.add(target);
        } else {
            state.selection.select_only(target);
        }
        self.last_click = Some(part);
        self.cycle = 0;
    }

    fn paint_outlines(
        &self,
        painter: &egui::Painter,
        offset: Pos2,
        view: &ViewProjection,
        state: &EditorState,
    ) {
        let to = |p: DVec3| {
            view.project(p)
                .map(|(x, y)| offset + Vec2::new(x as f32, y as f32))
        };
        let mut drawn = 0;
        for &id in state.selection.ids() {
            if drawn >= MAX_OUTLINES {
                break;
            }
            if state.world.is_a(id, "BasePart") {
                if let Some(t) = state.picks.transform(id) {
                    box_edges(
                        painter,
                        &to,
                        &t.corners(),
                        Stroke::new(2.0, ui_tokens().accent),
                    );
                    drawn += 1;
                }
            } else if let Some(b) = Aabb::of_parts(
                &state.world,
                &crate::selection::Selection::parts_of(&state.world, id),
            ) {
                // A Model or folder draws its bounding-box corners, so it reads differently from a part.
                box_corners(painter, &to, b, Stroke::new(2.0, ui_tokens().accent));
                drawn += 1;
            }
        }
        if let Some(h) = self.hovered_part
            && !state.selection.contains(h)
            && let Some(t) = state.picks.transform(h)
        {
            box_edges(
                painter,
                &to,
                &t.corners(),
                Stroke::new(1.0, ui_tokens().accent.gamma_multiply(0.6)),
            );
        }
    }
}

/// Pointer distance (points) that still counts as on a pivot dot.
const DOT_HIT_PX: f64 = 8.0;

fn expr_char(c: char) -> bool {
    c.is_ascii_digit() || "+-*/.() ".contains(c)
}

/// The camera ray through world point `p` (re-anchors a stamped drag at its grab point).
fn drag_ray(camera: &EditorCamera, rect: Rect, p: DVec3) -> crate::transform::Ray {
    let view = camera.view((f64::from(rect.width()), f64::from(rect.height())));
    match view.project(p) {
        Some((x, y)) => view.ray(x, y),
        None => crate::transform::Ray {
            origin: view.eye,
            dir: (p - view.eye).normalize_or(DVec3::NEG_Z),
        },
    }
}

/// The outermost Model between `part` and Workspace.
fn outermost_model(state: &EditorState, part: InstanceId) -> Option<InstanceId> {
    let ws = state.world.root("Workspace")?;
    let mut found = None;
    let mut cur = state.world.parent(part);
    while let Some(c) = cur {
        if c == ws {
            break;
        }
        if state.world.is_a(c, "Model") {
            found = Some(c);
        }
        cur = state.world.parent(c);
    }
    found
}

const EDGES: [(usize, usize); 12] = [
    (0, 1),
    (2, 3),
    (4, 5),
    (6, 7),
    (0, 2),
    (1, 3),
    (4, 6),
    (5, 7),
    (0, 4),
    (1, 5),
    (2, 6),
    (3, 7),
];

fn box_edges(
    painter: &egui::Painter,
    to: &dyn Fn(DVec3) -> Option<Pos2>,
    corners: &[DVec3; 8],
    stroke: Stroke,
) {
    let mut pts = [None; 8];
    for (p, c) in pts.iter_mut().zip(corners) {
        *p = to(*c);
    }
    for (a, b) in EDGES {
        if let (Some(a), Some(b)) = (pts[a], pts[b]) {
            painter.line_segment([a, b], stroke);
        }
    }
}

fn box_corners(
    painter: &egui::Painter,
    to: &dyn Fn(DVec3) -> Option<Pos2>,
    b: Aabb,
    stroke: Stroke,
) {
    let size = b.size();
    let corner = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { b.min.x } else { b.max.x },
            if i & 2 == 0 { b.min.y } else { b.max.y },
            if i & 4 == 0 { b.min.z } else { b.max.z },
        )
    };
    for i in 0..8 {
        let c = corner(i);
        let Some(sc) = to(c) else { continue };
        for (axis, len) in [(0usize, size.x), (1, size.y), (2, size.z)] {
            let sign = if i & (1 << axis) == 0 { 1.0 } else { -1.0 };
            let mut d = DVec3::ZERO;
            d[axis] = sign * (len * 0.2).min(2.0);
            if let Some(e) = to(c + d) {
                painter.line_segment([sc, e], stroke);
            }
        }
    }
}

/// The hint-relevant facts of a drag that is about to end.
fn summarise(drag: &GizmoDrag, state: &EditorState) -> FinishedDrag {
    let moved = drag.current();
    let step = state.snapping.move_step.max(1e-6);
    let mut lo = DVec3::splat(f64::INFINITY);
    for m in moved {
        for c in m.corners() {
            lo = lo.min(c);
        }
    }
    let mut near_corner = false;
    let mut tested = 0usize;
    'outer: for m in moved.iter().take(8) {
        for (id, other, _) in state.picks.iter() {
            if drag.moving().contains(&id) {
                continue;
            }
            tested += 1;
            if tested > 2000 {
                break 'outer;
            }
            let reach = (m.size.length() + other.size.length()) * 0.5 + 0.2;
            if m.position.distance_squared(other.position) > reach * reach {
                continue;
            }
            for a in m.corners() {
                for b in other.corners() {
                    let d = a.distance(b);
                    if d > 1e-3 && d <= 0.2 {
                        near_corner = true;
                        break 'outer;
                    }
                }
            }
        }
    }
    let off = |v: f64| (v - (v / step).round() * step).abs() > 1e-4;
    let mut ids = drag.part_ids();
    let one = match (ids.next(), ids.next(), moved) {
        (Some(id), None, [t]) => Some((id, t.position.y)),
        _ => None,
    };
    FinishedDrag {
        axis: match drag.handle {
            crate::gizmo::Handle::Axis(a) => Some(a),
            _ => None,
        },
        body: drag.handle == crate::gizmo::Handle::Body,
        snapping_off: !state.snapping.grid,
        off_grid: lo.is_finite() && (off(lo.x) || off(lo.y) || off(lo.z)),
        near_corner,
        one,
    }
}

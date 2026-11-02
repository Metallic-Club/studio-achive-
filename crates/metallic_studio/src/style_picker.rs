//! The building style picker (`docs/design/building-tools.md` section 12.5, mockup
//! `docs/design/building-tools/style-picker.html`): a 480 x 360 popover that sets the
//! `LookOverride` of the selection (#222, [`metallic_core::look_override`]).
//!
//! - **Open**: the `style.open` action (L over the viewport, the contextual toolbar's Style
//!   button, the Style row in Properties). It opens at once with placeholder tiles.
//! - **Tiles**: a 4 x 2 radio group, `inherit` first (labelled "Place Look" with the place
//!   Look's name), then the seven styles. Each tile shows the current selection rendered in
//!   that style at 208 x 156 (2x the 104 x 78 tile). Thumbnails come from a
//!   [`ThumbnailSource`], one per frame, into a cache that lives until the popover closes;
//!   nothing is re-rendered while it stays open. Until a tile's image is ready it shows a
//!   placeholder with "Rendering". Pixel styles get 3-pixel blocks so they read at tile size.
//! - **Hover** a tile for 150 ms (or press Space on it): the viewport previews that style on
//!   the selection through the preview overlay ([`crate::preview`]); leaving the tiles restores
//!   it. **Click** (or Enter) applies one undo step "Style: Retro / PS1 (1 model)". **Esc**
//!   cancels a preview and closes, leaving the selection as it was.
//! - **Apply to**: Model writes the value to each selected Model (its parts keep `inherit`) and
//!   to selected parts; Each part writes it to every part in the selection, Models' parts
//!   included. The default is Model when a Model is selected.
//!
//! Per-part style drawing is #223's work. Until it lands, the viewport preview and the
//! thumbnails render the parts with the place Look's shading; the value written, the undo step
//! and the popover are final. The GPU thumbnails ([`GpuThumbnails`]) render an isolated copy of
//! the selection offscreen with [`metallic_render::Renderer::capture`].
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use eframe::egui::{
    self, Align2, Area, Color32, CornerRadius, Frame, Id, Key, Margin, Order, Rect, RichText,
    Sense, Stroke, TextureHandle, TextureOptions, Vec2, pos2, vec2,
};
use eframe::egui_wgpu;
use metallic_core::look_override::{self, INHERIT, LOOK_OVERRIDES, PROPERTY};
use metallic_core::{InstanceId, Value, World};
use metallic_render::glam::Vec3;
use metallic_render::{Camera, RenderContext, Renderer};

use crate::actions::{Action, ActionRegistry, ToolbarGroup};
use crate::commands::PropName;
use crate::editor::EditorState;
use crate::icons::Icon;
use crate::selection::Selection;
use crate::theme::{self, fonts};
use crate::transform::Aabb;
use crate::widgets::button::Button;
use crate::widgets::segmented::segmented;

/// Action id that opens the picker.
pub const OPEN_ID: &str = "style.open";
/// Popover size (points).
pub const SIZE: Vec2 = vec2(480.0, 360.0);
/// One tile's image (points).
pub const TILE: Vec2 = vec2(104.0, 78.0);
/// Thumbnail size in pixels (2x the tile).
pub const THUMB_PX: (u32, u32) = (208, 156);
/// Hover time before the viewport previews a tile.
pub const HOVER_DELAY: Duration = Duration::from_millis(150);
/// Pixel styles are shown with blocks this many thumbnail pixels wide.
const PIXEL_BLOCK: usize = 3;
/// Tiles: `inherit` and the seven styles, in [`LOOK_OVERRIDES`] order.
pub const TILES: usize = 8;
const COLUMNS: usize = 4;

/// One line per style for the screen reader and the tooltip (section 12.5).
const DESCRIPTIONS: [&str; TILES] = [
    "uses the place Look",
    "physically based light and materials",
    "soft light, gentle shadows",
    "banded light and outlines",
    "flat-shaded facets",
    "blocky surfaces",
    "pixelated image with crisp edges",
    "wobbly vertices, warped textures, 15-bit colour",
];

/// The note under the grid (section 12.3).
pub const NOTE: &str = "Lighting, bloom and fog stay with the place Look.";

/// Where a click writes the value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyTo {
    /// Selected Models get the value; their parts keep `inherit`. Selected parts get it too.
    Model,
    /// Every part in the selection gets the value, the parts of selected Models included.
    EachPart,
}

/// Renders the thumbnails. [`GpuThumbnails`] is the real one; tests use a stand-in.
pub trait ThumbnailSource {
    /// Draws `world` from `camera` at `size` and returns tightly packed RGBA8 bytes, or `None`
    /// when it cannot (the tile then keeps its placeholder).
    fn render(&mut self, world: &World, camera: &Camera, size: (u32, u32)) -> Option<Vec<u8>>;
}

/// Offscreen thumbnails with a renderer of their own on Studio's GPU device.
pub struct GpuThumbnails {
    renderer: Renderer,
}

impl GpuThumbnails {
    /// A thumbnail renderer sharing eframe's device and queue.
    pub fn new(rs: &egui_wgpu::RenderState) -> GpuThumbnails {
        let ctx =
            RenderContext::from_eframe(rs.device.clone(), rs.queue.clone(), rs.adapter.clone());
        GpuThumbnails::from_context(&ctx)
    }

    /// A thumbnail renderer on any device (headless pictures and tests).
    pub fn from_context(ctx: &RenderContext) -> GpuThumbnails {
        GpuThumbnails {
            renderer: Renderer::new(ctx, wgpu::TextureFormat::Rgba8UnormSrgb),
        }
    }
}

impl ThumbnailSource for GpuThumbnails {
    fn render(&mut self, world: &World, camera: &Camera, size: (u32, u32)) -> Option<Vec<u8>> {
        self.renderer.sync(world);
        self.renderer.capture(camera, size).ok()
    }
}

/// The picker's state (one per editor).
pub struct StylePicker {
    open: bool,
    requested: Rc<Cell<bool>>,
    apply_to: ApplyTo,
    /// Keyboard focus among the tiles.
    focus: usize,
    /// The tile under the pointer and since when.
    hover: Option<(usize, Instant)>,
    /// The tile the viewport previews, and whether Space pinned it.
    previewing: Option<(usize, bool)>,
    selection_revision: u64,
    prop: PropName,
    thumbs: Thumbnails,
    /// Where the popover was drawn last frame (tests and the hint card).
    pub last_rect: Option<Rect>,
}

/// The thumbnail cache of one opening.
#[derive(Default)]
struct Thumbnails {
    source: Option<Box<dyn ThumbnailSource>>,
    /// The selection alone, copied when the popover opened.
    world: Option<World>,
    parts: Vec<InstanceId>,
    camera: Option<Camera>,
    images: [Option<TextureHandle>; TILES],
    next: usize,
    /// Renders done since the popover opened (tests: never more than one per frame).
    pub rendered: usize,
}

impl StylePicker {
    /// Registers `style.open` (disabled while playing or with nothing selected) and returns
    /// the picker.
    pub fn new(actions: &mut ActionRegistry) -> StylePicker {
        let requested = Rc::new(Cell::new(false));
        let r = requested.clone();
        actions.register(
            Action::new(
                OPEN_ID,
                "Style",
                Icon::Style,
                ToolbarGroup::Hidden,
                move |_| r.set(true),
            )
            .enabled(|s| !s.play.is_playing() && !s.selection.is_empty()),
        );
        StylePicker {
            open: false,
            requested,
            apply_to: ApplyTo::Model,
            focus: 0,
            hover: None,
            previewing: None,
            selection_revision: 0,
            prop: PropName::from(PROPERTY),
            thumbs: Thumbnails::default(),
            last_rect: None,
        }
    }

    /// Uses `source` for the thumbnails (the editor builds a [`GpuThumbnails`] on first use).
    pub fn set_thumbnail_source(&mut self, source: Box<dyn ThumbnailSource>) {
        self.thumbs.source = Some(source);
    }

    /// True while the popover shows.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The tile the viewport previews, if any.
    pub fn previewing(&self) -> Option<&'static str> {
        self.previewing.map(|(i, _)| LOOK_OVERRIDES[i])
    }

    /// The current Apply to choice.
    pub fn apply_to(&self) -> ApplyTo {
        self.apply_to
    }

    /// Sets Apply to (the footer segment).
    pub fn set_apply_to(&mut self, state: &mut EditorState, to: ApplyTo) {
        if self.apply_to != to {
            self.end_preview(state);
            self.apply_to = to;
        }
    }

    /// Thumbnails rendered since the popover opened, and how many are ready.
    pub fn thumbnail_progress(&self) -> (usize, usize) {
        (
            self.thumbs.rendered,
            self.thumbs.images.iter().filter(|i| i.is_some()).count(),
        )
    }

    /// Opens the popover on the selection (does nothing with an empty selection or while
    /// playing).
    pub fn open(&mut self, state: &EditorState) {
        if state.selection.is_empty() || state.play.is_playing() {
            return;
        }
        self.open = true;
        self.selection_revision = state.selection.revision();
        self.apply_to = default_apply_to(&state.world, &state.selection);
        self.focus = current_tile(&state.world, &self.targets(state)).unwrap_or(0);
        self.hover = None;
        self.previewing = None;
        let source = self.thumbs.source.take();
        self.thumbs = Thumbnails {
            source,
            ..Thumbnails::default()
        };
        let parts = state.selection.parts(&state.world);
        self.thumbs.camera = thumbnail_camera(&state.world, &parts);
        self.thumbs.world = Some(isolate(&state.world, &parts));
        self.thumbs.parts = parts;
    }

    /// Closes the popover; a preview is cancelled (the selection is as before).
    pub fn close(&mut self, state: &mut EditorState) {
        self.end_preview(state);
        self.open = false;
        self.last_rect = None;
        self.thumbs.world = None;
        self.thumbs.images = Default::default();
    }

    /// The instances a click writes to.
    pub fn targets(&self, state: &EditorState) -> Vec<InstanceId> {
        targets(&state.world, &state.selection, self.apply_to)
    }

    /// Previews tile `i` on the targets through the preview overlay.
    pub fn preview(&mut self, state: &mut EditorState, i: usize, pinned: bool) {
        let Some(&style) = LOOK_OVERRIDES.get(i) else {
            return;
        };
        for id in self.targets(state) {
            let _ = state.preview.set(
                &mut state.world,
                id,
                &self.prop,
                Value::String(style.into()),
            );
        }
        self.previewing = Some((i, pinned));
    }

    /// Ends a preview without applying it.
    pub fn end_preview(&mut self, state: &mut EditorState) {
        if self.previewing.take().is_some() && state.preview.is_active() {
            state.preview.cancel(&mut state.world);
        }
    }

    /// Applies tile `i`: one undo step, named for the style and the targets.
    pub fn apply(&mut self, state: &mut EditorState, i: usize) {
        if state.play.is_playing() || i >= TILES {
            return;
        }
        let targets = self.targets(state);
        let name = step_name(&state.world, &targets, LOOK_OVERRIDES[i]);
        self.preview(state, i, false);
        self.previewing = None;
        match state.preview.commit(name.clone()) {
            Some(step) => {
                state.record(step);
                state.note(name);
            }
            // Every target already had this value.
            None => state.preview.cancel(&mut state.world),
        }
        self.focus = i;
    }

    /// Shows the popover (when open) and runs its keys; call once per frame before the
    /// editor's key dispatch, so its Esc, arrows, Space and Enter come first. `viewport` is the
    /// 3D view's rectangle; `rs` builds the GPU thumbnails on first use.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        state: &mut EditorState,
        viewport: Rect,
        rs: Option<&egui_wgpu::RenderState>,
    ) {
        if self.requested.take() {
            self.open(state);
        }
        if !self.open {
            return;
        }
        if state.play.is_playing() || state.selection.revision() != self.selection_revision {
            self.close(state);
            return;
        }
        if self.thumbs.source.is_none()
            && let Some(rs) = rs
        {
            self.thumbs.source = Some(Box::new(GpuThumbnails::new(rs)));
        }
        if self.keys(ctx, state) {
            return;
        }
        let at = pos2(
            (viewport.right() - SIZE.x - 12.0).max(viewport.left() + 12.0),
            viewport.top() + 56.0,
        );
        let mut close = false;
        let mut hovered_tile = None;
        let mut clicked_tile = None;
        let area = Id::NULL.with("style-picker");
        if egui::AreaState::load(ctx, area).is_none() {
            // Known size: skip egui's invisible sizing pass so the popover shows on the frame
            // it opens.
            let seed = egui::AreaState {
                pivot_pos: Some(at),
                size: Some(SIZE),
                interactable: true,
                ..Default::default()
            };
            ctx.memory_mut(|m| {
                m.areas_mut()
                    .set_state(egui::LayerId::new(Order::Foreground, area), seed)
            });
        }
        let shown = Area::new(area)
            .order(Order::Foreground)
            .fixed_pos(at)
            .show(ctx, |ui| {
                Frame::new()
                    .fill(theme::bg_raised())
                    .corner_radius(CornerRadius::same(theme::R_MD))
                    .stroke(Stroke::new(1.0, theme::line()))
                    .shadow(theme::SHADOW_POP)
                    .inner_margin(Margin::same(14))
                    .show(ui, |ui| {
                        ui.set_width(SIZE.x - 28.0);
                        ui.set_min_height(SIZE.y - 28.0);
                        close = self.header(ui, state);
                        ui.add_space(8.0);
                        let current = current_tile(&state.world, &self.targets(state));
                        self.grid(ui, state, current, &mut hovered_tile, &mut clicked_tile);
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(NOTE)
                                    .font(fonts::small())
                                    .color(theme::text_2()),
                            );
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.add(egui::Label::new(
                                        RichText::new("Place Look...")
                                            .font(fonts::small())
                                            .color(theme::text_off()),
                                    ))
                                    .on_hover_text("The Look picker comes with #64");
                                },
                            );
                        });
                        ui.add_space(10.0);
                        self.footer(ui, state);
                    });
            });
        self.last_rect = Some(shown.response.rect);
        if close {
            self.close(state);
            return;
        }
        if let Some(i) = clicked_tile {
            self.apply(state, i);
        } else {
            self.hover_preview(ctx, state, hovered_tile);
        }
        if let Some((i, _)) = self.previewing {
            banner(ctx, viewport, state, i);
        }
        self.render_one(ctx);
    }

    /// Arrows, Space, Enter and Esc; returns true when the popover closed.
    fn keys(&mut self, ctx: &egui::Context, state: &mut EditorState) -> bool {
        if ctx.text_edit_focused() {
            return false;
        }
        let none = egui::Modifiers::NONE;
        let (esc, left, right, up, down, space, enter) = ctx.input_mut(|i| {
            (
                i.consume_key(none, Key::Escape),
                i.consume_key(none, Key::ArrowLeft),
                i.consume_key(none, Key::ArrowRight),
                i.consume_key(none, Key::ArrowUp),
                i.consume_key(none, Key::ArrowDown),
                i.consume_key(none, Key::Space),
                i.consume_key(none, Key::Enter),
            )
        });
        if esc {
            self.close(state);
            return true;
        }
        let moved = move_focus(self.focus, left, right, up, down);
        if moved != self.focus {
            self.focus = moved;
            if self.previewing.is_some_and(|(_, pinned)| pinned) {
                self.preview(state, moved, true);
            }
        }
        if space {
            if self.previewing.is_some_and(|(i, _)| i == self.focus) {
                self.end_preview(state);
            } else {
                self.preview(state, self.focus, true);
            }
        }
        if enter {
            self.apply(state, self.focus);
        }
        false
    }

    fn header(&mut self, ui: &mut egui::Ui, state: &EditorState) -> bool {
        let mut close = false;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Style")
                    .font(fonts::strong())
                    .color(theme::text_1()),
            );
            ui.label(
                RichText::new(subtitle(&state.world, &state.selection))
                    .font(fonts::small())
                    .color(theme::text_3()),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(Button::ghost("").compact().glyph(Icon::Close).alt("Close"))
                    .clicked()
                {
                    close = true;
                }
            });
        });
        close
    }

    fn grid(
        &mut self,
        ui: &mut egui::Ui,
        state: &EditorState,
        current: Option<usize>,
        hovered: &mut Option<usize>,
        clicked: &mut Option<usize>,
    ) {
        let place = place_look(&state.world);
        let gap = vec2(
            (ui.available_width() - TILE.x * COLUMNS as f32) / (COLUMNS as f32 - 1.0),
            8.0,
        );
        for row in 0..TILES / COLUMNS {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap.x;
                for col in 0..COLUMNS {
                    let i = row * COLUMNS + col;
                    let r = self.tile(ui, i, current == Some(i), place);
                    if r.hovered() {
                        *hovered = Some(i);
                    }
                    if r.clicked() {
                        *clicked = Some(i);
                    }
                }
            });
            ui.add_space(gap.y);
        }
    }

    fn tile(
        &mut self,
        ui: &mut egui::Ui,
        i: usize,
        selected: bool,
        place: &'static str,
    ) -> egui::Response {
        let (name, sub) = tile_label(i, place);
        let (rect, r) = ui.allocate_exact_size(vec2(TILE.x, TILE.y + 34.0), Sense::click());
        let description = format!("{name}: {}", DESCRIPTIONS[i]);
        r.widget_info(|| {
            egui::WidgetInfo::selected(egui::Role::RadioButton, true, selected, &description)
        });
        let image = Rect::from_min_size(rect.min, TILE);
        let radius = CornerRadius::same(theme::R_SM);
        let painter = ui.painter();
        painter.rect_filled(image, radius, theme::bg_deep());
        match &self.thumbs.images[i] {
            Some(tex) => {
                painter.image(
                    tex.id(),
                    image.shrink(1.0),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
            None => {
                painter.text(
                    image.center(),
                    Align2::CENTER_CENTER,
                    "Rendering",
                    fonts::small(),
                    theme::text_3(),
                );
            }
        }
        let focused = self.focus == i;
        let stroke = if selected {
            Stroke::new(2.0, theme::accent())
        } else if focused || r.hovered() {
            Stroke::new(1.0, theme::line_strong())
        } else {
            Stroke::new(1.0, theme::line())
        };
        painter.rect_stroke(image, radius, stroke, egui::StrokeKind::Inside);
        if focused && ui.ctx().memory(|m| m.focused().is_none()) {
            crate::widgets::button::focus_ring(painter, image.expand(2.0), radius);
        }
        painter.text(
            pos2(rect.left(), image.bottom() + 6.0),
            Align2::LEFT_TOP,
            name,
            fonts::small(),
            theme::text_1(),
        );
        if let Some(sub) = sub {
            painter.text(
                pos2(rect.left(), image.bottom() + 21.0),
                Align2::LEFT_TOP,
                sub,
                fonts::caption(),
                theme::text_3(),
            );
        }
        r.on_hover_text(description)
    }

    fn footer(&mut self, ui: &mut egui::Ui, state: &mut EditorState) {
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("Apply to")
                    .font(fonts::small())
                    .color(theme::text_2()),
            );
            let mut to = self.apply_to;
            if segmented(
                ui,
                &mut to,
                &[(ApplyTo::Model, "Model"), (ApplyTo::EachPart, "Each part")],
            )
            .changed()
            {
                self.set_apply_to(state, to);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("Hover previews · Enter applies")
                        .font(fonts::small())
                        .color(theme::text_3()),
                );
            });
        });
    }

    /// Hover 150 ms previews; leaving the tiles restores (unless Space pinned a preview).
    fn hover_preview(&mut self, ctx: &egui::Context, state: &mut EditorState, tile: Option<usize>) {
        let now = Instant::now();
        match tile {
            Some(i) => {
                let since = match self.hover {
                    Some((h, t)) if h == i => t,
                    _ => {
                        self.hover = Some((i, now));
                        now
                    }
                };
                self.focus = i;
                let waited = now.duration_since(since);
                if waited >= HOVER_DELAY {
                    // A tile that already is the targets' value has nothing to preview. An empty
                    // preview would still open a gesture, and Undo waits for gestures: after a
                    // click on a tile, with the pointer still on it, Undo would do nothing.
                    if self.previewing.map(|(p, _)| p) != Some(i)
                        && current_tile(&state.world, &self.targets(state)) != Some(i)
                    {
                        self.preview(state, i, false);
                    }
                } else {
                    ctx.request_repaint_after(HOVER_DELAY - waited);
                }
            }
            None => {
                self.hover = None;
                if self.previewing.is_some_and(|(_, pinned)| !pinned) {
                    self.end_preview(state);
                }
            }
        }
    }

    /// Renders the next missing thumbnail (one per frame).
    fn render_one(&mut self, ctx: &egui::Context) {
        let t = &mut self.thumbs;
        if t.next >= TILES {
            return;
        }
        let (Some(source), Some(world), Some(camera)) = (&mut t.source, &mut t.world, &t.camera)
        else {
            return;
        };
        let i = t.next;
        t.next += 1;
        for &id in &t.parts {
            let _ = world.set(id, PROPERTY, Value::String(LOOK_OVERRIDES[i].into()));
        }
        t.rendered += 1;
        if let Some(mut rgba) = source.render(world, camera, THUMB_PX) {
            if matches!(LOOK_OVERRIDES[i], "pixel-3d" | "retro-32") {
                pixelate(&mut rgba, THUMB_PX, PIXEL_BLOCK);
            }
            let size = [THUMB_PX.0 as usize, THUMB_PX.1 as usize];
            if rgba.len() == size[0] * size[1] * 4 {
                let image = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                t.images[i] = Some(ctx.load_texture(
                    format!("style-thumb-{i}"),
                    image,
                    TextureOptions::LINEAR,
                ));
            }
        }
        if t.next < TILES {
            ctx.request_repaint();
        }
    }
}

/// "Previewing Retro / PS1 on Props · click applies one undo step · Esc cancels".
fn banner(ctx: &egui::Context, viewport: Rect, state: &EditorState, i: usize) {
    let (name, _) = tile_label(i, place_look(&state.world));
    let on = state
        .selection
        .active()
        .map(|id| state.display_name(id))
        .unwrap_or_default();
    Area::new(Id::NULL.with("style-picker-banner"))
        .order(Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(pos2(viewport.left() + 12.0, viewport.bottom() - 12.0))
        .interactable(false)
        .show(ctx, |ui| {
            Frame::new()
                .fill(theme::bg_raised())
                .corner_radius(CornerRadius::same(theme::R_SM))
                .stroke(Stroke::new(1.0, theme::line()))
                .inner_margin(Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let small = |t: &str, c| RichText::new(t).font(fonts::small()).color(c);
                        ui.label(small("Previewing", theme::text_2()));
                        ui.label(
                            RichText::new(name)
                                .font(fonts::strong())
                                .color(theme::text_1()),
                        );
                        ui.label(small(
                            &format!("on {on} · click applies one undo step · Esc cancels"),
                            theme::text_2(),
                        ));
                    });
                });
        });
}

/// The tile's name and the line under it: `inherit` is "Place Look" over the place Look's name.
pub fn tile_label(i: usize, place: &'static str) -> (&'static str, Option<&'static str>) {
    if i == 0 {
        return (
            "Place Look",
            Some(look_override::display_name(place).unwrap_or(place)),
        );
    }
    (
        look_override::display_name(LOOK_OVERRIDES[i]).unwrap_or(LOOK_OVERRIDES[i]),
        None,
    )
}

/// The place Look's id: `Lighting.Look` when the world has one (#64), else `realistic`.
pub fn place_look(world: &World) -> &'static str {
    let lighting = world.root("Lighting");
    lighting
        .and_then(|l| match world.get_ref(l, "Look") {
            Ok(Value::String(s)) => look_override::look_override_id(s),
            _ => None,
        })
        .filter(|id| *id != INHERIT)
        .unwrap_or("realistic")
}

/// Model when any Model is selected, else Each part.
pub fn default_apply_to(world: &World, selection: &Selection) -> ApplyTo {
    if selection.ids().iter().any(|&id| world.is_a(id, "Model")) {
        ApplyTo::Model
    } else {
        ApplyTo::EachPart
    }
}

/// The instances a click writes to (see [`ApplyTo`]); only instances with a `LookOverride`.
pub fn targets(world: &World, selection: &Selection, to: ApplyTo) -> Vec<InstanceId> {
    let has = |id| world.get_ref(id, PROPERTY).is_ok();
    match to {
        ApplyTo::Model => selection
            .ids()
            .iter()
            .copied()
            .filter(|&id| has(id))
            .collect(),
        ApplyTo::EachPart => selection
            .parts(world)
            .into_iter()
            .filter(|&id| has(id))
            .collect(),
    }
}

/// The tile matching the targets' own value (`inherit` included); `None` when they differ.
pub fn current_tile(world: &World, targets: &[InstanceId]) -> Option<usize> {
    let mut value = None;
    for &id in targets {
        let v = look_override::own(world, id).unwrap_or(INHERIT);
        if value.is_some_and(|w| w != v) {
            return None;
        }
        value = Some(v);
    }
    LOOK_OVERRIDES.iter().position(|s| Some(*s) == value)
}

/// "Props · 1 model, 3 parts" (the active item's name, then the counts).
pub fn subtitle(world: &World, selection: &Selection) -> String {
    let models = selection
        .ids()
        .iter()
        .filter(|&&id| world.is_a(id, "Model"))
        .count();
    let parts = selection.parts(world).len();
    let name = selection
        .active()
        .and_then(|id| world.get_ref(id, "Name").ok())
        .and_then(|v| match v {
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .unwrap_or_default();
    let mut counts = Vec::new();
    if models > 0 {
        counts.push(plural(models, "model"));
    }
    counts.push(plural(parts, "part"));
    format!("{name} · {}", counts.join(", "))
}

fn plural(n: usize, what: &str) -> String {
    if n == 1 {
        format!("1 {what}")
    } else {
        format!("{n} {what}s")
    }
}

/// The undo step name: "Style: Retro / PS1 (1 model)", "Style: Inherit (3 parts)".
pub fn step_name(world: &World, targets: &[InstanceId], style: &str) -> String {
    let models = targets
        .iter()
        .filter(|&&id| world.is_a(id, "Model"))
        .count();
    let parts = targets.len() - models;
    let mut what = Vec::new();
    if models > 0 {
        what.push(plural(models, "model"));
    }
    if parts > 0 || models == 0 {
        what.push(plural(parts, "part"));
    }
    let name = look_override::display_name(style).unwrap_or(style);
    format!("Style: {name} ({})", what.join(", "))
}

/// Tile focus after the arrow keys (4 x 2 grid, wrapping within the row and column).
pub fn move_focus(i: usize, left: bool, right: bool, up: bool, down: bool) -> usize {
    let (mut row, mut col) = (i / COLUMNS, i % COLUMNS);
    let rows = TILES / COLUMNS;
    if left {
        col = (col + COLUMNS - 1) % COLUMNS;
    }
    if right {
        col = (col + 1) % COLUMNS;
    }
    if up {
        row = (row + rows - 1) % rows;
    }
    if down {
        row = (row + 1) % rows;
    }
    row * COLUMNS + col
}

/// A copy of `world` holding only `parts` (and their ancestors) in the Workspace, with the
/// lighting, for the thumbnails.
pub fn isolate(world: &World, parts: &[InstanceId]) -> World {
    let mut copy = world.deep_clone();
    let keep: std::collections::HashSet<InstanceId> = parts.iter().copied().collect();
    let Some(ws) = copy.root("Workspace") else {
        return copy;
    };
    let mut drop = Vec::new();
    let mut stack = vec![ws];
    while let Some(id) = stack.pop() {
        for &c in copy.children(id) {
            if keep.contains(&c) {
                continue;
            }
            if copy.is_a(c, "BasePart") {
                drop.push(c);
            } else {
                stack.push(c);
            }
        }
    }
    for id in drop {
        copy.destroy(id);
    }
    copy
}

/// A three-quarter view that fills a tile with `parts`.
pub fn thumbnail_camera(world: &World, parts: &[InstanceId]) -> Option<Camera> {
    let b = Aabb::of_parts(world, parts)?;
    let centre = b.centre();
    let radius = (b.size().length() * 0.5).max(0.5);
    let fov: f32 = 40.0;
    let distance = radius / f64::from((fov * 0.5).to_radians().sin()) * 1.05;
    let dir = metallic_render::glam::DVec3::new(1.0, 0.8, 1.0).normalize();
    let eye = centre + dir * distance;
    Some(Camera {
        position: Vec3::new(eye.x as f32, eye.y as f32, eye.z as f32),
        look_at: Vec3::new(centre.x as f32, centre.y as f32, centre.z as f32),
        fov_deg: fov,
    })
}

/// Replaces each `block` x `block` square with its top-left pixel (pixel styles read at tile
/// size; the viewport preview shows the true block size).
pub fn pixelate(rgba: &mut [u8], size: (u32, u32), block: usize) {
    let (w, h) = (size.0 as usize, size.1 as usize);
    if rgba.len() != w * h * 4 || block < 2 {
        return;
    }
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (x - x % block, y - y % block);
            if (sx, sy) == (x, y) {
                continue;
            }
            let (s, d) = ((sy * w + sx) * 4, (y * w + x) * 4);
            let px = [rgba[s], rgba[s + 1], rgba[s + 2], rgba[s + 3]];
            rgba[d..d + 4].copy_from_slice(&px);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scene() -> (World, InstanceId, Vec<InstanceId>) {
        let mut w = World::new_with_services();
        let ws = w.root("Workspace").unwrap();
        let model = w.create("Model", Some(ws)).unwrap();
        w.set(model, "Name", Value::String("Props".into())).unwrap();
        let parts: Vec<_> = (0..3)
            .map(|i| {
                let p = w.create("Part", Some(model)).unwrap();
                w.set(
                    p,
                    "Position",
                    Value::Vector3(metallic_core::Vec3::new(i as f64 * 4.0, 2.0, 0.0)),
                )
                .unwrap();
                p
            })
            .collect();
        w.create("Part", Some(ws)).unwrap();
        (w, model, parts)
    }

    #[test]
    fn targets_follow_apply_to() {
        let (w, model, parts) = scene();
        let mut sel = Selection::new();
        sel.select_only(model);
        assert_eq!(default_apply_to(&w, &sel), ApplyTo::Model);
        assert_eq!(targets(&w, &sel, ApplyTo::Model), vec![model]);
        assert_eq!(targets(&w, &sel, ApplyTo::EachPart), parts);
        sel.set(parts.clone());
        assert_eq!(default_apply_to(&w, &sel), ApplyTo::EachPart);
        assert_eq!(
            subtitle(&w, &{
                let mut s = Selection::new();
                s.select_only(model);
                s
            }),
            "Props · 1 model, 3 parts"
        );
    }

    #[test]
    fn names_and_labels_follow_the_spec() {
        let (w, model, parts) = scene();
        assert_eq!(
            step_name(&w, &[model], "retro-32"),
            "Style: Retro / PS1 (1 model)"
        );
        assert_eq!(step_name(&w, &parts, "inherit"), "Style: Inherit (3 parts)");
        assert_eq!(tile_label(0, "soft"), ("Place Look", Some("Soft")));
        assert_eq!(tile_label(7, "soft"), ("Retro / PS1", None));
        assert_eq!(place_look(&w), "realistic");
        assert_eq!(current_tile(&w, &parts), Some(0));
    }

    #[test]
    fn arrows_move_in_a_four_by_two_grid() {
        assert_eq!(move_focus(0, false, true, false, false), 1);
        assert_eq!(move_focus(3, false, true, false, false), 0);
        assert_eq!(move_focus(0, true, false, false, false), 3);
        assert_eq!(move_focus(1, false, false, false, true), 5);
        assert_eq!(move_focus(5, false, false, true, false), 1);
    }

    #[test]
    fn the_thumbnail_world_holds_only_the_selection() {
        let (w, _, parts) = scene();
        let iso = isolate(&w, &parts);
        let ws = iso.root("Workspace").unwrap();
        let mut count = 0;
        let mut stack = vec![ws];
        while let Some(id) = stack.pop() {
            for &c in iso.children(id) {
                if iso.is_a(c, "BasePart") {
                    count += 1;
                    assert!(parts.contains(&c));
                }
                stack.push(c);
            }
        }
        assert_eq!(count, 3);
        assert!(thumbnail_camera(&w, &parts).is_some());
    }

    #[test]
    fn pixelate_copies_each_block_corner() {
        let mut rgba: Vec<u8> = (0..6 * 3 * 4).map(|i| i as u8).collect();
        pixelate(&mut rgba, (6, 3), 3);
        assert_eq!(&rgba[4..8], &rgba[0..4]);
        assert_eq!(&rgba[(2 * 6 + 2) * 4..(2 * 6 + 3) * 4], &rgba[0..4]);
        assert_eq!(&rgba[(3) * 4..(4) * 4], &rgba[(3) * 4..(4) * 4]);
    }
}

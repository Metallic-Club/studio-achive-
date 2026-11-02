//! A small HUD plate with the controls that matter most (Move, Jump, Run, Camera, Menu) in the
//! player's own bindings, drawn with the runtime's cached glyphs. It shows for a few seconds at the
//! start and again whenever a glyph changes (another device, a rebind, the glyph style), so a
//! rebinding is visible in play at once. It is the only controls row: games need no controls text
//! of their own (#480: Parkour's old text row sat under this plate).
//!
//! The Player and Studio's Play draw this one widget, each from its own [`InputRuntime`] and input
//! clock: once a frame [`ControlsHint::update`], then [`ControlsHint::paint`] into the painter's
//! clip rectangle (the Player's whole window, Studio's viewport). It only paints, so it never takes
//! the pointer or the keyboard from the game.
use super::glyph::slot_glyph;
use super::glyph_paint::{
    BG_RAISED, Ink, LINE_CONTROL, TEXT_1, TEXT_2, TEXT_3, glyph_width, paint_glyph,
};
use super::overrides::binding_device;
use super::{
    ActionId, CHARACTER_CONTROLS, ControlKind, DeviceKind, Glyph, InputRuntime, MENU_ACTION, Slot,
};
use egui::{Align2, Color32, FontId, Galley, Painter, Rect, Shape, Stroke, pos2, vec2};
use metallic_tokens::consts as tokens;
use std::sync::Arc;

/// Seconds the plate stays.
const SHOW_SECONDS: f64 = 8.0;
/// Seconds it takes to fade out.
const FADE_SECONDS: f64 = 0.4;

/// Space between the plate and the left and bottom edges of the painted area at full size.
const MARGIN: f32 = 24.0;
/// Height of the plate at full size.
const PLATE_H: f32 = 48.0;
/// Height of a glyph at full size.
const GLYPH_H: f32 = 26.0;
/// The padding at the plate's left end at full size, and after each row's word.
const PAD: f32 = 20.0;
/// Between a glyph and its word at full size.
const GLYPH_GAP: f32 = 8.0;
/// A narrow area shrinks the plate down to this size; narrower still, the area's clip cuts it.
const MIN_SCALE: f32 = 0.5;

/// The words on the plate, left to right; [`plate_actions`] gives the actions in the same order.
const LABELS: [&str; 5] = ["Move", "Jump", "Run", "Camera", "Menu"];

/// The controls hint.
#[derive(Debug, Default)]
pub struct ControlsHint {
    shown_at: Option<f64>,
    seen: Option<u64>,
    items: Vec<(Glyph, &'static str)>,
    on_touch: bool,
}

/// One row of the plate, measured at some scale.
struct Laid<'a> {
    glyph: &'a Glyph,
    glyph_width: f32,
    word: Arc<Galley>,
}

impl ControlsHint {
    /// A hint that shows from the first [`update`](Self::update).
    pub fn new() -> Self {
        Self::default()
    }

    /// Catches up with the input at `now` seconds on its clock: a glyph change (or the first call)
    /// rebuilds the rows and shows the plate again. Call once a frame before painting.
    pub fn update(&mut self, rt: &InputRuntime, now: f64) {
        let changes = rt.glyph_changes();
        if self.seen != Some(changes) {
            self.seen = Some(changes);
            self.shown_at = Some(now);
            self.items = plate_items(rt);
        }
        self.on_touch = rt.last_device() == DeviceKind::Touch;
    }

    /// The rows, left to right: each glyph on the last-used device and its word.
    pub fn rows(&self) -> &[(Glyph, &'static str)] {
        &self.items
    }

    /// The plate's opacity at `now` (1 until the fade), `None` while it is not showing. On touch it
    /// never shows: the overlay's buttons carry their own labels (#252).
    pub fn opacity(&self, now: f64) -> Option<f32> {
        if self.on_touch || self.items.is_empty() {
            return None;
        }
        let age = now - self.shown_at?;
        (age <= SHOW_SECONDS).then(|| ((SHOW_SECONDS - age) / FADE_SECONDS).clamp(0.0, 1.0) as f32)
    }

    /// Paints the plate at the bottom left of `p`'s clip rectangle, shrunk if that is narrow.
    pub fn paint(&self, p: &Painter, now: f64) {
        let Some(alpha) = self.opacity(now) else {
            return;
        };
        let area = p.clip_rect();
        let (mut rows, mut width) = self.lay_out(p, 1.0);
        let s = (area.width() / (width + 2.0 * MARGIN)).clamp(MIN_SCALE, 1.0);
        if s < 1.0 {
            (rows, width) = self.lay_out(p, s);
        }
        let plate = Rect::from_min_size(
            pos2(
                area.left() + MARGIN * s,
                area.bottom() - (MARGIN + PLATE_H) * s,
            ),
            vec2(width, PLATE_H * s),
        );
        let [r, g, b, _] = tokens::BG_DEEP;
        let fill = Color32::from_rgba_unmultiplied(r, g, b, (224.0 * alpha) as u8);
        let cut = 12.0 * s;
        p.add(Shape::convex_polygon(
            vec![
                plate.left_top(),
                pos2(plate.right() - cut, plate.top()),
                pos2(plate.right(), plate.top() + cut),
                plate.right_bottom(),
                plate.left_bottom(),
            ],
            fill,
            Stroke::NONE,
        ));
        let ink = Ink {
            line: LINE_CONTROL.gamma_multiply(alpha),
            face: BG_RAISED.gamma_multiply(alpha),
            fill: TEXT_1.gamma_multiply(alpha),
            text: TEXT_1.gamma_multiply(alpha),
        };
        let h = GLYPH_H * s;
        let mut x = plate.left() + PAD * s;
        for row in rows {
            let r = Rect::from_min_size(
                pos2(x, plate.center().y - h / 2.0),
                vec2(row.glyph_width, h),
            );
            paint_glyph(p, r, row.glyph, ink);
            x += row.glyph_width + GLYPH_GAP * s;
            let size = row.word.size();
            p.galley(
                pos2(x, plate.center().y - size.y / 2.0),
                row.word,
                TEXT_2.gamma_multiply(alpha),
            );
            x += size.x + PAD * s;
            if row.glyph.toggle {
                p.text(
                    pos2(x - 16.0 * s, plate.top() + 8.0 * s),
                    Align2::RIGHT_TOP,
                    "toggle",
                    FontId::proportional(10.0 * s),
                    TEXT_3.gamma_multiply(alpha),
                );
            }
        }
    }

    /// Measures the rows at scale `s`; also returns the plate's width.
    fn lay_out(&self, p: &Painter, s: f32) -> (Vec<Laid<'_>>, f32) {
        let mut width = PAD * s;
        let rows = self
            .items
            .iter()
            .map(|(glyph, label)| {
                let word =
                    p.layout_no_wrap(label.to_string(), FontId::proportional(15.0 * s), TEXT_2);
                let glyph_width = glyph_width(p, glyph, GLYPH_H * s);
                width += glyph_width + GLYPH_GAP * s + word.size().x + PAD * s;
                Laid {
                    glyph,
                    glyph_width,
                    word,
                }
            })
            .collect();
        (rows, width)
    }
}

/// The plate's actions in [`LABELS`] order (`None` when the game has no such action).
fn plate_actions(rt: &InputRuntime) -> [Option<ActionId>; 5] {
    let character = |name: &str| rt.find_action(&format!("{CHARACTER_CONTROLS}/{name}"));
    [
        character("Move"),
        character("Jump"),
        character("Run"),
        character("Look"),
        rt.find_action(MENU_ACTION),
    ]
}

/// The plate's glyphs and words on the last-used device; actions the game lacks are left out.
/// Built once per glyph change, not per frame.
fn plate_items(rt: &InputRuntime) -> Vec<(Glyph, &'static str)> {
    plate_actions(rt)
        .into_iter()
        .zip(LABELS)
        .filter_map(|(id, label)| Some((plate_glyph(rt, id?)?, label)))
        .collect()
}

/// The glyph of an action's first binding on the last-used device, except that mouse movement
/// held behind a button (Look: the right button, "right-drag") shows that button: holding it is
/// the part to learn, the mouse moving is not.
fn plate_glyph(rt: &InputRuntime, id: ActionId) -> Option<Glyph> {
    let glyph = rt.glyph(id, None)?;
    let device = rt.last_device();
    let held = rt
        .bindings(id)
        .find(|b| binding_device(b) == Some(device))
        .filter(|b| b.key.is_some_and(|k| k.kind() == ControlKind::Delta))
        .and_then(|b| b.modifiers[0]);
    Some(match held {
        Some(button) => Glyph {
            toggle: glyph.toggle,
            ..slot_glyph(&Slot::Single(Some(button)), device, rt.gamepad_style())
        },
        None => glyph.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::glyph::{GlyphArt, MousePart};
    use std::path::Path;

    fn parkour() -> InputRuntime {
        let scene =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates/parkour/main.mscene");
        let world = metallic_core::load_scene(&scene).expect("Parkour scene");
        InputRuntime::from_world(&world)
    }

    fn art(g: &Glyph) -> GlyphArt {
        g.parts[0].expect("bound").art
    }

    /// Parkour's controls make one plate with every key the old text row had (#480).
    #[test]
    fn parkour_gets_one_plate_with_move_and_camera() {
        let items = plate_items(&parkour());
        let labels: Vec<&str> = items.iter().map(|(_, l)| *l).collect();
        assert_eq!(labels, LABELS);
        let words: Vec<&str> = items.iter().map(|(g, _)| g.text.as_str()).collect();
        assert_eq!(
            words,
            ["W A S D", "Space", "Left Shift", "Right Click", "Esc"]
        );
        assert_eq!(
            art(&items[3].0),
            GlyphArt::Mouse(MousePart::Right),
            "Camera is right-drag"
        );
    }

    /// A pad shows its sticks: the held-button rule is for mouse movement only.
    #[test]
    fn gamepad_plate_shows_the_sticks() {
        let mut rt = parkour();
        rt.set_forced_device(Some(DeviceKind::Gamepad));
        let items = plate_items(&rt);
        assert_eq!(items.len(), LABELS.len());
        assert_eq!(items[0].0.text, "Left Stick");
        assert_eq!(items[3].0.text, "Right Stick");
        assert!(items.iter().all(|(g, _)| g.device == DeviceKind::Gamepad));
    }

    /// Solid for the show time, fading at the very end, gone after it; a glyph change restarts it.
    #[test]
    fn it_shows_at_the_start_and_after_a_glyph_change() {
        let mut rt = parkour();
        let mut hint = ControlsHint::new();
        assert_eq!(hint.opacity(0.0), None, "nothing before the first update");
        hint.update(&rt, 2.0);
        assert_eq!(hint.opacity(2.0), Some(1.0));
        assert_eq!(hint.opacity(2.0 + SHOW_SECONDS - FADE_SECONDS), Some(1.0));
        let fading = hint
            .opacity(2.0 + SHOW_SECONDS - FADE_SECONDS / 2.0)
            .unwrap();
        assert!((fading - 0.5).abs() < 1e-3, "{fading}");
        hint.update(&rt, 11.0);
        assert_eq!(
            hint.opacity(11.0),
            None,
            "gone, and no change brings it back"
        );
        rt.set_forced_device(Some(DeviceKind::Gamepad));
        hint.update(&rt, 12.0);
        assert_eq!(hint.opacity(12.0), Some(1.0));
        assert_eq!(hint.rows()[0].0.text, "Left Stick", "the rows follow");
    }

    /// The touch overlay labels its own buttons.
    #[test]
    fn it_stays_away_on_touch() {
        let mut rt = parkour();
        rt.set_forced_device(Some(DeviceKind::Touch));
        let mut hint = ControlsHint::new();
        hint.update(&rt, 0.0);
        assert_eq!(hint.opacity(0.0), None);
    }

    /// Everything it paints stays in the clip rectangle, and a narrow one shrinks the plate.
    #[test]
    fn it_paints_inside_the_area_and_shrinks_to_fit() {
        let rt = parkour();
        let mut hint = ControlsHint::new();
        hint.update(&rt, 0.0);
        for width in [1000.0, 400.0, 300.0] {
            let area = Rect::from_min_size(pos2(50.0, 30.0), vec2(width, 300.0));
            let ctx = egui::Context::default();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                hint.paint(&ui.painter().with_clip_rect(area), 0.0);
            });
            out.textures_delta.clear();
            let mut bounds = Rect::NOTHING;
            for shape in &out.shapes {
                assert_eq!(shape.clip_rect, area);
                bounds = bounds.union(shape.shape.visual_bounding_rect());
            }
            assert!(bounds.is_positive(), "the plate painted");
            assert!(area.contains_rect(bounds), "{bounds:?} in {area:?}");
        }
    }
}

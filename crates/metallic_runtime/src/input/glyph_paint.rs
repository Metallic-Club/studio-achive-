//! Paints the engine's glyphs ([`super::glyph`]) as vector shapes and text, for the Player menu and
//! the controls plate ([`super::ControlsHint`]) in the Player and in Studio's Play.
//!
//! All art here is drawn from scratch for Metallic: rounded keycaps, a plain mouse outline, and the
//! parts of a generic gamepad (round face buttons, a diamond of positions, bumpers, triggers,
//! sticks, a cross-shaped d-pad, two small centre buttons). Monochrome graphite and silver only;
//! no logos, word marks, product names, controller outlines or face-button colour schemes.
use super::glyph::{ControlGlyph, Dir, FacePos, Glyph, GlyphArt, MousePart, Side};
use egui::epaint::{CircleShape, PathShape, PathStroke};
use egui::{Align2, Color32, CornerRadius, FontId, Painter, Pos2, Rect, Shape, Stroke, StrokeKind};
use egui::{pos2, vec2};
use metallic_tokens::consts as tokens;

/// An opaque design token as an egui colour.
const fn color([r, g, b, _]: [u8; 4]) -> Color32 {
    Color32::from_rgb(r, g, b)
}

/// `role.bg-deep`: the base under a keycap.
const BG_DEEP: Color32 = color(tokens::BG_DEEP);
/// `role.bg-base`: dimmed faces.
const BG_BASE: Color32 = color(tokens::BG_BASE);
/// `role.bg-raised`: keycap faces.
pub(super) const BG_RAISED: Color32 = color(tokens::BG_RAISED);
/// `role.line-strong`: dimmed outlines.
const LINE_STRONG: Color32 = color(tokens::LINE_STRONG);
/// `role.line-control`: keycap and control outlines.
pub(super) const LINE_CONTROL: Color32 = color(tokens::LINE_CONTROL);
/// `role.text-1`.
pub(super) const TEXT_1: Color32 = color(tokens::TEXT_1);
/// `role.text-2`.
pub(super) const TEXT_2: Color32 = color(tokens::TEXT_2);
/// `role.text-3`.
pub(super) const TEXT_3: Color32 = color(tokens::TEXT_3);
/// `role.warning`: unbound slots.
const WARNING: Color32 = color(tokens::WARNING);

/// Colours of one glyph.
#[derive(Debug, Clone, Copy)]
pub struct Ink {
    /// Outlines and legends.
    pub line: Color32,
    /// Faces of caps and buttons.
    pub face: Color32,
    /// The pressed part (filled).
    pub fill: Color32,
    /// Legend text.
    pub text: Color32,
}

impl Ink {
    /// The normal look.
    pub const NORMAL: Ink = Ink {
        line: LINE_CONTROL,
        face: BG_RAISED,
        fill: TEXT_1,
        text: TEXT_1,
    };
    /// Dimmed (disabled rows).
    pub const DIM: Ink = Ink {
        line: LINE_STRONG,
        face: BG_BASE,
        fill: TEXT_3,
        text: TEXT_3,
    };
}

fn stroke(ink: Ink) -> Stroke {
    Stroke::new(1.25, ink.line)
}

/// Width a control's glyph takes at height `h`.
pub fn control_width(p: &Painter, g: &ControlGlyph, h: f32) -> f32 {
    match g.art {
        GlyphArt::Keycap { label, wide } => {
            let text = p
                .layout_no_wrap(label.to_string(), legend_font(h, label), Color32::WHITE)
                .size()
                .x;
            if wide {
                (text + h * 0.7).max(h * 1.6)
            } else {
                (text + h * 0.5).max(h)
            }
        }
        GlyphArt::Bumper(_) | GlyphArt::Center(_) => h * 1.5,
        _ => h,
    }
}

/// Width of a whole glyph at height `h`.
pub fn glyph_width(p: &Painter, g: &Glyph, h: f32) -> f32 {
    match g.count {
        0 => {
            p.layout_no_wrap(
                g.text.clone(),
                FontId::proportional(h * 0.42),
                Color32::WHITE,
            )
            .size()
            .x
        }
        1 => g.parts[0].map_or(h, |c| control_width(p, &c, h)),
        _ if is_dpad(g) => h,
        _ => h * 0.62 * 3.0 + 4.0,
    }
}

fn is_dpad(g: &Glyph) -> bool {
    g.count == 4
        && g.parts
            .iter()
            .all(|p| p.is_some_and(|c| matches!(c.art, GlyphArt::DPad(Some(_)))))
}

fn legend_font(h: f32, label: &str) -> FontId {
    let size = if label.chars().count() <= 2 {
        h * 0.48
    } else {
        h * 0.36
    };
    FontId::proportional(size)
}

/// Paints a glyph centred in `rect` (height = `rect.height()`). Composites draw as four small
/// keycaps in an inverted T (Up over Down; Left, Down, Right), or one d-pad when all four parts
/// are d-pad arms. Returns the width used.
pub fn paint_glyph(p: &Painter, rect: Rect, g: &Glyph, ink: Ink) -> f32 {
    let h = rect.height();
    match g.count {
        0 => {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                &g.text,
                FontId::proportional(h * 0.42),
                WARNING,
            );
            glyph_width(p, g, h)
        }
        1 => {
            let Some(c) = g.parts[0] else { return 0.0 };
            let w = control_width(p, &c, h);
            let r = Rect::from_center_size(rect.center(), vec2(w, h));
            paint_control(p, r, &c, ink);
            w
        }
        _ if is_dpad(g) => {
            let r = Rect::from_center_size(rect.center(), vec2(h, h));
            paint_dpad(p, r, None, ink);
            h
        }
        _ => {
            let s = h * 0.62 - 2.0;
            let w = s * 3.0 + 4.0;
            let left = rect.center().x - w / 2.0;
            let top = rect.center().y - h / 2.0;
            let cell = |col: f32, row: f32| {
                Rect::from_min_size(
                    pos2(left + col * (s + 2.0), top + row * (h - s)),
                    vec2(s, s),
                )
            };
            // Parts are Up, Down, Left, Right.
            let at = [
                cell(1.0, 0.0),
                cell(1.0, 1.0),
                cell(0.0, 1.0),
                cell(2.0, 1.0),
            ];
            for (i, part) in g.parts.iter().enumerate() {
                match part {
                    Some(c) => paint_control(p, at[i], c, ink),
                    None => {
                        p.rect_stroke(
                            at[i],
                            tokens::R_XS,
                            Stroke::new(1.0, WARNING),
                            StrokeKind::Inside,
                        );
                    }
                }
            }
            w
        }
    }
}

/// Paints one control's glyph into `r`.
pub fn paint_control(p: &Painter, r: Rect, g: &ControlGlyph, ink: Ink) {
    match g.art {
        GlyphArt::Keycap { label, .. } => {
            keycap(p, r, ink);
            p.text(
                r.center() - vec2(0.0, r.height() * 0.04),
                Align2::CENTER_CENTER,
                label,
                legend_font(r.height(), label),
                ink.text,
            );
        }
        GlyphArt::Arrow(dir) => {
            keycap(p, r, ink);
            triangle(p, r.center(), r.height() * 0.2, dir, ink.text);
        }
        GlyphArt::Mouse(part) => paint_mouse(p, r, part, ink),
        GlyphArt::Face { pos, letter } => match letter {
            Some(l) => {
                let rad = r.height() * 0.42;
                p.add(CircleShape {
                    center: r.center(),
                    radius: rad,
                    fill: ink.face,
                    stroke: stroke(ink),
                });
                p.text(
                    r.center(),
                    Align2::CENTER_CENTER,
                    l,
                    FontId::proportional(r.height() * 0.5),
                    ink.text,
                );
            }
            None => paint_face_diamond(p, r, pos, ink),
        },
        GlyphArt::Bumper(side) => {
            let b = Rect::from_center_size(r.center(), vec2(r.width() * 0.92, r.height() * 0.5));
            let cr = CornerRadius {
                nw: (b.height() * 0.5) as u8,
                ne: (b.height() * 0.5) as u8,
                sw: 2,
                se: 2,
            };
            p.rect(b, cr, ink.face, stroke(ink), StrokeKind::Inside);
            side_letter(p, b.center(), r.height() * 0.36, side, ink);
        }
        GlyphArt::Trigger(side) => {
            let w = r.height() * 0.66;
            let b = Rect::from_center_size(r.center(), vec2(w, r.height() * 0.86));
            let cr = CornerRadius {
                nw: (w * 0.5) as u8,
                ne: (w * 0.5) as u8,
                sw: 3,
                se: 3,
            };
            p.rect(b, cr, ink.face, stroke(ink), StrokeKind::Inside);
            side_letter(
                p,
                b.center() + vec2(0.0, r.height() * 0.08),
                r.height() * 0.36,
                side,
                ink,
            );
        }
        GlyphArt::Stick { side, press } => {
            let c = r.center();
            let outer = r.height() * 0.44;
            p.circle_stroke(c, outer, stroke(ink));
            p.circle_filled(c, outer * 0.62, ink.face);
            p.circle_stroke(c, outer * 0.62, stroke(ink));
            side_letter(p, c, r.height() * 0.32, side, ink);
            if press {
                // Two small marks under the stick: "push down".
                let y = c.y + outer + 1.0;
                for dx in [-outer * 0.35, outer * 0.35] {
                    p.circle_filled(pos2(c.x + dx, y), 1.4, ink.fill);
                }
                p.circle_stroke(c, outer * 0.8, Stroke::new(1.0, ink.fill));
            }
        }
        GlyphArt::DPad(dir) => paint_dpad(p, r, dir, ink),
        GlyphArt::Center(side) => {
            let b = Rect::from_center_size(r.center(), vec2(r.width() * 0.95, r.height() * 0.5));
            p.rect(
                b,
                (b.height() * 0.5) as u8,
                ink.face,
                stroke(ink),
                StrokeKind::Inside,
            );
            let word = match side {
                Side::Left => "Select",
                Side::Right => "Start",
            };
            p.text(
                b.center(),
                Align2::CENTER_CENTER,
                word,
                FontId::proportional(r.height() * 0.26),
                ink.text,
            );
        }
        GlyphArt::Touch => {
            let c = r.center();
            p.circle_stroke(c, r.height() * 0.4, stroke(ink));
            p.circle_filled(c, r.height() * 0.16, ink.fill);
        }
        GlyphArt::Unbound => {}
    }
}

fn keycap(p: &Painter, r: Rect, ink: Ink) {
    let r = r.shrink(0.5);
    // A slightly darker base under the face gives the cap its height.
    p.rect_filled(r, tokens::R_XS, BG_DEEP);
    let face = Rect::from_min_max(r.min, pos2(r.max.x, r.max.y - r.height() * 0.1));
    p.rect(
        face,
        tokens::R_XS,
        ink.face,
        stroke(ink),
        StrokeKind::Inside,
    );
}

fn side_letter(p: &Painter, at: Pos2, size: f32, side: Side, ink: Ink) {
    let l = match side {
        Side::Left => "L",
        Side::Right => "R",
    };
    p.text(
        at,
        Align2::CENTER_CENTER,
        l,
        FontId::proportional(size),
        ink.text,
    );
}

fn triangle(p: &Painter, c: Pos2, s: f32, dir: Dir, color: Color32) {
    let pts = match dir {
        Dir::Up => [
            pos2(c.x, c.y - s),
            pos2(c.x + s, c.y + s * 0.7),
            pos2(c.x - s, c.y + s * 0.7),
        ],
        Dir::Down => [
            pos2(c.x, c.y + s),
            pos2(c.x - s, c.y - s * 0.7),
            pos2(c.x + s, c.y - s * 0.7),
        ],
        Dir::Left => [
            pos2(c.x - s, c.y),
            pos2(c.x + s * 0.7, c.y - s),
            pos2(c.x + s * 0.7, c.y + s),
        ],
        Dir::Right => [
            pos2(c.x + s, c.y),
            pos2(c.x - s * 0.7, c.y + s),
            pos2(c.x - s * 0.7, c.y - s),
        ],
    };
    p.add(Shape::convex_polygon(pts.to_vec(), color, Stroke::NONE));
}

fn paint_mouse(p: &Painter, r: Rect, part: MousePart, ink: Ink) {
    let h = r.height() * 0.9;
    let w = h * 0.64;
    let body = Rect::from_center_size(r.center(), vec2(w, h));
    let round = (w * 0.5) as u8;
    p.rect(body, round, ink.face, stroke(ink), StrokeKind::Inside);
    let split = body.top() + h * 0.42;
    let mid = body.center().x;
    let half = |left: bool| {
        let x0 = if left { body.left() } else { mid };
        let x1 = if left { mid } else { body.right() };
        let cr = if left {
            CornerRadius {
                nw: round,
                ..Default::default()
            }
        } else {
            CornerRadius {
                ne: round,
                ..Default::default()
            }
        };
        (
            Rect::from_min_max(pos2(x0, body.top()), pos2(x1, split)).shrink(1.5),
            cr,
        )
    };
    match part {
        MousePart::Left | MousePart::Right => {
            let (rect, cr) = half(part == MousePart::Left);
            p.rect_filled(rect, cr, ink.fill);
        }
        _ => {}
    }
    p.line_segment(
        [pos2(body.left(), split), pos2(body.right(), split)],
        stroke(ink),
    );
    p.line_segment([pos2(mid, body.top()), pos2(mid, split)], stroke(ink));
    let wheel = Rect::from_center_size(pos2(mid, body.top() + h * 0.22), vec2(w * 0.18, h * 0.2));
    let wheel_fill = matches!(part, MousePart::Middle | MousePart::Wheel);
    p.rect(
        wheel,
        (wheel.width() * 0.5) as u8,
        if wheel_fill { ink.fill } else { ink.face },
        stroke(ink),
        StrokeKind::Middle,
    );
    if part == MousePart::Wheel {
        let c = wheel.center();
        triangle(p, pos2(c.x + w * 0.38, c.y - 2.5), 2.5, Dir::Up, ink.fill);
        triangle(p, pos2(c.x + w * 0.38, c.y + 3.5), 2.5, Dir::Down, ink.fill);
    }
    if part == MousePart::Move {
        let c = pos2(mid, body.top() + h * 0.7);
        let s = w * 0.16;
        for (dx, dy, d) in [
            (0.0, -s, Dir::Up),
            (0.0, s, Dir::Down),
            (-s, 0.0, Dir::Left),
            (s, 0.0, Dir::Right),
        ] {
            triangle(p, pos2(c.x + dx, c.y + dy), s * 0.55, d, ink.fill);
        }
    }
}

fn paint_face_diamond(p: &Painter, r: Rect, pos: FacePos, ink: Ink) {
    let c = r.center();
    let d = r.height() * 0.28;
    let rad = r.height() * 0.15;
    for (fp, off) in [
        (FacePos::North, vec2(0.0, -d)),
        (FacePos::South, vec2(0.0, d)),
        (FacePos::West, vec2(-d, 0.0)),
        (FacePos::East, vec2(d, 0.0)),
    ] {
        let at = c + off;
        if fp == pos {
            p.circle_filled(at, rad, ink.fill);
        } else {
            p.circle_stroke(at, rad, stroke(ink));
        }
    }
}

fn paint_dpad(p: &Painter, r: Rect, dir: Option<Dir>, ink: Ink) {
    let c = r.center();
    let a = r.height() * 0.44;
    let t = r.height() * 0.17;
    // The cross outline, drawn as one closed path.
    let pts = vec![
        pos2(c.x - t, c.y - a),
        pos2(c.x + t, c.y - a),
        pos2(c.x + t, c.y - t),
        pos2(c.x + a, c.y - t),
        pos2(c.x + a, c.y + t),
        pos2(c.x + t, c.y + t),
        pos2(c.x + t, c.y + a),
        pos2(c.x - t, c.y + a),
        pos2(c.x - t, c.y + t),
        pos2(c.x - a, c.y + t),
        pos2(c.x - a, c.y - t),
        pos2(c.x - t, c.y - t),
    ];
    // The cross is concave, so it is filled as two bars and outlined as one path.
    p.rect_filled(
        Rect::from_min_max(pos2(c.x - t, c.y - a), pos2(c.x + t, c.y + a)),
        0.0,
        ink.face,
    );
    p.rect_filled(
        Rect::from_min_max(pos2(c.x - a, c.y - t), pos2(c.x + a, c.y + t)),
        0.0,
        ink.face,
    );
    p.add(Shape::Path(PathShape {
        points: pts,
        closed: true,
        fill: Color32::TRANSPARENT,
        stroke: PathStroke::new(1.25, ink.line),
    }));
    let arm = |d: Dir| -> Rect {
        let inset = 2.0;
        match d {
            Dir::Up => Rect::from_min_max(pos2(c.x - t, c.y - a), pos2(c.x + t, c.y - t)),
            Dir::Down => Rect::from_min_max(pos2(c.x - t, c.y + t), pos2(c.x + t, c.y + a)),
            Dir::Left => Rect::from_min_max(pos2(c.x - a, c.y - t), pos2(c.x - t, c.y + t)),
            Dir::Right => Rect::from_min_max(pos2(c.x + t, c.y - t), pos2(c.x + a, c.y + t)),
        }
        .shrink(inset)
    };
    if let Some(d) = dir {
        p.rect_filled(arm(d), 1.0, ink.fill);
    }
}

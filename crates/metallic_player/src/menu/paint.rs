//! Draws the Player menu and the Controls page with an egui painter, in the visual language's
//! graphite, silver and restrained violet (focus ring, selected tab, the one primary choice).
use super::controls::{
    CONTENT_W, CellArt, Choice, ControlsPage, HOLD_X, Item, Mode, ResetKind, Row, RowKind, SLOT_X,
    SliderId, Tab, segments_rect,
};
use super::{MainItem, theme};
use egui::{
    Align2, Color32, CornerRadius, FontId, LayerId, Order, Painter, Pos2, Rect, Shape, Stroke,
    StrokeKind, pos2, vec2,
};
use metallic_runtime::input::glyph::{ControlGlyph, GamepadStyle, control_glyph};
use metallic_runtime::input::glyph_paint::{Ink, control_width, paint_control, paint_glyph};
use metallic_runtime::input::overrides::GamepadGlyphPref;
use metallic_runtime::input::rebind::LISTEN_TIMEOUT;
use metallic_runtime::input::touch::StickMode;
use metallic_runtime::input::{Control, DeviceKind, MENU_ACTION};

fn font(size: f32) -> FontId {
    FontId::proportional(size)
}

fn painter(ctx: &egui::Context) -> Painter {
    ctx.layer_painter(LayerId::new(
        Order::Foreground,
        egui::Id::unique("metallic_menu"),
    ))
}

/// Points of a rounded rectangle whose top-right corner is cut at 45 degrees (the visual
/// language's signature shape). Convex, so it fills as one polygon.
fn cut_points(r: Rect, radius: f32, cut: f32) -> Vec<Pos2> {
    let mut pts = Vec::with_capacity(32);
    let arc = |pts: &mut Vec<Pos2>, c: Pos2, from: f32| {
        for i in 0..=6 {
            let a = from + i as f32 / 6.0 * std::f32::consts::FRAC_PI_2;
            pts.push(pos2(c.x + radius * a.cos(), c.y + radius * a.sin()));
        }
    };
    use std::f32::consts::PI;
    arc(&mut pts, pos2(r.left() + radius, r.top() + radius), PI);
    pts.push(pos2(r.right() - cut, r.top()));
    pts.push(pos2(r.right(), r.top() + cut));
    arc(&mut pts, pos2(r.right() - radius, r.bottom() - radius), 0.0);
    arc(
        &mut pts,
        pos2(r.left() + radius, r.bottom() - radius),
        PI * 0.5,
    );
    pts
}

fn window(p: &Painter, r: Rect) {
    // `shadow.pop`: 0 8 24 at 0.45, approximated with three soft layers.
    for (grow, alpha) in [(18.0, 30u8), (10.0, 45), (4.0, 60)] {
        p.rect_filled(
            r.translate(vec2(0.0, 8.0)).expand(grow * 0.5),
            theme::R_LG + grow * 0.5,
            Color32::from_black_alpha(alpha),
        );
    }
    p.add(Shape::convex_polygon(
        cut_points(r, theme::R_LG, theme::CUT),
        theme::BG_PANEL,
        Stroke::new(1.0, theme::LINE_STRONG),
    ));
}

fn focus_ring(p: &Painter, r: Rect, radius: f32) {
    p.rect_stroke(
        r.expand(2.0),
        radius + 2.0,
        Stroke::new(theme::FOCUS_W, theme::FOCUS),
        StrokeKind::Outside,
    );
}

/// A secondary button; `primary` paints the violet fill.
fn button(p: &Painter, r: Rect, label: &str, focused: bool, primary: bool) {
    let (fill, text, line) = if primary {
        (theme::ACCENT, theme::ON_ACCENT, theme::ACCENT)
    } else if focused {
        (theme::BG_HOVER, theme::TEXT_1, theme::LINE_CONTROL)
    } else {
        (theme::BG_RAISED, theme::TEXT_1, theme::LINE_STRONG)
    };
    p.rect(
        r,
        theme::R_SM,
        fill,
        Stroke::new(1.0, line),
        StrokeKind::Inside,
    );
    p.text(r.center(), Align2::CENTER_CENTER, label, font(15.0), text);
    if focused {
        focus_ring(p, r, theme::R_SM);
    }
}

/// Hints like "[A] Select  [B] Back", right-aligned to `right`, centred on `y`.
fn hints(p: &Painter, right: f32, y: f32, items: &[(ControlGlyph, &str)]) {
    let h = 22.0;
    let mut x = right;
    for (g, label) in items.iter().rev() {
        let text = p.layout_no_wrap(label.to_string(), font(13.0), theme::TEXT_2);
        x -= text.size().x;
        p.galley(pos2(x, y - text.size().y / 2.0), text, theme::TEXT_2);
        x -= 8.0;
        let w = control_width(p, g, h);
        x -= w;
        paint_control(
            p,
            Rect::from_min_size(pos2(x, y - h / 2.0), vec2(w, h)),
            g,
            Ink::NORMAL,
        );
        x -= 20.0;
    }
}

fn hint_glyphs(
    device: DeviceKind,
    style: GamepadStyle,
    controls: bool,
) -> Vec<(ControlGlyph, &'static str)> {
    let g = |c| control_glyph(c, style);
    match (device, controls) {
        (DeviceKind::Gamepad, false) => vec![
            (g(Control::ButtonA), "Select"),
            (g(Control::ButtonB), "Back"),
        ],
        (DeviceKind::Gamepad, true) => vec![
            (g(Control::ButtonA), "Change"),
            (g(Control::ButtonX), "Clear"),
            (g(Control::ButtonY), "Reset row"),
            (g(Control::ButtonB), "Back"),
        ],
        (_, false) => vec![(g(Control::Return), "Select"), (g(Control::Escape), "Back")],
        (_, true) => vec![
            (g(Control::Return), "Change"),
            (g(Control::Delete), "Clear"),
            (g(Control::R), "Reset row"),
            (g(Control::Escape), "Back"),
        ],
    }
}

fn scrim(p: &Painter, screen: Rect) {
    p.rect_filled(screen, 0.0, theme::SCRIM);
}

/// The first page: Resume, Controls, Leave game. Returns the buttons' rectangles.
pub fn main_page(
    ctx: &egui::Context,
    game: &str,
    focus: usize,
    device: DeviceKind,
    style: GamepadStyle,
) -> Vec<(Rect, usize)> {
    let p = painter(ctx);
    let screen = ctx.content_rect();
    scrim(&p, screen);
    let size = vec2(440.0_f32.min(screen.width() - 32.0), 360.0);
    let r = Rect::from_center_size(screen.center(), size);
    window(&p, r);
    p.text(
        pos2(r.left() + 32.0, r.top() + 30.0),
        Align2::LEFT_TOP,
        "Metallic player",
        font(13.0),
        theme::TEXT_3,
    );
    p.text(
        pos2(r.left() + 32.0, r.top() + 50.0),
        Align2::LEFT_TOP,
        game,
        font(26.0),
        theme::TEXT_1,
    );
    let mut rects = Vec::with_capacity(3);
    for (i, item) in MainItem::ALL.iter().enumerate() {
        let b = Rect::from_min_size(
            pos2(r.left() + 32.0, r.top() + 110.0 + i as f32 * 56.0),
            vec2(r.width() - 64.0, 44.0),
        );
        button(&p, b, item.label(), i == focus, false);
        rects.push((b, i));
    }
    hints(
        &p,
        r.right() - 32.0,
        r.bottom() - 28.0,
        &hint_glyphs(device, style, false),
    );
    rects
}

/// The Controls page.
pub fn controls_page(
    ctx: &egui::Context,
    page: &mut ControlsPage,
    game: &str,
    device: DeviceKind,
    style: GamepadStyle,
) {
    let p = painter(ctx);
    let screen = ctx.content_rect();
    scrim(&p, screen);
    let w = (CONTENT_W + 72.0).min(screen.width() - 24.0);
    let h = (screen.height() - 48.0).clamp(320.0, 860.0);
    let r = Rect::from_center_size(screen.center(), vec2(w, h));
    window(&p, r);

    // Title and tabs.
    p.text(
        pos2(r.left() + 36.0, r.top() + 26.0),
        Align2::LEFT_TOP,
        "Controls",
        font(26.0),
        theme::TEXT_1,
    );
    p.text(
        pos2(r.right() - 36.0, r.top() + 36.0),
        Align2::RIGHT_TOP,
        game,
        font(14.0),
        theme::TEXT_3,
    );
    let tabs_y = r.top() + 72.0;
    let mut x = r.left() + 36.0;
    let (prev, next) = match device {
        DeviceKind::Gamepad => (
            control_glyph(Control::ButtonL1, style),
            control_glyph(Control::ButtonR1, style),
        ),
        _ => (
            control_glyph(Control::Q, style),
            control_glyph(Control::E, style),
        ),
    };
    let gw = control_width(&p, &prev, 22.0);
    paint_control(
        &p,
        Rect::from_min_size(pos2(x, tabs_y + 7.0), vec2(gw, 22.0)),
        &prev,
        Ink::NORMAL,
    );
    x += gw + 10.0;
    page.tab_rects.clear();
    for tab in Tab::ALL {
        let label = p.layout_no_wrap(tab.title().to_string(), font(15.0), theme::TEXT_1);
        let tr = Rect::from_min_size(pos2(x, tabs_y), vec2(label.size().x + 28.0, 36.0));
        let selected = tab == page.tab();
        if selected {
            p.rect_filled(tr, theme::R_SM, theme::SELECT);
            p.rect_filled(
                Rect::from_min_max(
                    pos2(tr.left() + 10.0, tr.bottom() - 3.0),
                    pos2(tr.right() - 10.0, tr.bottom() - 1.0),
                ),
                1.0,
                theme::ACCENT,
            );
        }
        p.text(
            tr.center(),
            Align2::CENTER_CENTER,
            tab.title(),
            font(15.0),
            if selected {
                theme::TEXT_1
            } else {
                theme::TEXT_2
            },
        );
        page.tab_rects.push((tr, tab));
        x = tr.right() + 6.0;
    }
    x += 4.0;
    let gw = control_width(&p, &next, 22.0);
    paint_control(
        &p,
        Rect::from_min_size(pos2(x, tabs_y + 7.0), vec2(gw, 22.0)),
        &next,
        Ink::NORMAL,
    );
    let sep = r.top() + 120.0;
    p.line_segment(
        [pos2(r.left() + 24.0, sep), pos2(r.right() - 24.0, sep)],
        Stroke::new(1.0, theme::LINE),
    );

    // Content.
    let content = Rect::from_min_max(
        pos2(r.left(), sep + 1.0),
        pos2(r.right(), r.bottom() - 56.0),
    );
    page.viewport_h = content.height();
    let left = r.center().x - CONTENT_W / 2.0;
    page.origin = pos2(left, content.top() + 8.0);
    let clip = p.with_clip_rect(content);
    let offset = vec2(left, content.top() + 8.0 - page.scroll);
    let focus = page.focus();
    let browsing = matches!(page.mode(), Mode::Browse);
    for (ri, row) in page.rows().iter().enumerate() {
        let top = row.top + offset.y;
        if top > content.bottom() || top + row.height < content.top() {
            continue;
        }
        paint_row(&clip, page, row, offset, device);
        for (ci, cell) in row.cells.iter().enumerate() {
            let rect = cell.rect.translate(offset);
            let focused = browsing && focus == Some((ri, ci));
            paint_cell(&clip, page, row, cell, rect, focused);
        }
    }
    // Scroll indicator.
    let total = page.content_height();
    if total > page.viewport_h {
        let track = Rect::from_min_max(
            pos2(r.right() - 14.0, content.top() + 8.0),
            pos2(r.right() - 10.0, content.bottom() - 8.0),
        );
        p.rect_filled(track, 2.0, theme::BG_RAISED);
        let frac = page.viewport_h / total;
        let pos = page.scroll / (total - page.viewport_h);
        let th = (track.height() * frac).max(24.0);
        let thumb = Rect::from_min_size(
            pos2(track.left(), track.top() + (track.height() - th) * pos),
            vec2(track.width(), th),
        );
        p.rect_filled(thumb, 2.0, theme::LINE_CONTROL);
    }

    // Bottom bar.
    p.line_segment(
        [
            pos2(r.left() + 24.0, r.bottom() - 56.0),
            pos2(r.right() - 24.0, r.bottom() - 56.0),
        ],
        Stroke::new(1.0, theme::LINE),
    );
    hints(
        &p,
        r.right() - 36.0,
        r.bottom() - 28.0,
        &hint_glyphs(device, style, true),
    );
    if let Some(n) = page.notice() {
        p.text(
            pos2(r.left() + 36.0, r.bottom() - 28.0),
            Align2::LEFT_CENTER,
            n,
            font(14.0),
            theme::WARNING,
        );
    }

    // Toast with Undo.
    page.undo_rect = None;
    if let Some(t) = page.toast() {
        let text = t.text.clone();
        let tw = 420.0;
        let tr = Rect::from_center_size(pos2(r.center().x, r.bottom() - 92.0), vec2(tw, 48.0));
        p.rect(
            tr,
            theme::R_MD,
            theme::BG_RAISED,
            Stroke::new(1.0, theme::LINE_CONTROL),
            StrokeKind::Inside,
        );
        p.text(
            pos2(tr.left() + 18.0, tr.center().y),
            Align2::LEFT_CENTER,
            &text,
            font(15.0),
            theme::TEXT_1,
        );
        let ub = Rect::from_min_size(pos2(tr.right() - 128.0, tr.top() + 8.0), vec2(112.0, 32.0));
        button(&p, ub, "", false, false);
        let undo_glyph = match device {
            DeviceKind::Gamepad => control_glyph(Control::ButtonY, style),
            _ => control_glyph(Control::R, style),
        };
        paint_control(
            &p,
            Rect::from_min_size(pos2(ub.left() + 12.0, ub.top() + 5.0), vec2(22.0, 22.0)),
            &undo_glyph,
            Ink::NORMAL,
        );
        p.text(
            pos2(ub.left() + 42.0, ub.center().y),
            Align2::LEFT_CENTER,
            "Undo",
            font(15.0),
            theme::TEXT_1,
        );
        page.undo_rect = Some(ub);
    }

    // Dialogs.
    page.cancel_rect = None;
    page.dialog_rects.clear();
    match page.mode().clone() {
        Mode::Browse => {}
        Mode::Listening { target, .. } => {
            let name = action_name(page, &target.path);
            let slot = slot_label(target.slot, target.part);
            let keyboard = target.device == DeviceKind::KeyboardMouse;
            let menu = target.path == MENU_ACTION;
            let ask = if keyboard {
                "Press a key or a mouse button"
            } else {
                "Press a button or move a stick"
            };
            let help = match (keyboard, menu) {
                (true, true) => {
                    "Esc binds Esc here. Backspace clears. Click Cancel or wait to cancel."
                }
                (true, false) => "Esc or the gamepad's back button cancels. Backspace clears.",
                (false, _) => "Esc on the keyboard cancels, Backspace clears, or wait.",
            };
            let d = dialog(&p, screen, &format!("{name}: {slot}"), ask, help);
            // Countdown.
            let left = (page.listen_left() / LISTEN_TIMEOUT) as f32;
            let bar = Rect::from_min_size(
                pos2(d.left() + 28.0, d.bottom() - 74.0),
                vec2(d.width() - 56.0, 4.0),
            );
            p.rect_filled(bar, 2.0, theme::BG_DEEP);
            p.rect_filled(
                Rect::from_min_size(bar.min, vec2(bar.width() * left, bar.height())),
                2.0,
                theme::TEXT_2,
            );
            let cb = Rect::from_min_size(
                pos2(d.right() - 148.0, d.bottom() - 58.0),
                vec2(120.0, 38.0),
            );
            button(&p, cb, "Cancel", false, false);
            page.cancel_rect = Some(cb);
        }
        Mode::Conflict {
            target,
            control,
            conflict,
            choice,
        } => {
            let name = action_name(page, &target.path);
            let key = control_glyph(control, page.style()).text;
            let body = format!("{key} is already used by {}.", conflict.other_name);
            let help = format!(
                "Swap gives {} the control {name} had. Replace leaves {} unbound.",
                conflict.other_name, conflict.other_name
            );
            let d = dialog(
                &p,
                screen,
                &format!("{name}: {}", slot_label(target.slot, target.part)),
                &body,
                &help,
            );
            let choices = page.conflict_choices();
            let mut x = d.right() - 28.0;
            for c in choices.iter().rev() {
                let label = match c {
                    Choice::Swap => "Swap",
                    Choice::Replace => "Replace",
                    Choice::Cancel => "Cancel",
                };
                let b = Rect::from_min_size(pos2(x - 112.0, d.bottom() - 58.0), vec2(112.0, 38.0));
                button(&p, b, label, *c == choice, *c == Choice::Swap);
                page.dialog_rects.push((b, *c));
                x -= 124.0;
            }
        }
        Mode::Confirm { reset, yes } => {
            let (title, body) = match reset {
                ResetKind::Tab => (
                    format!("Reset {}?", page.tab().title()),
                    "The game's own controls come back for this tab.",
                ),
                ResetKind::All => (
                    "Reset all controls?".to_string(),
                    "Every binding and setting goes back to the game's own.",
                ),
            };
            let d = dialog(
                &p,
                screen,
                &title,
                body,
                "You can undo this for 10 seconds.",
            );
            let cb = Rect::from_min_size(
                pos2(d.right() - 140.0, d.bottom() - 58.0),
                vec2(112.0, 38.0),
            );
            let rb = Rect::from_min_size(
                pos2(d.right() - 264.0, d.bottom() - 58.0),
                vec2(112.0, 38.0),
            );
            button(&p, rb, "Reset", yes, true);
            button(&p, cb, "Cancel", !yes, false);
            // The painter reuses `Choice::Swap` for "Reset" in confirmations.
            page.dialog_rects.push((rb, Choice::Swap));
            page.dialog_rects.push((cb, Choice::Cancel));
        }
    }
}

fn dialog(p: &Painter, screen: Rect, title: &str, body: &str, help: &str) -> Rect {
    p.rect_filled(screen, 0.0, Color32::from_black_alpha(110));
    let d = Rect::from_center_size(
        screen.center(),
        vec2(520.0_f32.min(screen.width() - 32.0), 220.0),
    );
    window(p, d);
    p.text(
        pos2(d.left() + 28.0, d.top() + 24.0),
        Align2::LEFT_TOP,
        title,
        font(19.0),
        theme::TEXT_1,
    );
    p.text(
        pos2(d.left() + 28.0, d.top() + 62.0),
        Align2::LEFT_TOP,
        body,
        font(15.0),
        theme::TEXT_1,
    );
    let g = p.layout(
        help.to_string(),
        font(13.0),
        theme::TEXT_2,
        d.width() - 56.0,
    );
    p.galley(pos2(d.left() + 28.0, d.top() + 92.0), g, theme::TEXT_2);
    d
}

fn action_name(page: &ControlsPage, path: &str) -> String {
    page.base()
        .action(path)
        .map(|(_, a)| {
            if a.display_name.is_empty() {
                a.name.clone()
            } else {
                a.display_name.clone()
            }
        })
        .unwrap_or_default()
}

fn slot_label(slot: usize, part: Option<usize>) -> String {
    let slot = match slot {
        0 => "Primary",
        1 => "Alternate",
        _ => "Extra",
    };
    match part {
        Some(i) => format!("{} ({slot})", ["Up", "Down", "Left", "Right"][i.min(3)]),
        None => slot.to_string(),
    }
}

fn paint_row(p: &Painter, page: &ControlsPage, row: &Row, offset: egui::Vec2, device: DeviceKind) {
    let top = row.top + offset.y;
    let left = offset.x;
    let full = Rect::from_min_size(pos2(left, top), vec2(CONTENT_W, row.height));
    match &row.kind {
        RowKind::Header => {
            let y = top + row.height / 2.0;
            let t = |x: f32, s: &str| {
                p.text(
                    pos2(left + x, y),
                    Align2::LEFT_CENTER,
                    s,
                    font(12.0),
                    theme::TEXT_3,
                );
            };
            t(12.0, "ACTION");
            t(SLOT_X[0], "PRIMARY");
            t(SLOT_X[1], "ALTERNATE");
            t(HOLD_X, "HOLD OR TOGGLE");
        }
        RowKind::Banner(n) => {
            let b = full.shrink2(vec2(0.0, 4.0));
            p.rect(
                b,
                theme::R_SM,
                theme::WARNING_BG,
                Stroke::new(1.0, theme::WARNING),
                StrokeKind::Inside,
            );
            let text = if *n == 1 {
                "1 action has no binding on this device".to_string()
            } else {
                format!("{n} actions have no binding on this device")
            };
            p.circle_filled(pos2(b.left() + 18.0, b.center().y), 4.0, theme::WARNING);
            p.text(
                pos2(b.left() + 32.0, b.center().y),
                Align2::LEFT_CENTER,
                text,
                font(14.0),
                theme::WARNING,
            );
        }
        RowKind::Group(name) => {
            let label = match name.as_str() {
                "System" => "PLAYER".to_string(),
                other => other.to_uppercase(),
            };
            let y = top + row.height - 10.0;
            let t = p.text(
                pos2(left + 12.0, y),
                Align2::LEFT_BOTTOM,
                label,
                font(12.0),
                theme::TEXT_3,
            );
            p.line_segment(
                [
                    pos2(t.right() + 10.0, t.center().y),
                    pos2(left + CONTENT_W, t.center().y),
                ],
                Stroke::new(1.0, theme::LINE),
            );
        }
        RowKind::Section(title) => {
            p.text(
                pos2(left + 12.0, top + row.height - 12.0),
                Align2::LEFT_BOTTOM,
                *title,
                font(17.0),
                theme::TEXT_1,
            );
        }
        RowKind::Action {
            name,
            enabled,
            unbound,
            toggle,
            ..
        } => {
            p.rect_filled(full.shrink2(vec2(0.0, 2.0)), theme::R_SM, theme::BG_BASE);
            let color = if *enabled {
                theme::TEXT_1
            } else {
                theme::TEXT_3
            };
            let nt = p.text(
                pos2(left + 14.0, top + row.height / 2.0),
                Align2::LEFT_CENTER,
                name,
                font(16.0),
                color,
            );
            let mut x = nt.right() + 8.0;
            if *unbound {
                p.circle_filled(pos2(x + 4.0, nt.center().y), 3.5, theme::WARNING);
                x += 14.0;
            }
            if *toggle && page.tab() != Tab::Assist {
                chip(p, pos2(x, nt.center().y), "Toggle");
            }
            if !*enabled {
                chip(p, pos2(x, nt.center().y), "Off now");
            }
            let _ = device;
        }
        RowKind::Setting(label, help) => {
            p.text(
                pos2(left + 14.0, top + row.height / 2.0 - 8.0),
                Align2::LEFT_CENTER,
                *label,
                font(15.0),
                theme::TEXT_1,
            );
            p.text(
                pos2(left + 14.0, top + row.height / 2.0 + 10.0),
                Align2::LEFT_CENTER,
                *help,
                font(12.0),
                theme::TEXT_3,
            );
        }
        RowKind::Footer => {
            p.line_segment(
                [pos2(left, top + 2.0), pos2(left + CONTENT_W, top + 2.0)],
                Stroke::new(1.0, theme::LINE),
            );
        }
    }
}

fn chip(p: &Painter, at: Pos2, text: &str) {
    let g = p.layout_no_wrap(text.to_string(), font(11.0), theme::TEXT_2);
    let r = Rect::from_min_size(pos2(at.x, at.y - 9.0), vec2(g.size().x + 12.0, 18.0));
    p.rect(
        r,
        theme::R_XS,
        theme::BG_RAISED,
        Stroke::new(1.0, theme::LINE_STRONG),
        StrokeKind::Inside,
    );
    p.galley(
        pos2(r.left() + 6.0, r.center().y - g.size().y / 2.0),
        g,
        theme::TEXT_2,
    );
}

fn paint_cell(
    p: &Painter,
    page: &ControlsPage,
    row: &Row,
    cell: &super::controls::Cell,
    r: Rect,
    focused: bool,
) {
    let row_unbound = matches!(row.kind, RowKind::Action { unbound: true, .. });
    let ink = match row.kind {
        RowKind::Action { enabled: false, .. } => Ink::DIM,
        _ => Ink::NORMAL,
    };
    match &cell.item {
        Item::Slot(_) => {
            let fill = if focused {
                theme::BG_HOVER
            } else {
                theme::BG_DEEP
            };
            p.rect(
                r,
                theme::R_SM,
                fill,
                Stroke::new(1.0, theme::LINE),
                StrokeKind::Inside,
            );
            match &cell.art {
                CellArt::Glyph(g) => {
                    if g.is_unbound() {
                        let (text, color) = if row_unbound {
                            ("Unbound", theme::WARNING)
                        } else {
                            ("Empty", theme::TEXT_OFF)
                        };
                        p.text(r.center(), Align2::CENTER_CENTER, text, font(13.0), color);
                    } else {
                        let gr = Rect::from_center_size(r.center(), vec2(r.width() - 12.0, 26.0));
                        paint_glyph(p, gr, g, ink);
                    }
                }
                CellArt::Part(part, dir) => {
                    let gr = Rect::from_center_size(
                        r.center() - vec2(0.0, 4.0),
                        vec2(r.width() - 6.0, 22.0),
                    );
                    match part {
                        Some(c) => {
                            let w = control_width(p, c, 22.0).min(r.width() - 4.0);
                            paint_control(
                                p,
                                Rect::from_center_size(gr.center(), vec2(w, 22.0)),
                                c,
                                ink,
                            );
                        }
                        None => {
                            // An empty part: the outline of a keycap with nothing on it.
                            let color = if row_unbound {
                                theme::WARNING
                            } else {
                                theme::LINE_STRONG
                            };
                            p.rect_stroke(
                                Rect::from_center_size(gr.center(), vec2(18.0, 18.0)),
                                theme::R_XS,
                                Stroke::new(1.0, color),
                                StrokeKind::Inside,
                            );
                        }
                    }
                    // A small marker under the cap: which direction this part is.
                    let m = pos2(r.center().x, r.bottom() - 5.0);
                    small_arrow(p, m, *dir, theme::TEXT_3);
                }
                CellArt::None => {}
            }
        }
        Item::Hold(path) => {
            let on = page.toggles(path);
            segmented(p, r, &["Hold", "Toggle"], usize::from(on));
        }
        Item::ResetRow(_) => {
            let fill = if focused {
                theme::BG_HOVER
            } else {
                theme::BG_RAISED
            };
            p.rect(
                r,
                theme::R_SM,
                fill,
                Stroke::new(1.0, theme::LINE_STRONG),
                StrokeKind::Inside,
            );
            reset_icon(p, r.center(), 8.0, theme::ICON);
        }
        Item::SwapArrows(_) => {
            button(p, r, "Swap", false, false);
        }
        Item::Slider(id) => {
            let (v, (lo, hi), text) = page.slider(*id);
            let track_w = r.width() - 72.0;
            let track = Rect::from_min_size(pos2(r.left(), r.center().y - 2.0), vec2(track_w, 4.0));
            p.rect_filled(track, 2.0, theme::BG_ACTIVE);
            let t = ((v - lo) / (hi - lo)).clamp(0.0, 1.0);
            p.rect_filled(
                Rect::from_min_size(track.min, vec2(track_w * t, 4.0)),
                2.0,
                theme::TEXT_2,
            );
            // A tick at the game's default value.
            let default = match id {
                SliderId::Sensitivity(_)
                | SliderId::HoldScale
                | SliderId::TouchOpacity
                | SliderId::TouchSize => 1.0,
                SliderId::Deadzone(s) => s.control().default_deadzone(),
            };
            let dt = ((default - lo) / (hi - lo)).clamp(0.0, 1.0);
            p.line_segment(
                [
                    pos2(track.left() + track_w * dt, track.top() - 5.0),
                    pos2(track.left() + track_w * dt, track.bottom() + 5.0),
                ],
                Stroke::new(1.0, theme::LINE_CONTROL),
            );
            p.circle(
                pos2(track.left() + track_w * t, track.center().y),
                8.0,
                theme::TEXT_1,
                Stroke::new(1.0, theme::BG_DEEP),
            );
            p.text(
                pos2(r.right(), r.center().y),
                Align2::RIGHT_CENTER,
                text,
                font(15.0),
                theme::TEXT_1,
            );
            if focused {
                focus_ring(
                    p,
                    Rect::from_min_max(
                        pos2(r.left() - 6.0, r.top()),
                        pos2(track.right() + 6.0, r.bottom()),
                    ),
                    theme::R_SM,
                );
            }
            return;
        }
        Item::Switch(id) => {
            let on = page.switch(*id);
            let pill = Rect::from_min_size(pos2(r.left(), r.center().y - 12.0), vec2(48.0, 24.0));
            let fill = if on { theme::SELECT } else { theme::BG_DEEP };
            let line = if on {
                theme::ACCENT
            } else {
                theme::LINE_CONTROL
            };
            p.rect(pill, 12.0, fill, Stroke::new(1.0, line), StrokeKind::Inside);
            let kx = if on {
                pill.right() - 12.0
            } else {
                pill.left() + 12.0
            };
            p.circle_filled(
                pos2(kx, pill.center().y),
                8.0,
                if on { theme::TEXT_1 } else { theme::TEXT_3 },
            );
            p.text(
                pos2(pill.right() + 12.0, pill.center().y),
                Align2::LEFT_CENTER,
                if on { "On" } else { "Off" },
                font(14.0),
                theme::TEXT_2,
            );
            if focused {
                focus_ring(p, pill, 12.0);
            }
            return;
        }
        Item::GlyphStyle => {
            let pref = page.glyph_pref();
            let sel = match pref {
                GamepadGlyphPref::Auto => 0,
                GamepadGlyphPref::Letters => 1,
                GamepadGlyphPref::Positions => 2,
            };
            let seg = segments_rect(r);
            segmented(p, seg, &["Auto", "Letters", "Positions"], sel);
            // A preview of the bottom face button in the style in use.
            let g = control_glyph(Control::ButtonA, page.style());
            paint_control(
                p,
                Rect::from_min_size(
                    pos2(r.right() - 30.0, r.center().y - 14.0),
                    vec2(28.0, 28.0),
                ),
                &g,
                Ink::NORMAL,
            );
            if focused {
                focus_ring(p, seg, theme::R_SM);
            }
            return;
        }
        Item::StickMode => {
            let fixed = page.touch().stick_mode == StickMode::Fixed;
            let (labels, sel) = (["Floating", "Fixed"], usize::from(fixed));
            let seg = segments_rect(r);
            segmented(p, seg, &labels, sel);
            if focused {
                focus_ring(p, seg, theme::R_SM);
            }
            return;
        }
        Item::ResetTab => button(p, r, "Reset this tab", false, false),
        Item::ResetAll => button(p, r, "Reset all", false, false),
    }
    if focused {
        focus_ring(p, r, theme::R_SM);
    }
}

fn segmented(p: &Painter, r: Rect, labels: &[&str], selected: usize) {
    p.rect(
        r,
        theme::R_SM,
        theme::BG_DEEP,
        Stroke::new(1.0, theme::LINE_STRONG),
        StrokeKind::Inside,
    );
    let w = r.width() / labels.len() as f32;
    for (i, l) in labels.iter().enumerate() {
        let s = Rect::from_min_size(pos2(r.left() + i as f32 * w, r.top()), vec2(w, r.height()))
            .shrink(2.0);
        let on = i == selected;
        if on {
            p.rect_filled(s, CornerRadius::same(3), theme::SELECT);
        }
        p.text(
            s.center(),
            Align2::CENTER_CENTER,
            *l,
            font(13.0),
            if on { theme::TEXT_1 } else { theme::TEXT_3 },
        );
    }
}

fn small_arrow(p: &Painter, at: Pos2, part: usize, color: Color32) {
    let s = 2.5;
    let pts = match part {
        0 => vec![
            pos2(at.x, at.y - s),
            pos2(at.x + s, at.y + s * 0.6),
            pos2(at.x - s, at.y + s * 0.6),
        ],
        1 => vec![
            pos2(at.x, at.y + s),
            pos2(at.x - s, at.y - s * 0.6),
            pos2(at.x + s, at.y - s * 0.6),
        ],
        2 => vec![
            pos2(at.x - s, at.y),
            pos2(at.x + s * 0.6, at.y - s),
            pos2(at.x + s * 0.6, at.y + s),
        ],
        _ => vec![
            pos2(at.x + s, at.y),
            pos2(at.x - s * 0.6, at.y + s),
            pos2(at.x - s * 0.6, at.y - s),
        ],
    };
    p.add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A circular arrow: three quarters of a ring with a head.
fn reset_icon(p: &Painter, c: Pos2, rad: f32, color: Color32) {
    let start = -std::f32::consts::FRAC_PI_2 + 0.5;
    let pts: Vec<Pos2> = (0..=18)
        .map(|i| {
            let a = start + i as f32 / 18.0 * std::f32::consts::PI * 1.55;
            pos2(c.x + rad * a.cos(), c.y + rad * a.sin())
        })
        .collect();
    let tip = pts[0];
    p.add(Shape::line(pts, Stroke::new(1.6, color)));
    p.add(Shape::convex_polygon(
        vec![
            pos2(tip.x - 4.5, tip.y - 3.5),
            pos2(tip.x + 1.5, tip.y - 4.5),
            pos2(tip.x - 0.5, tip.y + 1.5),
        ],
        color,
        Stroke::NONE,
    ));
}

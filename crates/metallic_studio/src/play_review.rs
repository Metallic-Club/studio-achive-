//! The offer after Stop (#130): a strip above Output, "Review N changes from Play", that opens
//! into the list of changed Instances with a Keep box per row.
//!
//! It never blocks anything: Stop has already returned to the level as it was, the panels stay
//! live, and ignoring the strip keeps nothing. [`crate::play::review`] holds the data and the
//! rules; [`EditorState`] holds the offer and ends it (Dismiss, Esc, starting Play, the first
//! edit, Keep). This module only draws it and reports what was pressed.

use eframe::egui::{
    self, Align, Align2, Layout, Rect, RichText, Sense, Stroke, Ui, UiBuilder, WidgetInfo, vec2,
};

use crate::editor::EditorState;
use crate::icons;
use crate::panel::{self, RowLook};
use crate::play::review::{MAX_LISTED, PlayReview};
use crate::theme::{self, fonts};
use crate::widgets::{Button, truncated};

/// Height of the strip.
const STRIP: f32 = 40.0;
/// Height of a list row (a compact button fits).
const ROW: f32 = 28.0;
/// Rows visible before the list scrolls.
const VISIBLE_ROWS: usize = 8;
/// Height of the line under the list.
const FOOTER: f32 = 40.0;

/// "3,214".
fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "1 change", "3,214 changes".
fn changes_text(n: usize) -> String {
    match n {
        1 => "1 change".to_string(),
        n => format!("{} changes", grouped(n)),
    }
}

/// "Review 3 changes from Play": what the strip offers.
pub fn offer_text(review: &PlayReview) -> String {
    format!("Review {} from Play", changes_text(review.total()))
}

/// What the line under the list says: the cap when there is one, else how Keep works.
pub fn footer_text(review: &PlayReview) -> String {
    match review.unlisted() {
        0 => "Kept changes apply as one undo step.".to_string(),
        _ => format!(
            "Showing the first {} of {} changes. The rest cannot be kept.",
            grouped(MAX_LISTED),
            grouped(review.total())
        ),
    }
}

/// One line of the list: a group's title, or a row of a group.
#[derive(Clone, Copy)]
enum Line {
    Title(usize),
    Item(usize, usize),
}

fn lines(review: &PlayReview) -> Vec<Line> {
    let mut out = Vec::new();
    for (g, group) in review.groups.iter().enumerate() {
        out.push(Line::Title(g));
        out.extend((0..group.items.len()).map(|i| Line::Item(g, i)));
    }
    out
}

/// The height of the panel while `review` shows, with `room` px between the panels around it. The
/// open list takes at most half of that, so the level stays in view in a small window.
pub fn height(review: &PlayReview, room: f32) -> f32 {
    if review.open {
        let fit = ((room * 0.5 - STRIP - FOOTER) / ROW).floor().max(2.0) as usize;
        let rows = lines(review).len().min(VISIBLE_ROWS).min(fit);
        STRIP + rows as f32 * ROW + FOOTER
    } else {
        STRIP
    }
}

/// What the creator pressed this frame.
#[derive(Default)]
struct Pressed {
    dismiss: bool,
    toggle: bool,
    keep: bool,
    select: Option<metallic_core::InstanceId>,
}

/// Draws the offer into a bottom panel. Does nothing without one.
pub fn ui(ui: &mut Ui, state: &mut EditorState) {
    let mut pressed = Pressed::default();
    let busy = state.edit_refusal().is_some();
    let selection = state.selection.ids().to_vec();
    let Some(review) = state.play_review_mut() else {
        return;
    };
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    strip(ui, review, &mut pressed);
    if review.open {
        list(ui, review, &selection, &mut pressed);
        footer(ui, review, busy, &mut pressed);
    }
    if pressed.keep {
        state.keep_play_changes();
    } else if pressed.dismiss {
        state.dismiss_play_review();
    } else if pressed.toggle {
        state.toggle_play_review();
    }
    if let Some(id) = pressed.select
        && state.is_live(id)
    {
        state.selection.select_only(id);
    }
}

fn strip(ui: &mut Ui, review: &PlayReview, pressed: &mut Pressed) {
    let (rect, line) = ui.allocate_exact_size(vec2(ui.available_width(), STRIP), Sense::hover());
    line.widget_info(|| WidgetInfo::labeled(egui::Role::Label, true, offer_text(review)));
    ui.painter().line_segment(
        [rect.left_bottom(), rect.right_bottom()],
        Stroke::new(1.0, theme::line()),
    );
    let inner = rect.shrink2(vec2(12.0, 0.0));
    let mut buttons = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    buttons.spacing_mut().item_spacing.x = 6.0;
    let label = if review.open { "Hide" } else { "Review" };
    pressed.toggle = buttons
        .add(Button::secondary(label).compact())
        .on_hover_text("List what Play changed")
        .clicked();
    pressed.dismiss = buttons
        .add(Button::ghost("Dismiss").compact().keycap("Esc"))
        .on_hover_text("Keep nothing and close this")
        .clicked();
    let left = buttons.min_rect().left() - 12.0;
    let painter = ui.painter();
    let title = painter.layout_no_wrap(offer_text(review), fonts::strong(), theme::text_1());
    let title_width = title.size().x;
    let y = rect.center().y;
    painter.galley(
        egui::pos2(inner.left(), y - title.size().y / 2.0),
        title,
        theme::text_1(),
    );
    let note_left = inner.left() + title_width + 12.0;
    if left > note_left {
        let note = truncated(
            painter,
            "The level is back as it was. Nothing is kept unless you choose it.",
            fonts::small(),
            theme::text_3(),
            left - note_left,
        );
        let at = egui::pos2(note_left, y - note.size().y / 2.0);
        painter.galley(at, note, theme::text_3());
    }
}

fn list(
    ui: &mut Ui,
    review: &mut PlayReview,
    selection: &[metallic_core::InstanceId],
    pressed: &mut Pressed,
) {
    let lines = lines(review);
    egui::ScrollArea::vertical()
        .id_salt("play_review_list")
        .auto_shrink([false, false])
        .max_height(ui.available_height() - FOOTER)
        .show_rows(ui, ROW, lines.len(), |ui, range| {
            for line in &lines[range] {
                match *line {
                    Line::Title(g) => group_title(ui, review, g),
                    Line::Item(g, i) => item_row(ui, review, g, i, selection, pressed),
                }
            }
        });
}

fn group_title(ui: &mut Ui, review: &mut PlayReview, g: usize) {
    let group = &mut review.groups[g];
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::hover());
    ui.painter().text(
        egui::pos2(rect.left() + 12.0, rect.center().y),
        Align2::LEFT_CENTER,
        format!("{} ({})", group.title, changes_text(group.total())),
        fonts::caption(),
        theme::text_2(),
    );
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(rect.shrink2(vec2(10.0, 0.0)))
            .layout(Layout::right_to_left(Align::Center)),
    );
    let all = group.items.iter().all(|i| i.keep);
    let label = if all { "Keep none" } else { "Keep all" };
    if right.add(Button::ghost(label).compact()).clicked() {
        for item in &mut group.items {
            item.keep = !all;
        }
    }
}

fn item_row(
    ui: &mut Ui,
    review: &mut PlayReview,
    g: usize,
    i: usize,
    selection: &[metallic_core::InstanceId],
    pressed: &mut Pressed,
) {
    let item = &mut review.groups[g].items[i];
    let (rect, row) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), Sense::click());
    let summary = item.summary();
    row.widget_info(|| {
        WidgetInfo::labeled(
            egui::Role::Button,
            true,
            format!("{}: {summary}", item.name),
        )
    });
    let edit_id = item.edit_id();
    let selected = edit_id.is_some_and(|id| selection.contains(&id));
    panel::paint_row(
        ui.painter(),
        rect,
        RowLook {
            hovered: row.hovered(),
            selected,
            ..RowLook::default()
        },
    );
    let mut boxed = ui.new_child(
        UiBuilder::new()
            .id_salt(("play_review_keep", g, i))
            .max_rect(Rect::from_min_max(
                rect.min + vec2(12.0, 0.0),
                egui::pos2(rect.left() + 76.0, rect.bottom()),
            ))
            .layout(Layout::left_to_right(Align::Center)),
    );
    boxed.checkbox(&mut item.keep, "Keep");

    let mut x = rect.left() + 84.0;
    let y = rect.center().y;
    let icon = Rect::from_center_size(egui::pos2(x + 8.0, y), vec2(16.0, 16.0));
    icons::paint_class(ui.painter(), icon, item.class, theme::icon());
    x += 24.0;
    let room = (rect.right() - 12.0 - x).max(0.0);
    let painter = ui.painter();
    let name = truncated(
        painter,
        &item.name,
        fonts::body(),
        theme::text_1(),
        room * 0.35,
    );
    let name_width = name.size().x;
    painter.galley(
        egui::pos2(x, y - name.size().y / 2.0),
        name,
        theme::text_1(),
    );
    x += name_width + 12.0;
    let room = (rect.right() - 12.0 - x).max(0.0);
    let what = truncated(
        painter,
        &summary,
        fonts::small(),
        theme::text_2(),
        room * 0.6,
    );
    let what_width = what.size().x;
    painter.galley(
        egui::pos2(x, y - what.size().y / 2.0),
        what,
        theme::text_2(),
    );
    x += what_width + 12.0;
    let room = (rect.right() - 12.0 - x).max(0.0);
    let place = truncated(painter, &item.place, fonts::small(), theme::text_3(), room);
    painter.galley(
        egui::pos2(
            rect.right() - 12.0 - place.size().x,
            y - place.size().y / 2.0,
        ),
        place,
        theme::text_3(),
    );

    let row = row.on_hover_ui(|ui| {
        ui.label(RichText::new(item.details()).font(fonts::small()));
    });
    if row.clicked() {
        pressed.select = edit_id;
    }
}

fn footer(ui: &mut Ui, review: &PlayReview, busy: bool, pressed: &mut Pressed) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), FOOTER), Sense::hover());
    ui.painter().line_segment(
        [rect.left_top(), rect.right_top()],
        Stroke::new(1.0, theme::line()),
    );
    let inner = rect.shrink2(vec2(12.0, 0.0));
    let picked = review.picked();
    let mut right = ui.new_child(
        UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    let label = match picked {
        0 => "Keep changes".to_string(),
        n => format!("Keep {}", changes_text(n)),
    };
    pressed.keep = right
        .add_enabled(picked > 0 && !busy, Button::primary(&label).compact())
        .clicked();
    let room = (right.min_rect().left() - 12.0 - inner.left()).max(0.0);
    let colour = if review.unlisted() > 0 {
        theme::text_2()
    } else {
        theme::text_3()
    };
    let galley = truncated(
        ui.painter(),
        &footer_text(review),
        fonts::small(),
        colour,
        room,
    );
    ui.painter().galley(
        egui::pos2(inner.left(), rect.center().y - galley.size().y / 2.0),
        galley,
        colour,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::play::review::ChangeGroup;

    fn review(listed: usize, unlisted: usize) -> PlayReview {
        let items = (0..listed)
            .map(|n| crate::play::review::ChangeItem {
                name: format!("P{n}"),
                class: "Part",
                place: "Workspace".into(),
                kind: crate::play::review::ChangeKind::Removed {
                    id: metallic_core::InstanceId::from_bits(n as u64),
                    inside: 0,
                },
                keep: false,
            })
            .collect();
        let group = ChangeGroup {
            title: "Changed during Play".into(),
            items,
            unlisted,
        };
        PlayReview::new(vec![group], 0).unwrap()
    }

    #[test]
    fn the_offer_counts_changes() {
        assert_eq!(offer_text(&review(1, 0)), "Review 1 change from Play");
        assert_eq!(offer_text(&review(3, 0)), "Review 3 changes from Play");
        assert_eq!(
            offer_text(&review(500, 2714)),
            "Review 3,214 changes from Play"
        );
    }

    #[test]
    fn the_footer_says_when_the_list_is_cut() {
        assert_eq!(
            footer_text(&review(3, 0)),
            "Kept changes apply as one undo step."
        );
        assert_eq!(
            footer_text(&review(500, 2714)),
            "Showing the first 500 of 3,214 changes. The rest cannot be kept."
        );
    }

    #[test]
    fn the_open_list_is_taller_and_stops_growing() {
        let mut small = review(2, 0);
        assert_eq!(height(&small, 800.0), STRIP);
        small.open = true;
        assert_eq!(height(&small, 800.0), STRIP + 3.0 * ROW + FOOTER);
        let mut big = review(400, 0);
        big.open = true;
        assert_eq!(
            height(&big, 800.0),
            STRIP + VISIBLE_ROWS as f32 * ROW + FOOTER
        );
        assert_eq!(height(&big, 300.0), STRIP + 2.0 * ROW + FOOTER);
    }
}

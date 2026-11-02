//! Lasso and paint selection over the viewport, and the Select similar popover (spec 10.1).
//!
//! In Lasso and Paint mode the trail is collected here; on release every Workspace part becomes
//! a [`ScreenItem`] (its [`geom::snapshot`] box projected to the screen), [`lasso_hits`] or
//! [`paint_hits`] picks the hits, and [`combine`] applies Shift (add) and Ctrl (remove). A click
//! without travel selects the part under the cursor, or clears. The trail keeps at most
//! [`MAX_TRAIL`] points (a long paint stroke thins itself), and only parts whose screen box
//! reaches the trail's bounds are hit-tested.
//!
//! The box drag itself stays with the viewport, which owns the press; on release it calls
//! [`finish_box`], which picks with [`box_hits`] (fully inside, or touching with Alt) over the
//! same items and combines the same way.
//!
//! The Select similar popover's letters (C M L S K N T P) and Esc are keymap rows in
//! [`crate::keymap::KeyContext::Popover`], so they follow the keymap's text-focus and
//! right-mouse fly guards.
use eframe::egui::{self, Pos2, Stroke};
use glam::DVec2;
use metallic_core::{InstanceId, Value};

use super::{Popover, ViewContext};
use crate::camera::ViewProjection;
use crate::editor::EditorState;
use crate::theme;
use crate::tools::geom;
use crate::tools::select::{
    PAINT_BRUSH_DIAMETER_PX, PartTraits, ScreenItem, ScreenRect, SelectMode, SelectOp, SimilarKey,
    box_hits, combine, lasso_hits, paint_hits, select_similar,
};

/// Most points a lasso or paint trail keeps.
pub const MAX_TRAIL: usize = 512;

/// A lasso or paint trail in progress.
#[derive(Debug, Default, Clone)]
pub struct Gesture {
    trail: Vec<Pos2>,
    active: bool,
}

impl Gesture {
    /// True while a trail is being drawn.
    pub fn is_active(&self) -> bool {
        self.active
    }

    /// Drops the trail without selecting anything.
    pub fn cancel(&mut self) {
        self.trail.clear();
        self.active = false;
    }

    fn push(&mut self, p: Pos2) {
        if self.trail.last().is_some_and(|l| (*l - p).length() <= 2.0) {
            return;
        }
        if self.trail.len() >= MAX_TRAIL {
            // Keep every other point (the first stays, so a lasso still closes on it).
            let mut keep = false;
            self.trail.retain(|_| {
                keep = !keep;
                keep
            });
        }
        self.trail.push(p);
    }
}

/// Every BasePart under Workspace.
pub fn workspace_parts(state: &EditorState) -> Vec<InstanceId> {
    let Some(ws) = state.world.root("Workspace") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    crate::selection::collect_parts(&state.world, ws, &mut out, &mut seen);
    out
}

/// Screen items for every Workspace part in front of the camera.
pub fn screen_items(state: &EditorState, view: &ViewProjection) -> Vec<ScreenItem<InstanceId>> {
    let ids = workspace_parts(state);
    geom::snapshot(&state.world, &ids)
        .into_iter()
        .filter_map(|s| {
            let pts: Vec<DVec2> = s
                .corners()
                .iter()
                .map(|c| view.project(*c).map(|(x, y)| DVec2::new(x, y)))
                .collect::<Option<Vec<_>>>()?;
            let (x, y) = view.project(s.position)?;
            let rect = pts
                .iter()
                .fold(ScreenRect::from_corners(pts[0], pts[0]), |r, p| {
                    ScreenRect::from_corners(r.min.min(*p), r.max.max(*p))
                });
            let locked = matches!(state.world.get_ref(s.id, "Locked"), Ok(Value::Bool(true)));
            Some(ScreenItem {
                id: s.id,
                rect,
                centre: DVec2::new(x, y),
                locked,
            })
        })
        .collect()
}

/// Lasso or paint gesture handling and painting.
pub fn viewport(
    ui: &egui::Ui,
    state: &mut EditorState,
    cx: &ViewContext<'_>,
    painter: &egui::Painter,
    mode: SelectMode,
    g: &mut Gesture,
) {
    let (pressed, down, mods) = ui.input(|i| {
        (
            i.pointer.primary_pressed(),
            i.pointer.primary_down(),
            i.modifiers,
        )
    });
    let pos = ui.input(|i| i.pointer.interact_pos());
    if pressed && let Some(p) = cx.pointer {
        g.trail.clear();
        g.trail.push(p);
        g.active = true;
    }
    if g.active {
        if let Some(p) = pos {
            g.push(p);
        }
        if !down {
            g.active = false;
            finish(
                state,
                cx,
                mode,
                g,
                SelectOp::from_modifiers(mods.shift, mods.command),
            );
        }
    }
    let stroke = Stroke::new(1.5, theme::accent());
    if g.active && g.trail.len() > 1 {
        for w in g.trail.windows(2) {
            painter.line_segment([w[0], w[1]], stroke);
        }
        if mode == SelectMode::Lasso
            && let (Some(first), Some(last)) = (g.trail.first(), g.trail.last())
        {
            painter.line_segment([*last, *first], stroke);
        }
    }
    if mode == SelectMode::Paint
        && let Some(p) = cx.pointer
    {
        painter.circle_stroke(p, (PAINT_BRUSH_DIAMETER_PX * 0.5) as f32, stroke);
    }
}

fn finish(
    state: &mut EditorState,
    cx: &ViewContext<'_>,
    mode: SelectMode,
    g: &mut Gesture,
    op: SelectOp,
) {
    let trail: Vec<DVec2> = g
        .trail
        .drain(..)
        .map(|p| {
            let (x, y) = cx.local(p);
            DVec2::new(x, y)
        })
        .collect();
    let travel = trail.windows(2).map(|w| w[0].distance(w[1])).sum::<f64>();
    let hits = if travel < 4.0 && mode == SelectMode::Lasso {
        // A click: the part under the cursor.
        cx.hovered.into_iter().collect()
    } else {
        // Only parts whose screen box reaches the trail's bounds (plus the paint brush) can hit.
        let reach = if mode == SelectMode::Paint {
            PAINT_BRUSH_DIAMETER_PX * 0.5
        } else {
            0.0
        };
        let bounds = trail.iter().fold(None::<ScreenRect>, |r, p| {
            Some(r.map_or(ScreenRect::from_corners(*p, *p), |r| {
                ScreenRect::from_corners(r.min.min(*p), r.max.max(*p))
            }))
        });
        let mut items = screen_items(state, &cx.view);
        if let Some(b) = bounds {
            let (lo, hi) = (b.min - DVec2::splat(reach), b.max + DVec2::splat(reach));
            items.retain(|i| {
                i.rect.max.x >= lo.x
                    && i.rect.min.x <= hi.x
                    && i.rect.max.y >= lo.y
                    && i.rect.min.y <= hi.y
            });
        }
        match mode {
            SelectMode::Lasso => lasso_hits(&items, &trail),
            _ => paint_hits(&items, &trail),
        }
    };
    let next = combine(state.selection.ids(), &hits, op);
    state.selection.set(next);
}

/// Ends a box drag over `area` (viewport-relative screen points): parts fully inside, or touching
/// with `touching` (Alt), combined with the selection by `op`.
pub fn finish_box(
    state: &mut EditorState,
    view: &ViewProjection,
    area: ScreenRect,
    touching: bool,
    op: SelectOp,
) {
    let hits = box_hits(&screen_items(state, view), area, touching);
    let next = combine(state.selection.ids(), &hits, op);
    state.selection.set(next);
}

/// What Select similar knows about each Workspace part.
pub fn part_traits(state: &EditorState) -> Vec<PartTraits<InstanceId>> {
    let ids = workspace_parts(state);
    let text = |id: InstanceId, prop: &str| match state.world.get_ref(id, prop) {
        Ok(Value::String(s)) => s.clone(),
        _ => String::new(),
    };
    geom::snapshot(&state.world, &ids)
        .into_iter()
        .map(|s| {
            let color = match state.world.get_ref(s.id, "Color") {
                Ok(Value::Color3(c)) => [c.r, c.g, c.b],
                _ => [0.0; 3],
            };
            let style =
                Some(text(s.id, "LookOverride")).filter(|t| !t.is_empty() && t != "Inherit");
            PartTraits {
                id: s.id,
                class: state.world.class_name(s.id).unwrap_or_default().to_string(),
                name: text(s.id, "Name"),
                color,
                material: text(s.id, "Material"),
                style,
                size: s.size,
                tags: state.world.tags(s.id).map(str::to_string).collect(),
                parent: state.world.parent(s.id),
            }
        })
        .collect()
}

/// Runs Select similar on `key` and replaces the selection with the result.
pub fn run_similar(state: &mut EditorState, key: SimilarKey) -> usize {
    let traits = part_traits(state);
    let selected = state.selection.parts(&state.world);
    let found = select_similar(&traits, &selected, key, None);
    let n = found.len();
    if n == 0 {
        super::report(state, "Select similar: select a part first");
        return 0;
    }
    state.selection.set(found);
    state.note(format!("{}: {n} selected", key.label()));
    n
}

/// The Select similar popover: one button per key. Its letters and Esc are keymap rows
/// (`similar.*`, `popover.close`), handled by the editor's key dispatch.
pub fn similar_window(ctx: &egui::Context, state: &mut EditorState, popover: &mut Option<Popover>) {
    let mut open = true;
    let mut chosen = None;
    egui::Window::new("Select similar")
        .collapsible(false)
        .resizable(false)
        .open(&mut open)
        .default_pos(Pos2::new(420.0, 90.0))
        .show(ctx, |ui| {
            for key in SimilarKey::ALL {
                let b = egui::Button::new(key.label()).shortcut_text(key.key().to_string());
                if ui.add_sized([200.0, 22.0], b).clicked() {
                    chosen = Some(key);
                }
            }
        });
    if let Some(key) = chosen {
        run_similar(state, key);
        *popover = None;
    }
    if !open {
        *popover = None;
    }
}

/// Keymap ids of the Select similar letters.
pub const SIMILAR_ACTIONS: [(&str, SimilarKey); 8] = [
    ("similar.color", SimilarKey::Color),
    ("similar.material", SimilarKey::Material),
    ("similar.style", SimilarKey::Style),
    ("similar.size", SimilarKey::Size),
    ("similar.class", SimilarKey::Class),
    ("similar.name", SimilarKey::Name),
    ("similar.tags", SimilarKey::Tags),
    ("similar.parent", SimilarKey::Parent),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_trail_thins_itself_and_keeps_its_start() {
        let mut g = Gesture::default();
        for i in 0..5000 {
            g.push(Pos2::new(i as f32 * 3.0, 0.0));
        }
        assert!(g.trail.len() <= MAX_TRAIL);
        assert_eq!(g.trail[0], Pos2::new(0.0, 0.0));
        assert_eq!(*g.trail.last().unwrap(), Pos2::new(4999.0 * 3.0, 0.0));
        g.cancel();
        assert!(g.trail.is_empty() && !g.is_active());
    }
}

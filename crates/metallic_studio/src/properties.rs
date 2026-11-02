//! Properties: every registry property shared by the selected instances, in sections.
//!
//! The class files decide what shows ([`metallic_core::classes::Editor`]): each property's section,
//! label, control, visibility and whether it can be edited, and its tooltip. Sections sort by their
//! own order: Data, Appearance, Transform, Physics, Behaviour, with any a class adds between
//! them. Values that differ across a multi-selection show "Mixed". Number fields scrub on
//! drag and accept expressions (`3*4`, `+2` adds to the current value, see [`crate::expr`]). Each
//! shows by its kind (studs, degrees, 0-1 fraction, integer), right-aligned in the interface font
//! ([`crate::widgets::number`]).
//!
//! **One gesture, one undo step:** while the pointer is held (scrubbing a number) changes go
//! through the editor's [`crate::preview::PreviewOverlay`], which writes them as a live preview
//! and remembers the first value of each touched property. When the pointer is released the
//! whole gesture is recorded as one `SetProperties` step. Typed values and toggles apply at once
//! as one step each. A Color3 row's swatch opens the colour popover ([`crate::editor_colour`]),
//! which previews and commits as one step the same way.
use std::collections::HashMap;

use eframe::egui::{self, RichText};
use metallic_core::classes::{NumberUnit, PropDef, Section, Widget, all_properties, find_property};
use metallic_core::look_override;
use metallic_core::{CFrame, InstanceId, UDim2, Value, Vec3};

use crate::commands::{Command, PropName};
use crate::editor::{EditorState, UiRequest};
use crate::expr::eval_relative;
use crate::tokens::ui_tokens;
use crate::widgets::number::{self, NumberKind};
use metallic_core::transform::{clamp_part_size, orthonormal};

/// Width of an axis letter's cell (X, Y, Z) before a number field.
const AXIS_WIDTH: f32 = 10.0;
/// Width of the "Position" / "Rotation" cell of a CFrame row, so both rows' fields line up.
const PART_LABEL_WIDTH: f32 = 56.0;
/// The narrowest a number field gets in a narrow panel.
const MIN_FIELD_WIDTH: f32 = 36.0;
/// How many lines a [`Widget::Multiline`] box shows.
const MULTILINE_ROWS: usize = 3;

/// How a Number property is shown (see [`crate::widgets::number`]).
fn number_kind(p: &PropDef) -> NumberKind {
    match p.editor.widget {
        Widget::Drag(NumberUnit::Studs) => NumberKind::Studs,
        Widget::Drag(NumberUnit::Degrees) => NumberKind::Degrees,
        Widget::Drag(NumberUnit::Fraction) => NumberKind::Fraction,
        Widget::Drag(NumberUnit::Integer) => NumberKind::Integer,
        _ => match p.range {
            Some(r) if r.min == 0.0 && r.max == 1.0 => NumberKind::Fraction,
            _ => NumberKind::Studs,
        },
    }
}

/// Width of each of `n` number fields sharing what is left of the row, each after an axis letter
/// when `letters` is set.
fn field_width(ui: &egui::Ui, n: usize, letters: bool) -> f32 {
    let gap = ui.spacing().item_spacing.x;
    let letter = if letters { AXIS_WIDTH + gap } else { 0.0 };
    let n = n as f32;
    ((ui.available_width() - n * (letter + gap) + gap) / n)
        .floor()
        .max(MIN_FIELD_WIDTH)
}

/// The axis letter before a vector component's field, in the axis colour.
fn axis_letter(ui: &mut egui::Ui, axis: usize) {
    ui.add_sized(
        [AXIS_WIDTH, number::height(ui)],
        egui::Label::new(
            RichText::new(["X", "Y", "Z"][axis])
                .color(ui_tokens().axis[axis])
                .strong(),
        ),
    );
}

/// What a number attribute's drag asked for this frame.
enum AttributeScrub {
    /// The attribute now has this value (a frame of a drag).
    Value(String, f64),
    /// The drag ended.
    End,
}

/// A short text for an attribute value the panel cannot edit.
fn attribute_summary(value: &Value) -> String {
    match value {
        Value::Vector3(v) => format!("Vector3({}, {}, {})", v.x, v.y, v.z),
        Value::Color3(c) => format!("Color3({}, {}, {})", c.r, c.g, c.b),
        Value::UDim2(u) => format!(
            "UDim2({}, {}, {}, {})",
            u.x_scale, u.x_offset, u.y_scale, u.y_offset
        ),
        Value::CFrame(_) => "CFrame".to_string(),
        _ => String::new(),
    }
}

/// `props` as the panel's sections, in display order. Rows sort by their order within a section;
/// equal rows keep the order they came in, base class first.
pub fn sections(props: &[&'static PropDef]) -> Vec<(Section, Vec<&'static PropDef>)> {
    let mut sections: Vec<(Section, Vec<&'static PropDef>)> = Vec::new();
    for &p in props {
        match sections.iter_mut().find(|(s, _)| *s == p.editor.section) {
            Some((_, rows)) => rows.push(p),
            None => sections.push((p.editor.section, vec![p])),
        }
    }
    sections.sort_by_key(|(s, _)| (s.order, s.name));
    for (_, rows) in &mut sections {
        rows.sort_by_key(|p| p.editor.order);
    }
    sections
}

/// The text of a property's row: its label, or its name when it has none.
pub fn row_label(p: &PropDef) -> &'static str {
    p.editor.label.unwrap_or(p.name)
}

/// A property's tooltip: the name scripts use, then what it does.
fn row_tooltip(class: &str, p: &PropDef) -> String {
    if p.description.is_empty() {
        format!("{class}.{}", p.name)
    } else {
        format!("{class}.{}\n{}", p.name, p.description)
    }
}

/// True when `p`'s condition holds on every instance of `ids`, or it has none.
fn row_visible(world: &metallic_core::World, ids: &[InstanceId], p: &PropDef) -> bool {
    p.editor.visible_when.is_none_or(|when| {
        ids.iter()
            .all(|&id| when.holds(world.get_ref(id, when.property()).ok()))
    })
}

/// Properties panel state.
#[derive(Default)]
pub struct Properties {
    filter: String,
    /// The property of the open live-preview gesture (its values live in the overlay).
    gesture: Option<String>,
    /// Esc cancelled the gesture; ignore the widget until the pointer is released.
    cancelled: bool,
    /// Text being typed into a string field, by property name.
    text: HashMap<String, String>,
    /// The Attributes section: text being typed into string attributes, by attribute name.
    attr_text: HashMap<String, String>,
    /// The Attributes section: name and type (0 Bool, 1 Number, 2 String) of the attribute to add.
    attr_new_name: String,
    attr_new_kind: usize,
    /// A number attribute being scrubbed: its first value, so the whole drag is one undo step.
    attr_gesture: Option<AttrGesture>,
    /// The Tags section: the tag being typed.
    tag_new: String,
    /// A Color3 row was clicked: the property and the swatch's rectangle.
    colour_request: Option<(String, egui::Rect)>,
}

/// The open scrub of a number attribute.
struct AttrGesture {
    id: InstanceId,
    name: String,
    /// The value before the drag (`Nil` when it did not exist).
    old: Value,
}

/// What a widget asked for this frame.
enum Edit {
    /// Replace the value on every selected instance.
    Set(String, Value),
    /// Change one component of a vector-like value on each instance (keeps the others).
    Component(String, usize, f64),
}

impl Properties {
    /// An empty panel.
    pub fn new() -> Properties {
        Properties::default()
    }

    /// The Color3 row clicked this frame, if any: the editor opens the colour popover
    /// ([`crate::editor_colour`]) next to `rect` for that property.
    pub fn take_colour_request(&mut self) -> Option<(String, egui::Rect)> {
        self.colour_request.take()
    }

    /// True while a live-preview gesture is open.
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// True while a scrub is open or was cancelled with the pointer still held: global keys wait.
    pub fn is_busy(&self) -> bool {
        self.gesture.is_some() || self.cancelled
    }

    /// Esc during a scrub: every previewed value goes back and no step is recorded.
    pub fn cancel_gesture(&mut self, state: &mut EditorState) {
        if self.gesture.take().is_some() {
            state.preview.cancel(&mut state.world);
            self.cancelled = true;
        }
    }

    /// Live value of a number attribute while it is dragged: the world takes it at once and the
    /// history gets nothing until [`Self::end_attribute_scrub`], which records the whole drag as
    /// one step (from the value before the first call).
    pub fn scrub_attribute(
        &mut self,
        state: &mut EditorState,
        id: InstanceId,
        name: &str,
        value: f64,
    ) {
        if self
            .attr_gesture
            .as_ref()
            .is_some_and(|g| g.id != id || g.name != name)
        {
            self.end_attribute_scrub(state);
        }
        if self.attr_gesture.is_none() {
            let old = state
                .world
                .attribute(id, name)
                .cloned()
                .unwrap_or(Value::Nil);
            self.attr_gesture = Some(AttrGesture {
                id,
                name: name.to_string(),
                old,
            });
        }
        let _ = state.world.set_attribute(id, name, Value::Number(value));
    }

    /// Ends the drag of a number attribute and records it as one undo step, if it changed.
    pub fn end_attribute_scrub(&mut self, state: &mut EditorState) {
        let Some(g) = self.attr_gesture.take() else {
            return;
        };
        let new = state
            .world
            .attribute(g.id, &g.name)
            .cloned()
            .unwrap_or(Value::Nil);
        if new != g.old {
            state.record(Command::SetAttribute {
                id: g.id,
                name: g.name,
                old: g.old,
                new,
            });
        }
    }

    /// Shows `value` for `prop` on the selection as a live scrub would (pointer held): it is
    /// previewed, and recorded as one step when the pointer is released. For scripted checks.
    pub fn scrub(&mut self, state: &mut EditorState, prop: &str, value: Value) {
        let ids = state.selection.ids().to_vec();
        self.commit(state, &ids, Edit::Set(prop.to_string(), value), true);
    }

    /// Records the open live-preview gesture as one undo step (pointer released, or Play
    /// starting).
    pub fn finish_gesture(&mut self, state: &mut EditorState) {
        let Some(prop) = self.gesture.take() else {
            return;
        };
        let n = state.preview.len();
        let name = if n == 1 {
            format!("Set {prop}")
        } else {
            format!("Set {prop} ({n} changes)")
        };
        if let Some(cmd) = state.preview.commit(name) {
            state.record(cmd);
        }
    }

    /// Sets `prop` to `value` on every selected instance as one step, exactly as typing the value
    /// into the panel does (clamps and pivot rules included). For scripted checks and commands.
    pub fn set(&mut self, state: &mut EditorState, prop: &str, value: Value) {
        let ids = state.selection.ids().to_vec();
        self.commit(state, &ids, Edit::Set(prop.to_string(), value), false);
    }

    /// Draws the panel.
    pub fn ui(&mut self, ui: &mut egui::Ui, state: &mut EditorState) {
        let pointer_down = ui.input(|i| i.pointer.primary_down());
        if !pointer_down {
            self.finish_gesture(state);
            self.end_attribute_scrub(state);
            self.cancelled = false;
        } else if self.gesture.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_gesture(state);
        }
        let ids: Vec<InstanceId> = state.selection.ids().to_vec();
        let Some(&active) = ids.last() else {
            ui.add_space(8.0);
            ui.label(
                RichText::new("Select something to see its properties.")
                    .color(ui_tokens().text_dim),
            );
            return;
        };
        let class = state.world.class_name(active).unwrap_or("?");
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(
                egui::Vec2::splat(ui_tokens().icon_size),
                egui::Sense::hover(),
            );
            crate::icons::paint_class(ui.painter(), r, class, ui_tokens().icon);
            let title = if ids.len() == 1 {
                state.display_name(active)
            } else {
                format!("{} objects", ids.len())
            };
            ui.label(RichText::new(title).size(ui_tokens().title_size).strong());
            let class_label = ui.label(
                RichText::new(if ids.len() == 1 { class } else { "" }).color(ui_tokens().text_dim),
            );
            crate::editor::with_class_tooltip(class_label, class);
        });
        ui.add(
            egui::TextEdit::singleline(&mut self.filter)
                .hint_text("Filter properties")
                .desired_width(f32::INFINITY),
        );

        // Properties every selected instance has.
        let mut props: Vec<&'static PropDef> = all_properties(class);
        for &id in &ids {
            let c = state.world.class_name(id).unwrap_or("?");
            props.retain(|p| find_property(c, p.name).is_some());
        }
        // Engine bookkeeping (for example a Model's scale) is not shown.
        props.retain(|p| !p.hidden && row_visible(&state.world, &ids, p));
        let filter = self.filter.to_lowercase();
        props.retain(|p| {
            filter.is_empty()
                || p.name.to_lowercase().contains(&filter)
                || row_label(p).to_lowercase().contains(&filter)
        });

        let mut edits: Vec<Edit> = Vec::new();
        let mut attribute_edit: Option<Command> = None;
        let mut attribute_scrub: Option<AttributeScrub> = None;
        // Inputs in the property column are 24 px rows (spec 4.3).
        ui.spacing_mut().interact_size.y = crate::theme::ROW_HEIGHT;
        ui.spacing_mut().item_spacing.y = 2.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (section, in_section) in sections(&props) {
                    egui::CollapsingHeader::new(
                        RichText::new(section.name)
                            .font(crate::theme::fonts::strong())
                            .color(crate::theme::text_2()),
                    )
                    .default_open(true)
                    .show(ui, |ui| {
                        for p in in_section {
                            self.row(ui, state, &ids, active, p, &mut edits);
                        }
                    });
                }
                if ids.len() == 1 {
                    let (attr, scrub) = self.attributes_section(ui, state, active);
                    let tag = self.tags_section(ui, state, active);
                    attribute_edit = attr.or(tag);
                    attribute_scrub = scrub;
                }
            });

        for e in edits {
            self.commit(state, &ids, e, pointer_down);
        }
        match attribute_scrub {
            Some(AttributeScrub::Value(name, value)) => {
                self.scrub_attribute(state, active, &name, value);
            }
            Some(AttributeScrub::End) => self.end_attribute_scrub(state),
            None => {}
        }
        if let Some(cmd) = attribute_edit {
            self.end_attribute_scrub(state);
            let _ = state.apply(cmd);
        }
    }

    /// The Tags section of one instance: remove tags and add a new one. Returns the command for
    /// the one change made this frame, if any.
    fn tags_section(
        &mut self,
        ui: &mut egui::Ui,
        state: &EditorState,
        id: InstanceId,
    ) -> Option<Command> {
        let mut out = None;
        let tags: Vec<String> = state.world.tags(id).map(str::to_string).collect();
        egui::CollapsingHeader::new(
            RichText::new("Tags")
                .font(crate::theme::fonts::strong())
                .color(crate::theme::text_2()),
        )
        .default_open(true)
        .show(ui, |ui| {
            for tag in &tags {
                ui.horizontal(|ui| {
                    ui.label(tag.as_str());
                    if ui.small_button("Remove").clicked() {
                        out = Some(Command::SetTag {
                            id,
                            tag: tag.clone(),
                            on: false,
                            changed: false,
                        });
                    }
                });
            }
            ui.horizontal(|ui| {
                let response = ui.add(
                    egui::TextEdit::singleline(&mut self.tag_new)
                        .hint_text("New tag")
                        .desired_width(140.0),
                );
                let tag = self.tag_new.trim().to_string();
                let problem = metallic_core::check_tag(&tag).err().or_else(|| {
                    state
                        .world
                        .has_tag(id, &tag)
                        .then(|| "already tagged".into())
                });
                let add = ui.add_enabled(problem.is_none(), egui::Button::new("Add"));
                let enter = response.lost_focus()
                    && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    && problem.is_none();
                if (add.clicked() || enter) && problem.is_none() {
                    out = Some(Command::SetTag {
                        id,
                        tag,
                        on: true,
                        changed: false,
                    });
                    self.tag_new.clear();
                }
            });
        });
        out
    }

    /// The Attributes section of one instance: edit, remove and add attributes. Returns the
    /// command for the one change made this frame, if any (each change is one undo step).
    fn attributes_section(
        &mut self,
        ui: &mut egui::Ui,
        state: &EditorState,
        id: InstanceId,
    ) -> (Option<Command>, Option<AttributeScrub>) {
        let mut out = None;
        let mut scrub = None;
        let attributes: Vec<(String, Value)> = state
            .world
            .attributes(id)
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        let set = |name: &str, old: &Value, new: Value| Command::SetAttribute {
            id,
            name: name.to_string(),
            old: old.clone(),
            new,
        };
        egui::CollapsingHeader::new(
            RichText::new("Attributes")
                .font(crate::theme::fonts::strong())
                .color(crate::theme::text_2()),
        )
        .default_open(true)
        .show(ui, |ui| {
            for (name, value) in &attributes {
                ui.horizontal(|ui| {
                    // As in `row`: the label shares the centre line of the control after it.
                    ui.set_min_height(number::height(ui));
                    ui.label(name.as_str());
                    match value {
                        Value::Bool(b) => {
                            let mut now = *b;
                            if ui.checkbox(&mut now, "").changed() {
                                out = Some(set(name, value, Value::Bool(now)));
                            }
                        }
                        Value::Number(n) => {
                            let mut now = *n;
                            let cur = *n;
                            let response =
                                number::field(ui, &mut now, NumberKind::Studs, 80.0, move |s| {
                                    eval_relative(s, cur)
                                });
                            if response.changed() {
                                if response.dragged() {
                                    // Every frame of a drag: live, one undo step at the end.
                                    scrub = Some(AttributeScrub::Value(name.clone(), now));
                                } else {
                                    out = Some(set(name, value, Value::Number(now)));
                                }
                            }
                            if response.drag_stopped() {
                                scrub = Some(AttributeScrub::End);
                            }
                        }
                        Value::String(text) => {
                            let buffer = self
                                .attr_text
                                .entry(name.clone())
                                .or_insert_with(|| text.clone());
                            let response = ui.add(
                                egui::TextEdit::singleline(buffer).desired_width(f32::INFINITY),
                            );
                            if response.lost_focus() {
                                let typed = std::mem::take(buffer);
                                self.attr_text.remove(name);
                                if typed != *text {
                                    out = Some(set(name, value, Value::String(typed)));
                                }
                            }
                        }
                        other => {
                            ui.label(
                                RichText::new(format!(
                                    "{} (edit in a script)",
                                    attribute_summary(other)
                                ))
                                .color(ui_tokens().text_dim),
                            );
                        }
                    }
                    if ui.small_button("Remove").clicked() {
                        out = Some(set(name, value, Value::Nil));
                    }
                });
            }
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.attr_new_name)
                        .hint_text("New attribute name")
                        .desired_width(140.0),
                );
                egui::ComboBox::from_id_salt("attribute-type")
                    .selected_text(["Bool", "Number", "String"][self.attr_new_kind])
                    .show_ui(ui, |ui| {
                        for (i, label) in ["Bool", "Number", "String"].iter().enumerate() {
                            ui.selectable_value(&mut self.attr_new_kind, i, *label);
                        }
                    });
                let name = self.attr_new_name.trim().to_string();
                let problem = metallic_core::check_attribute_name(&name)
                    .err()
                    .or_else(|| {
                        state
                            .world
                            .attribute(id, &name)
                            .map(|_| "that attribute already exists".to_string())
                    });
                let add = ui.add_enabled(problem.is_none(), egui::Button::new("Add"));
                if let Some(problem) = &problem
                    && !name.is_empty()
                {
                    add.on_disabled_hover_text(problem);
                } else if add.clicked() {
                    let value = match self.attr_new_kind {
                        0 => Value::Bool(false),
                        1 => Value::Number(0.0),
                        _ => Value::String(String::new()),
                    };
                    out = Some(set(&name, &Value::Nil, value));
                    self.attr_new_name.clear();
                }
            });
        });
        (out, scrub)
    }

    fn row(
        &mut self,
        ui: &mut egui::Ui,
        state: &mut EditorState,
        ids: &[InstanceId],
        active: InstanceId,
        p: &PropDef,
        edits: &mut Vec<Edit>,
    ) {
        let Some(value) = read(&state.world, active, p.name) else {
            return;
        };
        let mixed = if p.name == "WorldPivot" {
            ids.iter()
                .any(|&id| read(&state.world, id, p.name).as_ref() != Some(&value))
        } else {
            ids.iter()
                .any(|&id| state.world.get_ref(id, p.name).ok() != Some(&value))
        };
        let playing = state.play.is_playing();
        let hover_bg = ui.painter().add(egui::Shape::Noop);
        let row = ui.horizontal(|ui| {
            // As tall as a control from the start: a row grows only downwards, so a label placed
            // before a taller control would otherwise sit above the control's centre line.
            ui.set_min_height(number::height(ui));
            // Names sit left aligned in a fixed column (about 40 % of the default panel).
            let label_w = ui_tokens().label_width;
            ui.allocate_ui_with_layout(
                egui::vec2(label_w, crate::theme::ROW_HEIGHT),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_min_size(egui::vec2(label_w, crate::theme::ROW_HEIGHT));
                    let label = ui.add(
                        egui::Label::new(RichText::new(row_label(p)).color(crate::theme::text_2()))
                            .truncate(),
                    );
                    label.on_hover_text(row_tooltip(
                        state.world.class_name(active).unwrap_or(""),
                        p,
                    ));
                },
            );
            if mixed {
                ui.label(RichText::new("Mixed").color(ui_tokens().text_dim).italics());
            }
            let name = p.name.to_string();
            // A dropdown offers the widget's list, or else the values the property allows.
            let options = match p.editor.widget {
                Widget::Enum(values) => Some(values),
                _ => p.allowed,
            };
            // While playing, values are read-only and drawn disabled.
            ui.add_enabled_ui(!playing && !p.editor.readonly, |ui| {
                match (&value, options) {
                    (Value::Bool(b), _) => {
                        let mut v = *b;
                        if ui.checkbox(&mut v, "").changed() {
                            edits.push(Edit::Set(name, Value::Bool(v)));
                        }
                    }
                    (Value::Number(n), _) => {
                        let mut v = *n;
                        let cur = *n;
                        // A slider goes in front of the same field, which shows the exact value.
                        let slid = match p.editor.widget {
                            Widget::Slider { min, max } => {
                                let slider = egui::Slider::new(&mut v, min..=max).show_value(false);
                                ui.add(slider).changed()
                            }
                            _ => false,
                        };
                        let w = field_width(ui, 1, false);
                        let r = number::field(ui, &mut v, number_kind(p), w, move |s| {
                            eval_relative(s, cur)
                        });
                        if slid || r.changed() {
                            edits.push(Edit::Set(name, Value::Number(v)));
                        }
                    }
                    (Value::String(s), _) if p.editor.widget == Widget::StylePicker => {
                        // "Inherit · Realistic" or the style; a click opens the style picker.
                        let text = style_row_text(state, active, s);
                        if ui
                            .add(egui::Button::new(text).min_size(egui::vec2(160.0, 0.0)))
                            .on_hover_text("Open the style picker (L)")
                            .clicked()
                        {
                            state.request(UiRequest::RunAction(crate::style_picker::OPEN_ID));
                        }
                    }
                    (Value::String(s), Some(allowed)) => {
                        let mut chosen = s.clone();
                        // The arrow's cell is no taller than the text, so the box is as tall as a
                        // number field and the row keeps one centre line.
                        ui.spacing_mut().icon_width = ui.text_style_height(&egui::TextStyle::Body);
                        egui::ComboBox::from_id_salt(p.name)
                            .selected_text(&chosen)
                            .show_ui(ui, |ui| {
                                for &a in allowed {
                                    ui.selectable_value(&mut chosen, a.to_string(), a);
                                }
                            });
                        if &chosen != s {
                            edits.push(Edit::Set(name, Value::String(chosen)));
                        }
                    }
                    (Value::String(s), None) if p.editor.widget == Widget::Script => {
                        let lines = s.lines().count();
                        if ui.button(format!("Edit script ({lines} lines)")).clicked() {
                            state.request(UiRequest::OpenScript(active));
                        }
                    }
                    (Value::String(s), None) => {
                        let buf = self.text.entry(name.clone()).or_insert_with(|| s.clone());
                        let edit = if p.editor.widget == Widget::Multiline {
                            egui::TextEdit::multiline(buf).desired_rows(MULTILINE_ROWS)
                        } else {
                            egui::TextEdit::singleline(buf)
                        };
                        let r = ui.add(edit.desired_width(f32::INFINITY));
                        if r.lost_focus() {
                            if buf != s {
                                edits.push(Edit::Set(name.clone(), Value::String(buf.clone())));
                            }
                            self.text.remove(&name);
                        } else if !r.has_focus() {
                            *buf = s.clone();
                        }
                    }
                    (Value::Vector3(v), _) => {
                        let comps = [v.x, v.y, v.z];
                        let kind = number_kind(p);
                        let w = field_width(ui, 3, true);
                        for (axis, &c) in comps.iter().enumerate() {
                            axis_letter(ui, axis);
                            let mut x = c;
                            let r =
                                number::field(ui, &mut x, kind, w, move |s| eval_relative(s, c));
                            if r.changed() {
                                edits.push(Edit::Component(name.clone(), axis, x));
                            }
                        }
                    }
                    (Value::CFrame(cf), _) => {
                        // Shown and edited as position plus orientation (degrees, Y then X then Z).
                        let o = cf.orientation_degrees();
                        let rows = [
                            (
                                "Position",
                                [cf.position.x, cf.position.y, cf.position.z],
                                0,
                                NumberKind::Studs,
                            ),
                            ("Rotation", [o.x, o.y, o.z], 3, NumberKind::Degrees),
                        ];
                        ui.vertical(|ui| {
                            for (label, comps, base, kind) in rows {
                                ui.horizontal(|ui| {
                                    ui.allocate_ui_with_layout(
                                        egui::vec2(PART_LABEL_WIDTH, number::height(ui)),
                                        egui::Layout::left_to_right(egui::Align::Center),
                                        |ui| {
                                            ui.set_min_width(PART_LABEL_WIDTH);
                                            ui.label(
                                                RichText::new(label).color(ui_tokens().text_dim),
                                            );
                                        },
                                    );
                                    let w = field_width(ui, 3, true);
                                    for (axis, &c) in comps.iter().enumerate() {
                                        axis_letter(ui, axis);
                                        let mut x = c;
                                        let r = number::field(ui, &mut x, kind, w, move |s| {
                                            eval_relative(s, c)
                                        });
                                        if r.changed() {
                                            edits.push(Edit::Component(
                                                name.clone(),
                                                base + axis,
                                                x,
                                            ));
                                        }
                                    }
                                });
                            }
                        });
                    }
                    (Value::Color3(c), _) => {
                        let rgb = [to_u8(c.r), to_u8(c.g), to_u8(c.b)];
                        // The swatch shows the property's own colour; a click opens the popover.
                        let swatch = egui::Button::new("")
                            .fill(egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]))
                            .min_size(egui::vec2(36.0, 18.0));
                        let r = ui
                            .add(swatch)
                            .on_hover_text(format!("Edit {name} in the colour popover"));
                        let label = format!("{name} swatch");
                        r.widget_info(|| {
                            egui::WidgetInfo::labeled(egui::Role::Button, true, &label)
                        });
                        if r.clicked() && !playing {
                            self.colour_request = Some((name.clone(), r.rect));
                        }
                        ui.label(
                            RichText::new(format!("{}, {}, {}", rgb[0], rgb[1], rgb[2]))
                                .color(ui_tokens().text_dim),
                        );
                    }
                    (Value::UDim2(u), _) => {
                        let comps = [u.x_scale, u.x_offset, u.y_scale, u.y_offset];
                        let w = field_width(ui, 4, false);
                        for (i, &c) in comps.iter().enumerate() {
                            let mut x = c;
                            // Scales are fractions of the parent, offsets whole pixels.
                            let kind = if i % 2 == 0 {
                                NumberKind::Fraction
                            } else {
                                NumberKind::Integer
                            };
                            let r =
                                number::field(ui, &mut x, kind, w, move |s| eval_relative(s, c));
                            if r.changed() {
                                edits.push(Edit::Component(name.clone(), i, x));
                            }
                        }
                    }
                    (Value::Ref(r), _) => {
                        let text = match r {
                            Some(id) if state.world.is_alive(*id) => state.display_name(*id),
                            _ => "None".to_string(),
                        };
                        ui.label(RichText::new(text).color(ui_tokens().text_dim));
                    }
                    (Value::Nil, _) => {
                        ui.label(RichText::new("nil").color(ui_tokens().text_dim));
                    }
                }
            });
        });
        if !playing && ui.rect_contains_pointer(row.response.rect) {
            ui.painter().set(
                hover_bg,
                egui::Shape::rect_filled(
                    row.response.rect.expand2(egui::vec2(4.0, 1.0)),
                    egui::CornerRadius::same(crate::theme::R_XS),
                    crate::theme::bg_hover(),
                ),
            );
        }
    }

    fn commit(&mut self, state: &mut EditorState, ids: &[InstanceId], edit: Edit, live: bool) {
        if state.play.is_playing() || (live && self.cancelled) {
            return;
        }
        // A viewport drag owns the overlay; a Properties edit waits for it.
        if !live && state.preview.is_active() {
            return;
        }
        let (prop, values): (String, Vec<(InstanceId, Value)>) = match edit {
            Edit::Set(prop, v) => {
                let vals = ids.iter().map(|&id| (id, v.clone())).collect();
                (prop, vals)
            }
            Edit::Component(prop, axis, x) => {
                let vals = ids
                    .iter()
                    .filter_map(|&id| {
                        let old = read(&state.world, id, &prop)?;
                        Some((id, with_component(&old, axis, x)?))
                    })
                    .collect();
                (prop, vals)
            }
        };
        // Part sizes stay inside the engine's limits, as a script's write would.
        let mut refused = None;
        let values: Vec<(InstanceId, Value)> = values
            .into_iter()
            .filter_map(|(id, v)| match v {
                Value::Vector3(s) if prop == "Size" && state.world.is_a(id, "BasePart") => {
                    Some((id, Value::Vector3(clamp_part_size(s))))
                }
                // A Model's WorldPivot is made orthonormal like core's `set_world_pivot`; one that
                // cannot be (degenerate axes, not finite) is refused, as core refuses it.
                Value::CFrame(cf) if prop == "WorldPivot" => match orthonormal(*cf, "WorldPivot") {
                    Ok(cf) => Some((id, Value::CFrame(Box::new(cf)))),
                    Err(e) => {
                        refused = Some(e.to_string());
                        None
                    }
                },
                v => Some((id, v)),
            })
            .collect();
        if let Some(why) = refused {
            state.note(format!("WorldPivot not set: {why}"));
        }
        // Setting WorldPivot also marks it set (`WorldPivotSet`), as core's `set_world_pivot`
        // does; without that the pivot keeps following the bounding box.
        let marks: Vec<InstanceId> = if prop == "WorldPivot" {
            values
                .iter()
                .map(|(id, _)| *id)
                .filter(|&id| state.world.is_a(id, "Model"))
                .collect()
        } else {
            Vec::new()
        };
        if live {
            let name: PropName = prop.as_str().into();
            let set: PropName = "WorldPivotSet".into();
            for (id, v) in values {
                let _ = state.preview.set(&mut state.world, id, &name, v);
            }
            for id in marks {
                let _ = state
                    .preview
                    .set(&mut state.world, id, &set, Value::Bool(true));
            }
            self.gesture = Some(prop);
            return;
        }
        let mut commands: Vec<Command> = values
            .into_iter()
            .filter_map(|(id, v)| {
                let old = state.world.get(id, &prop).ok()?;
                (old != v).then(|| Command::set(id, &prop, old, v))
            })
            .collect();
        for id in marks {
            if let Ok(old) = state.world.get(id, "WorldPivotSet")
                && old != Value::Bool(true)
            {
                commands.push(Command::set(id, "WorldPivotSet", old, Value::Bool(true)));
            }
        }
        match commands.len() {
            0 => {}
            1 => {
                let _ = state.apply(commands.remove(0));
            }
            n => {
                let _ = state.apply(Command::Batch {
                    name: format!("Set {prop} of {n} objects"),
                    commands,
                });
            }
        }
    }
}

/// The value Properties shows and edits: a Model's `WorldPivot` is its pivot as Luau reads it
/// (the bounding-box centre until set); everything else is the stored value.
fn read(world: &metallic_core::World, id: InstanceId, prop: &str) -> Option<Value> {
    if prop == "WorldPivot" && world.is_a(id, "Model") {
        let cf = metallic_core::transform::world_pivot(world, id).ok()?;
        return Some(Value::CFrame(Box::new(cf)));
    }
    world.get(id, prop).ok()
}

fn to_u8(c: f64) -> u8 {
    c.round().clamp(0.0, 255.0) as u8
}

/// `value` with component `i` replaced by `x` (Vector3: x, y, z; UDim2: xs, xo, ys, yo;
/// CFrame: position x, y, z then orientation x, y, z in degrees).
fn with_component(value: &Value, i: usize, x: f64) -> Option<Value> {
    match *value {
        Value::Vector3(v) => {
            let mut c = [v.x, v.y, v.z];
            *c.get_mut(i)? = x;
            Some(Value::Vector3(Vec3::new(c[0], c[1], c[2])))
        }
        Value::UDim2(u) => {
            let mut c = [u.x_scale, u.x_offset, u.y_scale, u.y_offset];
            *c.get_mut(i)? = x;
            Some(Value::UDim2(UDim2::new(c[0], c[1], c[2], c[3])))
        }
        // 0..3 position, 3..6 orientation in degrees.
        Value::CFrame(ref cf) => {
            let o = cf.orientation_degrees();
            let mut c = [cf.position.x, cf.position.y, cf.position.z, o.x, o.y, o.z];
            *c.get_mut(i)? = x;
            Some(Value::CFrame(Box::new(CFrame::from_position_orientation(
                Vec3::new(c[0], c[1], c[2]),
                Vec3::new(c[3], c[4], c[5]),
            ))))
        }
        _ => None,
    }
}

/// The Style row's text: the stored style, or "Inherit · <what it resolves to>".
fn style_row_text(state: &EditorState, id: InstanceId, stored: &str) -> String {
    if stored != look_override::INHERIT {
        return look_override::display_name(stored)
            .unwrap_or(stored)
            .to_string();
    }
    let resolved = look_override::resolve(&state.world, id)
        .unwrap_or_else(|| crate::style_picker::place_look(&state.world));
    format!(
        "Inherit · {}",
        look_override::display_name(resolved).unwrap_or(resolved)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cframe_edits_position_and_orientation_separately() {
        let cf =
            CFrame::from_position_orientation(Vec3::new(1.0, 2.0, 3.0), Vec3::new(0.0, 30.0, 0.0));
        let v = Value::CFrame(Box::new(cf));
        let Some(Value::CFrame(moved)) = with_component(&v, 1, 5.0) else {
            panic!()
        };
        assert_eq!(moved.position, Vec3::new(1.0, 5.0, 3.0));
        assert!((moved.orientation_degrees().y - 30.0).abs() < 1e-9);
        let Some(Value::CFrame(turned)) = with_component(&v, 4, 90.0) else {
            panic!()
        };
        assert_eq!(turned.position, Vec3::new(1.0, 2.0, 3.0));
        assert!((turned.orientation_degrees().y - 90.0).abs() < 1e-9);
    }

    #[test]
    fn component_edit_keeps_other_axes() {
        let v = Value::Vector3(Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(
            with_component(&v, 1, 9.0),
            Some(Value::Vector3(Vec3::new(1.0, 9.0, 3.0)))
        );
        assert_eq!(with_component(&Value::Bool(true), 0, 1.0), None);
    }

    #[test]
    fn sections_keep_transform_together() {
        let section = |prop: &str| find_property("Part", prop).unwrap().editor.section;
        assert_eq!(section("Position"), section("Size"));
        assert_eq!(section("Anchored").name, "Physics");
        assert_eq!(section("Name").name, "Data");
    }

    /// The rows of `class` as the panel lists them, as (section, row text) pairs.
    fn panel_rows(class: &str) -> Vec<(&'static str, &'static str)> {
        let mut props = all_properties(class);
        props.retain(|p| !p.hidden);
        sections(&props)
            .into_iter()
            .flat_map(|(s, rows)| rows.into_iter().map(move |p| (s.name, row_label(p))))
            .collect()
    }

    #[test]
    fn part_model_script_and_gui_rows_keep_their_places() {
        assert_eq!(
            panel_rows("Part"),
            [
                ("Data", "Name"),
                ("Data", "PivotOffset"),
                ("Appearance", "Color"),
                ("Appearance", "Material"),
                ("Appearance", "Transparency"),
                ("Appearance", "Shape"),
                ("Appearance", "Style"),
                ("Transform", "Position"),
                ("Transform", "Orientation"),
                ("Transform", "Size"),
                ("Physics", "Anchored"),
                ("Physics", "CanCollide"),
            ]
        );
        assert_eq!(
            panel_rows("Model"),
            [
                ("Data", "Name"),
                ("Data", "PrimaryPart"),
                ("Data", "WorldPivot"),
                ("Appearance", "Style"),
            ]
        );
        assert_eq!(
            panel_rows("Script"),
            [
                ("Data", "Name"),
                ("Data", "Source"),
                ("Behaviour", "Enabled"),
            ]
        );
        assert_eq!(
            panel_rows("TextLabel"),
            [
                ("Data", "Name"),
                ("Data", "ZIndex"),
                ("Data", "Active"),
                ("Data", "ClipsDescendants"),
                ("Appearance", "BackgroundColor3"),
                ("Appearance", "BackgroundTransparency"),
                ("Appearance", "Visible"),
                ("Appearance", "Text"),
                ("Appearance", "TextColor3"),
                ("Appearance", "TextSize"),
                ("Transform", "Position"),
                ("Transform", "Size"),
            ]
        );
    }

    /// The issue's acceptance: one class, in one descriptor, is all Insert and Properties need.
    #[test]
    fn a_new_class_shows_in_insert_and_properties_from_its_descriptor_alone() {
        use metallic_core::ValueType;
        use metallic_core::classes::{ClassDesc, ClassFlags, Editor, PropDesc, resolve};
        static ROOT: ClassDesc = ClassDesc::new("Root", None)
            .flags(ClassFlags::ABSTRACT)
            .props(&[PropDesc::new("Name", ValueType::String, || {
                Value::String(String::new())
            })]);
        static BEACON: ClassDesc = ClassDesc::new("Beacon", Some("Root"))
            .flags(ClassFlags::CREATABLE)
            .category("Effects")
            .describe("A light that flashes.")
            .props(&[
                PropDesc::new("Range", ValueType::Number, || Value::Number(16.0))
                    .editor(
                        Editor::section(Section::new("Beacon", 50))
                            .label("Reach")
                            .slider(0.0, 200.0),
                    )
                    .describe("How far the light reaches, in studs."),
            ]);
        // No editor metadata and no insert order: everything has a default.
        static PLAIN: ClassDesc = ClassDesc::new("Plain", Some("Root"))
            .flags(ClassFlags::CREATABLE)
            .props(&[PropDesc::new("Lit", ValueType::Bool, || Value::Bool(false))]);
        static SECRET: ClassDesc = ClassDesc::new("Secret", Some("Root"))
            .flags(ClassFlags::CREATABLE)
            .no_insert();
        let registry = resolve(&[&ROOT, &BEACON, &PLAIN, &SECRET]).unwrap();

        let offered: Vec<&str> = registry.insertable().iter().map(|c| c.name).collect();
        assert_eq!(offered, ["Plain", "Beacon"]);

        let beacon = registry.class_by_name("Beacon").unwrap();
        let shown = sections(&beacon.props);
        let names: Vec<&str> = shown.iter().map(|(s, _)| s.name).collect();
        assert_eq!(
            names,
            ["Data", "Beacon"],
            "ordered by the section's own order"
        );
        let range = shown[1].1[0];
        assert_eq!(row_label(range), "Reach");
        assert_eq!(
            row_tooltip("Beacon", range),
            "Beacon.Range\nHow far the light reaches, in studs."
        );
        assert!(matches!(range.editor.widget, Widget::Slider { .. }));

        let plain = registry.class_by_name("Plain").unwrap();
        let lit = sections(&plain.props)[0].1[1];
        assert_eq!(row_label(lit), "Lit");
        assert_eq!(row_tooltip("Plain", lit), "Plain.Lit");
    }

    /// Controls no built-in class uses yet: a stand-in descriptor over a real property of a Part.
    #[test]
    fn slider_multiline_dropdown_and_readonly_draw_their_controls() {
        use egui::accesskit::Role;
        use egui_kittest::Harness;
        use egui_kittest::kittest::{NodeT, Queryable};
        use metallic_core::classes::{Editor, PropDesc};
        use metallic_core::{ValueType, World};
        static SLIDER: PropDesc = PropDesc::new("Transparency", ValueType::Number, || Value::Nil)
            .editor(Editor::new().slider(0.0, 1.0));
        static MULTILINE: PropDesc = PropDesc::new("Name", ValueType::String, || Value::Nil)
            .editor(Editor::new().multiline());
        static DROPDOWN: PropDesc = PropDesc::new("Name", ValueType::String, || Value::Nil)
            .editor(Editor::new().enumerated(&["Part", "Other"]));
        static READONLY: PropDesc = PropDesc::new("Transparency", ValueType::Number, || Value::Nil)
            .editor(Editor::new().readonly());

        fn draw(p: &'static PropDef) -> Harness<'static, (Properties, EditorState, InstanceId)> {
            let mut world = World::new_with_services();
            let ws = world.root("Workspace").unwrap();
            let part = world.create("Part", Some(ws)).unwrap();
            let output = crate::output::OutputPanel::new();
            let state = EditorState::new(
                world,
                std::path::Path::new("."),
                std::path::Path::new("unused.mscene"),
                "Test",
                output.log.sink(),
            );
            let mut themed = false;
            // Number fields have no accessible name yet, and this harness draws one on its first
            // run, where the check runs (the editor's harnesses draw no field until later).
            let builder = Harness::builder().with_accessibility_check(false);
            let mut h = builder.build_ui_state(
                move |ui, (panel, state, part): &mut (Properties, EditorState, InstanceId)| {
                    if !themed {
                        crate::theme::apply(ui.ctx(), crate::theme::ThemeId::Graphite);
                        themed = true;
                        ui.ctx().request_repaint();
                        return;
                    }
                    let ids = [*part];
                    panel.row(ui, state, &ids, *part, p, &mut Vec::new());
                },
                (Properties::new(), state, part),
            );
            h.run_steps(3);
            h
        }

        assert!(draw(&SLIDER).query_by_role(Role::Slider).is_some());
        assert!(
            draw(&MULTILINE)
                .query_by_role(Role::MultilineTextInput)
                .is_some()
        );
        assert!(draw(&DROPDOWN).query_by_role(Role::ComboBox).is_some());
        let readonly = draw(&READONLY);
        assert!(
            readonly
                .get_by_role(Role::SpinButton)
                .accesskit_node()
                .is_disabled()
        );
        let editable = draw(find_property("Part", "Transparency").unwrap());
        assert!(
            !editable
                .get_by_role(Role::SpinButton)
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn rows_sort_by_order_then_keep_their_declaration_order() {
        use metallic_core::ValueType;
        use metallic_core::classes::{Editor, PropDesc};
        static PROPS: [PropDesc; 3] = [
            PropDesc::new("A", ValueType::Bool, || Value::Bool(false))
                .editor(Editor::new().order(2)),
            PropDesc::new("B", ValueType::Bool, || Value::Bool(false))
                .editor(Editor::new().order(1)),
            PropDesc::new("C", ValueType::Bool, || Value::Bool(false))
                .editor(Editor::new().order(2)),
        ];
        let props: Vec<&'static PropDef> = PROPS.iter().collect();
        let rows: Vec<&str> = sections(&props)[0].1.iter().map(|p| p.name).collect();
        assert_eq!(rows, ["B", "A", "C"]);
    }

    #[test]
    fn a_row_shows_only_while_its_condition_holds_on_every_selected_instance() {
        use metallic_core::classes::{Editor, PropDesc, VisibleWhen};
        use metallic_core::{ValueType, World};
        let conditional = PropDesc::new("Size", ValueType::Vector3, || Value::Nil)
            .editor(Editor::new().visible_when(VisibleWhen::OneOf("Shape", &["Ball", "Cylinder"])));
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let block = world.create("Part", Some(ws)).unwrap();
        let ball = world.create("Part", Some(ws)).unwrap();
        world
            .set(ball, "Shape", Value::String("Ball".into()))
            .unwrap();
        assert!(row_visible(&world, &[ball], &conditional));
        assert!(!row_visible(&world, &[block], &conditional));
        assert!(!row_visible(&world, &[ball, block], &conditional));
        let plain = PropDesc::new("Size", ValueType::Vector3, || Value::Nil);
        assert!(row_visible(&world, &[block], &plain));
    }

    #[test]
    fn number_properties_get_their_kind() {
        let kind = |class: &str, prop: &str| number_kind(find_property(class, prop).unwrap());
        assert_eq!(kind("Part", "Transparency"), NumberKind::Fraction);
        assert_eq!(
            kind("Frame", "BackgroundTransparency"),
            NumberKind::Fraction
        );
        assert_eq!(kind("Frame", "ZIndex"), NumberKind::Integer);
        assert_eq!(kind("TextLabel", "TextSize"), NumberKind::Integer);
        assert_eq!(kind("Lighting", "GeographicLatitude"), NumberKind::Degrees);
        // A 0-1 range in the registry makes a fraction; other ranges keep the plain rule.
        assert_eq!(kind("Lighting", "ShadowSoftness"), NumberKind::Fraction);
        assert_eq!(kind("Lighting", "Brightness"), NumberKind::Studs);
    }
}

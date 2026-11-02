//! The building tools, the colour popover and the Input Actions panel wired into the editor
//! (#319): each tool family commits one undo step and undo restores the world exactly; the
//! Input Actions panel records into the editor's history and follows rebuilt ids.
use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_render::glam::DVec3;
use metallic_studio::actions::ToolbarGroup;
use metallic_studio::commands::Command;
use metallic_studio::editor::{EditorState, Tool};
use metallic_studio::editor_tools::arrange::{ArrangeSettings, ArrayMode, run_align, run_array};
use metallic_studio::editor_tools::{Modal, Popover};
use metallic_studio::editor_ui::EditorUi;
use metallic_studio::output::OutputPanel;
use metallic_studio::toolbar::MORE_LABEL;
use metallic_studio::tools::arrange::{Axis, Edge};
use metallic_studio::tools::select::SelectMode;

struct Scene {
    base: InstanceId,
    a: InstanceId,
    b: InstanceId,
    c: InstanceId,
}

fn part(world: &mut World, parent: InstanceId, name: &str, pos: Vec3, size: Vec3) -> InstanceId {
    let p = world.create("Part", Some(parent)).unwrap();
    world.set(p, "Name", Value::String(name.into())).unwrap();
    world.set(p, "Size", Value::Vector3(size)).unwrap();
    world.set(p, "Position", Value::Vector3(pos)).unwrap();
    world.set(p, "Anchored", Value::Bool(true)).unwrap();
    p
}

fn editor() -> (EditorUi, Scene) {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    let base = part(
        &mut world,
        ws,
        "Baseplate",
        Vec3::new(0.0, -0.5, 0.0),
        Vec3::new(64.0, 1.0, 64.0),
    );
    let a = part(
        &mut world,
        ws,
        "A",
        Vec3::new(-8.0, 2.0, 0.0),
        Vec3::new(4.0, 4.0, 4.0),
    );
    let b = part(
        &mut world,
        ws,
        "B",
        Vec3::new(6.0, 1.0, 3.0),
        Vec3::new(2.0, 2.0, 2.0),
    );
    world
        .set(b, "Orientation", Value::Vector3(Vec3::new(0.0, 30.0, 0.0)))
        .unwrap();
    let c = part(
        &mut world,
        ws,
        "C",
        Vec3::new(12.0, 1.0, -6.0),
        Vec3::new(2.0, 2.0, 2.0),
    );
    let output = OutputPanel::new();
    let mut state = EditorState::new(
        world,
        std::path::Path::new("."),
        std::path::Path::new("unused.mscene"),
        "Test",
        output.log.sink(),
    );
    state.can_save = false;
    (EditorUi::new(state, output), Scene { base, a, b, c })
}

/// The panels use the bundled font families, which the theme installs for the next pass: the
/// first pass only installs them.
fn harness(ui: EditorUi) -> Harness<'static, EditorUi> {
    harness_sized(ui, egui::vec2(1600.0, 900.0))
}

fn harness_sized(ui: EditorUi, size: egui::Vec2) -> Harness<'static, EditorUi> {
    let mut themed = false;
    let mut h = Harness::builder().with_size(size).build_ui_state(
        move |ui, editor: &mut EditorUi| {
            if !themed {
                metallic_studio::theme::apply(ui.ctx(), metallic_studio::theme::ThemeId::Graphite);
                themed = true;
                ui.ctx().request_repaint();
                return;
            }
            editor.ui(ui, None);
        },
        ui,
    );
    h.run_steps(3);
    h
}

fn button(h: &mut Harness<'static, EditorUi>, pos: Pos2, pressed: bool, modifiers: Modifiers) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers,
    });
    h.run_steps(2);
}

fn click(h: &mut Harness<'static, EditorUi>, pos: Pos2) {
    h.hover_at(pos);
    h.run_steps(2);
    button(h, pos, true, Modifiers::NONE);
    button(h, pos, false, Modifiers::NONE);
}

fn screen(h: &Harness<'static, EditorUi>, p: DVec3) -> Pos2 {
    h.state().viewport.screen_pos(p).expect("in view")
}

/// Every live instance under Workspace with the properties tools change.
fn snapshot(state: &EditorState) -> Vec<(InstanceId, String, Vec<Value>)> {
    let mut out = Vec::new();
    let mut stack = vec![state.world.root("Workspace").unwrap()];
    while let Some(id) = stack.pop() {
        if !state.is_live(id) {
            continue;
        }
        let props = ["Name", "Position", "Orientation", "Size", "Color"]
            .iter()
            .filter_map(|p| state.world.get(id, p).ok())
            .collect();
        out.push((
            id,
            state.world.class_name(id).unwrap_or("").to_string(),
            props,
        ));
        stack.extend(state.world.children(id).iter().copied());
    }
    out.sort_by_key(|e| e.0.to_bits());
    out
}

fn live_input_actions(state: &EditorState) -> Vec<InstanceId> {
    let mut out = Vec::new();
    let mut stack = vec![state.world.root("ReplicatedStorage").unwrap()];
    while let Some(id) = stack.pop() {
        if state.world.class_name(id) == Some("InputAction") && state.is_live(id) {
            out.push(id);
        }
        stack.extend(state.world.children(id).iter().copied());
    }
    out
}

#[test]
fn input_actions_panel_records_into_editor_undo_and_follows_new_ids() {
    let (ui, _) = editor();
    let mut h = harness(ui);
    h.state_mut().run_action("view.input_actions");
    h.run_steps(4);
    assert!(h.state().input_actions.is_open());

    // Create an action in the panel.
    h.get_by_label("Add").click();
    h.run_steps(3);
    h.get_by_label("Empty action").click();
    h.run_steps(4);
    let made = live_input_actions(&h.state().state);
    assert_eq!(made.len(), 1, "the panel added one action");
    let a0 = made[0];
    {
        let ui = h.state();
        assert_eq!(ui.input_actions.panel().selected_instance(), Some(a0));
        assert!(
            !ui.input_actions.panel().owns_history(),
            "the editor owns undo"
        );
        assert_eq!(ui.state.history.len(), 1);
        assert!(matches!(
            ui.state.history.peek_undo(),
            Some(Command::Step(_))
        ));
    }

    // Undo in the editor: the action is gone.
    h.state_mut().run_action("edit.undo");
    h.run_steps(3);
    assert!(live_input_actions(&h.state().state).is_empty());

    // Redo rebuilds it with a new id.
    h.state_mut().run_action("edit.redo");
    h.run_steps(3);
    let a1 = live_input_actions(&h.state().state)[0];
    assert_ne!(a1, a0, "rebuilt with a new id");
    {
        let ui = h.state_mut();
        let Value::String(name) = ui.state.world.get(a1, "Name").unwrap() else {
            panic!()
        };
        ui.input_actions
            .panel_mut()
            .select_action(&ui.state.world, &name);
    }
    h.run_steps(2);

    // Delete in the panel, then undo in the editor.
    h.get_by_label("Delete (Delete)").click();
    h.run_steps(4);
    assert!(live_input_actions(&h.state().state).is_empty());
    assert_eq!(h.state().state.history.len(), 2);
    h.state_mut().run_action("edit.undo");
    h.run_steps(3);
    let a2 = live_input_actions(&h.state().state)[0];
    assert_ne!(a2, a1);

    // Select it and hide the panel; redo the delete and undo it again from the editor. The
    // panel's selection (and its other ids) moves to the rebuilt action's new id.
    {
        let ui = h.state_mut();
        let name = match ui.state.world.get(a2, "Name").unwrap() {
            Value::String(s) => s,
            _ => panic!(),
        };
        ui.input_actions
            .panel_mut()
            .select_action(&ui.state.world, &name);
        assert_eq!(ui.input_actions.panel().selected_instance(), Some(a2));
        ui.input_actions.set_open(false);
    }
    h.run_steps(2);
    h.state_mut().run_action("edit.redo");
    h.run_steps(2);
    assert!(live_input_actions(&h.state().state).is_empty());
    h.state_mut().run_action("edit.undo");
    h.run_steps(2);
    let a3 = live_input_actions(&h.state().state)[0];
    assert_ne!(a3, a2);
    assert_eq!(
        h.state().input_actions.panel().selected_instance(),
        Some(a3),
        "the selection followed the remap"
    );
    h.state_mut().run_action("view.input_actions");
    h.run_steps(3);
    assert_eq!(
        h.state().input_actions.panel().selected_instance(),
        Some(a3)
    );
    // Undo everything: no action is left, and the history agrees.
    h.state_mut().run_action("edit.undo");
    h.run_steps(2);
    assert!(live_input_actions(&h.state().state).is_empty());
    assert!(h.state().state.history.is_empty());
}

#[test]
fn colour_popover_from_properties_previews_live_and_commits_one_step() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Select;
    h.state_mut().state.selection.set([sc.b, sc.c]);
    h.run_steps(3);
    let before = snapshot(&h.state().state);
    let old = h.state().state.world.get(sc.b, "Color").unwrap();

    h.get_by_label("Color swatch").click();
    h.run_steps(6);
    assert!(h.state().colour.is_open(), "the swatch opens the popover");

    // Drag across the saturation/value square: a live preview, no undo step yet.
    let centre = h.state().colour.popover().wheel_centre();
    h.hover_at(centre);
    h.run_steps(2);
    button(&mut h, centre, true, Modifiers::NONE);
    let to = centre + egui::vec2(40.0, -40.0);
    h.hover_at(to);
    h.run_steps(3);
    {
        let s = &h.state().state;
        assert_ne!(s.world.get(sc.b, "Color").unwrap(), old, "previewing");
        assert_eq!(
            s.world.get(sc.b, "Color").unwrap(),
            s.world.get(sc.c, "Color").unwrap(),
            "every target previews"
        );
        assert!(s.history.is_empty(), "no step while dragging");
    }
    button(&mut h, to, false, Modifiers::NONE);
    h.run_steps(2);
    {
        let s = &h.state().state;
        assert_eq!(s.history.len(), 1, "one drag, one undo step");
        let name = s.history.undo_name(&s.world).unwrap();
        assert!(name.starts_with("Colour: #"), "{name}");
        assert!(name.ends_with("(2 parts)"), "{name}");
    }
    h.state_mut().run_action("edit.undo");
    h.run_steps(2);
    assert_eq!(snapshot(&h.state().state), before, "undo restores exactly");
}

#[test]
fn arrange_ops_are_one_undo_step_each_and_undo_restores_exactly() {
    let (mut ui, sc) = editor();
    ui.state.selection.set([sc.a, sc.b, sc.c]);
    let before = snapshot(&ui.state);
    let settings = ArrangeSettings::default();

    // Align.
    run_align(&mut ui.state, &settings, Axis::Y, Edge::Max);
    assert_eq!(ui.state.history.len(), 1);
    assert_ne!(snapshot(&ui.state), before);
    ui.run_action("edit.undo");
    assert_eq!(snapshot(&ui.state), before);

    // Distribute through its action and popover settings.
    metallic_studio::editor_tools::arrange::run_distribute(&mut ui.state, &settings, Axis::X);
    assert_eq!(ui.state.history.len(), 1);
    ui.run_action("edit.undo");
    assert_eq!(snapshot(&ui.state), before);

    // Mirror in place and mirror copy (Ctrl+M, Ctrl+Shift+M through the registry).
    ui.run_action("build.mirror");
    assert_eq!(ui.state.history.len(), 1);
    ui.run_action("edit.undo");
    assert_eq!(snapshot(&ui.state), before);
    ui.run_action("build.mirror_copy");
    assert_eq!(ui.state.history.len(), 1);
    let copies = ui.state.selection.ids().to_vec();
    assert_eq!(copies.len(), 3, "the copies are selected");
    ui.run_action("edit.undo");
    assert_eq!(snapshot(&ui.state), before);
    ui.run_action("edit.redo");
    assert!(
        copies.iter().all(|&c| ui.state.is_live(c)),
        "redo brings the same copies back"
    );
    ui.run_action("edit.undo");
    ui.state.selection.set([sc.a, sc.b, sc.c]);

    // Arrays: linear (grouped), radial, grid and path, each one step.
    let mut s = ArrangeSettings::default();
    for mode in [
        ArrayMode::Linear,
        ArrayMode::Radial,
        ArrayMode::Grid,
        ArrayMode::Path,
    ] {
        s.mode = mode;
        s.path.points = vec![
            DVec3::new(0.0, 0.0, 0.0),
            DVec3::new(10.0, 0.0, 5.0),
            DVec3::new(20.0, 0.0, 0.0),
        ];
        let steps = ui.state.history.len();
        assert!(run_array(&mut ui.state, &mut s), "{mode:?}");
        assert_eq!(ui.state.history.len(), steps + 1, "{mode:?}: one step");
        ui.run_action("edit.undo");
        assert_eq!(
            snapshot(&ui.state),
            before,
            "{mode:?}: undo restores exactly"
        );
    }
    // The linear array grouped its copies under a Model "Array".
    s.mode = ArrayMode::Linear;
    s.linear.count = 2;
    assert!(run_array(&mut ui.state, &mut s));
    let ws = ui.state.world.root("Workspace").unwrap();
    let array = ui
        .state
        .world
        .find_first_child(ws, "Array")
        .expect("grouped");
    assert_eq!(ui.state.world.children(array).len(), 6);
    ui.run_action("edit.undo");

    // The copy cap is an error in Output, not a panic or a step.
    s.linear.count = 5000;
    assert!(!run_array(&mut ui.state, &mut s));
    assert!(ui.state.history.is_empty());
    assert_eq!(snapshot(&ui.state), before);

    // A opens the Align popover only when it applies.
    ui.state.selection.set([sc.a]);
    assert!(!ui.actions.get("build.align").unwrap().is_enabled(&ui.state));
    ui.state.selection.set([sc.a, sc.b]);
    ui.run_action("build.align");
    assert_eq!(ui.tools.state().popover, Some(Popover::Align));
    let _ = sc.base;
}

#[test]
fn a_brush_stroke_is_one_step_and_erase_removes_only_its_own_instances() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.c]);
    h.state_mut().run_action("tool.brush");
    h.run_steps(2);
    assert_eq!(h.state().tools.state().modal, Some(Modal::Brush));
    assert!(
        h.state().tools.state().brush.set.is_some(),
        "B uses the selection"
    );
    let before = snapshot(&h.state().state);

    // Paint: press over the baseplate, hold a few frames, move, release.
    let at = screen(&h, DVec3::new(-16.0, 0.0, 16.0));
    h.hover_at(at);
    h.run_steps(2);
    button(&mut h, at, true, Modifiers::NONE);
    h.hover_at(at + egui::vec2(30.0, 0.0));
    h.run_steps(4);
    button(&mut h, at + egui::vec2(30.0, 0.0), false, Modifiers::NONE);
    let ws = h.state().state.world.root("Workspace").unwrap();
    let painted = {
        let s = &h.state().state;
        assert_eq!(s.history.len(), 1, "one stroke, one step");
        let name = s.history.undo_name(&s.world).unwrap();
        assert!(name.starts_with("Brush C ("), "{name}");
        let brush = s.world.find_first_child(ws, "Brush").expect("brush folder");
        let set = s.world.find_first_child(brush, "C").expect("set folder");
        s.world.children(set).to_vec()
    };
    assert!(!painted.is_empty(), "the stroke placed copies");
    let after_paint = snapshot(&h.state().state);
    h.state_mut().run_action("edit.undo");
    h.run_steps(2);
    assert_eq!(snapshot(&h.state().state), before, "undo restores exactly");
    h.state_mut().run_action("edit.redo");
    h.run_steps(2);
    assert_eq!(
        snapshot(&h.state().state),
        after_paint,
        "redo brings the same copies"
    );

    // Shift-drag erases this brush's copies only, as one step.
    h.event(Event::ModifiersChanged(Modifiers::SHIFT));
    button(&mut h, at, true, Modifiers::SHIFT);
    h.run_steps(2);
    button(&mut h, at, false, Modifiers::SHIFT);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
    {
        let s = &h.state().state;
        assert_eq!(s.history.len(), 2);
        let name = s.history.undo_name(&s.world).unwrap();
        assert!(name.starts_with("Erase C ("), "{name}");
        assert!(painted.iter().any(|&p| !s.is_live(p)), "erased some");
        assert!(
            s.is_live(sc.base) && s.is_live(sc.c),
            "never what it did not place"
        );
    }
    h.state_mut().run_action("edit.undo");
    h.run_steps(2);
    assert_eq!(snapshot(&h.state().state), after_paint);
}

#[test]
fn selection_modes_select_similar_and_measure_from_the_viewport() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    let centre = h.state().viewport.rect().center();
    h.hover_at(centre);
    h.run_steps(2);
    // Q selects the Select tool, Q again cycles to Lasso.
    h.key_press(egui::Key::Q);
    h.run_steps(2);
    h.key_press(egui::Key::Q);
    h.run_steps(2);
    assert_eq!(h.state().state.tool, Tool::Select);
    assert_eq!(h.state().tools.state().select_mode, SelectMode::Lasso);

    // A lasso around A's centre selects A only.
    let a = screen(&h, DVec3::new(-8.0, 2.0, 0.0));
    let ring = [(-25.0, -25.0), (25.0, -25.0), (25.0, 25.0), (-25.0, 25.0)];
    let first = a + egui::vec2(ring[0].0, ring[0].1);
    h.hover_at(first);
    h.run_steps(2);
    button(&mut h, first, true, Modifiers::NONE);
    for (x, y) in ring.iter().skip(1) {
        h.hover_at(a + egui::vec2(*x, *y));
        h.run_steps(1);
    }
    button(&mut h, first, false, Modifiers::NONE);
    assert_eq!(h.state().state.selection.ids(), &[sc.a]);
    assert!(
        h.state().state.history.is_empty(),
        "selecting is not an edit"
    );

    // Select similar (Shift+G, then S): B and C share a size.
    h.state_mut().state.selection.set([sc.b]);
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::G);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().popover, Some(Popover::Similar));
    h.key_press(egui::Key::S);
    h.run_steps(2);
    let mut got = h.state().state.selection.ids().to_vec();
    got.sort_by_key(|i| i.to_bits());
    let mut want = vec![sc.b, sc.c];
    want.sort_by_key(|i| i.to_bits());
    assert_eq!(got, want);

    // Measure (Shift+M): two clicks near A's top corners snap to them.
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::M);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().modal, Some(Modal::Measure));
    let p = DVec3::new(-10.0, 4.0, -2.0);
    let q = DVec3::new(-6.0, 4.0, 2.0);
    let top = screen(&h, DVec3::new(-8.0, 4.0, 0.0));
    for corner in [p, q] {
        let s = screen(&h, corner);
        // A few points in from the corner, so the ray hits the part.
        click(&mut h, s + (top - s).normalized() * 4.0);
    }
    let m = *h
        .state()
        .tools
        .state()
        .measure
        .measurements()
        .last()
        .expect("measured");
    assert!(
        (m.distance() - p.distance(q)).abs() < 1e-4,
        "{}",
        m.distance()
    );
    assert!(h.state().state.history.is_empty());
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().measure.measurements().len(), 0);
}

#[test]
fn a_narrow_toolbar_collapses_whole_groups_into_the_more_menu() {
    for width in [520.0, 1280.0, 1600.0] {
        let (ui, sc) = editor();
        let mut h = harness_sized(ui, egui::vec2(width, 800.0));
        h.state_mut().state.selection.set([sc.a, sc.b]);
        h.run_steps(4);
        let layout = h.state().toolbar_layout().clone();
        let shown: Vec<ToolbarGroup> = layout.shown.iter().map(|(g, _)| *g).collect();
        // The top bar holds Edit, Play, View and Build only: Tools, Settings and Snap are in the
        // viewport header (#384), never twice.
        for g in [
            ToolbarGroup::Tools,
            ToolbarGroup::Settings,
            ToolbarGroup::Snap,
        ] {
            assert!(
                !shown.contains(&g) && !layout.overflow.contains(&g),
                "{width}: {g:?}"
            );
        }
        // Play never collapses.
        assert!(shown.contains(&ToolbarGroup::Play), "{width}: {shown:?}");
        // Nothing drawn is cut by the window edge: every group and the chevron end inside.
        let right = layout.available.max.x + 0.5;
        for (g, r) in &layout.shown {
            assert!(
                r.max.x <= right,
                "{width}: {g:?} ends at {} > {right}",
                r.max.x
            );
        }
        if width >= 1280.0 {
            assert!(layout.overflow.is_empty(), "{width}: {:?}", layout.overflow);
            assert!(layout.chevron.is_none());
            continue;
        }
        assert!(
            layout.overflow.contains(&ToolbarGroup::Build),
            "{width}: Build collapses: {:?}",
            layout.overflow
        );
        let chevron = layout.chevron.expect("a More button");
        assert!(
            chevron.max.x <= right,
            "{width}: chevron at {}",
            chevron.max.x
        );

        // The menu lists the collapsed actions with label and shortcut, and runs them.
        h.get_by_label(MORE_LABEL).click();
        h.run_steps(3);
        h.get_by_label("Align A").click();
        h.run_steps(3);
        assert_eq!(
            h.state().tools.state().popover,
            Some(Popover::Align),
            "{width}: Align ran from the menu"
        );
    }
}

// --- Review fixes: Esc routing, stroke cancel, one gesture flag, guarded commits, stuck drag.

#[test]
fn esc_reaches_the_colour_popover_then_the_building_popover_then_the_tool() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.b, sc.c]);
    h.state_mut().run_action("tool.measure");
    h.state_mut().run_action("build.align");
    h.run_steps(2);
    let centre = h.state().viewport.rect().center();
    h.hover_at(centre);
    h.run_steps(2);

    // The colour popover is topmost: Esc closes it and nothing else.
    {
        let ui = h.state_mut();
        assert!(ui.colour.open(&ui.state, "Color", Pos2::new(900.0, 200.0)));
    }
    h.run_steps(2);
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().colour.is_open(), "Esc closed the colour popover");
    assert_eq!(h.state().tools.state().popover, Some(Popover::Align));
    assert_eq!(h.state().tools.state().modal, Some(Modal::Measure));
    assert_eq!(h.state().state.selection.len(), 2, "the selection stays");

    // Then the Align popover, with the pointer over the viewport.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().popover, None, "Esc closed Align");
    assert_eq!(h.state().tools.state().modal, Some(Modal::Measure));
    assert_eq!(h.state().state.selection.len(), 2);

    // Then the tool, and only then the selection.
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().modal, None, "Esc left Measure");
    assert_eq!(h.state().state.selection.len(), 2);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(
        h.state().state.selection.is_empty(),
        "Esc cleared the selection"
    );
}

#[test]
fn popover_keys_follow_the_keymap_guards() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    let centre = h.state().viewport.rect().center();
    h.state_mut().state.selection.set([sc.b]);
    h.state_mut().run_action("select.similar");
    h.hover_at(centre);
    h.run_steps(2);
    // Right mouse held (flying): S is a fly key, not Select similar.
    h.event(Event::PointerButton {
        pos: centre,
        button: PointerButton::Secondary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(1);
    h.key_press(egui::Key::S);
    h.run_steps(1);
    h.event(Event::PointerButton {
        pos: centre,
        button: PointerButton::Secondary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
    assert_eq!(
        h.state().state.selection.len(),
        1,
        "no Select similar while flying"
    );
    assert_eq!(h.state().tools.state().popover, Some(Popover::Similar));
    h.key_press(egui::Key::S);
    h.run_steps(2);
    assert_eq!(h.state().state.selection.len(), 2, "S picked Size");

    // Array: Enter makes it (one step) through the keymap.
    h.state_mut().state.selection.set([sc.a]);
    h.state_mut().run_action("build.array");
    h.run_steps(2);
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(h.state().state.history.len(), 1);
    assert_eq!(h.state().tools.state().popover, None);
}

/// Presses over the baseplate with the brush on and drags a little; returns where.
fn start_stroke(h: &mut Harness<'static, EditorUi>, sc: &Scene) -> Pos2 {
    h.state_mut().state.selection.set([sc.c]);
    h.state_mut().run_action("tool.brush");
    h.run_steps(2);
    let at = screen(h, DVec3::new(-16.0, 0.0, 16.0));
    h.hover_at(at);
    h.run_steps(2);
    button(h, at, true, Modifiers::NONE);
    h.hover_at(at + egui::vec2(20.0, 0.0));
    h.run_steps(3);
    assert!(h.state().tools.gesture(), "a stroke is held");
    assert!(h.state().state.gesture_active());
    at + egui::vec2(20.0, 0.0)
}

#[test]
fn a_brush_stroke_cancelled_by_esc_tool_switch_focus_loss_or_play_never_commits() {
    for how in ["esc", "tool", "focus", "play"] {
        let (ui, sc) = editor();
        let mut h = harness(ui);
        let before = snapshot(&h.state().state);
        let at = start_stroke(&mut h, &sc);
        match how {
            "esc" => h.key_press(egui::Key::Escape),
            "tool" => h.state_mut().run_action("tool.move"),
            "play" => h.state_mut().run_action("play.start"),
            _ => h.input_mut().focused = false,
        }
        h.run_steps(2);
        h.input_mut().focused = true;
        button(&mut h, at, false, Modifiers::NONE);
        if how == "play" {
            assert!(h.state().state.play.is_playing(), "Play started");
            h.state_mut().run_action("play.stop");
            h.run_steps(3);
            assert!(!h.state().state.play.is_playing());
        }
        assert!(!h.state().tools.gesture(), "{how}: the stroke ended");
        // Turning the brush on again does not commit the old stroke either.
        if h.state().tools.state().modal != Some(Modal::Brush) {
            h.state_mut().run_action("tool.brush");
        }
        h.run_steps(3);
        let s = &h.state().state;
        assert!(s.history.is_empty(), "{how}: no step");
        assert_eq!(snapshot(s), before, "{how}: the world is unchanged");
    }
}

#[test]
fn save_and_tool_edits_wait_for_a_colour_drag() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.a, sc.b]);
    h.run_steps(2);
    h.get_by_label("Color swatch").click();
    h.run_steps(4);
    let centre = h.state().colour.popover().wheel_centre();
    h.hover_at(centre);
    h.run_steps(2);
    button(&mut h, centre, true, Modifiers::NONE);
    let to = centre + egui::vec2(30.0, -30.0);
    h.hover_at(to);
    h.run_steps(3);
    let mid = snapshot(&h.state().state);
    {
        let ui = h.state_mut();
        assert!(ui.state.gesture_active(), "the colour preview is a gesture");
        assert!(ui.state.save().is_err(), "preview colours are never saved");
        // Tool commits take the same refusal as EditorState::apply.
        assert!(
            !ui.actions.run("build.mirror", &mut ui.state),
            "mirror waits"
        );
        run_align(
            &mut ui.state,
            &ArrangeSettings::default(),
            Axis::Y,
            Edge::Max,
        );
        assert!(ui.state.history.is_empty(), "no step during the drag");
    }
    assert_eq!(snapshot(&h.state().state), mid, "nothing moved");
    // Ctrl+S during the drag does nothing either.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::S);
    h.run_steps(1);
    button(&mut h, to, false, Modifiers::NONE);
    h.run_steps(2);
    let s = &h.state().state;
    assert!(!s.gesture_active());
    assert_eq!(s.history.len(), 1, "only the colour step");
}

#[test]
fn a_viewport_drag_still_ends_when_a_tool_takes_the_pointer_mid_drag() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Move;
    h.state_mut().state.selection.set([sc.a]);
    h.run_steps(3);
    let pivot = screen(&h, DVec3::new(-8.0, 2.0, 0.0));
    let grab = pivot - egui::vec2(0.0, 54.0);
    h.hover_at(grab);
    h.run_steps(2);
    button(&mut h, grab, true, Modifiers::NONE);
    h.hover_at(grab - egui::vec2(0.0, 60.0));
    h.run_steps(2);
    assert!(h.state().viewport.is_dragging(), "the gizmo drag started");
    // A tool turns pointer capture on mid-drag (any path: a hover over the array handle, ...).
    h.state().tools.state().modal = Some(Modal::Measure);
    h.hover_at(grab - egui::vec2(0.0, 90.0));
    h.run_steps(2);
    button(&mut h, grab - egui::vec2(0.0, 90.0), false, Modifiers::NONE);
    h.run_steps(2);
    {
        let ui = h.state();
        assert!(!ui.viewport.is_dragging(), "the release ended the drag");
        assert!(!ui.state.gesture_active(), "keys are free again");
        assert_eq!(ui.state.history.len(), 1, "the drag is one step");
    }
    // A viewport key works again.
    let centre = h.state().viewport.rect().center();
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().state.tool, Tool::Rotate);
}

// --- #384 merge: contextual toolbar, hint card and key claiming.

/// A point near the viewport's top-right corner, clear of the popovers (top left), the
/// contextual toolbar (by the selection) and the hint card (bottom left).
fn viewport_top(h: &Harness<'static, EditorUi>) -> Pos2 {
    let vp = h.state().viewport.rect();
    Pos2::new(vp.right() - 100.0, vp.top() + 80.0)
}

#[test]
fn the_context_bar_lists_the_building_actions_and_hides_while_a_tool_has_the_pointer() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Move;
    h.state_mut().state.selection.set([sc.a, sc.b]);
    let revision = h.state().state.selection.revision();
    h.state_mut().context_bar.arm(revision);
    h.run_steps(4);
    assert!(h.state().context_bar.last_rect.is_some(), "the bar shows");
    let shown: Vec<_> = h.state().context_bar.shown_actions().collect();
    for id in ["colour.open", "build.align", "build.mirror", "build.array"] {
        assert!(shown.contains(&id), "{id} in {shown:?}");
    }
    // The brush has the pointer: the bar steps aside, and comes back when it is off.
    h.state_mut().run_action("tool.brush");
    h.run_steps(3);
    assert!(
        h.state().context_bar.last_rect.is_none(),
        "hidden for the brush"
    );
    h.state_mut().run_action("tool.brush");
    h.state_mut().run_action("select.mode_lasso");
    h.run_steps(3);
    assert!(
        h.state().context_bar.last_rect.is_none(),
        "hidden for the lasso"
    );
    h.state_mut().run_action("select.mode_box");
    h.state_mut().state.tool = Tool::Move;
    h.run_steps(3);
    assert!(h.state().context_bar.last_rect.is_some(), "back");
}

#[test]
fn esc_closes_a_tool_popover_before_the_context_bar_lets_go() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Move;
    h.state_mut().state.selection.set([sc.a, sc.b]);
    let revision = h.state().state.selection.revision();
    h.state_mut().context_bar.arm(revision);
    h.run_steps(4);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Period);
    h.run_steps(3);
    assert!(h.state().context_bar.has_focus(), "Ctrl+. focuses the bar");
    h.state_mut().run_action("build.align");
    h.run_steps(3);
    assert_eq!(h.state().tools.state().popover, Some(Popover::Align));

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert_eq!(
        h.state().tools.state().popover,
        None,
        "Esc closed Align first"
    );
    assert!(h.state().context_bar.has_focus(), "the bar keeps focus");
    assert_eq!(h.state().state.selection.len(), 2);

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().context_bar.has_focus(), "then the bar lets go");
    assert_eq!(h.state().state.selection.len(), 2, "the selection stays");
}

#[test]
fn an_open_tool_claims_only_the_keys_it_handles() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.a, sc.b]);
    h.state_mut().run_action("build.align");
    assert_eq!(h.state().tools.state().popover, Some(Popover::Align));
    h.run_steps(3);
    let at = viewport_top(&h);
    h.hover_at(at);
    h.run_steps(3);
    // Align is open: T, P and C keep their viewport meaning (they are Select similar letters).
    h.key_press(egui::Key::T);
    h.run_steps(2);
    assert_eq!(h.state().state.tool, Tool::Transform, "T picked Transform");
    assert_eq!(
        h.state().tools.state().popover,
        Some(Popover::Align),
        "after T"
    );
    h.key_press(egui::Key::C);
    h.run_steps(3);
    assert!(h.state().colour.is_open(), "C opened the colour popover");
    assert_eq!(
        h.state().tools.state().popover,
        Some(Popover::Align),
        "after C"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().colour.is_open());
    assert_eq!(h.state().tools.state().popover, Some(Popover::Align));
    // Enter is Array's; with Align open it makes nothing.
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(h.state().state.history.is_empty());

    // Measure is on: [ and ] are the brush's, so they still step the move increment.
    h.state_mut().run_action("popover.close");
    h.state_mut().run_action("tool.measure");
    h.hover_at(at);
    h.run_steps(2);
    let before = h.state().state.snapping.move_step;
    h.key_press(egui::Key::CloseBracket);
    h.run_steps(2);
    assert!(
        h.state().state.snapping.move_step > before,
        "] stepped the increment up"
    );
}

#[test]
fn the_hint_card_waits_for_a_gesture() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    let got_it = |h: &Harness<'static, EditorUi>| h.query_by_label("Got it").is_some();
    h.state_mut().state.selection.set([sc.c]);
    h.run_steps(4);
    assert!(
        h.state().hints.current().is_some(),
        "the first selection fires a hint"
    );
    assert!(got_it(&h), "the card shows");
    let at = start_stroke(&mut h, &sc);
    assert!(h.state().state.gesture_active());
    assert!(!got_it(&h), "no card during the stroke");
    button(&mut h, at, false, Modifiers::NONE);
    h.run_steps(3);
    assert!(!h.state().state.gesture_active());
    assert!(got_it(&h), "the card is back");
}

// --- #218 and #220: box select, Same Tags, smart duplicate, the wheel over the array handle.

/// The ids unordered, for comparing.
fn sorted(mut ids: Vec<InstanceId>) -> Vec<InstanceId> {
    ids.sort_by_key(|i| i.to_bits());
    ids
}

fn position(state: &EditorState, id: InstanceId) -> DVec3 {
    match state.world.get(id, "Position").unwrap() {
        Value::Vector3(v) => DVec3::new(v.x, v.y, v.z),
        other => panic!("not a position: {other:?}"),
    }
}

fn assert_close(a: DVec3, b: DVec3) {
    assert!(a.distance(b) < 1e-9, "{a} is not {b}");
}

/// Moves `id` by `by` as one step, the way Properties does.
fn nudge(state: &mut EditorState, id: InstanceId, by: DVec3) {
    let to = position(state, id) + by;
    state.set_property(
        &[id],
        "Position",
        Value::Vector3(Vec3::new(to.x, to.y, to.z)),
    );
}

/// The screen box of a world box, grown by `margin` points.
fn screen_box(
    h: &Harness<'static, EditorUi>,
    centre: DVec3,
    size: DVec3,
    margin: f32,
) -> egui::Rect {
    let mut r = egui::Rect::NOTHING;
    for x in [-0.5, 0.5] {
        for y in [-0.5, 0.5] {
            for z in [-0.5, 0.5] {
                r.extend_with(screen(h, centre + size * DVec3::new(x, y, z)));
            }
        }
    }
    r.expand(margin)
}

/// A box drag from `from` to `to` with `at_press` held, and `at_release` once it travelled.
fn box_drag(
    h: &mut Harness<'static, EditorUi>,
    from: Pos2,
    to: Pos2,
    at_press: Modifiers,
    at_release: Modifiers,
) {
    h.event(Event::ModifiersChanged(at_press));
    h.hover_at(from);
    h.run_steps(2);
    button(h, from, true, at_press);
    h.hover_at(to);
    h.run_steps(2);
    assert!(h.state().viewport.is_busy(), "the box drag started");
    h.event(Event::ModifiersChanged(at_release));
    h.run_steps(1);
    button(h, to, false, at_release);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(2);
}

/// An editor for dragging over parts: no baseplate (a drag from a part would move it), the
/// Select tool, and the camera close enough that the parts are well apart on screen. The
/// viewport narrows when the first part is selected (Properties opens), so screen points must
/// be read right before they are used.
fn box_editor() -> (Harness<'static, EditorUi>, Scene) {
    let (mut ui, sc) = editor();
    ui.state.world.destroy(sc.base);
    ui.state.tool = Tool::Select;
    ui.viewport.camera.position = DVec3::new(0.0, 6.0, 24.0);
    ui.viewport.camera.look_at(DVec3::ZERO);
    (harness(ui), sc)
}

const AT_A: DVec3 = DVec3::new(-8.0, 2.0, 0.0);
const AT_B: DVec3 = DVec3::new(6.0, 1.0, 3.0);
const AT_C: DVec3 = DVec3::new(12.0, 1.0, -6.0);

fn around_a(h: &Harness<'static, EditorUi>) -> egui::Rect {
    screen_box(h, AT_A, DVec3::splat(4.0), 6.0)
}

fn around_b(h: &Harness<'static, EditorUi>) -> egui::Rect {
    // B is turned 30 degrees: its screen box needs room for the corners.
    screen_box(h, AT_B, DVec3::splat(3.0), 6.0)
}

fn around_c(h: &Harness<'static, EditorUi>) -> egui::Rect {
    screen_box(h, AT_C, DVec3::splat(2.0), 6.0)
}

/// A box that holds A's centre but only part of A.
fn part_of_a(h: &Harness<'static, EditorUi>) -> (Pos2, Pos2) {
    (around_a(h).min, screen(h, AT_A) + egui::vec2(3.0, 3.0))
}

#[test]
fn box_select_takes_parts_fully_inside_and_follows_the_modifiers() {
    let (mut h, sc) = box_editor();
    let none = Modifiers::NONE;
    let shift = Modifiers::SHIFT;
    let ctrl = Modifiers::COMMAND;

    // Fully inside: A only.
    let r = around_a(&h);
    box_drag(&mut h, r.min, r.max, none, none);
    assert_eq!(h.state().state.selection.ids(), &[sc.a]);
    assert!(
        h.state().state.history.is_empty(),
        "selecting is not an edit"
    );

    // Holding A's centre is not enough, and a plain box replaces.
    let (from, to) = part_of_a(&h);
    box_drag(&mut h, from, to, none, none);
    assert!(
        h.state().state.selection.is_empty(),
        "the centre is not enough"
    );

    // Alt, once the drag has started, takes what it touches.
    let (from, to) = part_of_a(&h);
    box_drag(&mut h, from, to, none, Modifiers::ALT);
    assert_eq!(h.state().state.selection.ids(), &[sc.a]);

    // Shift adds, and the new part is the active one.
    let r = around_b(&h);
    box_drag(&mut h, r.min, r.max, shift, shift);
    assert_eq!(h.state().state.selection.ids(), &[sc.a, sc.b]);

    // Ctrl removes.
    let r = around_a(&h);
    box_drag(&mut h, r.min, r.max, ctrl, ctrl);
    assert_eq!(h.state().state.selection.ids(), &[sc.b]);

    // Ctrl over a part that is not selected changes nothing.
    let r = around_c(&h);
    box_drag(&mut h, r.min, r.max, ctrl, ctrl);
    assert_eq!(h.state().state.selection.ids(), &[sc.b]);

    // The modifiers held at release decide, as for the lasso.
    let r = around_c(&h);
    box_drag(&mut h, r.min, r.max, none, shift);
    assert_eq!(h.state().state.selection.ids(), &[sc.b, sc.c]);

    // Alt with Shift: added by touch.
    h.state_mut().state.selection.set([sc.b]);
    h.run_steps(2);
    let (from, to) = part_of_a(&h);
    box_drag(&mut h, from, to, shift, shift | Modifiers::ALT);
    assert_eq!(h.state().state.selection.ids(), &[sc.b, sc.a]);

    // Alt with Ctrl: removed by touch.
    let (from, to) = part_of_a(&h);
    box_drag(&mut h, from, to, ctrl, ctrl | Modifiers::ALT);
    assert_eq!(h.state().state.selection.ids(), &[sc.b]);
    assert!(h.state().state.history.is_empty());
}

#[test]
fn clicks_follow_the_same_modifiers_as_the_box() {
    let (mut h, sc) = box_editor();
    let press = |h: &mut Harness<'static, EditorUi>, on: Option<DVec3>, m: Modifiers| {
        h.run_steps(2);
        let at = on.map_or_else(|| around_a(h).min, |p| screen(h, p));
        h.event(Event::ModifiersChanged(m));
        h.hover_at(at);
        h.run_steps(2);
        button(h, at, true, m);
        button(h, at, false, m);
        h.event(Event::ModifiersChanged(Modifiers::NONE));
        h.run_steps(1);
    };
    press(&mut h, Some(AT_A), Modifiers::NONE);
    assert_eq!(h.state().state.selection.ids(), &[sc.a]);
    press(&mut h, Some(AT_C), Modifiers::SHIFT);
    assert_eq!(h.state().state.selection.ids(), &[sc.a, sc.c], "Shift adds");
    press(&mut h, None, Modifiers::SHIFT);
    assert_eq!(
        h.state().state.selection.len(),
        2,
        "Shift on nothing keeps it"
    );
    press(&mut h, Some(AT_A), Modifiers::COMMAND);
    assert_eq!(
        h.state().state.selection.ids(),
        &[sc.c],
        "Ctrl-click toggles"
    );
    press(&mut h, Some(AT_A), Modifiers::COMMAND);
    assert_eq!(h.state().state.selection.ids(), &[sc.c, sc.a]);
    press(&mut h, None, Modifiers::NONE);
    assert!(h.state().state.selection.is_empty(), "a plain click clears");
}

#[test]
fn select_similar_by_tags_reads_the_engine_tags() {
    let (mut ui, sc) = editor();
    let w = &mut ui.state.world;
    w.add_tag(sc.a, "Wall").unwrap();
    w.add_tag(sc.a, "Stone").unwrap();
    // The same tags in the other order; B has only one of them.
    w.add_tag(sc.c, "Stone").unwrap();
    w.add_tag(sc.c, "Wall").unwrap();
    w.add_tag(sc.b, "Wall").unwrap();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.a]);
    h.run_steps(2);
    let centre = h.state().viewport.rect().center();
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::SHIFT, egui::Key::G);
    h.run_steps(2);
    assert_eq!(h.state().tools.state().popover, Some(Popover::Similar));
    h.key_press(egui::Key::T);
    h.run_steps(2);
    assert_eq!(
        sorted(h.state().state.selection.ids().to_vec()),
        sorted(vec![sc.a, sc.c])
    );
}

#[test]
fn smart_duplicate_repeats_the_offset_one_undo_step_each() {
    let (mut ui, sc) = editor();
    let start = position(&ui.state, sc.b);
    ui.state.selection.set([sc.b]);
    ui.run_action("edit.duplicate");
    let first = ui.state.selection.ids()[0];
    assert_eq!(ui.state.history.len(), 1);
    let height = ui.state.selection_bounds().unwrap().size().y;
    assert_close(
        position(&ui.state, first),
        start + DVec3::new(0.0, height, 0.0),
    );
    assert_eq!(ui.state.last_duplicate_offset(), None, "not moved yet");

    // Move the copy: the next one repeats source-to-copy, on the copy.
    let offset = DVec3::new(5.0, 0.0, -1.5);
    let to = start + offset;
    let to = Value::Vector3(Vec3::new(to.x, to.y, to.z));
    ui.state.set_property(&[first], "Position", to);
    assert_eq!(ui.state.last_duplicate_offset(), Some(offset));
    let keep = ui.state.world.get(first, "Orientation").unwrap();
    ui.run_action("edit.duplicate");
    let second = ui.state.selection.ids()[0];
    assert_ne!(second, first);
    assert_eq!(ui.state.selection.len(), 1, "the new copy is selected");
    assert_close(position(&ui.state, second), start + offset * 2.0);
    assert_eq!(ui.state.world.get(second, "Orientation").unwrap(), keep);
    assert_eq!(ui.state.history.len(), 3, "duplicate, move, duplicate");
    assert_eq!(
        ui.state.history.undo_name(&ui.state.world).as_deref(),
        Some("Duplicate 1 object")
    );

    // It keeps going without another move.
    ui.run_action("edit.duplicate");
    let third = ui.state.selection.ids()[0];
    assert_close(position(&ui.state, third), start + offset * 3.0);
    assert_eq!(ui.state.history.len(), 4);
    assert_eq!(ui.state.last_duplicate_offset(), Some(offset));

    // Moving the newest copy changes what repeats.
    nudge(&mut ui.state, third, DVec3::new(0.0, 1.0, 0.0));
    ui.run_action("edit.duplicate");
    let fourth = ui.state.selection.ids()[0];
    let further = offset + DVec3::new(0.0, 1.0, 0.0);
    assert_close(
        position(&ui.state, fourth),
        start + offset * 2.0 + further * 2.0,
    );
    let chain = position(&ui.state, fourth);

    // One undo per Ctrl+D: the last copy goes, and only it.
    ui.run_action("edit.undo");
    assert!(!ui.state.is_live(fourth));
    assert!(ui.state.is_live(third));
    assert_eq!(ui.state.history.len(), 5);
    assert!(ui.state.selection.is_empty());
    ui.run_action("edit.redo");
    assert!(ui.state.is_live(fourth), "redo brings the same copy back");
    assert_close(position(&ui.state, fourth), chain);
    for _ in 0..6 {
        ui.run_action("edit.undo");
    }
    assert!(ui.state.history.is_empty());
    for c in [first, second, third, fourth] {
        assert!(!ui.state.is_live(c));
    }
    assert_close(position(&ui.state, sc.b), start);
}

#[test]
fn smart_duplicate_resets_on_another_edit_or_selection_change() {
    // Each case: duplicate, move the copy, do `other`; Ctrl+D is the plain duplicate again.
    type Other = fn(&mut EditorUi, &Scene, InstanceId);
    let cases: [(&str, Other); 7] = [
        ("colour of the copy", |ui, _, copy| {
            ui.state.set_property(
                &[copy],
                "Color",
                Value::Color3(metallic_core::Color3::new(0.9, 0.1, 0.1)),
            );
        }),
        ("size of the copy", |ui, _, copy| {
            ui.state
                .set_property(&[copy], "Size", Value::Vector3(Vec3::new(3.0, 2.0, 3.0)));
        }),
        ("an edit of another part", |ui, sc, _| {
            nudge(&mut ui.state, sc.c, DVec3::new(1.0, 0.0, 0.0));
        }),
        ("an edit of the source", |ui, sc, _| {
            nudge(&mut ui.state, sc.b, DVec3::new(1.0, 0.0, 0.0));
        }),
        (
            "selecting something else and coming back",
            |ui, sc, copy| {
                ui.state.selection.set([sc.a]);
                ui.state.selection.set([copy]);
            },
        ),
        ("adding to the selection and taking it out", |ui, sc, _| {
            ui.state.selection.add(sc.a);
            let keep = ui.state.selection.ids()[0];
            ui.state.selection.set([keep]);
        }),
        ("undoing the move", |ui, _, _| {
            ui.run_action("edit.undo");
        }),
    ];
    for (what, other) in cases {
        let (mut ui, sc) = editor();
        ui.state.selection.set([sc.b]);
        ui.run_action("edit.duplicate");
        let copy = ui.state.selection.ids()[0];
        nudge(&mut ui.state, copy, DVec3::new(5.0, 0.0, 0.0));
        other(&mut ui, &sc, copy);
        assert_eq!(ui.state.last_duplicate_offset(), None, "{what}");
        let at = position(&ui.state, copy);
        let height = ui.state.selection_bounds().unwrap().size().y;
        ui.run_action("edit.duplicate");
        let next = ui.state.selection.ids()[0];
        assert_ne!(next, copy, "{what}");
        assert_close(position(&ui.state, next), at + DVec3::new(0.0, height, 0.0));
    }

    // The copy moved back onto its source: nothing to repeat, so not a stack in one spot.
    let (mut ui, sc) = editor();
    let start = position(&ui.state, sc.b);
    ui.state.selection.set([sc.b]);
    ui.run_action("edit.duplicate");
    let copy = ui.state.selection.ids()[0];
    let height = position(&ui.state, copy).y - start.y;
    nudge(&mut ui.state, copy, DVec3::new(0.0, -height, 0.0));
    assert_close(position(&ui.state, copy), start);
    ui.run_action("edit.duplicate");
    let next = ui.state.selection.ids()[0];
    assert_close(
        position(&ui.state, next),
        start + DVec3::new(0.0, height, 0.0),
    );
}

#[test]
fn smart_duplicate_repeats_a_turn_and_moves_models_as_one() {
    use metallic_studio::tools::geom::rotation_from_orientation_degrees as turn;

    // A move and a quarter turn: the next copy turns again, about the same axis (spec 8.1).
    let (mut ui, sc) = editor();
    let start = position(&ui.state, sc.c);
    ui.state.selection.set([sc.c]);
    ui.run_action("edit.duplicate");
    let first = ui.state.selection.ids()[0];
    let to = start + DVec3::new(4.0, 0.0, 0.0);
    ui.state.set_property(
        &[first],
        "Position",
        Value::Vector3(Vec3::new(to.x, to.y, to.z)),
    );
    ui.state.set_property(
        &[first],
        "Orientation",
        Value::Vector3(Vec3::new(0.0, 90.0, 0.0)),
    );
    ui.run_action("edit.duplicate");
    let second = ui.state.selection.ids()[0];
    // Y by 90 degrees takes +X to -Z.
    assert_close(position(&ui.state, second), to + DVec3::new(0.0, 0.0, -4.0));
    let Value::Vector3(o) = ui.state.world.get(second, "Orientation").unwrap() else {
        panic!("no orientation");
    };
    let q = turn(DVec3::new(o.x, o.y, o.z));
    assert!(q.angle_between(turn(DVec3::new(0.0, 180.0, 0.0))) < 1e-6);
    assert_eq!(ui.state.history.len(), 4, "duplicate, 2 edits, duplicate");
    ui.run_action("edit.undo");
    assert!(!ui.state.is_live(second));
    assert_eq!(ui.state.history.len(), 3);

    // A Model: its parts keep their places in it.
    let (mut ui, sc) = editor();
    ui.state.selection.set([sc.a, sc.b]);
    ui.run_action("edit.group");
    let model = ui.state.selection.ids()[0];
    ui.run_action("edit.duplicate");
    let copy = ui.state.selection.ids()[0];
    assert_ne!(copy, model);
    let parts = ui.state.selection.parts(&ui.state.world);
    assert_eq!(parts.len(), 2);
    let offset = DVec3::new(0.0, 0.0, 9.0);
    let (a, b) = (position(&ui.state, sc.a), position(&ui.state, sc.b));
    let at = |p: DVec3| Value::Vector3(Vec3::new(p.x, p.y, p.z));
    let moved: Vec<Command> = parts
        .iter()
        .zip([a, b])
        .map(|(&p, src)| {
            let old = ui.state.world.get(p, "Position").unwrap();
            Command::set(p, "Position", old, at(src + offset))
        })
        .collect();
    ui.state
        .apply(Command::Batch {
            name: "Move 2 parts".into(),
            commands: moved,
        })
        .unwrap();
    ui.run_action("edit.duplicate");
    let next = ui.state.selection.parts(&ui.state.world);
    assert_eq!(next.len(), 2);
    assert_close(position(&ui.state, next[0]), a + offset * 2.0);
    assert_close(position(&ui.state, next[1]), b + offset * 2.0);
    assert_eq!(ui.state.selection.len(), 1, "the Model's copy is selected");
}

#[test]
fn ctrl_d_after_a_real_gizmo_drag_repeats_it_and_seeds_the_array() {
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.a]);
    let source = position(&h.state().state, sc.a);
    h.state_mut().run_action("edit.duplicate");
    let first = h.state().state.selection.ids()[0];
    h.state_mut().state.tool = Tool::Move;
    h.run_steps(3);
    // The Y arrow, as in the viewport drag test above.
    let pivot = screen(&h, position(&h.state().state, first));
    let grab = pivot - egui::vec2(0.0, 54.0);
    h.hover_at(grab);
    h.run_steps(2);
    button(&mut h, grab, true, Modifiers::NONE);
    h.hover_at(grab - egui::vec2(0.0, 60.0));
    h.run_steps(2);
    assert!(h.state().viewport.is_dragging());
    button(&mut h, grab - egui::vec2(0.0, 60.0), false, Modifiers::NONE);
    h.run_steps(2);
    assert_eq!(h.state().state.history.len(), 2, "duplicate, drag");
    assert_eq!(h.state().state.selection.ids(), &[first], "still the copy");
    let at = position(&h.state().state, first);
    let offset = at - source;
    assert!(offset.y > 4.0, "the drag moved it up: {offset}");
    assert_eq!(h.state().state.last_duplicate_offset(), Some(offset));

    // The Array popover opens with that offset.
    h.state_mut().run_action("build.array");
    h.run_steps(2);
    assert_eq!(h.state().tools.state().arrange.linear.offset, offset);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);

    let centre = viewport_top(&h);
    h.hover_at(centre);
    h.run_steps(1);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::D);
    h.run_steps(2);
    let second = h.state().state.selection.ids()[0];
    assert_ne!(second, first);
    assert_close(position(&h.state().state, second), at + offset);
    assert_eq!(h.state().state.history.len(), 3);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(!h.state().state.is_live(second));
    assert_eq!(h.state().state.history.len(), 2);

    // Without a tracked offset the popover has its old default: the size on X.
    h.state_mut().state.selection.set([sc.c]);
    h.state_mut().run_action("build.array");
    h.run_steps(2);
    assert_eq!(
        h.state().tools.state().arrange.linear.offset,
        DVec3::new(2.0, 0.0, 0.0)
    );
}

fn wheel(h: &mut Harness<'static, EditorUi>, up: bool, modifiers: Modifiers) {
    h.event(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Line,
        delta: egui::vec2(0.0, if up { 1.0 } else { -1.0 }),
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        modifiers,
    });
    h.run_steps(2);
}

#[test]
fn the_wheel_over_the_array_handle_changes_the_count_and_not_the_camera() {
    use metallic_studio::tools::arrange::array::handle_position;
    let (ui, sc) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.set([sc.a]);
    h.state_mut().run_action("build.array");
    h.run_steps(3);
    let count = |h: &Harness<'static, EditorUi>| h.state().tools.state().arrange.linear.count;
    let step = h.state().tools.state().arrange.linear.offset;
    assert_eq!(count(&h), 5);

    // Over the handle: the count, not the camera.
    let ring = screen(&h, handle_position(DVec3::new(-8.0, 2.0, 0.0), step, 5));
    h.hover_at(ring);
    h.run_steps(3);
    assert!(h.state().tools.captures_wheel());
    let camera = h.state().viewport.camera;
    wheel(&mut h, true, Modifiers::NONE);
    assert_eq!(count(&h), 6);
    assert_eq!(h.state().tools.state().arrange.count_text, "6");
    // The ring moved away from the pointer; the wheel stays with it until the pointer moves.
    wheel(&mut h, true, Modifiers::NONE);
    assert_eq!(count(&h), 7);
    wheel(&mut h, false, Modifiers::NONE);
    wheel(&mut h, false, Modifiers::NONE);
    wheel(&mut h, false, Modifiers::NONE);
    assert_eq!(count(&h), 4);
    h.run_steps(8);
    assert_eq!(
        h.state().viewport.camera,
        camera,
        "no dolly over the handle"
    );
    assert!(h.state().state.history.is_empty(), "nothing committed");

    // Pointer gone: the camera has its wheel back and the count stays.
    let away = viewport_top(&h);
    h.hover_at(away);
    h.run_steps(3);
    assert!(!h.state().tools.captures_wheel());
    wheel(&mut h, true, Modifiers::NONE);
    h.run_steps(8);
    assert_eq!(count(&h), 4);
    assert_ne!(h.state().viewport.camera, camera, "the wheel dollies");

    // Ctrl+wheel keeps working anywhere, without the dolly.
    let camera = h.state().viewport.camera;
    wheel(&mut h, true, Modifiers::COMMAND);
    h.run_steps(8);
    assert_eq!(count(&h), 5);
    assert_eq!(h.state().viewport.camera, camera);
}

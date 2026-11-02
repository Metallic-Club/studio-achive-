//! Scripted Studio checks: the editor window driven by pointer and key events (no GPU needed;
//! the viewport falls back to a flat background but picking and gizmos work the same).
use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_render::glam::DVec3;
use metallic_studio::editor::{EditorState, Tool};
use metallic_studio::editor_ui::{EditorExit, EditorUi};
use metallic_studio::output::OutputPanel;

fn editor() -> (EditorUi, InstanceId, InstanceId) {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    let model = world.create("Model", Some(ws)).unwrap();
    let part = world.create("Part", Some(model)).unwrap();
    world
        .set(part, "Size", Value::Vector3(Vec3::new(4.0, 4.0, 4.0)))
        .unwrap();
    world.set(part, "Anchored", Value::Bool(true)).unwrap();
    let output = OutputPanel::new();
    let mut state = EditorState::new(
        world,
        std::path::Path::new("."),
        std::path::Path::new("unused.mscene"),
        "Test",
        output.log.sink(),
    );
    state.can_save = false;
    (EditorUi::new(state, output), model, part)
}

/// The panels use the bundled font families, which the theme installs for the next pass: the
/// first pass only installs them. Returns true on that first pass (draw nothing).
fn install_theme(ui: &egui::Ui, themed: &mut bool) -> bool {
    if *themed {
        return false;
    }
    metallic_studio::theme::apply(ui.ctx(), metallic_studio::theme::ThemeId::Graphite);
    *themed = true;
    ui.ctx().request_repaint();
    true
}

fn harness(ui: EditorUi) -> Harness<'static, EditorUi> {
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            move |ui, editor: &mut EditorUi| {
                if install_theme(ui, &mut themed) {
                    return;
                }
                editor.ui(ui, None);
            },
            ui,
        );
    h.run_steps(3);
    h
}

fn press(h: &mut Harness<'static, EditorUi>, pos: Pos2, pressed: bool) {
    h.event(Event::PointerButton {
        pos,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    });
    h.run_steps(2);
}

fn click(h: &mut Harness<'static, EditorUi>, pos: Pos2) {
    h.hover_at(pos);
    h.run_steps(2);
    press(h, pos, true);
    press(h, pos, false);
}

#[test]
fn click_selects_model_first_then_the_part() {
    let (ui, model, part) = editor();
    let mut h = harness(ui);
    let at = h
        .state()
        .viewport
        .screen_pos(DVec3::new(0.0, 0.0, 1.9))
        .unwrap();
    // Select tool so no gizmo sits over the part.
    h.state_mut().state.tool = Tool::Select;
    click(&mut h, at);
    assert_eq!(h.state().state.selection.ids(), &[model]);
    click(&mut h, at);
    assert_eq!(h.state().state.selection.ids(), &[part]);
    // Clicking empty space clears.
    let empty = h.state().viewport.rect().left_top() + egui::vec2(10.0, 10.0);
    click(&mut h, empty);
    assert!(h.state().state.selection.is_empty());
}

#[test]
fn gizmo_drag_moves_in_whole_studs_as_one_undo_step() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(part);
    h.state_mut().state.tool = Tool::Move;
    h.run_steps(2);
    // Grab the Y arrow halfway along, drag up.
    let ui = h.state();
    let origin = ui.viewport.screen_pos(DVec3::ZERO).unwrap();
    let up = ui.viewport.screen_pos(DVec3::new(0.0, 1.0, 0.0)).unwrap();
    let px_per_stud = (origin.y - up.y).abs();
    let arrow_len_px = 90.0;
    let grab = origin - egui::vec2(0.0, arrow_len_px * 0.6);
    h.hover_at(grab);
    h.run_steps(2);
    press(&mut h, grab, true);
    let target = grab - egui::vec2(0.0, px_per_stud * 3.3);
    h.hover_at(target);
    h.run_steps(2);
    press(&mut h, target, false);
    let s = &h.state().state;
    let Value::Vector3(p) = s.world.get(part, "Position").unwrap() else {
        panic!()
    };
    assert!((p.y - 3.0).abs() < 1e-9, "snapped to 3 studs, got {p:?}");
    assert_eq!(p.x, 0.0);
    assert_eq!(s.history.len(), 1);
    assert_eq!(
        s.history.undo_name(&s.world).as_deref(),
        Some("Move 1 part")
    );
    // Ctrl+Z through the keymap.
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert_eq!(
        h.state().state.world.get(part, "Position").unwrap(),
        Value::Vector3(Vec3::new(0.0, 0.0, 0.0))
    );
}

#[test]
fn escape_cancels_a_drag() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(part);
    h.run_steps(2);
    let origin = h.state().viewport.screen_pos(DVec3::ZERO).unwrap();
    let grab = origin - egui::vec2(0.0, 54.0);
    h.hover_at(grab);
    h.run_steps(2);
    press(&mut h, grab, true);
    h.hover_at(grab - egui::vec2(0.0, 120.0));
    h.run_steps(2);
    assert!(h.state().viewport.is_dragging());
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    press(&mut h, grab - egui::vec2(0.0, 120.0), false);
    let s = &h.state().state;
    assert_eq!(
        s.world.get(part, "Position").unwrap(),
        Value::Vector3(Vec3::new(0.0, 0.0, 0.0))
    );
    assert!(s.history.is_empty());
}

#[test]
fn delete_key_and_tool_keys_work_over_the_viewport() {
    let (ui, model, _part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(model);
    let centre = h.state().viewport.rect().center();
    h.hover_at(centre);
    h.run_steps(2);
    h.key_press(egui::Key::E);
    h.run_steps(2);
    assert_eq!(h.state().state.tool, Tool::Rotate);
    h.key_press(egui::Key::Delete);
    h.run_steps(2);
    assert!(!h.state().state.is_live(model));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    assert!(h.state().state.is_live(model));
}

#[test]
fn alt_click_cycles_to_the_part_behind() {
    let (mut ui, model, front) = editor();
    let back = ui.state.world.create("Part", Some(model)).unwrap();
    ui.state
        .world
        .set(back, "Position", Value::Vector3(Vec3::new(0.0, 0.0, -10.0)))
        .unwrap();
    ui.state
        .world
        .set(back, "Size", Value::Vector3(Vec3::new(30.0, 30.0, 1.0)))
        .unwrap();
    ui.state.tool = Tool::Select;
    let mut h = harness(ui);
    let at = h
        .state()
        .viewport
        .screen_pos(DVec3::new(0.0, 0.0, 1.9))
        .unwrap();
    h.hover_at(at);
    h.run_steps(2);
    // Alt is held for the whole click, as on a real keyboard.
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::ALT,
        });
        h.run_steps(2);
    }
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
    assert_eq!(h.state().state.selection.ids(), &[back]);
    assert_ne!(back, front);
}

struct Shell {
    editor: EditorUi,
    exit: EditorExit,
}

fn dirty_shell(scene: std::path::PathBuf) -> (Harness<'static, Shell>, InstanceId) {
    let (mut ui, _model, part) = editor();
    ui.state.scene_path = scene;
    ui.state.can_save = true;
    ui.state
        .set_property(&[part], "Transparency", Value::Number(0.5));
    assert!(ui.state.is_dirty());
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .build_ui_state(
            move |ui, shell: &mut Shell| {
                if install_theme(ui, &mut themed) {
                    return;
                }
                let exit = shell.editor.ui(ui, None);
                if exit != EditorExit::Stay {
                    shell.exit = exit;
                }
            },
            Shell {
                editor: ui,
                exit: EditorExit::Stay,
            },
        );
    h.run_steps(2);
    (h, part)
}

#[test]
fn closing_the_window_with_unsaved_changes_asks_first() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = dirty_shell(dir.path().join("main.mscene"));
    assert!(
        !h.state_mut().editor.request_quit(),
        "dirty: the close is held"
    );
    h.run_steps(2);
    assert!(h.state().editor.is_confirming());
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    assert!(!h.state().editor.is_confirming());
    assert_eq!(h.state().exit, EditorExit::Stay);
    // Ask again and choose Don't save: the shell is told to quit.
    assert!(!h.state_mut().editor.request_quit());
    h.run_steps(2);
    h.get_by_label("Don't save").click();
    h.run_steps(2);
    assert_eq!(h.state().exit, EditorExit::Quit);
}

#[test]
fn save_from_the_prompt_closes_only_when_the_save_worked() {
    let dir = tempfile::tempdir().unwrap();
    // The folder does not exist, so Save fails.
    let (mut h, _) = dirty_shell(dir.path().join("missing").join("main.mscene"));
    assert!(!h.state_mut().editor.request_quit());
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().exit,
        EditorExit::Stay,
        "a failed save keeps the game open"
    );
    assert!(h.state().editor.is_confirming());
    assert!(h.state().editor.save_error().is_some());
    assert!(h.state().editor.state.is_dirty());
    // Fix the path and save again: now it quits.
    h.state_mut().editor.state.scene_path = dir.path().join("main.mscene");
    // Move away and wait past the double-click window, as a person would.
    h.hover_at(egui::pos2(5.0, 400.0));
    h.step();
    std::thread::sleep(std::time::Duration::from_millis(400));
    h.run_steps(2);
    h.get_by_label("Save").click();
    h.run_steps(2);
    assert_eq!(
        h.state().exit,
        EditorExit::Quit,
        "{:?}",
        h.state().editor.save_error()
    );
    assert!(dir.path().join("main.mscene").exists());
}

#[test]
fn dragging_the_body_moves_the_part_as_one_step() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Select;
    h.state_mut().state.selection.select_only(part);
    h.run_steps(2);
    let grab = h
        .state()
        .viewport
        .screen_pos(DVec3::new(0.0, 0.0, 2.0))
        .unwrap();
    h.hover_at(grab);
    h.run_steps(2);
    press(&mut h, grab, true);
    for i in 1..=6 {
        h.hover_at(grab + egui::vec2(15.0 * i as f32, 0.0));
        h.run_steps(1);
    }
    assert!(h.state().viewport.is_dragging());
    press(&mut h, grab + egui::vec2(90.0, 0.0), false);
    let s = &h.state().state;
    let Value::Vector3(p) = s.world.get(part, "Position").unwrap() else {
        panic!()
    };
    assert!(p.x > 0.5, "moved right: {p:?}");
    assert!((p.x - p.x.round()).abs() < 1e-9, "on the grid: {p:?}");
    assert!(p.y.abs() < 1e-9, "stays on its start height: {p:?}");
    assert_eq!(s.history.len(), 1);
    assert_eq!(
        s.history.undo_name(&s.world).as_deref(),
        Some("Move 1 part")
    );
    assert_eq!(s.selection.ids(), &[part]);
}

#[test]
fn typing_during_a_drag_sets_the_exact_amount() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(part);
    h.state_mut().state.tool = Tool::Move;
    h.run_steps(2);
    let origin = h.state().viewport.screen_pos(DVec3::ZERO).unwrap();
    let grab = origin - egui::vec2(0.0, 54.0);
    h.hover_at(grab);
    h.run_steps(2);
    press(&mut h, grab, true);
    h.hover_at(grab - egui::vec2(0.0, 30.0));
    h.run_steps(2);
    for c in ["3", "*", "4"] {
        h.event(Event::Text(c.into()));
        h.run_steps(1);
    }
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert!(!h.state().viewport.is_dragging(), "Enter ends the drag");
    press(&mut h, grab - egui::vec2(0.0, 30.0), false);
    let s = &h.state().state;
    assert_eq!(
        s.world.get(part, "Position").unwrap(),
        Value::Vector3(Vec3::new(0.0, 12.0, 0.0))
    );
    assert_eq!(s.history.len(), 1);
}

#[test]
fn closing_with_unsaved_script_text_asks_first() {
    use egui::accesskit::Role;
    let (mut ui, _model, _part) = editor();
    let ws = ui.state.world.root("Workspace").unwrap();
    let script = ui.state.world.create("Script", Some(ws)).unwrap();
    ui.state.history.mark_saved();
    assert!(!ui.state.is_dirty());
    ui.state
        .request(metallic_studio::editor::UiRequest::OpenScript(script));
    let mut h = harness(ui);
    h.get_by_role(Role::MultilineTextInput).click();
    h.run_steps(2);
    h.event(Event::Text("print(1)".into()));
    h.run_steps(2);
    // The text is only in the tab so far; Alt+F4 (the window close) must still ask.
    assert!(!h.state().state.is_dirty());
    assert!(
        !h.state_mut().request_quit(),
        "unsaved script text holds the close"
    );
    h.run_steps(2);
    assert!(h.state().is_confirming());
    assert_eq!(
        h.state().state.world.get(script, "Source").unwrap(),
        Value::String("print(1)".into())
    );
}

#[test]
fn ctrl_comma_asks_for_the_settings_dialog() {
    let (ui, _, _) = editor();
    let mut h = harness(ui);
    assert!(!h.state_mut().take_settings_request());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Comma);
    h.run_steps(3);
    assert!(h.state_mut().take_settings_request());
    assert!(!h.state_mut().take_settings_request(), "asked once");
}

#[test]
fn ctrl_w_closes_the_game_through_the_unsaved_changes_prompt() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, _) = dirty_shell(dir.path().join("main.mscene"));
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::W);
    h.run_steps(3);
    assert!(h.state().editor.is_confirming(), "dirty: asks first");
    assert_eq!(h.state().exit, EditorExit::Stay);
    h.get_by_label("Cancel").click();
    h.run_steps(2);
    // Clean game: Ctrl+W closes at once.
    h.state_mut().editor.state.history.mark_saved();
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::W);
    h.run_steps(3);
    assert_eq!(h.state().exit, EditorExit::Close);
}

#[test]
fn escape_clears_the_selection_and_never_closes() {
    let (ui, model, _part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(model);
    h.hover_at(egui::pos2(640.0, 400.0));
    h.run_steps(2);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(h.state().state.selection.ids().is_empty());
}

#[test]
fn ctrl_w_does_nothing_while_a_text_field_has_focus() {
    use egui::accesskit::Role;
    let (mut ui, _model, part) = editor();
    let ws = ui.state.world.root("Workspace").unwrap();
    let script = ui.state.world.create("Script", Some(ws)).unwrap();
    ui.state
        .set_property(&[part], "Transparency", Value::Number(0.5));
    assert!(ui.state.is_dirty());
    ui.state
        .request(metallic_studio::editor::UiRequest::OpenScript(script));
    let mut h = harness(ui);
    h.get_by_role(Role::MultilineTextInput).click();
    h.run_steps(2);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::W);
    h.run_steps(3);
    assert!(
        !h.state().is_confirming(),
        "Ctrl+W must not start closing the game from a text field"
    );
}

/// Properties labels share the centre line of the control beside them: single fields, vector
/// rows, the first row of a CFrame group, dropdowns, checkboxes and attribute rows.
#[test]
fn properties_labels_are_centred_on_their_controls() {
    use egui::accesskit::Role;
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    let state = &mut h.state_mut().state;
    state.selection.select_only(part);
    state
        .world
        .set_attribute(part, "Score", Value::Number(1.0))
        .unwrap();
    h.run_steps(2);
    for (label, role) in [
        ("Name", Role::TextInput),
        ("PivotOffset", Role::SpinButton),
        ("Material", Role::ComboBox),
        ("Transparency", Role::SpinButton),
        ("Shape", Role::ComboBox),
        ("Style", Role::Button),
        ("Orientation", Role::SpinButton),
        ("Size", Role::SpinButton),
        ("Anchored", Role::CheckBox),
        ("Score", Role::SpinButton),
    ] {
        let shown = h.get_by_label(label).rect();
        let y = shown.center().y;
        let nearest = h
            .get_all_by_role(role)
            .filter(|n| n.rect().left() > shown.right())
            .map(|n| n.rect().center().y)
            .min_by(|a, b| (a - y).abs().total_cmp(&(b - y).abs()))
            .unwrap();
        assert!(
            (nearest - y).abs() <= 0.5,
            "{label}: label centre {y}, control centre {nearest}"
        );
    }
}

#[test]
fn global_keys_wait_during_a_properties_scrub_and_esc_cancels_it() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(part);
    h.run_steps(2);
    // One earlier step, so a stray Ctrl+Z would show.
    {
        let e = h.state_mut();
        let _ = e.state.apply(metallic_studio::commands::Command::set(
            part,
            "Transparency",
            Value::Number(0.0),
            Value::Number(0.25),
        ));
    }
    // Hold the pointer (as a scrub does) over the status bar, away from the viewport.
    let held = egui::pos2(640.0, 796.0);
    h.hover_at(held);
    press(&mut h, held, true);
    {
        let e = h.state_mut();
        let EditorUi {
            properties, state, ..
        } = e;
        properties.scrub(state, "Transparency", Value::Number(0.75));
    }
    h.run_steps(1);
    assert!(h.state().properties.in_gesture());
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Z);
    h.run_steps(2);
    let s = &h.state().state;
    assert_eq!(s.history.len(), 1, "Ctrl+Z waits for the scrub");
    assert_eq!(
        s.world.get(part, "Transparency").unwrap(),
        Value::Number(0.75)
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(!h.state().properties.in_gesture());
    assert_eq!(
        h.state().state.world.get(part, "Transparency").unwrap(),
        Value::Number(0.25),
        "Esc puts the value back"
    );
    assert_eq!(
        h.state().state.selection.ids(),
        &[part],
        "Esc did not also run a global action"
    );
    press(&mut h, held, false);
    h.run_steps(2);
    let s = &h.state().state;
    assert_eq!(s.history.len(), 1, "a cancelled scrub records nothing");
    assert_eq!(
        s.world.get(part, "Transparency").unwrap(),
        Value::Number(0.25)
    );
}

#[test]
fn losing_focus_cancels_a_drag() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.selection.select_only(part);
    h.run_steps(2);
    let origin = h.state().viewport.screen_pos(DVec3::ZERO).unwrap();
    let grab = origin - egui::vec2(0.0, 54.0);
    h.hover_at(grab);
    h.run_steps(2);
    press(&mut h, grab, true);
    h.hover_at(grab - egui::vec2(0.0, 120.0));
    h.run_steps(2);
    assert!(h.state().viewport.is_dragging());
    h.event(Event::WindowFocused(false));
    h.run_steps(2);
    assert!(!h.state().viewport.is_dragging());
    let s = &h.state().state;
    assert_eq!(
        s.world.get(part, "Position").unwrap(),
        Value::Vector3(Vec3::new(0.0, 0.0, 0.0))
    );
    assert!(s.history.is_empty());
    assert!(!s.preview.is_active());
}

#[test]
fn ctrl_d_before_an_alt_drag_moves_keeps_the_selection_on_live_parts() {
    let (ui, _model, part) = editor();
    let mut h = harness(ui);
    h.state_mut().state.tool = Tool::Select;
    h.state_mut().state.selection.select_only(part);
    h.run_steps(2);
    let grab = h
        .state()
        .viewport
        .screen_pos(DVec3::new(0.0, 0.0, 2.0))
        .unwrap();
    h.hover_at(grab);
    h.run_steps(2);
    h.event(Event::ModifiersChanged(Modifiers::ALT));
    press(&mut h, grab, true);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    h.run_steps(1);
    assert!(
        h.state().viewport.is_dragging(),
        "Alt on the body drags a copy"
    );
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::D);
    h.run_steps(2);
    press(&mut h, grab, false);
    h.run_steps(2);
    let s = &h.state().state;
    assert!(!s.selection.ids().is_empty());
    for &id in s.selection.ids() {
        assert!(s.is_live(id), "selection on a live part");
    }
}

/// The viewport header never scrolls or cuts a control off: at 1280 and 1600 px every button in
/// the strip lies inside it, and what does not fit is in the "»" menu (#226 review).
#[test]
fn the_viewport_header_fits_at_1280_and_1600() {
    use egui_kittest::kittest::By;
    for width in [1280.0_f32, 1600.0] {
        let (ui, _, _) = editor();
        let mut themed = false;
        let mut h = Harness::builder()
            .with_size(egui::vec2(width, 900.0))
            .build_ui_state(
                move |ui, editor: &mut EditorUi| {
                    if install_theme(ui, &mut themed) {
                        return;
                    }
                    editor.ui(ui, None);
                },
                ui,
            );
        // A few frames for the measured widths to settle.
        h.run_steps(8);
        let strip = h.state().header.rect();
        assert!(strip.width() > 300.0, "{width}: header {strip:?}");
        let mut inside = 0;
        for node in h.query_all(By::new().role(egui::accesskit::Role::Button)) {
            let r = node.rect();
            if r.center().y < strip.top() || r.center().y > strip.bottom() {
                continue;
            }
            assert!(
                r.left() >= strip.left() - 0.5 && r.right() <= strip.right() + 0.5,
                "{width} px: {:?} at {r:?} is outside the header {strip:?}",
                format!("{node:?}").chars().take(160).collect::<String>()
            );
            inside += 1;
        }
        assert!(inside >= 8, "{width} px: only {inside} header buttons");
        let overflow = h.state().header.overflow().to_vec();
        if !overflow.is_empty() {
            h.get_by_label("More header options");
        }
        if width <= 1280.0 {
            assert!(!overflow.is_empty(), "1280 px should need the » menu");
            assert!(!overflow.contains(&"snap.grid"), "Grid stays on the strip");
        }
    }
}

/// Ctrl+. focuses the contextual toolbar once; a click elsewhere takes the focus away for good, so
/// shortcuts work again (#384 review).
#[test]
fn the_context_bar_lets_go_of_focus_after_an_outside_click() {
    let (mut ui, _, part) = editor();
    ui.state.selection.select_only(part);
    ui.state.tool = Tool::Move;
    let revision = ui.state.selection.revision();
    ui.context_bar.arm(revision);
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_accessibility_check(false)
        .build_ui_state(
            move |ui, editor: &mut EditorUi| {
                if install_theme(ui, &mut themed) {
                    return;
                }
                editor.ui(ui, None);
            },
            ui,
        );
    h.run_steps(4);
    h.key_press_modifiers(Modifiers::COMMAND, egui::Key::Period);
    h.run_steps(3);
    assert!(h.state().context_bar.has_focus(), "Ctrl+. focuses the bar");
    // Click the Explorer's filter field, then leave it with Esc.
    h.hover_at(egui::pos2(140.0, 126.0));
    h.run_steps(1);
    press(&mut h, egui::pos2(140.0, 126.0), true);
    press(&mut h, egui::pos2(140.0, 126.0), false);
    h.run_steps(3);
    assert!(
        !h.state().context_bar.has_focus(),
        "an outside click releases the bar"
    );
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    // Viewport shortcuts work again: E picks Rotate (pointer over the viewport, clear of the
    // bar, which sits under the selection and is wider with the Style button).
    let vp = h.state().viewport.rect();
    h.hover_at(egui::pos2(vp.center().x, vp.top() + 40.0));
    h.run_steps(2);
    h.key_press(egui::Key::E);
    h.run_steps(3);
    assert_eq!(h.state().state.tool, Tool::Rotate);
}

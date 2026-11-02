//! The building style picker (#224): opening, thumbnails one per frame, hover preview, Esc,
//! one undo step, Apply to, keyboard, the Play lock. No GPU: thumbnails come from a stand-in.
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_render::Camera;
use metallic_studio::editor::EditorState;
use metallic_studio::editor_ui::EditorUi;
use metallic_studio::output::OutputPanel;
use metallic_studio::style_picker::{ApplyTo, OPEN_ID, THUMB_PX, TILES, ThumbnailSource};

const RETRO: &str = "Retro / PS1: wobbly vertices, warped textures, 15-bit colour";

struct Scene {
    model: InstanceId,
    parts: Vec<InstanceId>,
}

fn editor() -> (EditorUi, Scene) {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    let model = world.create("Model", Some(ws)).unwrap();
    world
        .set(model, "Name", Value::String("Props".into()))
        .unwrap();
    let parts = (0..3)
        .map(|i| {
            let p = world.create("Part", Some(model)).unwrap();
            world
                .set(
                    p,
                    "Position",
                    Value::Vector3(Vec3::new(f64::from(i) * 5.0, 2.0, 0.0)),
                )
                .unwrap();
            world.set(p, "Anchored", Value::Bool(true)).unwrap();
            p
        })
        .collect();
    let output = OutputPanel::new();
    let mut state = EditorState::new(
        world,
        std::path::Path::new("."),
        std::path::Path::new("unused.mscene"),
        "Test",
        output.log.sink(),
    );
    state.can_save = false;
    (EditorUi::new(state, output), Scene { model, parts })
}

/// A thumbnail source that counts its renders and returns a flat grey image.
struct Counting(Arc<AtomicUsize>);

impl ThumbnailSource for Counting {
    fn render(&mut self, _: &World, _: &Camera, size: (u32, u32)) -> Option<Vec<u8>> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Some(vec![128; (size.0 * size.1 * 4) as usize])
    }
}

fn harness(mut ui: EditorUi) -> (Harness<'static, EditorUi>, Arc<AtomicUsize>) {
    let count = Arc::new(AtomicUsize::new(0));
    ui.style_picker
        .set_thumbnail_source(Box::new(Counting(count.clone())));
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 860.0))
        .build_ui_state(
            move |ui, editor: &mut EditorUi| {
                if !themed {
                    metallic_studio::theme::apply(
                        ui.ctx(),
                        metallic_studio::theme::ThemeId::Graphite,
                    );
                    themed = true;
                    ui.ctx().request_repaint();
                    return;
                }
                editor.ui(ui, None);
            },
            ui,
        );
    h.run_steps(3);
    (h, count)
}

fn look(h: &Harness<'static, EditorUi>, id: InstanceId) -> String {
    match h.state().state.world.get(id, "LookOverride").unwrap() {
        Value::String(s) => s,
        v => panic!("{v:?}"),
    }
}

fn open(h: &mut Harness<'static, EditorUi>) {
    h.state_mut().run_action(OPEN_ID);
    h.step();
    assert!(h.state().style_picker.is_open(), "open after one frame");
}

#[test]
fn it_opens_at_once_and_renders_one_thumbnail_per_frame() {
    let (ui, sc) = editor();
    let (mut h, count) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    // The tiles and their names are there from the first frame (placeholders until rendered).
    assert!(h.query_by_label(RETRO).is_some());
    assert!(
        h.query_by_label("Place Look: uses the place Look")
            .is_some()
    );
    assert!(
        count.load(Ordering::SeqCst) <= 1,
        "at most one render per frame"
    );
    for frame in 2..=TILES {
        h.step();
        assert!(count.load(Ordering::SeqCst) <= frame, "frame {frame}");
    }
    h.run_steps(3);
    assert_eq!(
        count.load(Ordering::SeqCst),
        TILES,
        "each style rendered once"
    );
    assert_eq!(h.state().style_picker.thumbnail_progress(), (TILES, TILES));
    // Staying open renders nothing more.
    h.run_steps(5);
    assert_eq!(count.load(Ordering::SeqCst), TILES);
    let _ = THUMB_PX;
}

#[test]
fn hover_previews_and_esc_leaves_the_selection_unchanged() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    h.run_steps(2);
    h.get_by_label(RETRO).hover();
    h.run_steps(1);
    std::thread::sleep(Duration::from_millis(170));
    h.run_steps(2);
    assert_eq!(h.state().style_picker.previewing(), Some("retro-32"));
    assert_eq!(look(&h, sc.model), "retro-32", "the viewport previews");
    assert!(h.state().state.history.is_empty(), "a preview is no step");

    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    let s = &h.state();
    assert!(!s.style_picker.is_open());
    assert!(s.state.history.is_empty());
    assert_eq!(
        s.state.selection.ids(),
        [sc.model].as_slice(),
        "the selection stays"
    );
    assert_eq!(s.state.selection.ids(), &[sc.model], "the selection stays");
}

#[test]
fn a_click_applies_one_undo_step_to_the_model() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    h.run_steps(2);
    h.get_by_label(RETRO).click();
    h.run_steps(2);
    let s = &h.state().state;
    assert_eq!(s.history.len(), 1);
    assert_eq!(
        s.history.undo_name(&s.world).as_deref(),
        Some("Style: Retro / PS1 (1 model)")
    );
    assert_eq!(look(&h, sc.model), "retro-32");
    for p in &sc.parts {
        assert_eq!(look(&h, *p), "inherit", "children keep inherit");
    }
    h.state_mut().state.undo();
    assert_eq!(look(&h, sc.model), "inherit");
}

/// A person rests on a tile, clicks it and leaves the pointer there: Undo still works, however
/// long the frames take. Staying on the applied tile used to open an empty preview gesture.
#[test]
fn undo_works_while_the_pointer_stays_on_the_applied_tile() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    h.run_steps(2);
    h.get_by_label(RETRO).hover();
    h.run_steps(1);
    std::thread::sleep(Duration::from_millis(170));
    h.run_steps(2);
    assert_eq!(h.state().style_picker.previewing(), Some("retro-32"));
    h.get_by_label(RETRO).click();
    h.run_steps(2);
    assert_eq!(h.state().state.history.len(), 1);
    std::thread::sleep(Duration::from_millis(170));
    h.run_steps(2);
    assert!(!h.state().state.gesture_active(), "nothing left to preview");
    h.state_mut().state.undo();
    assert_eq!(look(&h, sc.model), "inherit");
}

#[test]
fn each_part_writes_every_part_and_the_keyboard_drives_the_tiles() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    {
        let ui = h.state_mut();
        ui.style_picker
            .set_apply_to(&mut ui.state, ApplyTo::EachPart);
    }
    // Move the pointer away from the popover, then: Right three times reaches Toon (tile 3),
    // Space previews it, Enter applies.
    h.hover_at(egui::pos2(5.0, 840.0));
    h.run_steps(2);
    for _ in 0..3 {
        h.key_press(egui::Key::ArrowRight);
        h.run_steps(1);
    }
    h.key_press(egui::Key::Space);
    h.run_steps(2);
    assert_eq!(h.state().style_picker.previewing(), Some("toon"));
    assert!(h.state().state.history.is_empty());
    h.key_press(egui::Key::Enter);
    h.run_steps(2);
    assert_eq!(
        h.state()
            .state
            .history
            .undo_name(&h.state().state.world)
            .as_deref(),
        Some("Style: Toon (3 parts)")
    );
    for p in &sc.parts {
        assert_eq!(look(&h, *p), "toon");
    }
    assert_eq!(look(&h, sc.model), "inherit");
}

#[test]
fn style_open_is_locked_while_playing_and_needs_a_selection() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    let enabled = |h: &Harness<'static, EditorUi>| {
        let ui = h.state();
        ui.actions
            .get(OPEN_ID)
            .is_some_and(|a| a.is_enabled(&ui.state))
    };
    assert!(!enabled(&h), "nothing selected");
    h.state_mut().state.selection.select_only(sc.model);
    assert!(enabled(&h));
    assert!(h.state_mut().state.start_play("play.start"));
    assert!(!enabled(&h), "locked while playing");
}

#[test]
fn play_during_a_hover_preview_closes_the_picker_and_starts_on_the_unchanged_world() {
    let (ui, sc) = editor();
    let (mut h, _) = harness(ui);
    h.state_mut().state.selection.select_only(sc.model);
    h.run_steps(2);
    open(&mut h);
    h.run_steps(2);
    h.get_by_label(RETRO).hover();
    h.run_steps(1);
    std::thread::sleep(Duration::from_millis(170));
    h.run_steps(2);
    assert!(h.state().state.gesture_active(), "a preview is a gesture");
    h.state_mut().run_action("play.start");
    h.run_steps(3);
    assert!(h.state().state.play.is_playing(), "Play started");
    assert!(!h.state().style_picker.is_open());
    assert_eq!(look(&h, sc.model), "inherit", "the preview was cancelled");
    assert!(h.state().state.history.is_empty());
}

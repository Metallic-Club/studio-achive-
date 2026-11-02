//! Keeping edits from Play (#130): Stop returns to the level unchanged, then offers what the game
//! changed; only what the creator ticks reaches the edit world, as one undo step.
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use metallic_core::{InstanceId, Value, Vec3, World, save_scene};
use metallic_runtime::TICK;
use metallic_studio::editor::{EditorState, Gesture};
use metallic_studio::editor_ui::EditorUi;
use metallic_studio::output::{OutputLog, OutputPanel};
use metallic_studio::play::review::{ChangeKind, MAX_LISTED, compare};
use metallic_studio::play::{PlayMode, PlaySession, PlayStart, PlayStatus};

/// The level the tests play: a Probe (anchored), a Crate with a Lid inside it (anchored), a loose
/// Rock high up, and a server Script with `source`.
struct Level {
    world: World,
    probe: InstanceId,
    crate_: InstanceId,
    rock: InstanceId,
}

fn named(world: &mut World, class: &str, parent: InstanceId, name: &str) -> InstanceId {
    let id = world.create(class, Some(parent)).unwrap();
    world.set(id, "Name", Value::String(name.into())).unwrap();
    id
}

fn level(source: &str) -> Level {
    let mut world = World::new_with_services();
    let ws = world.root("Workspace").unwrap();
    let probe = named(&mut world, "Part", ws, "Probe");
    world.set(probe, "Anchored", Value::Bool(true)).unwrap();
    let crate_ = named(&mut world, "Model", ws, "Crate");
    let lid = named(&mut world, "Part", crate_, "Lid");
    world.set(lid, "Anchored", Value::Bool(true)).unwrap();
    let rock = named(&mut world, "Part", ws, "Rock");
    world
        .set(rock, "Position", Value::Vector3(Vec3::new(0.0, 50.0, 0.0)))
        .unwrap();
    let scripts = world.root("ServerScriptService").unwrap();
    let script = named(&mut world, "Script", scripts, "Game");
    world
        .set(script, "Source", Value::String(source.into()))
        .unwrap();
    Level {
        world,
        probe,
        crate_,
        rock,
    }
}

fn state(level: Level, log: &OutputLog) -> (EditorState, InstanceId, InstanceId, InstanceId) {
    metallic_script::use_test_budgets_on_this_thread();
    let Level {
        world,
        probe,
        crate_,
        rock,
    } = level;
    let mut s = EditorState::new(
        world,
        Path::new("."),
        Path::new("unused.mscene"),
        "Test",
        log.sink(),
    );
    s.can_save = false;
    (s, probe, crate_, rock)
}

/// Plays `frames` ticks, then stops.
fn play(s: &mut EditorState, frames: usize) {
    assert!(s.start_play("play.start"));
    for _ in 0..frames {
        s.play.update(TICK);
    }
    s.stop_play();
}

fn scratch() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "metallic-play-review-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// What Save writes for the level, as bytes.
fn saved(s: &EditorState) -> Vec<u8> {
    let dir = scratch();
    let path = dir.join("level.mscene");
    save_scene(&s.clean_world(), &path).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

fn tick_all(s: &mut EditorState) {
    for item in s.play_review_mut().unwrap().items_mut() {
        item.keep = true;
    }
}

fn tick(s: &mut EditorState, name: &str) {
    let mut found = false;
    for item in s.play_review_mut().unwrap().items_mut() {
        if item.name == name {
            item.keep = true;
            found = true;
        }
    }
    assert!(found, "no row for {name}");
}

fn rows(s: &EditorState) -> Vec<String> {
    s.play_review()
        .map(|r| {
            r.items()
                .map(|i| format!("{}: {}", i.name, i.summary()))
                .collect()
        })
        .unwrap_or_default()
}

fn child(world: &World, parent: InstanceId, name: &str) -> Option<InstanceId> {
    world.find_first_child(parent, name)
}

fn workspace(s: &EditorState) -> InstanceId {
    s.world.root("Workspace").unwrap()
}

const CHANGES_ALL_THREE: &str = "
local probe = workspace:FindFirstChild('Probe')
probe.Position = Vector3.new(7, 8, 9)
workspace:FindFirstChild('Crate'):Destroy()
local made = Instance.new('Model')
made.Name = 'Made'
local inner = Instance.new('Part')
inner.Name = 'Inner'
inner.Anchored = true
inner.Parent = made
made.PrimaryPart = inner
made:SetAttribute('Score', 5)
made.Parent = workspace
";

#[test]
fn stop_leaves_the_level_byte_identical_when_nothing_is_kept() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(CHANGES_ALL_THREE), &log);
    let before = saved(&s);
    play(&mut s, 3);
    assert_eq!(s.play_review().unwrap().total(), 3, "{:?}", rows(&s));
    assert_eq!(saved(&s), before, "Stop changed the level");
    assert!(!s.is_dirty());
    assert!(s.history.is_empty());
    // Ticking is not keeping.
    tick_all(&mut s);
    assert_eq!(saved(&s), before, "a tick changed the level");
}

#[test]
fn a_script_property_change_is_listed_and_keep_applies_it() {
    let log = OutputLog::new(32);
    let (mut s, probe, ..) = state(
        level("workspace:FindFirstChild('Probe').Position = Vector3.new(7, 8, 9)"),
        &log,
    );
    let start = s.world.get(probe, "Position").unwrap();
    play(&mut s, 2);
    assert_eq!(rows(&s), ["Probe: Position"]);
    assert_eq!(s.world.get(probe, "Position").unwrap(), start);
    tick(&mut s, "Probe");
    assert_eq!(s.keep_play_changes(), 1);
    assert_eq!(
        s.world.get(probe, "Position").unwrap(),
        Value::Vector3(Vec3::new(7.0, 8.0, 9.0))
    );
    assert!(s.play_review().is_none(), "Keep ends the offer");
    assert_eq!(s.current_note(), Some("Kept 1 change from Play"));
}

#[test]
fn only_the_ticked_rows_are_kept() {
    let log = OutputLog::new(32);
    let (mut s, probe, crate_, _) = state(level(CHANGES_ALL_THREE), &log);
    let start = s.world.get(probe, "Position").unwrap();
    play(&mut s, 2);
    tick(&mut s, "Crate");
    assert_eq!(s.keep_play_changes(), 1);
    assert!(!s.is_live(crate_));
    assert_eq!(s.world.get(probe, "Position").unwrap(), start);
    assert!(child(&s.world, workspace(&s), "Made").is_none());
}

#[test]
fn one_undo_reverts_everything_kept_and_redo_brings_it_back() {
    let log = OutputLog::new(32);
    let (mut s, probe, crate_, _) = state(level(CHANGES_ALL_THREE), &log);
    let before = saved(&s);
    play(&mut s, 2);
    tick_all(&mut s);
    assert_eq!(s.keep_play_changes(), 3);
    let after = saved(&s);
    assert_ne!(after, before);
    assert_eq!(s.history.len(), 1, "one undo step");
    assert_eq!(
        s.history.undo_name(&s.world).as_deref(),
        Some("Keep 3 changes from Play")
    );
    s.undo();
    assert_eq!(saved(&s), before, "one Undo put the level back");
    assert!(s.is_live(crate_), "the same Instance is back");
    assert!(child(&s.world, workspace(&s), "Made").is_none());
    s.redo();
    assert_eq!(saved(&s), after, "Redo keeps them again");
    assert!(!s.is_live(crate_));
    assert_eq!(
        s.world.get(probe, "Position").unwrap(),
        Value::Vector3(Vec3::new(7.0, 8.0, 9.0))
    );
}

#[test]
fn a_created_instance_is_kept_with_its_subtree() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(CHANGES_ALL_THREE), &log);
    play(&mut s, 2);
    assert!(
        rows(&s).contains(&"Made: New, 1 inside".to_string()),
        "{:?}",
        rows(&s)
    );
    tick(&mut s, "Made");
    assert_eq!(s.keep_play_changes(), 1);
    let made = child(&s.world, workspace(&s), "Made").expect("the new model");
    let inner = child(&s.world, made, "Inner").expect("its child");
    assert_eq!(s.world.class_name(made), Some("Model"));
    assert_eq!(s.world.get(inner, "Anchored").unwrap(), Value::Bool(true));
    assert_eq!(
        s.world.get(made, "PrimaryPart").unwrap(),
        Value::Ref(Some(inner)),
        "a reference inside the subtree points at the new child"
    );
    assert_eq!(s.world.attribute(made, "Score"), Some(&Value::Number(5.0)));
    // It survives a save and load.
    let dir = scratch();
    let path = dir.join("level.mscene");
    save_scene(&s.clean_world(), &path).unwrap();
    let loaded = metallic_core::load_scene(&path).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let ws = loaded.root("Workspace").unwrap();
    let made = child(&loaded, ws, "Made").expect("saved");
    assert!(child(&loaded, made, "Inner").is_some());
    // The subtree undoes as one with the rest.
    s.undo();
    assert!(child(&s.world, workspace(&s), "Made").is_none());
}

#[test]
fn a_destroyed_instance_can_be_kept_destroyed() {
    let log = OutputLog::new(32);
    let (mut s, _, crate_, _) = state(level("workspace:FindFirstChild('Crate'):Destroy()"), &log);
    play(&mut s, 2);
    assert_eq!(rows(&s), ["Crate: Removed, 1 inside"]);
    assert!(s.is_live(crate_), "Stop alone removes nothing");
    tick(&mut s, "Crate");
    assert_eq!(s.keep_play_changes(), 1);
    assert!(!s.is_live(crate_));
    s.undo();
    assert!(s.is_live(crate_));
    assert!(child(&s.world, crate_, "Lid").is_some());
}

#[test]
fn a_moved_instance_is_listed_and_follows_a_new_parent_only_if_that_is_kept() {
    let source = "
local folder = Instance.new('Folder')
folder.Name = 'Box'
folder.Parent = workspace
workspace:FindFirstChild('Probe').Parent = folder
workspace:FindFirstChild('Rock').Parent = workspace:FindFirstChild('Crate')
";
    let log = OutputLog::new(32);
    let (mut s, probe, crate_, rock) = state(level(source), &log);
    let ws = workspace(&s);
    play(&mut s, 2);
    let listed = rows(&s);
    assert!(listed.contains(&"Probe: Moved".to_string()), "{listed:?}");
    assert!(listed.contains(&"Rock: Moved".to_string()), "{listed:?}");
    assert!(listed.contains(&"Box: New".to_string()), "{listed:?}");
    // The Probe's new parent is not kept: it stays. The Rock moves into the Crate.
    tick(&mut s, "Probe");
    tick(&mut s, "Rock");
    s.keep_play_changes();
    assert_eq!(s.world.parent(probe), Some(ws));
    assert_eq!(s.world.parent(rock), Some(crate_));
    s.undo();
    assert_eq!(s.world.parent(rock), Some(ws));

    // Again with the new parent.
    let (mut s, probe, ..) = state(level(source), &log);
    let ws = workspace(&s);
    play(&mut s, 2);
    tick(&mut s, "Probe");
    tick(&mut s, "Box");
    s.keep_play_changes();
    let new_parent = child(&s.world, ws, "Box").expect("the new folder");
    assert_eq!(s.world.parent(probe), Some(new_parent));
}

#[test]
fn a_reference_to_a_new_instance_not_kept_ends_up_empty() {
    // The two new parts have the same ids in the play world as the edit world hands out next, so
    // keeping only the second must not make the reference land on it.
    let source = "
local first = Instance.new('Part')
first.Name = 'First'
first.Parent = workspace
local second = Instance.new('Part')
second.Name = 'Second'
second.Parent = workspace
workspace:FindFirstChild('Crate').PrimaryPart = first
";
    let log = OutputLog::new(32);
    let (mut s, _, crate_, _) = state(level(source), &log);
    play(&mut s, 2);
    tick(&mut s, "Second");
    tick(&mut s, "Crate");
    s.keep_play_changes();
    assert!(child(&s.world, workspace(&s), "Second").is_some());
    assert!(child(&s.world, workspace(&s), "First").is_none());
    assert_eq!(
        s.world.get(crate_, "PrimaryPart").unwrap(),
        Value::Ref(None)
    );
}

#[test]
fn attributes_and_tags_are_listed_and_kept() {
    let source = "
local probe = workspace:FindFirstChild('Probe')
probe:SetAttribute('Speed', 4)
game:GetService('CollectionService'):AddTag(probe, 'Hot')
";
    let log = OutputLog::new(32);
    let (mut s, probe, ..) = state(level(source), &log);
    play(&mut s, 2);
    assert_eq!(rows(&s), ["Probe: Attribute Speed, Tag Hot"]);
    tick(&mut s, "Probe");
    s.keep_play_changes();
    assert_eq!(s.world.attribute(probe, "Speed"), Some(&Value::Number(4.0)));
    assert!(s.world.has_tag(probe, "Hot"));
    s.undo();
    assert_eq!(s.world.attribute(probe, "Speed"), None);
    assert!(!s.world.has_tag(probe, "Hot"));
}

#[test]
fn character_player_and_physics_churn_is_not_listed() {
    let log = OutputLog::new(32);
    let (mut s, _, _, rock) = state(level(""), &log);
    assert!(s.start_play("play.start"));
    for _ in 0..40 {
        s.play.update(TICK);
    }
    // The game really did all of it: a character and a falling part.
    let mut seen = None;
    s.play.view().unwrap().inspect_world(&mut |w| {
        let players = w.root("Players").unwrap();
        let player = w.find_first_child(players, "Player1").unwrap();
        seen = Some((
            w.get(player, "Character").unwrap(),
            w.get(rock, "Position").unwrap(),
            w.children(w.root("Workspace").unwrap()).len(),
        ));
    });
    let (character, position, children) = seen.unwrap();
    assert!(matches!(character, Value::Ref(Some(_))));
    assert_ne!(position, s.world.get(rock, "Position").unwrap(), "it fell");
    assert!(children > s.world.children(workspace(&s)).len());
    s.stop_play();
    assert!(s.play_review().is_none(), "{:?}", rows(&s));
}

#[test]
fn the_parkour_template_offers_nothing_for_just_playing_it() {
    let log = OutputLog::new(256);
    metallic_script::use_test_budgets_on_this_thread();
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/parkour");
    let mut s = EditorState::open(&dir, log.sink()).unwrap();
    s.can_save = false;
    let before = saved(&s);
    play(&mut s, 60);
    // Scripted platforms and spinners are real changes a creator could keep; nothing else shows.
    for row in rows(&s) {
        assert!(
            row.ends_with(": Position") || row.ends_with(": Orientation"),
            "unexpected row {row}"
        );
    }
    assert_eq!(saved(&s), before);
}

#[test]
fn dismissing_keeps_nothing() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(CHANGES_ALL_THREE), &log);
    let before = saved(&s);
    play(&mut s, 2);
    tick_all(&mut s);
    s.dismiss_play_review();
    assert!(s.play_review().is_none());
    assert_eq!(s.keep_play_changes(), 0);
    assert_eq!(saved(&s), before);
    assert!(s.history.is_empty());
}

#[test]
fn starting_play_again_drops_the_offer() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(CHANGES_ALL_THREE), &log);
    play(&mut s, 2);
    assert!(s.play_review().is_some());
    assert!(s.start_play("play.start"));
    assert!(s.play_review().is_none());
    s.stop_play();
    assert!(s.play_review().is_some(), "that second run made its own");
}

#[test]
fn nothing_changed_means_no_offer() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level("print('hello')"), &log);
    play(&mut s, 2);
    assert!(s.play_review().is_none());
}

#[test]
fn the_first_edit_drops_the_offer() {
    let log = OutputLog::new(32);
    let (mut s, probe, ..) = state(level(CHANGES_ALL_THREE), &log);
    play(&mut s, 2);
    tick_all(&mut s);
    s.set_property(&[probe], "Name", Value::String("Renamed".into()));
    assert!(s.play_review().is_none());
    assert_eq!(s.keep_play_changes(), 0);
    assert_eq!(
        s.world.get(probe, "Name").unwrap(),
        Value::String("Renamed".into())
    );
    assert_eq!(s.history.len(), 1, "only the rename");
}

#[test]
fn undo_counts_as_an_edit_for_the_offer() {
    let log = OutputLog::new(32);
    let (mut s, _, _, rock) = state(level(CHANGES_ALL_THREE), &log);
    s.set_property(&[rock], "Name", Value::String("Renamed".into()));
    play(&mut s, 2);
    assert!(s.play_review().is_some());
    s.undo();
    assert!(s.play_review().is_none());
}

#[test]
fn keep_waits_for_an_open_gesture_and_the_offer_stays() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(CHANGES_ALL_THREE), &log);
    let before = saved(&s);
    play(&mut s, 2);
    tick_all(&mut s);
    s.set_gesture(Gesture::Viewport, true);
    assert_eq!(s.keep_play_changes(), 0);
    assert_eq!(saved(&s), before);
    assert!(s.play_review().is_some());
    s.set_gesture(Gesture::Viewport, false);
    assert_eq!(s.keep_play_changes(), 3);
}

#[test]
fn the_list_stops_at_the_cap_and_counts_the_rest() {
    let extra = MAX_LISTED + 40;
    let source = format!(
        "for i = 1, {extra} do
            local p = Instance.new('Part')
            p.Name = 'Spawned' .. i
            p.Anchored = true
            p.Parent = workspace
        end"
    );
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(&source), &log);
    play(&mut s, 2);
    let review = s.play_review().unwrap();
    assert_eq!(review.total(), extra);
    assert_eq!(review.items().count(), MAX_LISTED);
    assert_eq!(review.unlisted(), 40);
    assert_eq!(
        metallic_studio::play_review::offer_text(review),
        format!("Review {extra} changes from Play")
    );
    assert_eq!(
        metallic_studio::play_review::footer_text(review),
        format!("Showing the first {MAX_LISTED} of {extra} changes. The rest cannot be kept.")
    );
    // Keep takes what is listed and nothing else.
    tick_all(&mut s);
    assert_eq!(s.keep_play_changes(), MAX_LISTED);
    let parts = s
        .world
        .children(workspace(&s))
        .iter()
        .filter(|&&c| {
            matches!(s.world.get_ref(c, "Name"), Ok(Value::String(n)) if n.starts_with("Spawned"))
        })
        .count();
    assert_eq!(parts, MAX_LISTED);
}

/// A mode whose game runs elsewhere, like Team Test: no [`PlaySession::view`].
struct Elsewhere;
struct ElsewhereSession;
impl PlayMode for Elsewhere {
    fn id(&self) -> &'static str {
        "play.elsewhere"
    }
    fn label(&self) -> String {
        "Elsewhere".into()
    }
    fn start(&mut self, _: PlayStart<'_>) -> Result<Box<dyn PlaySession>, String> {
        Ok(Box::new(ElsewhereSession))
    }
}
impl PlaySession for ElsewhereSession {
    fn status(&self) -> PlayStatus {
        PlayStatus::Running("2 players".into())
    }
    fn stop(self: Box<Self>) -> Result<(), String> {
        Ok(())
    }
}

#[test]
fn a_session_in_other_processes_has_no_offer() {
    let log = OutputLog::new(32);
    let (mut s, ..) = state(level(""), &log);
    s.play.register(Box::new(Elsewhere));
    assert!(s.start_play("play.elsewhere"));
    s.stop_play();
    assert!(s.play_review().is_none());
}

fn harness(source: &str) -> Harness<'static, EditorUi> {
    metallic_script::use_test_budgets_on_this_thread();
    let output = OutputPanel::new();
    let Level { world, .. } = level(source);
    let mut state = EditorState::new(
        world,
        Path::new("."),
        Path::new("unused.mscene"),
        "Test",
        output.log.sink(),
    );
    state.can_save = false;
    // The panels use the theme's bundled fonts, installed on the first pass (drawn empty).
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1400.0, 900.0))
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
            EditorUi::new(state, output),
        );
    h.run_steps(3);
    h
}

fn play_in(h: &mut Harness<'static, EditorUi>) {
    h.key_press(egui::Key::F5);
    h.run_steps(2);
    assert!(h.state().state.play.is_playing());
}

#[test]
fn esc_stops_play_and_a_second_esc_dismisses_the_offer() {
    let mut h = harness(CHANGES_ALL_THREE);
    play_in(&mut h);
    h.key_press(egui::Key::Escape);
    h.run_steps(3);
    assert!(!h.state().state.play.is_playing());
    assert!(h.state().state.play_review().is_some(), "still offered");
    h.get_by_label("Review 3 changes from Play");
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().state.play_review().is_none());
}

#[test]
fn holding_the_esc_that_stopped_play_does_not_dismiss_the_offer() {
    let mut h = harness(CHANGES_ALL_THREE);
    play_in(&mut h);
    let key = |pressed: bool, repeat: bool| egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed,
        repeat,
        modifiers: egui::Modifiers::NONE,
    };
    h.event(key(true, false));
    h.step();
    assert!(!h.state().state.play.is_playing());
    assert!(h.state().state.play_review().is_some());
    h.event(key(true, true));
    h.step();
    h.event(key(true, true));
    h.step();
    assert!(h.state().state.play_review().is_some(), "held, not pressed");
    h.event(key(false, false));
    h.step();
    h.step();
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().state.play_review().is_none());
}

#[test]
fn esc_dismisses_the_offer_before_it_clears_the_selection() {
    let mut h = harness(CHANGES_ALL_THREE);
    let probe = {
        let s = &h.state().state;
        child(&s.world, workspace(s), "Probe").unwrap()
    };
    play_in(&mut h);
    h.state_mut().state.stop_play();
    h.run_steps(2);
    h.state_mut().state.selection.select_only(probe);
    h.run_steps(1);
    h.hover_at(h.state().viewport.rect().center());
    h.run_steps(1);
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().state.play_review().is_none());
    assert_eq!(h.state().state.selection.active(), Some(probe));
    h.key_press(egui::Key::Escape);
    h.run_steps(2);
    assert!(h.state().state.selection.is_empty(), "the next Esc does");
}

#[test]
fn the_list_opens_a_row_is_ticked_and_keep_applies_it() {
    let mut h = harness("workspace:FindFirstChild('Probe').Position = Vector3.new(7, 8, 9)");
    play_in(&mut h);
    h.state_mut().state.stop_play();
    h.run_steps(2);
    let probe = {
        let s = &h.state().state;
        child(&s.world, workspace(s), "Probe").unwrap()
    };
    h.get_by_label("Review").click();
    h.run_steps(2);
    assert!(h.state().state.play_review().unwrap().open);
    h.get_by_label("Probe: Position");
    h.get_by_label("Keep").click();
    h.run_steps(2);
    assert_eq!(h.state().state.play_review().unwrap().picked(), 1);
    h.get_by_label("Keep 1 change").click();
    h.run_steps(2);
    let s = &h.state().state;
    assert!(s.play_review().is_none());
    assert_eq!(
        s.world.get(probe, "Position").unwrap(),
        Value::Vector3(Vec3::new(7.0, 8.0, 9.0))
    );
    assert_eq!(s.history.len(), 1);
}

#[test]
fn a_row_click_selects_the_instance_and_keep_all_ticks_everything() {
    let mut h = harness(CHANGES_ALL_THREE);
    play_in(&mut h);
    h.state_mut().state.stop_play();
    h.run_steps(2);
    h.get_by_label("Review").click();
    h.run_steps(2);
    h.get_by_label("Probe: Position").click();
    h.run_steps(2);
    let probe = {
        let s = &h.state().state;
        child(&s.world, workspace(s), "Probe").unwrap()
    };
    assert_eq!(h.state().state.selection.active(), Some(probe));
    assert!(
        h.state().state.play_review().is_some(),
        "selecting is no edit"
    );
    // A real pointer leaves the row over some frames, and the row's tooltip with it.
    let keep_all = h.get_by_label("Keep all").rect().center();
    h.hover_at(keep_all);
    h.run_steps(2);
    h.get_by_label("Keep all").click();
    h.run_steps(2);
    assert_eq!(h.state().state.play_review().unwrap().picked(), 3);
    h.get_by_label("Keep none").click();
    h.run_steps(2);
    assert_eq!(h.state().state.play_review().unwrap().picked(), 0);
}

#[test]
fn the_dismiss_button_ends_the_offer() {
    let mut h = harness(CHANGES_ALL_THREE);
    play_in(&mut h);
    h.state_mut().state.stop_play();
    h.run_steps(2);
    h.get_by_label("Dismiss").click();
    h.run_steps(2);
    assert!(h.state().state.play_review().is_none());
}

#[test]
fn the_actions_reach_the_offer_only_while_there_is_one() {
    let mut h = harness(CHANGES_ALL_THREE);
    let enabled = |h: &Harness<'static, EditorUi>, id: &str| {
        let ui = h.state();
        ui.actions.get(id).unwrap().is_enabled(&ui.state)
    };
    assert!(!enabled(&h, "play.review"));
    assert!(!enabled(&h, "play.review_dismiss"));
    play_in(&mut h);
    assert!(!enabled(&h, "play.review"));
    h.state_mut().state.stop_play();
    h.run_steps(1);
    assert!(enabled(&h, "play.review"));
    h.state_mut().run_action("play.review");
    assert!(h.state().state.play_review().unwrap().open);
    h.run_steps(1);
    h.state_mut().run_action("play.review_dismiss");
    assert!(h.state().state.play_review().is_none());
    assert!(!enabled(&h, "play.review"));
}

/// Saves the whole frame as `<dir>/<name>.png`.
fn picture(h: &mut Harness<'static, EditorUi>, dir: &Path, name: &str) {
    let frame = h.render().unwrap();
    let (w, height) = (frame.width() as usize, frame.height() as usize);
    let pixels = frame
        .pixels()
        .map(|p| {
            let [r, g, b, a] = p.0;
            egui::Color32::from_rgba_premultiplied(r, g, b, a)
        })
        .collect();
    let image = egui::ColorImage::new([w, height], pixels);
    metallic_studio::screenshot::save_png(&dir.join(format!("{name}.png")), &image).unwrap();
}

/// Pictures of the offer, with no window (`docs/agents/130-keep-play-edits/`). A relative
/// `METALLIC_PICTURES_DIR` is relative to this crate, so pass a full path.
///
/// ```text
/// METALLIC_PICTURES_DIR=/full/path cargo test -p metallic_studio --test play_review -j 4 -- --ignored capture_the_offer
/// ```
#[test]
#[ignore = "writes pictures; needs METALLIC_PICTURES_DIR"]
fn capture_the_offer() {
    metallic_script::use_test_budgets_on_this_thread();
    let dir = PathBuf::from(std::env::var("METALLIC_PICTURES_DIR").unwrap());
    let template = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/parkour");
    let mut ui = EditorUi::demo(&template).unwrap();
    ui.state.selection.clear();
    ui.hints = metallic_studio::hints::Hints::in_memory();
    let scripts = ui.state.world.root("ServerScriptService").unwrap();
    let script = named(&mut ui.state.world, "Script", scripts, "Decorate");
    let source = "
local course = workspace:FindFirstChild('Course')
local parts = course:GetChildren()
parts[3].Position = parts[3].Position + Vector3.new(0, 2, 0)
parts[3].Color = Color3.fromRGB(150, 160, 175)
parts[4].Size = parts[4].Size + Vector3.new(2, 0, 2)
parts[5]:Destroy()
local gate = Instance.new('Model')
gate.Name = 'Gate'
for i = 1, 3 do
    local post = Instance.new('Part')
    post.Name = 'Post' .. i
    post.Anchored = true
    post.Size = Vector3.new(1, 8, 1)
    post.Position = Vector3.new(i * 6, 4, -12)
    post.Parent = gate
end
gate.Parent = workspace
";
    ui.state
        .world
        .set(script, "Source", Value::String(source.into()))
        .unwrap();
    let mut themed = false;
    let mut h = Harness::builder()
        .with_size(egui::vec2(1440.0, 900.0))
        .wgpu()
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
    assert!(h.state_mut().state.start_play("play.start"));
    for _ in 0..30 {
        h.state_mut().state.play.update(TICK);
    }
    h.state_mut().state.stop_play();
    h.run_steps(3);
    picture(&mut h, &dir, "offer");

    h.get_by_label("Review").click();
    h.run_steps(3);
    let names: Vec<String> = h
        .state()
        .state
        .play_review()
        .unwrap()
        .items()
        .map(|i| i.name.clone())
        .collect();
    println!("rows: {names:?}");
    for item in h.state_mut().state.play_review_mut().unwrap().items_mut() {
        item.keep = item.name == "Gate" || matches!(item.kind, ChangeKind::Removed { .. });
    }
    h.hover_at(egui::pos2(720.0, 200.0));
    h.run_steps(3);
    picture(&mut h, &dir, "list-open");

    let label = h
        .state()
        .state
        .play_review()
        .map(|r| format!("Keep {} changes", r.picked()))
        .unwrap();
    h.get_by_label(&label).click();
    h.run_steps(3);
    picture(&mut h, &dir, "after-keep");

    // A game that makes thousands of things: the list is cut and says so.
    let many = "for i = 1, 3214 do
        local p = Instance.new('Part')
        p.Name = 'Spawned' .. i
        p.Anchored = true
        p.Parent = workspace
    end";
    let world = &mut h.state_mut().state.world;
    let script = world
        .find_first_child(world.root("ServerScriptService").unwrap(), "Decorate")
        .unwrap();
    world
        .set(script, "Source", Value::String(many.into()))
        .unwrap();
    assert!(h.state_mut().state.start_play("play.start"));
    h.state_mut().state.play.update(TICK);
    h.state_mut().state.stop_play();
    h.state_mut().state.toggle_play_review();
    h.run_steps(3);
    picture(&mut h, &dir, "list-capped");
}

/// How long the diff takes, for the progress note.
///
/// ```text
/// cargo test -p metallic_studio --release --test play_review -j 4 -- --ignored --nocapture
/// ```
#[test]
#[ignore = "prints a timing"]
fn the_diff_at_stop_is_quick() {
    for parts in [5_000usize, 20_000, 100_000] {
        let mut edit = World::new_with_services();
        let ws = edit.root("Workspace").unwrap();
        for i in 0..parts {
            let p = edit.create("Part", Some(ws)).unwrap();
            edit.set(p, "Name", Value::String(format!("P{i}"))).unwrap();
            edit.set(p, "Anchored", Value::Bool(i % 2 == 0)).unwrap();
        }
        let mut play = edit.deep_clone();
        let ids: Vec<InstanceId> = play.children(ws).to_vec();
        for (i, id) in ids.iter().enumerate() {
            let at = Vec3::new(i as f64, 1.0, 2.0);
            play.set(*id, "Position", Value::Vector3(at)).unwrap();
        }
        let roots = edit.roots().to_vec();
        let start = std::time::Instant::now();
        let group = compare(&edit, &roots, &play);
        println!(
            "{parts} parts, half moved: {:?}, {} listed, {} past the cap",
            start.elapsed(),
            group.items.len(),
            group.unlisted
        );
        assert!(matches!(group.items[0].kind, ChangeKind::Changed { .. }));
    }
}

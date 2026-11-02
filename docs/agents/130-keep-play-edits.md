# #130 Keep edits from Play

Branch `studio/130-keep-play-edits`. Spec: Stop section of `docs/research/2026-10-30-studio-ui-design.md` (5.7);
`docs/design/building-tools.md` 13.1 (#229 adds a group to this list later).

## Goal

Stop returns to the edited level unchanged, at once, with no prompt or dialog. After Stop, Studio shows a non-blocking
offer, "Review N changes from Play" (only when there are changes), listing changed Instances with Keep per row. Nothing
is kept unless the creator chooses it. Kept changes are one undo step. Dismissing or starting Play again drops the offer.

## Done

- `play/review.rs` (no UI): `compare`, `ChangeGroup`, `ChangeItem`, `ChangeKind`, `PlayReview`, `PlayReview::keep`.
- `editor.rs`: `stop_play` makes the offer; `play_review`, `play_review_mut`, `sync_play_review`, `toggle_play_review`,
  `dismiss_play_review`, `keep_play_changes`. `start_play` drops it.
- `play_review.rs` (UI): the strip and list. `editor_ui.rs` puts it in a bottom panel above Output and handles Esc
  (`play_review_keys`). `app.rs` tells the editor when a dialog is over it (`EditorUi::dialog_open`).
- `actions.rs`: `play.review`, `play.review_dismiss` (hidden from the toolbar, listed in the palette, off without an offer).
- ARCHITECTURE.md, crate and module docs.
- Tests: `tests/play_review.rs`, unit tests in `play/review.rs` and `play_review.rs`.
- Pictures: `docs/agents/130-keep-play-edits/` (`offer`, `list-open`, `after-keep`), from the ignored test
  `capture_the_offer`. No window, so no GPU view in the viewport; a real `--screenshot` would need a new app scene.

## Decisions

- **Matching.** Same id in both worlds is the same Instance: `deep_clone` keeps ids, ids are generational, and the edit
  world is locked during Play. Only in the play world means made during Play. No table.
  The two worlds share a free list, so the id an unkept new Instance has in the play world can equal one the edit world
  hands out while keeping another: `Runner::resolve` checks for that
  (`a_reference_to_a_new_instance_not_kept_ends_up_empty`).
- **Diff at Stop**, not per frame. Play has no new code on its path. Debug build: 32 ms at 5,000 parts, 128 ms at 20,000,
  660 ms at 100,000, half of them changed (`the_diff_at_stop_is_quick`, ignored). It runs once, on Stop.
- **Left out of the list:** everything under Players; each `Player.Character`; Cameras (the game writes the camera every
  frame, and makes one if there is none); `Position` and `Orientation` of a part not anchored in both worlds; a
  Checkpoint's `Color` (the engine paints one when reached); a `Ref` emptied by its target going; a `Ref` into runtime
  objects; sibling order; new Instances left detached. Hidden properties make no row, but travel with a row that shows.
- **Rows with a scripted anchored part are listed**, as in the spec's "MovingPlatform: Position": the Parkour template's
  platforms and spinner show after any run, and a creator just leaves them unticked. A script moving an unanchored part
  looks like physics and is not listed.
- **Cap:** 500 rows and 20,000 created Instances per group. The rest are counted in N; the footer says "Showing the first
  500 of 3,214 changes. The rest cannot be kept."
- **The list holds owned values**, not the play world, so the game's memory goes at Stop.
- **Keep:** boxes per row, then one "Keep N changes" button, because kept changes must be one undo step. The offer ends
  on Keep; rows left unticked are gone. "Keep all" / "Keep none" per group.
- **An edit after Stop drops the offer** (any change of `History::revision`, Undo and Redo included), with a status note.
  Simple, and values taken against the old level can never overwrite newer work. Selecting and saving are not edits.
- **A part the world refuses** (for example a move that would make a cycle because only one of two moves was kept) is left
  out with a warning in Output; the rest still apply.
- **A move into a new Instance that was not kept** leaves the Instance where it is. A `Ref` to one ends up empty.
- **Esc** dismisses, ahead of clearing the selection, after a drag, popover, palette, menu, hint card or text field had it.
  The press that stopped Play does not count while held. Not while a dialog (Settings, the save prompt) is open.
- **A session that ends on its own** (`PlayStatus::Ended`) has no offer: `PlayController::update` stops it without the edit
  world at hand. `LocalPlaySession` does not end on its own today. Close game and quit stop Play the usual way.
- **Rows select** the Instance on click (not a new one); the tooltip shows before and after.

## Tests

`crates/metallic_studio/tests/play_review.rs`, among them: `stop_leaves_the_level_byte_identical_when_nothing_is_kept`
(saves before and after and compares), `a_script_property_change_is_listed_and_keep_applies_it`,
`one_undo_reverts_everything_kept_and_redo_brings_it_back`, `a_created_instance_is_kept_with_its_subtree`,
`a_destroyed_instance_can_be_kept_destroyed`, `character_player_and_physics_churn_is_not_listed`,
`the_parkour_template_offers_nothing_for_just_playing_it`, `dismissing_keeps_nothing`,
`the_list_stops_at_the_cap_and_counts_the_rest`, `a_session_in_other_processes_has_no_offer`, kittest for the list, Esc and
the actions.

## Commands

```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_studio -j 4 --test play_review
METALLIC_PICTURES_DIR=/full/path/docs/agents/130-keep-play-edits cargo test -p metallic_studio -j 4 --test play_review -- --ignored capture_the_offer
cargo test -p metallic_studio -j 4 --test play_review -- --ignored the_diff_at_stop --nocapture
```

## Next

PR #501 touches `play/local.rs` and `properties.rs`; rebase if it lands. #229 adds a `ChangeGroup` to
`PlayReview::groups`; the list already draws several.

## Last check results (base `20d79fa`)

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` cannot compile on this base, with or without this
  branch: `crates/metallic_script/tests/enums.rs:269` uses `common::TEST_RESUME_BUDGET`, which is not in `tests/common`
  (#510 merged next to #513). Not fixed here. Everything else was run with that one target left out.
- Tests: 2154 passed, 1 failed, 9 ignored (workspace without `metallic_script`); 327 passed, 0 failed, 3 ignored
  (`metallic_script` without `enums`). The failure is `metallic_render --test shading
  gpu_timing_measures_a_frame_where_supported` (#436), which failed again alone.
- Clippy with the same target left out, `cargo fmt --check` and `cargo deny check licenses`: clean.

## Noticed, not fixed

- 3,214 unanchored Parts created at the same spot by a script took the test process past 9 GB and did not finish one
  update (found while making `list-capped.png`; killed, not investigated). The picture now anchors them.

## Open questions

None for the owner: the decision that Stop discards at once and the offer is non-blocking is on the issue.

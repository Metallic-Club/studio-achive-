# #218 / #220: the last gaps in selection and duplicate

Branch `studio/218-220-select-duplicate`. Spec: `docs/design/building-tools.md` sections 8.1, 8.2 and 10.1.

## Goal
Finish what was left of #218 and #220 after the cores (#217-#220) and the editor wiring (#319) merged.

## Done
- **Box select (#220)**: `viewport.rs` keeps the press; on release `editor_tools::select::finish_box` runs `box_hits`
  (fully inside, or touching with Alt at release) over the same `screen_items` as the lasso, then `combine` with
  `SelectOp::from_modifiers` (Shift adds, Ctrl removes). Clicks match: Shift-click adds, Shift on nothing keeps the
  selection, Ctrl-click still toggles. `PickCache::filter_centres` had no other caller and is gone.
- **Same Tags (#220)**: `part_traits` reads `World::tags`.
- **Smart duplicate (#218)**: `smart_duplicate.rs`. `EditorState::duplicate_selection` leaves a `SmartDuplicate` (the
  step's serial, the selection revision, each copied part with its source's position and orientation). The next Ctrl+D
  checks it: same selection revision, and every undo step newer than the duplicate `Command::only_moves` the copies
  (`History::steps_after`). Then the motion source-to-copy (offset and rotation, per part, Model pivots too) places the
  next copy, as one `Batch` named like any duplicate. `EditorState::last_duplicate_offset` feeds
  `default_linear_offset`; the Array popover refreshes its default when that offset changes.
- **Array wheel (#218)**: the plain wheel over the linear handle changes the count; `EditorTools::captures_wheel` tells
  the viewport not to dolly (`Viewport::capture_wheel`, set each frame like `capture_pointer`). After a notch the wheel
  stays with the array until the pointer moves 3 px (`wheel_latch`), because each notch moves the ring off the pointer.
  Ctrl+wheel anywhere still works; while flying the wheel stays the camera speed.
- Tests: `crates/metallic_studio/tests/editor_wiring.rs`, the section after `#218 and #220`.

## Next
Nothing left in the four items. Calls and gaps for the owner:
- Alt+drag "Duplicate and move" does not start a smart-duplicate chain (spec 8.1 speaks of Ctrl+D only).
- A Shift-drag that starts on a part stays a precision body drag, so Shift+box needs empty space to start from.
- `Locked` is not a `Part` property yet, so nothing is locked for the box or the lasso; `ScreenItem.locked` waits for it.

## Commands
```
cargo test -p metallic_studio -j 4 --test editor_wiring
cargo clippy -p metallic_studio --all-targets -j 4 -- -D warnings
```

## Open questions
- Same Tags still means the exact tag set (see `219-220-scatter-selection.md`).

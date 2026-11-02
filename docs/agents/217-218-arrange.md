# #217 / #218: arrange operations core

Goal: align, distribute, mirror and arrays for the building tools (spec `docs/design/building-tools.md` sections
6.1, 6.2, 7, 8.2), split so the maths lands before Studio editing (#10) exists.

## Done (branch `studio/217-218-align-array`, draft PR "Arrange ops core (#217 #218, UI after #10)")
- `crates/metallic_studio/src/tools/arrange/`: pure planning, no egui, no Studio state. `read_objects(&World, ids)`
  (last id = active object) builds `Object`s from the shared `tools::geom::PartSnapshot` (#303); every operation
  takes those objects and returns a `Plan`: `PropertyChange { id, property, before,
  after }` for existing parts, `PlannedCopy { source, parent, parts }` for new copies, optional `PlannedGroup`.
  - `align`: Min/Centre/Max per axis, relative to the active object, selection bounds or a grid; World or Local.
  - `distribute`: equal centres or equal gaps, keep span or a fixed spacing; 3 or more objects.
  - `mirror`: in place or copy, plane through the selection pivot, the active pivot or a point. Reflect, then flip
    one local axis back (Wedge: X; others: the axis nearest the normal) so the rotation stays proper.
  - Arrays: `linear` (Distance/Gap), `radial` (Count counts the original; `/7` = 7 items), `grid`, `along_path`
    (centripetal Catmull-Rom, equal arc length, optional alignment to the tangent). Drag handle and wheel count
    maths, `5x` and `/7` parsing, default offsets.
- Review fixes: every numeric input must be finite (NaN, infinity and overflow to infinity are rejected); arrays cap
  created parts (copies x parts) at `MAX_CREATED_PARTS` = 10,000; drag, wheel and path-station counts use clamped,
  saturating maths.
- Models (and Folders) move as one rigid unit; a part inside a selected Model is not moved twice. A Model's pivot
  is its bounding-box centre (the engine has no `WorldPivot` yet).
- Unit tests in each file (maths: rotated bounding boxes, spacing, mirrored rotated wedge corners, /7 angles, path
  spacing within 1% measured by independent integration). No `metallic_core` changes.

## Next (after #10 and #213 land)
- (History note: this list is from before #10. The wiring landed in #319; smart duplicate and the wheel over the handle
  in `docs/agents/218-220-select-duplicate.md`.)
- Commit a `Plan` as one `History` step (batched `SetProperties` plus clone/create); undo tests there.
- Live preview from the plan (moved parts in the overlay, copies as 60% ghosts); Esc clears it.
- Popovers: Align (A), Mirror (Ctrl+M / Ctrl+Shift+M), Array (Ctrl+Shift+D) with the drag handle.
- Still open from #217: resize to fill and gap fill (need face/edge picking). From #218: smart duplicate (needs the
  selection and edit tracking from #10).

## Commands
```
cargo test -p metallic_studio -j 2 --lib tools::arrange   # 45 tests
cargo clippy -p metallic_studio --all-targets -j 2 -- -D warnings
```

## Open questions
- Radial Count includes the original (pie slices); Linear and path Count are new copies. Owner may prefer one rule.

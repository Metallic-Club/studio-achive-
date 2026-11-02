# #481 Properties labels centred on their controls

Branch `studio/481-properties-label-align`. Goal: Properties row labels share the centre line of the control
beside them (single fields, vector rows, PivotOffset's group, dropdowns). They sat about 4 px high.

## Done
- **Cause.** `ui.horizontal` starts `interact_size.y` tall (`ROW_HEIGHT`, 24 px here) and a taller child grows the
  row downwards only (egui `Layout::next_frame_ignore_wrap`). The label comes first, so it was centred in the 24 px
  row (centre 12) while the 32 px control after it took the grown row (centre 16). Text after the control, such as
  the Color row's "214, 178, 126", was already right.
- **Fix** (`properties.rs`). `row` starts at `number::height(ui)` instead of `ROW_HEIGHT`, so the label's cell is
  centred in the control's height from the start. The attribute rows get the same line.
- **Dropdowns.** egui sizes a ComboBox by its arrow's cell, `icon_width`, 18 px in the theme against 16.1 px of
  text, so a dropdown was 34 px and its row's centre 1 px lower than the label's. The dropdown arm now sets
  `icon_width` to the body text height, so the box is 32 px like the fields; the arrow is about 2 px smaller.
- **Effects to expect.** Checkbox rows (Anchored, CanCollide) and other short rows are now 32 px, not 24 px, so
  every row in the column has one height. Dropdown rows are 2 px shorter. For a Part the content above Attributes is 13 px taller.
- **PivotOffset (a call).** The label centres on the group's first row ("Position"), not between the two rows: it
  reads as the row's label and lines up with the other rows' labels.
- **Test.** `properties_labels_are_centred_on_their_controls` (`tests/editor_ui.rs`) compares label and control
  centres through the accessibility tree, with the real theme, within 0.5 px: Name, PivotOffset, Material,
  Transparency, Shape, Style, Orientation, Size, Anchored and a number attribute. Before the fix it failed with
  `Name: label centre 237, control centre 241.0625`.
- **Screenshots** (`--scene editor`, 1440x900, crop x 1036, y 100, 404x800, Jump1 selected) in
  `docs/agents/481-properties-label-align/`: `properties-part-before.png`, `properties-part-after.png`. Looked at
  both, and at 3x crops of the Color to Style rows.

## Checks (2026-11-01, on the code in this commit)
- `cargo fmt --all -- --check`: clean. `cargo deny check licenses`: `licenses ok`.
- `cargo clippy --workspace --all-targets -j 4 -- -D warnings`: clean.
- `cargo test --workspace -j 4 --no-fail-fast`, run twice, never fully green; the failures differ between runs.
  - Run 1: 2263 passed, 3 failed: `gpu_timing_measures_a_frame_where_supported`,
    `combined_output_preserves_server_client_and_error_text_without_false_attribution`,
    `script_errors_go_to_output_with_the_play_tag`.
  - Run 2: 2261 passed, 5 failed: `gpu_timing_measures_a_frame_where_supported`,
    `parkour_smoke_scene_passes_headless`, `set_title_while_held_keeps_the_press`,
    `a_touch_press_fires_the_input_action`, `get_button_returns_the_overlay_button_of_a_bound_action`.
  - Rerun alone: all pass (the targets `local_play`, `play`, `smoke` and `touch_controls`) except the GPU test, the
    known #436. Run 2's touch failure reads `Script timeout: exhausted allowed execution time`, so this looks like
    load (another agent was building), but that cause is not proven for each test.

## Next
- Nothing for the issue. Not pushed; the PR is the conductor's batch.

## Commands
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_studio --test editor_ui -j 4 properties_labels
METALLIC_TEMPLATES_DIR=templates target/debug/metallic-studio --screenshot out.png --scene editor --size 1440x900
```

## Open questions
- None blocking. The two calls above (32 px short rows, PivotOffset on its first row) are easy to change.
- Not fixed, outside this issue: the "New attribute" row's dropdown is still 34 px beside a 32 px text box, and the
  comment "Inputs in the property column are 24 px rows (spec 4.3)" in `Properties::ui` does not match the 32 px
  controls.

# #497 load-flaky tests

Goal: full workspace test runs under load fail only #436 (`gpu_timing_measures_a_frame_where_supported`).

## Findings so far
- CPU load alone (24 spinning threads) rarely fails the listed tests. Memory pressure does: a 13 GB
  page-touching process on this 16 GB PC plus parallel test binaries reproduces
  `Script timeout` on trivial scripts (`ray_is_a_datatype`, `play_does_not_change_edit_world`).
- Mechanism: `metallic_script::use_test_budgets_on_this_thread` (rigs),
  `use_test_budgets_if_a_test_asked` (child-process binaries, debug builds only).
- `a_click_applies_one_undo_step_to_the_model` was a real bug, fixed in `style_picker.rs`.

## Next
Load runs, remaining wall-clock tests, docs. See the final section when it exists.

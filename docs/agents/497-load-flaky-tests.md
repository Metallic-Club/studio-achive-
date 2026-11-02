# #497 load-flaky tests

Goal: full workspace test runs under load fail only #436 (`gpu_timing_measures_a_frame_where_supported`).

## Status
Implemented and tested without load. **The three acceptance load runs are not done**: the conductor runs them
when no other agent is building, because the load below disturbs other sessions' builds and perf timing.

## Causes
- Trigger: memory pressure more than CPU. 24 spinning threads rarely fail the listed tests. A 13 GB page-touching
  process on this 16 GB PC with 12 spinning threads and parallel test binaries times out trivial scripts.
- Resume budget (0.1 s): `ray_is_a_datatype`, `the_camera_is_rotated_by_its_cframe`,
  `play_does_not_change_edit_world`, `parkour_smoke_scene_passes_headless`, `memory_cap_stops_script`,
  `one_thousand_ordinary_coroutines_...` and read_probe lib tests all logged `Script timeout`.
- Step budget (0.25 s): `a_backlog_beyond_the_log_...` printed `seen 0`; 90 budgeted steps did not reach the event.
- Real bug: `a_click_applies_one_undo_step_to_the_model`. After a click with the pointer still on the tile, an
  empty preview opened a gesture and Undo did nothing. Fixed in `style_picker.rs`, with a regression test.
- Real-time checks: palette, snapping, QUIC and the upper bounds in the limit tests.

## Mechanism
- `metallic_script::use_test_budgets_on_this_thread()` in rigs: per-thread default read when a VM or host is made.
- `use_test_budgets_if_a_test_asked()` in the `main` of metallic-player and metallic-sim: `METALLIC_TEST_BUDGETS`,
  read in debug builds only.
- `cfg!(test)` for the lib tests of metallic_script.
- Limit tests set the real values back and repeat up to five times (`tests/common/mod.rs`, `real_limits_test`).

## Known remaining under memory pressure (seen in the last partial stress, 12 binaries at once)
- `quic_stalled_reader_is_closed`, `quic_second_remote_stream_closes`, `quic_second_control_stream_closes`,
  `quic_bad_stream_tag_closes`: `no connection accepted within 5s` (`WAIT` in metallic_net/tests/transport.rs).
- `quic_hello_must_arrive_within_2_s`: `QuicClient::connect` returned `Timeout`, before the retry loop can help.
- `palette::tests::a_keystroke_...`: all 40 runs over the 40 ms debug budget (best about 42-83 ms).
Not fixed. Next step: longer `WAIT` where it only detects a failure; decide what the palette bound should be.

## Acceptance load run (only when no other agent builds or times anything)
Build `spin.rs` and `hog.rs` (scratchpad of the session that wrote them) or any equivalent: 12 threads walking
8 MB buffers, and a process touching random pages of 13 GB. Keep them running, then, from this worktree:
`export PATH="$HOME/.cargo/bin:$PATH"; cargo test --workspace --no-fail-fast -j 4 > run-N.log 2>&1`
Totals: `grep -E '^test result:' run-N.log`; failures: `grep -E '\.\.\. FAILED' run-N.log`. Repeat three times.
Stop the generators afterwards. Earlier run with 24 spinning threads alone took 42 minutes.

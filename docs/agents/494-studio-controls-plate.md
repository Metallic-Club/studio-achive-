# #494: The controls plate in Studio Play

Branch `studio/494-studio-controls-plate`. Follows #480.

## Goal
Studio's Play (F5) shows the Player's key-cap controls plate (Move, Jump, Run, Camera, Menu) at the start and after a
glyph change, from one shared widget. #480 removed Parkour's own text hint, so Studio Play had no controls row.

## Where the code lives, and why
`metallic_runtime::input`, behind the existing `presentation` feature:
- `input/controls_hint.rs` (was `metallic_player/src/menu/hud.rs`): `ControlsHint`.
- `input/glyph_paint.rs` (was `metallic_player/src/menu/glyph_paint.rs`): the glyph art; the Player menu imports it too.

Both apps already depend on the runtime, which owns `InputRuntime` and the glyph data and already paints ScreenGuis with
an egui painter (`gui.rs`). The Player paints this plate with egui too, so painting is shared along with data and layout,
and no per-app painter is needed. Rejected: Studio depending on `metallic_player` (a binary crate), and
`metallic_ui_theme` (it would have to depend on the runtime, and it is meant to stay free of game code).

Colours come from `metallic_tokens` instead of the Player's `menu/theme.rs`, so the runtime never depends on an app:
`metallic_tokens` went from a dev-dependency to an optional dependency enabled by `presentation`. `--no-default-features`
builds stay free of it.

## Done
- `ControlsHint` split into `update(&InputRuntime, now)` (rows, timing, touch) and `paint(&Painter, now)` (only draws, at
  the bottom left of the painter's clip rectangle), plus `rows()` and `opacity(now)`. Painting through `&self` fits
  `PlayView::paint_gui`.
- A narrow area shrinks the plate (down to half size, then the clip cuts it). Studio's viewport can be narrower than the
  plate, which needs about 600 points with margins in Studio's fonts. The Player is unaffected at normal window sizes.
- Player: `window.rs::paint_ui` makes the Foreground layer painter it always used and calls `update` and `paint`, only
  while the menu is closed, as before.
- Studio: `LocalPlaySession` owns a hint: `update` after each game frame on the session's input clock, `paint` in
  `paint_gui` after the ScreenGuis. The painter is `painter_at(viewport)`, so it stays inside the viewport. No widget is
  allocated, so the viewport keeps the pointer and the game keeps the keyboard.
- Tests:
  - `metallic_runtime` `input::controls_hint::tests`: the two moved tests, timing and fade, touch, painting inside the
    clip and shrinking.
  - `metallic_studio` `play::local::tests`: `controls_plate_follows_the_session_clock_and_glyph_changes` (start, gone
    after 8 s, back after a glyph change, hidden after Stop), `parkour_rows_are_the_players_rows`.
  - `metallic_studio/tests/play.rs`: `play_draws_the_controls_plate_in_the_viewport` (through the editor with F5: none
    while editing, clip equals the viewport, pointer over the plate is still over the game, gone after 8 s and after
    Stop).
- Screenshots in `494-studio-controls-plate/`: `before-studio.png`, `after-studio.png`, `after-studio-narrow.png`,
  `after-player.png`, `after-player-corner.png`.

## Next
Nothing for this issue. See open questions.

## Commands
```
export PATH="$HOME/.cargo/bin:$PATH"
cargo test -p metallic_runtime --lib -j 4 controls_hint
cargo test -p metallic_studio --lib -j 4 play::
cargo test -p metallic_studio --test play -j 4
cargo run -p metallic_studio -j 4 -- --screenshot shot.png --scene playing --size 1280x800
cargo run -p metallic_player -j 4 -- templates/parkour --no-overrides --capture 150:shot.png --frames 160
```

## Open questions
- The plate says "Esc Menu" in Studio Play, but Studio keeps Esc: it stops the session and never reaches the game. The
  rows are the Player's on purpose (the issue asks for the same plate); whether Studio should word that row differently
  is a product call.
- Studio Play never loads the Player's saved rebinds, so its plate shows the scene's bindings, which are also what Studio
  Play really uses.
- `menu/theme.rs` `LINE_CONTROL` (`#6e6a79`) is one step off the token (`#6f6b7a`). The shared glyphs now use the token,
  so keycap outlines in the Player change by that one step; the rest of the menu still uses the old value.

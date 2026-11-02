//! `metallic-player`: plays a Metallic project.
//!
//! ```text
//! metallic-player <project dir> [--scene <file relative to project>] [--headless --frames N]
//!                 [--quality low|high] [--fps] [--autopilot] [--frames N]
//!                 [--capture FRAME:PNG]... [--input-script <file>]
//!                 [--no-overrides] [--menu main|keyboard|gamepad|assist|touch]
//!                 [--emulate phone|tablet|small-phone[-portrait]] [--reduce-motion] [--minimised]
//! ```
//! Windowed by default. Input is read as actions (`metallic_runtime::input`); the default
//! `CharacterControls`: WASD/arrows or the d-pad move, Space or A jumps, Shift or L3 runs,
//! right-drag or the right stick turns the camera, the wheel or R1/L1 zoom; the left stick
//! moves; Esc (or Start) opens the Player menu: Resume, Controls (rebinding, hold or toggle,
//! stick and mouse settings; saved per game in the Player's data folder) and Leave game.
//! `--no-overrides` ignores the saved controls; `--menu main|keyboard|gamepad|assist|touch` starts with
//! the menu or a Controls tab open (for screenshots). `--headless` runs N frames of 1/60 s without a window or GPU (smoke tests).
//! `--input-script <file>` plays input events (`frame device control state [values]`, for
//! example `30 keyboard Space down`) in either mode, with 1/60 s of input time per frame. `--fps` plays 60 s in a fixed 1920x1080 window without
//! vsync and prints frame-rate statistics; with `--autopilot` the character runs in a circle and
//! jumps meanwhile, so the measurement includes the animated avatar. `--frames N` also ends a windowed run after N frames,
//! and `--capture FRAME:PNG` saves that frame (3D view and GUI) as a PNG.
//! `--emulate phone` plays as a touch device (docs/design/input-actions.md section 10.2): the
//! window takes the device's shape, the automatic touch controls show, the left mouse button is a
//! finger and a right click pins or lifts a second finger. On a real touch screen the overlay
//! fades in at the first touch. Input scripts drive fingers with lines like
//! `90 touch0 Down pos 140 300` (dp); headless runs with `--emulate` route them the same way.
//! `--reduce-motion` shows and hides the overlay without its 140 ms fade. `--minimised` starts with
//! the window minimised (to check CPU use while minimised; see `pacing.rs`).
//! Exit code: 0 on a clean exit, 1 when the project cannot be loaded or started, 2 for bad
//! arguments.
#![warn(missing_docs)]

mod args;
mod capture;
mod controls_store;
mod data_dir;
mod devices;
mod menu;
mod overlay;
mod pacing;
mod touch_overlay;
mod window;

use metallic_runtime::input::touch::{DeviceProfile, TouchControls};
use metallic_runtime::input::{DeviceKind, ScriptedSource};
use metallic_runtime::{Game, InputRuntime, StdoutSink, TICK};
use std::process::ExitCode;
use std::rc::Rc;

fn main() -> ExitCode {
    metallic_runtime::use_test_budgets_if_a_test_asked();
    let args = match args::Args::parse(std::env::args().skip(1)) {
        Ok(a) => a,
        Err(msg) => {
            eprintln!("metallic-player: {msg}\n\n{}", args::USAGE);
            return ExitCode::from(2);
        }
    };
    let mut game = match Game::open(&args.project, args.scene.as_deref(), Rc::new(StdoutSink)) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = game.start() {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    let script = match args.input_script.as_deref().map(load_script) {
        Some(Ok(s)) => Some(s),
        Some(Err(e)) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
        None => None,
    };
    let controls = if args.no_overrides {
        controls_store::Controls::disabled()
    } else {
        controls_store::Controls::open(game.name())
    };
    if args.headless {
        run_headless(
            &mut game,
            args.frames.unwrap_or(600),
            script,
            &controls,
            args.emulate,
        );
        ExitCode::SUCCESS
    } else {
        window::run(game, &args, script, controls)
    }
}

/// Reads and parses an `--input-script` file.
fn load_script(path: &std::path::Path) -> Result<ScriptedSource, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read input script {}: {e}", path.display()))?;
    ScriptedSource::parse(&text, TICK).map_err(|e| format!("input script {}: {e}", path.display()))
}

/// Runs `frames` frames of exactly one tick each, with the events of `script` (none: no input).
/// With `emulate`, the script's finger lines go through the touch controls of that device.
fn run_headless(
    game: &mut Game,
    frames: u64,
    mut script: Option<ScriptedSource>,
    controls: &controls_store::Controls,
    emulate: Option<DeviceProfile>,
) {
    let mut input = InputRuntime::from_world(&game.world().borrow());
    controls.apply(&mut input);
    // The player's touch settings (stick mode, sizes) apply to the emulated device.
    let t = input.overrides().touch;
    let mut touch = emulate.map(|p| {
        TouchControls::new(
            p.with_stick_mode(t.stick_mode)
                .with_scale(t.size)
                .with_large(t.large_controls),
        )
    });
    if touch.is_some() {
        input.set_forced_device(Some(DeviceKind::Touch));
        game.set_input_devices(true, false, false);
    } else if script.as_ref().is_some_and(ScriptedSource::has_fingers) {
        eprintln!("warning: the input script has finger lines; they need --emulate");
    }
    let (mut events, mut fingers) = (Vec::new(), Vec::new());
    for frame in 0..frames {
        let now = frame as f64 * TICK;
        if let Ok(world) = game.world().try_borrow() {
            input.sync(&world);
        }
        if let Some(touch) = touch.as_mut() {
            touch.sync(&mut input, now);
        }
        if let Some(script) = script.as_mut() {
            script.poll_frame(frame, now, &mut events);
            for e in events.drain(..) {
                input.push(e);
            }
            script.poll_fingers(frame, &mut fingers);
            for f in fingers.drain(..) {
                if let Some(touch) = touch.as_mut() {
                    touch.touch(&mut input, f.finger, f.phase, f.pos, now);
                }
            }
        }
        let frame_input = input.frame(now, TICK as f32);
        input.drain_events().for_each(drop);
        game.frame(&frame_input, TICK);
    }
    game.stop();
    println!(
        "metallic-player: {} ran {frames} frames ({:.1} s of game time)",
        game.name(),
        game.time()
    );
}

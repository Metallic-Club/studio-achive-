#![cfg(feature = "presentation")]
//! Plays the whole Parkour course with a simple bot that uses only player input (move, run,
//! jump), so every jump in the level is proven reachable with the real character controller.
//!
//! The bot walks from waypoint to waypoint (platform centres and the safe lanes of the lava walk),
//! jumps when there is no ground just ahead of it, waits for the moving platform and hops across
//! the spinning-bar arena. After a death it resumes from its last checkpoint. It must finish
//! within the time and death limits below, and the HUD must show the checkpoints and the time.
//!
//! The bot never builds a `FrameInput`: it sends raw events (a gamepad stick, and either the
//! keyboard or gamepad buttons) to an `InputRuntime` with the default `CharacterControls`, like the
//! Player does, and records them as an `--input-script`. A second run replays that script through
//! a `ScriptedSource` and must end the same way. The touch run puts fingers on the automatic touch
//! controls of the emulated phone (`metallic-player --emulate phone`): the floating stick, Jump
//! and Run, and its recorded finger script replays the same way.
use metallic_core::{InstanceId, Value, Vec3, World};
use metallic_physics::{HumanoidState, PhysicsWorld};
use metallic_runtime::input::touch::{DeviceProfile, TouchControls, TouchPhase};
use metallic_runtime::input::{Control, Device, DeviceKind, RawEvent, ScriptedSource};
use metallic_runtime::{FrameInput, Game, InputRuntime, StdoutSink, TICK};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Debug, Clone, Copy)]
enum Step {
    /// Walk (and jump) to this point on a platform top: x, top y, z.
    Go(f64, f64, f64),
    /// After a death, continue from the step after the last `Resume` passed.
    Resume,
    /// Stand still until the named moving platform's Z is at least this.
    MoverAtLeast(&'static str, f64),
    /// Walk onto the centre of the named moving platform, whose top is at this height.
    Mover(&'static str, f64),
    /// Ride the named moving platform until its Z is at most this.
    MoverAtMost(&'static str, f64),
    /// Keep jumping (true) or stop (false) during the following `Go` steps.
    Hop(bool),
}

use Step::*;

/// Where `examples/build_parkour/level.rs` places sections 2, 3 and 4: (Y, Z) added to the
/// section coordinates used below.
const SLATE_AT: (f64, f64) = (0.0, -61.0);
const MOSS_AT: (f64, f64) = (0.0, -115.0);
const ICE_AT: (f64, f64) = (10.0, -115.0);

/// The tower climb as built by the level (section 3 coordinates): centre (x, z), radius, first
/// angle and step (degrees, 0 = +Z), first top, rise, pad count.
const TOWER: ((f64, f64), f64, f64, f64, f64, f64, usize) =
    ((0.0, -450.0), 8.0, 250.0, 50.0, 22.0, 2.5, 14);

/// Moves a section's steps from section coordinates into the world.
fn placed(at: (f64, f64), steps: Vec<Step>) -> Vec<Step> {
    let (dy, dz) = at;
    steps
        .into_iter()
        .map(|s| match s {
            Go(x, y, z) => Go(x, y + dy, z + dz),
            MoverAtLeast(n, z) => MoverAtLeast(n, z + dz),
            Mover(n, top) => Mover(n, top + dy),
            MoverAtMost(n, z) => MoverAtMost(n, z + dz),
            other => other,
        })
        .collect()
}

/// A lava walk starting at `z0`: through each safe lane, turning only on the floor tiles between
/// strips. `side` is 1 for the first walk and -1 for the mirrored second one.
fn lava_walk(z0: f64, side: f64) -> Vec<Step> {
    [
        (5.0, 5.0),
        (5.0, 14.0),
        (-5.0, 18.0),
        (-5.0, 26.0),
        (0.0, 30.0),
        (0.0, 38.0),
        (5.25, 42.0),
        (5.25, 51.5),
        (0.0, 52.5),
    ]
    .into_iter()
    .map(|(x, d)| Go(x * side, 14.0, z0 - d))
    .collect()
}

fn route() -> Vec<Step> {
    let mut r = vec![
        Resume,
        Go(0.0, 0.0, -12.0),
        Go(0.0, 0.0, -22.0),
        Go(3.0, 0.0, -34.0),
        Go(-2.0, 2.0, -46.0),
        Go(2.0, 3.0, -58.0),
        Go(0.0, 5.0, -70.0),
        Go(0.0, 6.0, -80.0),
        Go(0.0, 8.0, -89.0),
        Go(0.0, 9.0, -98.0),
        Go(0.0, 11.0, -107.0),
        Go(-5.0, 11.0, -118.0),
        Go(3.0, 12.0, -129.0),
        Go(-3.0, 12.0, -141.0),
        Go(2.0, 14.0, -152.0),
        Go(0.0, 14.0, -158.0),
        Go(0.0, 14.0, -182.0),
        Go(-4.0, 15.0, -190.0),
        Go(3.0, 15.0, -201.0),
        Go(-2.0, 14.0, -212.0),
        Go(0.0, 14.0, -228.0),
        Resume,
    ];

    let mut slate = lava_walk(-175.0, 1.0);
    slate.extend([
        MoverAtLeast("MovingPlatform", -234.0),
        Mover("MovingPlatform", 14.0),
        MoverAtMost("MovingPlatform", -247.0),
        Go(0.0, 14.0, -258.0),
        // The beams over the lava river.
        Go(-2.0, 14.0, -265.0),
        Go(-2.0, 14.0, -278.0),
        Go(0.0, 14.0, -281.0),
        Go(2.0, 14.0, -285.0),
        Go(2.0, 14.0, -299.0),
    ]);
    slate.extend(lava_walk(-300.0, -1.0));
    slate.extend([Go(0.0, 14.0, -362.0), Resume]);
    r.extend(placed(SLATE_AT, slate));

    let mut moss = vec![
        Go(4.0, 14.0, -314.0),
        Hop(true),
        Go(4.0, 14.0, -346.0),
        Hop(false),
        Go(4.0, 15.0, -356.0),
        Go(-3.0, 17.0, -367.0),
        Go(-3.0, 18.0, -377.0),
        Go(-3.0, 18.0, -393.0),
        Go(-3.0, 18.0, -403.0),
        Go(-3.0, 20.0, -413.0),
        Go(2.0, 20.0, -419.0),
        Go(-2.0, 20.0, -425.0),
        Go(3.0, 20.0, -431.0),
        Go(-8.0, 20.0, -441.0),
        Hop(true),
    ];
    let ((cx, cz), radius, first, step, top, rise, pads) = TOWER;
    for i in 0..pads {
        let a = (first + step * i as f64).to_radians();
        moss.push(Go(
            cx + radius * a.sin(),
            // Whole-stud tops, as the level rounds them (spec 2.2 grid).
            (top + rise * i as f64).ceil(),
            cz + radius * a.cos(),
        ));
    }
    moss.extend([Hop(false), Go(0.0, 55.0, -474.0), Resume]);
    r.extend(placed(MOSS_AT, moss));

    r.extend(placed(
        ICE_AT,
        vec![
            Go(4.0, 46.0, -490.0),
            Go(-2.0, 48.0, -502.0),
            Go(3.0, 49.0, -515.0),
            Go(-1.0, 50.0, -528.0),
            Go(0.0, 50.0, -540.0),
            Go(0.0, 50.0, -543.5),
            MoverAtLeast("MovingPlatform2", -550.0),
            Mover("MovingPlatform2", 50.0),
            MoverAtMost("MovingPlatform2", -563.0),
            Go(0.0, 50.0, -573.0),
            Go(4.0, 51.0, -585.0),
            Go(-4.0, 52.0, -594.0),
            Go(4.0, 53.0, -603.0),
            Go(-4.0, 54.0, -612.0),
            Go(0.0, 55.0, -621.0),
            Go(0.0, 56.0, -638.0),
        ],
    ));
    r
}

/// The bot gives up when one step takes longer than this (seconds of game time).
const STEP_LIMIT: f64 = 20.0;
/// Most deaths allowed on the way.
const MAX_DEATHS: u32 = 6;
/// The run must end within this much game time.
const TIME_LIMIT: f64 = 400.0;

fn vec3(w: &World, id: InstanceId, prop: &str) -> Vec3 {
    match w.get(id, prop) {
        Ok(Value::Vector3(v)) => v,
        _ => Vec3::default(),
    }
}

fn flat(v: Vec3) -> Vec3 {
    Vec3::new(v.x, 0.0, v.z)
}

fn parts_of(w: &World, model: InstanceId) -> Vec<InstanceId> {
    let mut out = Vec::new();
    let mut stack = vec![model];
    while let Some(id) = stack.pop() {
        if w.is_a(id, "BasePart") {
            out.push(id);
        }
        stack.extend_from_slice(w.children(id));
    }
    out
}

fn hud_text(w: &World) -> String {
    let path = [
        "Players",
        "Player1",
        "PlayerGui",
        "HUD",
        "Banner",
        "Message",
    ];
    let mut cur = w.root(path[0]);
    for name in &path[1..] {
        cur = cur.and_then(|c| w.find_first_child(c, name));
    }
    match cur.map(|m| w.get(m, "Text")) {
        Some(Ok(Value::String(s))) => s,
        _ => String::new(),
    }
}

/// Which buttons the bot presses; the stick always moves.
#[derive(Clone, Copy, PartialEq)]
enum Buttons {
    /// Space and Shift on the keyboard.
    Keyboard,
    /// A and L3 on the gamepad.
    Gamepad,
    /// Fingers on the touch overlay of the emulated phone: the stick, Jump and Run.
    Touch,
}

/// Fingers of the touch run.
const STICK_FINGER: u64 = 0;
const JUMP_FINGER: u64 = 1;
const RUN_FINGER: u64 = 2;

struct Bot {
    input: InputRuntime,
    /// The touch overlay's controls (touch run only).
    touch: Option<TouchControls>,
    /// Where the stick finger is (touch run).
    stick_at: [f32; 2],
    buttons: Buttons,
    /// Frames played so far; event times are `frame * TICK`.
    frame: u64,
    /// What the player holds now (to send only changes).
    held: FrameInput,
    /// The events sent so far, as an `--input-script`.
    script: String,
    game: Game,
    route: Vec<Step>,
    /// Static copy of the level for "is there ground ahead" rays.
    probe: PhysicsWorld,
    step: usize,
    resume: usize,
    hop: bool,
    deaths: u32,
    step_time: f64,
    seen: Vec<String>,
}

impl Bot {
    fn world(&self) -> Rc<std::cell::RefCell<World>> {
        self.game.world()
    }

    fn root(&self) -> (Vec3, Vec<InstanceId>) {
        let world = self.world();
        let w = world.borrow();
        let model = self.game.character().expect("character");
        let root = w
            .find_first_child(model, "HumanoidRootPart")
            .expect("root part");
        (vec3(&w, root, "Position"), parts_of(&w, model))
    }

    fn mover_z(&self, name: &str) -> f64 {
        let world = self.world();
        let w = world.borrow();
        let ws = w.root("Workspace").expect("workspace");
        let mover = w.find_first_child(ws, name).expect("mover");
        vec3(&w, mover, "Position").z
    }

    /// True when a ray just ahead of the feet finds a floor no more than 0.7 studs down.
    fn ground_ahead(&self, root: Vec3, dir: Vec3, ignore: &[InstanceId]) -> bool {
        let feet = root.y - 3.0;
        let from = root + dir * 1.3;
        let origin = Vec3::new(from.x, feet + 0.5, from.z);
        self.probe
            .raycast(origin, Vec3::new(0.0, -1.2, 0.0), ignore)
            .is_some()
    }

    /// Sends one event to the input runtime and records it in the script.
    fn send(
        &mut self,
        device: Device,
        control: Control,
        state: &str,
        nums: &[f32],
        event: RawEvent,
    ) {
        let dev = match device {
            Device::Gamepad(n) => format!("gamepad{n}"),
            _ => "keyboard".to_string(),
        };
        let values: String = nums.iter().map(|n| format!(" {n}")).collect();
        let name = control.name();
        self.script.push_str(&format!(
            "{} {dev} {name} {state}{values}
",
            self.frame
        ));
        self.input.push(event);
    }

    /// Sends one finger event to the touch controls and records it in the script.
    fn finger(&mut self, id: u64, phase: TouchPhase, at: [f32; 2], time: f64) {
        let name = match phase {
            TouchPhase::Down => "Down",
            TouchPhase::Move => "Move",
            TouchPhase::Up => "Up",
            TouchPhase::Cancel => "Cancel",
        };
        self.script.push_str(&format!(
            "{} touch{id} {name} pos {} {}\n",
            self.frame, at[0], at[1]
        ));
        let touch = self.touch.as_mut().expect("touch controls");
        touch.touch(&mut self.input, id, phase, at, time);
    }

    /// The touch run: the stick finger lands on the stick's rest point and slides one radius in
    /// the wanted direction; Jump and Run are held with their own fingers.
    fn play_touch(&mut self, want: FrameInput, time: f64) {
        let touch = self.touch.as_mut().expect("touch controls");
        touch.sync(&mut self.input, time);
        let layout = touch.layout().clone();
        let stick = layout.stick.expect("Parkour has a stick");
        let centre = |label: &str| {
            let set = touch.set();
            layout
                .buttons
                .iter()
                .find(|b| set.buttons[b.index].label == label)
                .map(|b| b.center)
                .expect("button placed")
        };
        let (jump_at, run_at) = (centre("Jump"), centre("Run"));
        let m = want.movement;
        if m != self.held.movement {
            let target = [
                stick.center[0] + m[0] * stick.radius,
                stick.center[1] - m[1] * stick.radius,
            ];
            if m == [0.0; 2] {
                self.finger(STICK_FINGER, TouchPhase::Up, self.stick_at, time);
            } else {
                if self.held.movement == [0.0; 2] {
                    self.finger(STICK_FINGER, TouchPhase::Down, stick.center, time);
                }
                self.finger(STICK_FINGER, TouchPhase::Move, target, time);
            }
            self.stick_at = target;
        }
        for (now, before, id, at) in [
            (want.jump, self.held.jump, JUMP_FINGER, jump_at),
            (want.run, self.held.run, RUN_FINGER, run_at),
        ] {
            if now != before {
                let phase = if now {
                    TouchPhase::Down
                } else {
                    TouchPhase::Up
                };
                self.finger(id, phase, at, time);
            }
        }
    }

    /// Turns what the bot wants into raw events, evaluates the actions and returns the result.
    fn play_input(&mut self, want: FrameInput) -> FrameInput {
        let time = self.frame as f64 * TICK;
        if self.buttons == Buttons::Touch {
            self.play_touch(want, time);
            self.held = want;
            self.frame += 1;
            return self.input.frame(time, TICK as f32);
        }
        let pad = Device::Gamepad(0);
        if want.movement != self.held.movement {
            let m = want.movement;
            let e = RawEvent::vector(pad, Control::Thumbstick1, m, time);
            self.send(pad, Control::Thumbstick1, "axis", &m, e);
        }
        let (jump_dev, jump_key, run_dev, run_key) = match self.buttons {
            Buttons::Keyboard => (
                Device::KeyboardMouse,
                Control::Space,
                Device::KeyboardMouse,
                Control::LeftShift,
            ),
            Buttons::Gamepad | Buttons::Touch => (pad, Control::ButtonA, pad, Control::ButtonL3),
        };
        for (now, before, device, control) in [
            (want.jump, self.held.jump, jump_dev, jump_key),
            (want.run, self.held.run, run_dev, run_key),
        ] {
            if now != before {
                let e = RawEvent::button(device, control, f32::from(u8::from(now)), time);
                self.send(device, control, if now { "down" } else { "up" }, &[], e);
            }
        }
        self.held = want;
        self.frame += 1;
        self.input.frame(time, TICK as f32)
    }

    fn tick(&mut self, want: FrameInput) -> Option<HumanoidState> {
        let input = self.play_input(want);
        let out = self.game.frame(&input, TICK);
        self.step_time += TICK;
        let text = hud_text(&self.world().borrow());
        if self.seen.last() != Some(&text) {
            println!("{:6.1} s  HUD: {text}", self.game.time());
            self.seen.push(text);
        }
        out.state
    }

    /// Input that moves toward `target` (x, feet height, z); `None` once standing there.
    fn steer(&self, target: Vec3, state: Option<HumanoidState>) -> Option<FrameInput> {
        let (root, own) = self.root();
        let to = flat(target - root);
        let grounded = state == Some(HumanoidState::Running);
        let feet = root.y - 3.0;
        if to.magnitude() < 1.0 && grounded && (feet - target.y).abs() < 0.6 {
            return None;
        }
        let dir = to.unit();
        let jump = grounded && (self.hop || !self.ground_ahead(root, dir, &own));
        // Camera yaw stays 0, so "forward" is -Z and "right" is +X.
        let speed = if to.magnitude() < 1.0 { 0.0 } else { 1.0 };
        Some(FrameInput {
            movement: [(dir.x * speed) as f32, (-dir.z * speed) as f32],
            jump,
            run: true,
            ..FrameInput::default()
        })
    }

    fn run(&mut self) -> f64 {
        let mut state = self.tick(FrameInput::default());
        let mut last_character = self.game.character();
        while self.step < self.route.len() {
            assert!(
                self.game.time() < TIME_LIMIT,
                "out of time at step {}",
                self.step
            );
            assert!(
                self.step_time < STEP_LIMIT,
                "stuck at step {} {:?}, root {:?}",
                self.step,
                self.route[self.step],
                self.root().0
            );
            if state == Some(HumanoidState::Dead) {
                println!(
                    "death {} at step {} {:?}, root {:?}, t = {:.1}",
                    self.deaths + 1,
                    self.step,
                    self.route[self.step],
                    self.root().0,
                    self.game.time()
                );
                while self.game.character() == last_character {
                    self.tick(FrameInput::default());
                }
                last_character = self.game.character();
                self.deaths += 1;
                assert!(self.deaths <= MAX_DEATHS, "{} deaths", self.deaths);
                self.step = self.resume;
                self.step_time = 0.0;
                self.hop = false;
                self.probe.sync_from(&self.world().borrow());
                state = self.tick(FrameInput::default());
                continue;
            }
            let input = match self.route[self.step] {
                Go(x, y, z) => self.steer(Vec3::new(x, y, z), state),
                Resume => {
                    self.resume = self.step + 1;
                    None
                }
                Hop(on) => {
                    self.hop = on;
                    None
                }
                MoverAtLeast(name, z) => (self.mover_z(name) < z).then(FrameInput::default),
                Mover(name, top) => {
                    let target = Vec3::new(0.0, top, self.mover_z(name));
                    let (root, _) = self.root();
                    let on = flat(target - root).magnitude() < 2.5
                        && state == Some(HumanoidState::Running)
                        && (root.y - 3.0 - top).abs() < 0.6;
                    if on {
                        None
                    } else {
                        self.steer(target, state).or(Some(FrameInput::default()))
                    }
                }
                MoverAtMost(name, z) => (self.mover_z(name) > z).then(FrameInput::default),
            };
            match input {
                Some(i) => state = self.tick(i),
                None => {
                    self.step += 1;
                    self.step_time = 0.0;
                }
            }
        }
        // Let the Finish script and the HUD react.
        for _ in 0..30 {
            self.tick(FrameInput::default());
        }
        self.game.time()
    }
}

fn open_parkour() -> Game {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../templates/parkour");
    metallic_script::use_test_budgets_on_this_thread();
    let mut game = Game::open(&dir, None, Rc::new(StdoutSink)).expect("load Parkour");
    game.start().expect("start");
    game
}

/// Plays the course with the bot and checks the HUD; returns the bot (with its script).
fn finish_the_course(buttons: Buttons) -> Bot {
    let game = open_parkour();
    let mut input = InputRuntime::from_world(&game.world().borrow());
    let touch = (buttons == Buttons::Touch).then(|| {
        input.set_forced_device(Some(DeviceKind::Touch));
        TouchControls::new(DeviceProfile::phone())
    });
    let mut probe = PhysicsWorld::new();
    probe.sync_from(&game.world().borrow());
    let mut bot = Bot {
        input,
        touch,
        stick_at: [0.0; 2],
        buttons,
        frame: 0,
        held: FrameInput::default(),
        script: String::new(),
        game,
        route: route(),
        probe,
        step: 0,
        resume: 0,
        hop: false,
        deaths: 0,
        step_time: 0.0,
        seen: Vec::new(),
    };
    let time = bot.run();
    println!(
        "bot finished in {time:.1} s of game time with {} deaths; HUD: {:?}",
        bot.deaths, bot.seen
    );
    for expected in ["Checkpoint 1/3", "Checkpoint 2/3", "Checkpoint 3/3"] {
        assert!(bot.seen.iter().any(|t| t == expected), "{:?}", bot.seen);
    }
    let last = bot.seen.last().cloned().unwrap_or_default();
    assert!(
        last.starts_with("Finished in ") && last.ends_with(" s"),
        "HUD at the end: {last:?}"
    );
    let seconds: f64 = last["Finished in ".len()..last.len() - 2]
        .parse()
        .expect("a number");
    assert!(
        last.split('.').nth(1).map(str::len) == Some(3),
        "one decimal: {last}"
    );
    assert!(seconds > 20.0 && seconds <= time, "{seconds} vs {time}");
    bot
}

#[test]
fn a_bot_using_only_player_input_finishes_the_course() {
    // Stick plus keyboard buttons, then the recorded events replayed as an input script.
    let bot = finish_the_course(Buttons::Keyboard);
    let frames = bot.frame;
    let script = ScriptedSource::parse(&bot.script, TICK).expect("the recorded script parses");
    assert!(script.last_frame() < frames);
    println!(
        "replaying {} script lines over {frames} frames",
        bot.script.lines().count()
    );
    let mut replay = ScriptedSource::parse(&bot.script, TICK).expect("parse");
    let mut game = open_parkour();
    let mut input = InputRuntime::from_world(&game.world().borrow());
    for frame in 0..frames {
        input.poll_source(&mut replay);
        let f = input.frame(frame as f64 * TICK, TICK as f32);
        game.frame(&f, TICK);
    }
    assert_eq!(
        hud_text(&game.world().borrow()),
        bot.seen.last().cloned().unwrap_or_default()
    );
}

#[test]
fn the_course_can_be_finished_on_a_gamepad() {
    finish_the_course(Buttons::Gamepad);
}

/// Spec 14 and #252: Parkour plays fully in the touch emulator (`--emulate phone`): the stick,
/// Jump and Run, driven by finger events only; the recorded finger script replays the same way.
#[test]
fn the_course_can_be_finished_with_touch_controls() {
    let bot = finish_the_course(Buttons::Touch);
    assert!(
        bot.script.lines().all(|l| l.contains(" touch")),
        "fingers only"
    );
    assert!(bot.script.contains("touch1 Down"), "Jump was pressed");
    assert!(bot.script.contains("touch2 Down"), "Run was pressed");
    let frames = bot.frame;
    let mut replay = ScriptedSource::parse(&bot.script, TICK).expect("the finger script parses");
    let mut game = open_parkour();
    let mut input = InputRuntime::from_world(&game.world().borrow());
    input.set_forced_device(Some(DeviceKind::Touch));
    let mut touch = TouchControls::new(DeviceProfile::phone());
    let mut fingers = Vec::new();
    for frame in 0..frames {
        let now = frame as f64 * TICK;
        touch.sync(&mut input, now);
        replay.poll_fingers(frame, &mut fingers);
        for f in fingers.drain(..) {
            touch.touch(&mut input, f.finger, f.phase, f.pos, now);
        }
        let f = input.frame(now, TICK as f32);
        game.frame(&f, TICK);
    }
    assert_eq!(
        hud_text(&game.world().borrow()),
        bot.seen.last().cloned().unwrap_or_default()
    );
}

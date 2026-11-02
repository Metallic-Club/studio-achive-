//! The windowed player: winit window, wgpu surface, input, frame loop and statistics.
use crate::args::Args;
use crate::capture::CaptureTarget;
use crate::controls_store::Controls;
use crate::devices::{GamepadSource, key_control, mouse_control};
use crate::menu::{Menu, MenuOutcome, UiLayer};
use crate::overlay::Overlay;
use crate::pacing::{Pace, WindowState};
use crate::touch_overlay::{TouchOverlay, emulation_window};
use metallic_render::glam::Vec3;
use metallic_render::{Camera, RenderContext, RenderSettings, Renderer};
use metallic_runtime::HumanoidState;
use metallic_runtime::gui_input::{PointerId, PointerPhase};
use metallic_runtime::input::{
    ActionId, Control, ControlsHint, Device, DeviceKind, FingerEvent, MENU_ACTION, RawEvent,
    ScriptedSource,
};
use metallic_runtime::{FrameInput, Game, InputRuntime, TICK};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};
use winit::application::ApplicationHandler;
use winit::dpi::PhysicalSize;
use winit::event::{DeviceEvent, DeviceId, ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::PhysicalKey;
use winit::window::{CursorGrabMode, Window, WindowId};

/// How long `--fps` measures.
const FPS_SECONDS: f64 = 60.0;
/// `--autopilot` turns the camera (and so the run direction) this fast, in radians per second:
/// at the run speed of 24 studs/s that is a circle of 5 studs radius, which stays on the
/// Parkour spawn island.
const AUTOPILOT_TURN: f32 = 4.8;
/// `--autopilot` jumps this often, in seconds.
const AUTOPILOT_JUMP_EVERY: f64 = 1.5;

/// Plays `game` in a window until it is closed (or `--frames` / `--fps` ends it).
pub fn run(
    mut game: Game,
    args: &Args,
    script: Option<ScriptedSource>,
    controls: Controls,
) -> ExitCode {
    let event_loop = match EventLoop::new() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("error: cannot open a window: {e}");
            return ExitCode::from(1);
        }
    };
    event_loop.set_control_flow(ControlFlow::Poll);
    let epoch = Instant::now();
    let mut input = InputRuntime::from_world(&game.world().borrow());
    for warning in input.warnings() {
        eprintln!("warning: input: {warning}");
    }
    controls.apply(&mut input);
    let menu_action = input.find_action(MENU_ACTION);
    let menu_presses = menu_action.map_or(0, |id| input.presses(id));
    let mut menu = Menu::new(game.name());
    if let Some(page) = args.menu.as_deref() {
        menu.open(&input, 0.0);
        if page != "main" {
            menu.open_controls();
            menu.show_tab(match page {
                "gamepad" => crate::menu::Tab::Gamepad,
                "assist" => crate::menu::Tab::Assist,
                "touch" => crate::menu::Tab::Touch,
                _ => crate::menu::Tab::Keyboard,
            });
        }
    }
    if args.emulate.is_some() {
        // Device emulation: the game sees a touch device only (spec 10.2).
        input.set_forced_device(Some(DeviceKind::Touch));
        game.set_input_devices(true, false, false);
    }
    let touch = TouchOverlay::new(&args.project, args.emulate, args.reduce_motion);
    let mut app = App {
        game,
        args: args.clone(),
        gfx: None,
        input,
        touch,
        gamepads: GamepadSource::new(epoch),
        script,
        epoch,
        events: Vec::new(),
        fingers: Vec::new(),
        captured: false,
        last: Instant::now(),
        focused: true,
        occluded: false,
        paused: false,
        frame: 0,
        frame_times: Vec::new(),
        started: None,
        failed: false,
        quit: false,
        menu,
        hint: ControlsHint::new(),
        controls,
        menu_action,
        menu_presses,
        dead: false,
    };
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("error: {e}");
        return ExitCode::from(1);
    }
    app.game.stop();
    if let Some(report) = app.touch.timing_report() {
        println!("{report}");
    }
    app.controls.save(app.input.overrides());
    if app.failed {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    }
}

struct Gfx {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    config: wgpu::SurfaceConfiguration,
    ctx: RenderContext,
    renderer: Renderer,
    overlay: Overlay,
    /// The Player menu and the controls hint.
    ui: UiLayer,
    minimised: bool,
}

struct App {
    game: Game,
    args: Args,
    gfx: Option<Gfx>,
    input: InputRuntime,
    /// The automatic touch controls and device emulation.
    touch: TouchOverlay,
    gamepads: GamepadSource,
    /// `--input-script`: when set, the input clock counts frames, see `App::input_clock`.
    script: Option<ScriptedSource>,
    /// When the player started: the zero of the input clock.
    epoch: Instant,
    /// Scratch for the script's events.
    events: Vec<RawEvent>,
    /// Scratch for the script's finger events.
    fingers: Vec<FingerEvent>,
    /// The cursor is locked and mouse movement is sent as `MouseDelta`.
    captured: bool,
    last: Instant,
    /// The window has keyboard focus.
    focused: bool,
    /// The window is completely hidden (where the platform reports it).
    occluded: bool,
    /// The last pace was [`Pace::Paused`]: the next frame starts with a fresh frame time.
    paused: bool,
    frame: u64,
    frame_times: Vec<f64>,
    started: Option<Instant>,
    failed: bool,
    /// "Leave game" was chosen.
    quit: bool,
    /// The Player menu (Menu action: Esc or Start by default).
    menu: Menu,
    /// The Move / Jump / Run / Camera / Menu hint at the start and after a glyph change.
    hint: ControlsHint,
    /// Where the player's input overrides are stored.
    controls: Controls,
    menu_action: Option<ActionId>,
    /// `presses` of the Menu action when last looked at.
    menu_presses: u32,
    /// The character was dead last frame (toggles clear on death).
    dead: bool,
}

impl App {
    fn create_gfx(&mut self, event_loop: &ActiveEventLoop) -> Result<Gfx, String> {
        // `--fps` measures at exactly 1920x1080, so that window cannot be resized. An emulated
        // device gets a window of its shape.
        let (w, h) = match (&self.args.emulate, self.args.fps) {
            (Some(device), false) => emulation_window(device),
            _ => (1920, 1080),
        };
        let attrs = Window::default_attributes()
            .with_title(format!("{} - Metallic player", self.game.name()))
            .with_inner_size(PhysicalSize::new(w, h))
            .with_resizable(!self.args.fps);
        let window = Arc::new(event_loop.create_window(attrs).map_err(|e| e.to_string())?);
        let (ctx, surface) =
            RenderContext::new_for_window(window.clone()).map_err(|e| e.to_string())?;
        let adapter = ctx.adapter.as_ref().ok_or("no graphics adapter")?;
        let size = window.inner_size();
        let mut config = surface
            .get_default_config(adapter, size.width.max(1), size.height.max(1))
            .ok_or("the window surface is not supported by the graphics adapter")?;
        let caps = surface.get_capabilities(adapter);
        if let Some(f) = caps.formats.iter().find(|f| f.is_srgb()) {
            config.format = *f;
        }
        config.present_mode = if self.args.fps {
            wgpu::PresentMode::AutoNoVsync
        } else {
            wgpu::PresentMode::AutoVsync
        };
        surface.configure(&ctx.device, &config);
        let settings = RenderSettings {
            quality: self.args.quality,
        };
        let mut renderer = Renderer::with_settings(&ctx, config.format, settings);
        if let Ok(world) = self.game.world().try_borrow() {
            renderer.sync(&world);
        }
        renderer.prewarm((config.width, config.height));
        let overlay = Overlay::new(&ctx.device, config.format);
        let ui = UiLayer::new(&ctx.device, config.format);
        println!(
            "metallic-player: {} on {}, {}x{}, {:?} quality",
            self.game.name(),
            ctx.adapter_name,
            config.width,
            config.height,
            self.args.quality
        );
        Ok(Gfx {
            window,
            surface,
            config,
            ctx,
            renderer,
            overlay,
            ui,
            minimised: false,
        })
    }

    /// Seconds on the input clock: real time, or the frame count in ticks when an input script
    /// drives the run, so the script plays the same way every time.
    fn input_clock(&self) -> f64 {
        if self.script.is_some() {
            self.frame as f64 * TICK
        } else {
            self.epoch.elapsed().as_secs_f64()
        }
    }

    /// Collects the gamepad and script events of this frame. Gamepad events go to the menu
    /// while it is open.
    fn poll_devices(&mut self, clock: f64) {
        self.events.clear();
        self.gamepads.poll_into(&mut self.events);
        for i in 0..self.events.len() {
            let e = self.events[i];
            self.route(e, false);
        }
        if let Some(script) = self.script.as_mut() {
            self.events.clear();
            script.poll_frame(self.frame, clock, &mut self.events);
            for e in self.events.drain(..) {
                self.input.push(e);
            }
            script.poll_fingers(self.frame, &mut self.fingers);
            for f in self.fingers.drain(..) {
                self.touch.finger(&mut self.input, f, clock);
            }
        }
    }

    /// Gives a pointer event at window pixel `px` to the game's GUI (not while the menu is
    /// open). True when the GUI took it: it must not reach the game's input (#79).
    fn gui_pointer(&mut self, pointer: PointerId, phase: PointerPhase, px: [f32; 2]) -> bool {
        if self.menu.is_open() {
            return false;
        }
        let Some(gfx) = self.gfx.as_ref() else {
            return false;
        };
        let scale = gfx.window.scale_factor() as f32;
        if !(scale.is_finite() && scale > 0.0) {
            return false;
        }
        let at = [px[0] / scale, px[1] / scale];
        let screen = [
            gfx.config.width as f32 / scale,
            gfx.config.height as f32 / scale,
        ];
        self.game.gui_pointer(pointer, phase, at, screen)
    }

    /// Sends a raw event to the menu while it is open, else to the game's input.
    fn route(&mut self, e: RawEvent, repeat: bool) {
        if !self.menu.is_open() {
            self.input.push(e);
            return;
        }
        let out = self.menu.raw(e, repeat);
        self.menu_outcome(out);
    }

    /// A finger while the menu is open: it points and clicks like the mouse.
    fn menu_touch(&mut self, phase: winit::event::TouchPhase, px: [f32; 2], time: f64) {
        use winit::event::TouchPhase as P;
        let pos = RawEvent::vector(Device::KeyboardMouse, Control::MousePosition, px, time);
        self.route(pos, false);
        let value = match phase {
            P::Started => 1.0,
            P::Ended | P::Cancelled => 0.0,
            P::Moved => return,
        };
        let click = RawEvent::button(Device::KeyboardMouse, Control::MouseButton1, value, time);
        self.route(click, false);
    }

    fn menu_outcome(&mut self, out: MenuOutcome) {
        match out {
            MenuOutcome::None => {}
            MenuOutcome::Changed => {
                if let Some(o) = self.menu.overrides() {
                    self.input.set_overrides(o.clone());
                    self.update_glyph_style();
                    self.menu.sync(&self.input);
                    self.menu_action = self.input.find_action(MENU_ACTION);
                }
            }
            MenuOutcome::Save | MenuOutcome::Closed { save: true } => {
                self.controls.save(self.input.overrides());
            }
            MenuOutcome::Closed { save: false } => {}
            MenuOutcome::Quit => self.quit = true,
        }
        if matches!(out, MenuOutcome::Closed { .. }) {
            // The key that closed the menu must not reach the game as a press.
            self.menu_presses = self.menu_action.map_or(0, |id| self.input.presses(id));
        }
    }

    /// The face-button style: the player's choice, or by the vendor of the pad in use.
    fn update_glyph_style(&mut self) {
        let vendor = self.gamepads.last_vendor();
        self.menu.set_pad_vendor(vendor);
        let style = crate::menu::resolve_style(self.input.overrides().gamepad_glyphs, vendor);
        self.input.set_gamepad_style(style);
    }

    /// Opens the menu when the Menu action was pressed this frame.
    fn check_menu(&mut self, clock: f64) {
        let Some(id) = self.menu_action else { return };
        let presses = self.input.presses(id);
        if presses == self.menu_presses {
            return;
        }
        self.menu_presses = presses;
        if !self.menu.is_open() {
            self.touch.release_all(&mut self.input, clock);
            self.input.release_all(clock);
            self.set_captured(false);
            self.menu.open(&self.input, clock);
        }
    }

    /// Locks the cursor while a `MouseDelta` binding is in use (by default: right mouse held).
    fn set_captured(&mut self, on: bool) {
        if on == self.captured {
            return;
        }
        self.captured = on;
        let Some(gfx) = self.gfx.as_ref() else {
            return;
        };
        gfx.window.set_cursor_visible(!on);
        // A platform without cursor grabbing still orbits; the cursor just stays visible there.
        let _ = if on {
            gfx.window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| gfx.window.set_cursor_grab(CursorGrabMode::Confined))
        } else {
            gfx.window.set_cursor_grab(CursorGrabMode::None)
        };
    }

    /// How the loop should run now (see `pacing.rs`).
    fn pace(&self) -> Pace {
        let Some(gfx) = self.gfx.as_ref() else {
            return Pace::Full;
        };
        let state = WindowState {
            minimised: gfx.minimised || gfx.window.is_minimized() == Some(true),
            occluded: self.occluded,
            focused: self.focused,
        };
        let automated = self.args.fps
            || self.args.frames.is_some()
            || !self.args.captures.is_empty()
            || self.args.autopilot
            || self.script.is_some();
        Pace::for_window(state, automated)
    }

    fn redraw(&mut self, event_loop: &ActiveEventLoop) {
        if self.pace() == Pace::Paused {
            // The system may still ask for a paint while minimised: the game stays paused.
            // M2 follow-up: a networked client must not pause (the server keeps simulating and
            // the client has to keep receiving and acknowledging); it should keep ticking
            // without drawing instead.
            self.paused = true;
            return;
        }
        if std::mem::take(&mut self.paused) {
            // Back from minimised: the time away is not a frame.
            self.last = Instant::now();
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        let clock = self.input_clock();
        let (window, scale) = self.gfx.as_ref().map_or(((1, 1), 1.0), |g| {
            (
                (g.config.width, g.config.height),
                g.window.scale_factor() as f32,
            )
        });
        self.touch.update(
            &mut self.input,
            &mut self.game,
            window,
            scale,
            clock,
            dt as f32,
        );
        self.poll_devices(clock);
        self.update_glyph_style();
        let out = self.menu.tick(clock);
        self.menu_outcome(out);
        if self.quit {
            event_loop.exit();
            return;
        }
        let mut frame_input = self.game.input_frame(&mut self.input, clock, dt as f32);
        self.check_menu(clock);
        let want_capture = self.input.wants_mouse_delta() && !self.menu.is_open();
        self.set_captured(want_capture);
        if self.args.autopilot {
            autopilot(&mut frame_input, self.game.time(), dt);
        }
        let output = self.game.frame(&frame_input, dt);
        let dead = output.state == Some(HumanoidState::Dead);
        if dead && !self.dead {
            // A toggled Run or Aim does not survive death (spec 11.1).
            self.input.clear_toggles(clock);
        }
        self.dead = dead;
        self.frame += 1;

        let Some(gfx) = self.gfx.as_mut() else {
            return;
        };
        if gfx.minimised {
            return;
        }
        let size = (gfx.config.width, gfx.config.height);
        let scale = gfx.window.scale_factor() as f32;
        self.menu.set_scale(scale);
        if let Some(path) = capture_path(&self.args, self.frame) {
            let target = CaptureTarget::new(&gfx.ctx.device, gfx.config.format, size);
            let camera = match self.args.capture_view {
                Some([x, y, z, lx, ly, lz]) => Camera {
                    position: Vec3::new(x, y, z),
                    look_at: Vec3::new(lx, ly, lz),
                    ..self.game.camera()
                },
                None => self.game.camera(),
            };
            self.game
                .render_from(&mut gfx.renderer, &target.view, size, &camera);
            gfx.overlay.draw(
                &gfx.ctx.device,
                &gfx.ctx.queue,
                &target.view,
                size,
                scale,
                &self.game,
                Some(&mut self.touch),
            );
            let (menu, hint, input) = (&mut self.menu, &mut self.hint, &self.input);
            gfx.ui.draw(
                &gfx.ctx.device,
                &gfx.ctx.queue,
                &target.view,
                size,
                scale,
                clock,
                |ctx| {
                    paint_ui(ctx, menu, hint, input, clock);
                },
            );
            match target.save_png(&gfx.ctx.device, &gfx.ctx.queue, &path) {
                Ok(()) => println!(
                    "metallic-player: saved frame {} to {}",
                    self.frame,
                    path.display()
                ),
                Err(e) => eprintln!("warning: could not save {}: {e}", path.display()),
            }
        }
        match gfx.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => {
                let view = frame
                    .texture
                    .create_view(&wgpu::TextureViewDescriptor::default());
                self.game.render(&mut gfx.renderer, &view, size);
                gfx.overlay.draw(
                    &gfx.ctx.device,
                    &gfx.ctx.queue,
                    &view,
                    size,
                    scale,
                    &self.game,
                    Some(&mut self.touch),
                );
                let (menu, hint, input) = (&mut self.menu, &mut self.hint, &self.input);
                gfx.ui.draw(
                    &gfx.ctx.device,
                    &gfx.ctx.queue,
                    &view,
                    size,
                    scale,
                    clock,
                    |ctx| {
                        paint_ui(ctx, menu, hint, input, clock);
                    },
                );
                gfx.window.pre_present_notify();
                gfx.ctx.queue.present(frame);
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                gfx.surface.configure(&gfx.ctx.device, &gfx.config);
            }
            _ => {}
        }

        if self.args.fps {
            let started = *self.started.get_or_insert(now);
            self.frame_times.push(dt);
            if now.duration_since(started) >= Duration::from_secs_f64(FPS_SECONDS) {
                print_fps(&self.frame_times[1..], size);
                event_loop.exit();
            }
        }
        if self.args.frames.is_some_and(|n| self.frame >= n) {
            event_loop.exit();
        }
    }
}

/// Paints the controls hint (while playing) and the menu (while open).
fn paint_ui(
    ctx: &egui::Context,
    menu: &mut Menu,
    hint: &mut ControlsHint,
    input: &InputRuntime,
    clock: f64,
) {
    if menu.is_open() {
        menu.paint(ctx, input);
    } else {
        hint.update(input, clock);
        let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("metallic_hud"));
        hint.paint(&ctx.layer_painter(layer), clock);
    }
}

/// Replaces the player's input: run forward while turning, and jump every
/// [`AUTOPILOT_JUMP_EVERY`] seconds of game time.
fn autopilot(input: &mut FrameInput, time: f64, dt: f64) {
    input.movement = [0.0, 1.0];
    input.run = true;
    input.orbit[0] += AUTOPILOT_TURN * dt as f32;
    input.jump = time % AUTOPILOT_JUMP_EVERY < 0.1;
}

fn capture_path(args: &Args, frame: u64) -> Option<PathBuf> {
    args.captures
        .iter()
        .find(|(f, _)| *f == frame)
        .map(|(_, p)| p.clone())
}

/// Prints average fps, frame-time percentiles and the slowest frame.
fn print_fps(times: &[f64], size: (u32, u32)) {
    if times.is_empty() {
        println!("metallic-player fps: no frames");
        return;
    }
    let total: f64 = times.iter().sum();
    let mut sorted = times.to_vec();
    sorted.sort_by(f64::total_cmp);
    let pct = |p: f64| sorted[((sorted.len() - 1) as f64 * p).round() as usize] * 1000.0;
    println!(
        "metallic-player fps: {}x{}, {} frames in {:.1} s, average {:.1} fps, frame ms p50 {:.2} p99 {:.2} max {:.2}",
        size.0,
        size.1,
        times.len(),
        total,
        times.len() as f64 / total,
        pct(0.5),
        pct(0.99),
        pct(1.0)
    );
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_some() {
            return;
        }
        match self.create_gfx(event_loop) {
            Ok(gfx) => {
                if self.args.minimised {
                    gfx.window.set_minimized(true);
                }
                self.gfx = Some(gfx);
            }
            Err(e) => {
                eprintln!("error: cannot start graphics: {e}");
                self.failed = true;
                event_loop.exit();
            }
        }
        self.last = Instant::now();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let Some(gfx) = self.gfx.as_mut() {
                    gfx.minimised = size.width == 0 || size.height == 0;
                    if !gfx.minimised {
                        gfx.config.width = size.width;
                        gfx.config.height = size.height;
                        gfx.surface.configure(&gfx.ctx.device, &gfx.config);
                    }
                }
            }
            WindowEvent::Occluded(occluded) => self.occluded = occluded,
            WindowEvent::Focused(true) => self.focused = true,
            WindowEvent::Focused(false) => {
                self.focused = false;
                let now = self.input_clock();
                self.touch.release_all(&mut self.input, now);
                self.input.release_all(now);
                self.set_captured(false);
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // Every key, Escape included, is a control: Escape is the default binding of
                // the `Menu` action, which opens the Player menu.
                if let PhysicalKey::Code(code) = event.physical_key
                    && let Some(control) = key_control(code)
                {
                    let down = event.state == ElementState::Pressed;
                    let time = self.input_clock();
                    let value = f32::from(u8::from(down));
                    let e = RawEvent::button(Device::KeyboardMouse, control, value, time);
                    self.route(e, event.repeat);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let time = self.input_clock();
                let down = state == ElementState::Pressed;
                // The GUI first: a click on a button is not the game's (#79).
                let phase = if down {
                    PointerPhase::Down
                } else {
                    PointerPhase::Up
                };
                if button == winit::event::MouseButton::Left
                    && self.gui_pointer(PointerId::Mouse, phase, self.touch.cursor())
                {
                    return;
                }
                // In device emulation the mouse is a finger (but a mouse in the menu).
                let finger = !self.menu.is_open()
                    && self.touch.mouse_button(&mut self.input, button, down, time);
                if !finger && let Some(control) = mouse_control(button) {
                    let value = f32::from(u8::from(state == ElementState::Pressed));
                    let e = RawEvent::button(Device::KeyboardMouse, control, value, time);
                    self.route(e, false);
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                let time = self.input_clock();
                let pos = [position.x as f32, position.y as f32];
                // Hover; the game keeps getting the position either way.
                self.gui_pointer(PointerId::Mouse, PointerPhase::Move, pos);
                if !self.menu.is_open() && self.touch.cursor_moved(&mut self.input, pos, time) {
                    return;
                }
                let e = RawEvent::vector(Device::KeyboardMouse, Control::MousePosition, pos, time);
                self.route(e, false);
            }
            WindowEvent::Touch(t) => {
                let time = self.input_clock();
                let px = [t.location.x as f32, t.location.y as f32];
                if self.menu.is_open() {
                    // The menu is used like a mouse: a finger points and taps.
                    self.menu_touch(t.phase, px, time);
                } else {
                    use winit::event::TouchPhase as P;
                    let phase = match t.phase {
                        P::Started => PointerPhase::Down,
                        P::Moved => PointerPhase::Move,
                        P::Ended => PointerPhase::Up,
                        P::Cancelled => PointerPhase::Cancel,
                    };
                    // The overlay's own buttons first, then the game's GUI, then the controls.
                    let overlay = phase == PointerPhase::Down && self.touch.has_button_at(px);
                    if overlay || !self.gui_pointer(PointerId::Touch(t.id), phase, px) {
                        self.touch.touch(&mut self.input, t.id, t.phase, px, time);
                    }
                }
            }
            WindowEvent::CursorLeft { .. } => {
                let [x, y] = self.touch.cursor();
                let scale = self
                    .gfx
                    .as_ref()
                    .map_or(1.0, |g| g.window.scale_factor() as f32);
                self.game.gui_mouse_left([x / scale, y / scale]);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let notches = match delta {
                    MouseScrollDelta::LineDelta(_, y) => y,
                    MouseScrollDelta::PixelDelta(p) => (p.y / 60.0) as f32,
                };
                let time = self.input_clock();
                self.route(RawEvent::wheel(Device::KeyboardMouse, notches, time), false);
            }
            WindowEvent::RedrawRequested => self.redraw(event_loop),
            _ => {}
        }
    }

    fn device_event(&mut self, _event_loop: &ActiveEventLoop, _id: DeviceId, event: DeviceEvent) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.captured
            && !self.menu.is_open()
        {
            let pos = [delta.0 as f32, delta.1 as f32];
            let time = self.input_clock();
            self.input.push(RawEvent::vector(
                Device::KeyboardMouse,
                Control::MouseDelta,
                pos,
                time,
            ));
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let pace = self.pace();
        if pace == Pace::Paused {
            self.paused = true;
        }
        event_loop.set_control_flow(pace.control_flow(self.last));
        if pace.wants_frame(self.last, Instant::now())
            && let Some(gfx) = self.gfx.as_ref()
        {
            gfx.window.request_redraw();
        }
    }
}

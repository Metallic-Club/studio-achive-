//! The Player menu: opened by the `Menu` action (Esc or Start by default), with Resume, Controls
//! and Leave game, and the Controls page of docs/design/input-actions.md section 8.
//!
//! While the menu is open the game keeps running but gets no input: the window sends every raw
//! event here instead of to the [`InputRuntime`]. The menu turns them into [`Nav`] commands
//! (arrows or d-pad or left stick, Enter/Space or A, Esc or B, Tab or the bumpers for tabs,
//! Delete or X to clear, R or Y to reset a row / undo), mouse hover and clicks, or, while a slot
//! listens, into the control to bind. Everything is reachable without a mouse.
mod controls;
mod layer;
mod paint;
pub mod theme;

pub use controls::{ControlsPage, Nav, Tab};
pub use layer::UiLayer;

use metallic_runtime::input::glyph::{GamepadStyle, auto_gamepad_style};
use metallic_runtime::input::overrides::GamepadGlyphPref;
use metallic_runtime::input::{
    CONTROL_COUNT, Control, ControlKind, DeviceKind, InputConfig, InputRuntime, MENU_ACTION,
    PlayerOverrides, RawEvent,
};

/// A stick tilted past this moves the focus.
const STICK_NAV: f32 = 0.6;
/// A held direction repeats after this long ...
const REPEAT_DELAY: f64 = 0.4;
/// ... every this many seconds.
const REPEAT_RATE: f64 = 0.11;
/// A button or trigger counts as down from here (the runtime's default threshold).
const PRESSED: f32 = 0.5;

/// The buttons of the first page.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainItem {
    /// Close the menu.
    Resume,
    /// Open the Controls page.
    Controls,
    /// Quit the Player.
    Leave,
}

impl MainItem {
    /// In order.
    pub const ALL: [MainItem; 3] = [MainItem::Resume, MainItem::Controls, MainItem::Leave];

    /// The label.
    pub fn label(self) -> &'static str {
        match self {
            MainItem::Resume => "Resume",
            MainItem::Controls => "Controls",
            MainItem::Leave => "Leave game",
        }
    }
}

/// What the window should do after a menu call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuOutcome {
    /// Nothing.
    None,
    /// The overrides changed: apply them to the runtime now.
    Changed,
    /// The Controls page closed and the menu stays open: write the file.
    Save,
    /// The menu closed; `save` when the Controls page was open (write the file).
    Closed {
        /// The Controls page closed.
        save: bool,
    },
    /// Quit the Player.
    Quit,
}

/// The Player menu.
pub struct Menu {
    open: bool,
    game_name: String,
    main_focus: usize,
    controls: Option<ControlsPage>,
    /// Combined pressed state per control, for edges.
    held: [bool; CONTROL_COUNT],
    shift: bool,
    ctrl: bool,
    /// A held direction and when it repeats next.
    repeat: Option<(Nav, f64)>,
    /// Pointer position in points.
    pointer: Option<egui::Pos2>,
    /// Points per physical pixel (1 / scale factor).
    point_scale: f32,
    /// The controls bound to `Menu` (besides Esc): they close the menu.
    menu_controls: Vec<Control>,
    /// Screen rectangles of the main page's buttons, set by the painter.
    main_rects: Vec<(egui::Rect, usize)>,
    clock: f64,
    /// The device the hints show glyphs for.
    device: DeviceKind,
    /// Vendor id of the last gamepad, for the Auto glyph style.
    pad_vendor: Option<u16>,
    /// The creator's config and the player's overrides when the menu opened (or last synced).
    base: InputConfig,
    current: PlayerOverrides,
}

impl Menu {
    /// A closed menu for the game called `game_name`.
    pub fn new(game_name: &str) -> Self {
        Self {
            open: false,
            game_name: game_name.to_string(),
            main_focus: 0,
            controls: None,
            held: [false; CONTROL_COUNT],
            shift: false,
            ctrl: false,
            repeat: None,
            pointer: None,
            point_scale: 1.0,
            menu_controls: Vec::new(),
            main_rects: Vec::new(),
            clock: 0.0,
            device: DeviceKind::KeyboardMouse,
            pad_vendor: None,
            base: InputConfig::default(),
            current: PlayerOverrides::default(),
        }
    }

    /// True while the menu shows.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// The overrides the Controls page has edited (while it shows).
    pub fn overrides(&self) -> Option<&PlayerOverrides> {
        self.controls.as_ref().map(ControlsPage::overrides)
    }

    /// Opens the first page. `rt` gives the Menu bindings and the last-used device.
    pub fn open(&mut self, rt: &InputRuntime, now: f64) {
        self.open = true;
        self.main_focus = 0;
        self.controls = None;
        self.repeat = None;
        self.held = [false; CONTROL_COUNT];
        self.clock = now;
        self.sync(rt);
    }

    /// Follows the runtime: the Menu bindings and the device the hints are for.
    pub fn sync(&mut self, rt: &InputRuntime) {
        if self.controls.is_none() {
            self.base = rt.base_config().clone();
            self.current = rt.overrides().clone();
        } else if let Some(page) = self.controls.as_mut() {
            page.set_base(rt.base_config().clone());
        }
        self.device = match rt.last_device() {
            DeviceKind::Touch => DeviceKind::KeyboardMouse,
            d => d,
        };
        self.menu_controls.clear();
        if let Some(menu) = rt.find_action(MENU_ACTION) {
            for b in rt.bindings(menu) {
                self.menu_controls.extend(b.key);
            }
        }
    }

    /// Records the vendor of the gamepad in use (for the Auto glyph style).
    pub fn set_pad_vendor(&mut self, vendor: Option<u16>) {
        self.pad_vendor = vendor;
    }

    /// The glyph style `overrides` ask for on the pad in use.
    pub fn resolve_style(&self, overrides: &PlayerOverrides) -> GamepadStyle {
        resolve_style(overrides.gamepad_glyphs, self.pad_vendor)
    }

    /// Opens the Controls page (also directly, for screenshots and tests).
    pub fn open_controls(&mut self) {
        let style = self.resolve_style(&self.current);
        self.controls = Some(ControlsPage::new(
            self.base.clone(),
            self.current.clone(),
            style,
        ));
        self.main_focus = 1;
    }

    /// Shows a tab of the Controls page (for screenshots).
    pub fn show_tab(&mut self, tab: Tab) {
        if let Some(c) = self.controls.as_mut() {
            c.set_tab(tab);
        }
    }

    /// Closes everything.
    fn close(&mut self) -> MenuOutcome {
        self.open = false;
        let page = self.controls.take();
        let save = page.is_some();
        if let Some(page) = page {
            self.current = page.overrides().clone();
        }
        self.repeat = None;
        MenuOutcome::Closed { save }
    }

    /// The window's scale factor (physical pixels per point).
    pub fn set_scale(&mut self, scale: f32) {
        self.point_scale = if scale.is_finite() && scale > 0.0 {
            1.0 / scale
        } else {
            1.0
        };
    }

    /// Advances the clock: repeats a held direction, times out listening.
    pub fn tick(&mut self, now: f64) -> MenuOutcome {
        self.clock = now;
        if let Some(c) = self.controls.as_mut() {
            c.tick(now);
        }
        if let Some((nav, at)) = self.repeat
            && now >= at
        {
            self.repeat = Some((nav, now + REPEAT_RATE));
            return self.nav(nav);
        }
        MenuOutcome::None
    }

    /// A raw event from the keyboard, mouse or a gamepad; `repeat` for the keyboard's own key
    /// repeat.
    pub fn raw(&mut self, e: RawEvent, repeat: bool) -> MenuOutcome {
        if !self.open {
            return MenuOutcome::None;
        }
        self.clock = self.clock.max(e.time);
        let c = e.control;
        if e.device.kind() == DeviceKind::Gamepad {
            self.device = DeviceKind::Gamepad;
        } else if c.kind() == ControlKind::Button {
            self.device = DeviceKind::KeyboardMouse;
        }
        match c.kind() {
            ControlKind::Position => {
                let p = e.pos.unwrap_or([0.0; 2]);
                let pos = egui::pos2(p[0] * self.point_scale, p[1] * self.point_scale);
                self.pointer = Some(pos);
                self.hover(pos);
                return MenuOutcome::None;
            }
            ControlKind::Delta => return MenuOutcome::None,
            ControlKind::Wheel => {
                if self.listening() {
                    return self.listen(Control::MouseWheel, true);
                }
                if let Some(page) = self.controls.as_mut() {
                    page.scroll_by(-e.value * 48.0);
                }
                return MenuOutcome::None;
            }
            ControlKind::Stick => return self.stick(c, e.pos.unwrap_or([0.0; 2])),
            ControlKind::Button => {}
        }
        let down = e.value >= PRESSED;
        let was = self.held[c as usize];
        self.held[c as usize] = down;
        match c {
            Control::LeftShift | Control::RightShift => self.shift = down,
            Control::LeftControl | Control::RightControl => self.ctrl = down,
            _ => {}
        }
        if self.listening() {
            if down == was || repeat {
                return MenuOutcome::None;
            }
            if down && c == Control::MouseButton1 {
                let at = self.pointer.unwrap_or_default();
                if self.controls.as_ref().is_some_and(|p| p.click_cancels(at)) {
                    return self.click(at);
                }
            }
            return self.listen(c, down);
        }
        if !down {
            if self.repeat.is_some_and(|(n, _)| Some(n) == dpad_nav(c)) {
                self.repeat = None;
            }
            return MenuOutcome::None;
        }
        if was && !repeat {
            return MenuOutcome::None;
        }
        if c == Control::MouseButton1 {
            let at = self.pointer.unwrap_or_default();
            return self.click(at);
        }
        if !repeat && self.menu_controls.contains(&c) && !matches!(c, Control::Escape) {
            return self.close();
        }
        let Some(nav) = self.key_nav(c) else {
            return MenuOutcome::None;
        };
        if dpad_nav(c).is_some() && e.device.kind() == DeviceKind::Gamepad {
            self.repeat = Some((nav, self.clock + REPEAT_DELAY));
        }
        self.nav(nav)
    }

    fn listening(&self) -> bool {
        self.controls
            .as_ref()
            .is_some_and(ControlsPage::is_listening)
    }

    fn listen(&mut self, c: Control, down: bool) -> MenuOutcome {
        let out = match self.controls.as_mut() {
            Some(page) => page.listen(c, down),
            None => return MenuOutcome::None,
        };
        self.outcome(out)
    }

    fn outcome(&mut self, out: controls::Outcome) -> MenuOutcome {
        if out.back {
            if let Some(page) = self.controls.take() {
                self.current = page.overrides().clone();
            }
            self.main_focus = 1;
            return MenuOutcome::Save;
        }
        if out.changed {
            if let Some(page) = self.controls.as_mut() {
                let style = resolve_style(page.overrides().gamepad_glyphs, self.pad_vendor);
                page.set_style(style);
            }
            return MenuOutcome::Changed;
        }
        MenuOutcome::None
    }

    fn stick(&mut self, c: Control, v: [f32; 2]) -> MenuOutcome {
        let len = (v[0] * v[0] + v[1] * v[1]).sqrt();
        let down = len >= PRESSED;
        let was = self.held[c as usize];
        self.held[c as usize] = down;
        if self.listening() {
            if down && !was {
                return self.listen(c, true);
            }
            return MenuOutcome::None;
        }
        if c != Control::Thumbstick1 {
            return MenuOutcome::None;
        }
        let dir = if len < STICK_NAV {
            None
        } else if v[0].abs() > v[1].abs() {
            Some(if v[0] > 0.0 { Nav::Right } else { Nav::Left })
        } else {
            // gilrs: y up is positive.
            Some(if v[1] > 0.0 { Nav::Up } else { Nav::Down })
        };
        match dir {
            None => {
                self.repeat = None;
                MenuOutcome::None
            }
            Some(d) if self.repeat.map(|r| r.0) == Some(d) => MenuOutcome::None,
            Some(d) => {
                self.repeat = Some((d, self.clock + REPEAT_DELAY));
                self.nav(d)
            }
        }
    }

    fn key_nav(&self, c: Control) -> Option<Nav> {
        use Control as C;
        Some(match c {
            C::Up | C::DPadUp => Nav::Up,
            C::Down | C::DPadDown => Nav::Down,
            C::Left | C::DPadLeft => Nav::Left,
            C::Right | C::DPadRight => Nav::Right,
            C::Return | C::Space | C::ButtonA => Nav::Accept,
            C::Escape | C::ButtonB => Nav::Back,
            C::Tab if self.shift => Nav::PrevTab,
            C::Tab | C::E | C::ButtonR1 => Nav::NextTab,
            C::Q | C::ButtonL1 => Nav::PrevTab,
            C::Delete | C::Backspace | C::ButtonX => Nav::Clear,
            C::Z if self.ctrl => Nav::Reset,
            C::R | C::ButtonY => Nav::Reset,
            _ => return None,
        })
    }

    /// A navigation command.
    pub fn nav(&mut self, nav: Nav) -> MenuOutcome {
        if let Some(page) = self.controls.as_mut() {
            let out = page.nav(nav);
            return self.outcome(out);
        }
        let n = MainItem::ALL.len();
        match nav {
            Nav::Up | Nav::Left => self.main_focus = (self.main_focus + n - 1) % n,
            Nav::Down | Nav::Right => self.main_focus = (self.main_focus + 1) % n,
            Nav::Accept => return self.choose(MainItem::ALL[self.main_focus]),
            Nav::Back => return self.close(),
            _ => {}
        }
        MenuOutcome::None
    }

    fn choose(&mut self, item: MainItem) -> MenuOutcome {
        match item {
            MainItem::Resume => self.close(),
            MainItem::Controls => {
                self.open_controls();
                MenuOutcome::None
            }
            MainItem::Leave => MenuOutcome::Quit,
        }
    }

    fn hover(&mut self, at: egui::Pos2) {
        if let Some(page) = self.controls.as_mut() {
            page.hover(at);
        } else if let Some(&(_, i)) = self.main_rects.iter().find(|(r, _)| r.contains(at)) {
            self.main_focus = i;
        }
    }

    fn click(&mut self, at: egui::Pos2) -> MenuOutcome {
        if let Some(page) = self.controls.as_mut() {
            let out = page.click(at);
            return self.outcome(out);
        }
        if let Some(&(_, i)) = self.main_rects.iter().find(|(r, _)| r.contains(at)) {
            self.main_focus = i;
            return self.choose(MainItem::ALL[i]);
        }
        MenuOutcome::None
    }

    /// Paints the menu over everything.
    pub fn paint(&mut self, ctx: &egui::Context, rt: &InputRuntime) {
        if !self.open {
            return;
        }
        let device = self.device;
        if let Some(page) = self.controls.as_mut() {
            paint::controls_page(ctx, page, &self.game_name, device, rt.gamepad_style());
        } else {
            self.main_rects = paint::main_page(
                ctx,
                &self.game_name,
                self.main_focus,
                device,
                rt.gamepad_style(),
            );
        }
    }
}

/// The face-button style for a preference and the pad's vendor id.
pub fn resolve_style(pref: GamepadGlyphPref, vendor: Option<u16>) -> GamepadStyle {
    match pref {
        GamepadGlyphPref::Auto => auto_gamepad_style(vendor),
        GamepadGlyphPref::Letters => GamepadStyle::Letters,
        GamepadGlyphPref::Positions => GamepadStyle::Positions,
    }
}

fn dpad_nav(c: Control) -> Option<Nav> {
    match c {
        Control::DPadUp => Some(Nav::Up),
        Control::DPadDown => Some(Nav::Down),
        Control::DPadLeft => Some(Nav::Left),
        Control::DPadRight => Some(Nav::Right),
        _ => None,
    }
}

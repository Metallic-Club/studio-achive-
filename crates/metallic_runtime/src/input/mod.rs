//! Player input as actions (docs/design/input-actions.md).
//!
//! Games listen to actions, never to raw keys. The flow of one frame:
//!
//! ```text
//! winit / gilrs / --input-script  ->  RawEvent  ->  InputQueue
//!        InputRuntime::evaluate(now, dt):  bindings -> actions (values, triggers, events)
//!        InputRuntime::frame_input()    :  CharacterControls actions -> FrameInput
//!        Game::frame(&FrameInput, dt)      (unchanged: MoveInput and the camera read FrameInput)
//! ```
//!
//! Callers preparing canonical command data can opt into `LocalInput::from_frame` with the
//! evaluated frame and absolute camera yaw. The adapter retains this module's presentation
//! boundary and does not change the Game flow above.
//!
//! - [`control`]: devices, [`Control`]s (named like Roblox's `Enum.KeyCode`), [`RawEvent`], the
//!   [`RawSource`] trait and the bounded [`InputQueue`]. The window library stays out of this
//!   crate: `metallic_player` has the winit and gilrs sources.
//! - [`config`]: `InputContext`, `InputAction` and `InputBinding` as plain data, read from the
//!   scene ([`InputConfig::from_world`]), plus the built-in `CharacterControls` context that is
//!   used when the scene has none. Default keys: WASD or arrows move, Space jumps, Shift runs,
//!   right mouse drag turns the camera, the wheel zooms; gamepad left stick or d-pad moves, A
//!   jumps, L3 runs, right stick turns the camera, R1 and L1 zoom.
//! - [`InputRuntime`]: evaluates the bindings once per frame without allocating: modifiers, dead
//!   zone, response curve, scale; composites; Bool OR and largest-magnitude combine; the triggers
//!   Press, Release, Hold, Tap, DoubleTap and Repeat measured in event time; context priority
//!   and sink; the last-used device with its hysteresis rule.
//! - [`overrides`]: the player's changes (rebound controls, hold or toggle, stick and mouse
//!   sensitivity, inversion and dead zone, the hold assists) as a JSON file of differences per
//!   game ([`OverrideStore`]); [`InputRuntime::set_overrides`] applies them.
//! - [`rebind`]: the Controls page's rules: rows by `RebindGroup`, what a slot takes while
//!   listening, conflicts with Swap / Replace / Cancel, `Required` and `Menu`.
//! - [`glyph`]: how a binding looks (keycaps, mouse, a generic gamepad in Letters or Positions
//!   style); [`InputRuntime::glyphs`] caches them per action and device. [`glyph_paint`] draws them
//!   with an egui painter, and [`ControlsHint`] is the key-cap plate (Move, Jump, Run, Camera, Menu)
//!   the Player and Studio's Play both show at the start and after a glyph change.
//! - [`touch`]: the automatic touch controls (spec 7): which actions get a stick, a look pad or
//!   a button, the layout rule, finger routing and the overlay's drawing. Fingers become
//!   `Device::Touch` events on `TouchStick`, `TouchLook` and `TouchButton1` to `TouchButton7`.
//! - [`ScriptedSource`]: `--input-script` files and tests (also finger lines for touch).

mod config;
mod control;
mod controls_hint;
pub mod glyph;
pub mod glyph_paint;
mod local_input;
pub mod overrides;
pub mod rebind;
mod runtime;
mod script;
#[cfg(test)]
mod tests;
pub mod touch;

#[cfg(test)]
mod tests_controls;

pub use config::{
    ActionDef, ActionType, BindingDef, CHARACTER_CONTROLS, ContextDef, DEFAULT_GROUP, InputConfig,
    MENU_ACTION, MOUSE_ORBIT_SPEED, PAD_ZOOM_SPEED, Role, STICK_ORBIT_SPEED, SYSTEM_CONTEXT,
    SYSTEM_GROUP, SYSTEM_PRIORITY, TouchPlacement, TouchSlot, Trigger, ZOOM_PER_NOTCH,
    default_character_controls, system_context,
};
pub use control::{
    CONTROL_COUNT, Control, ControlKind, Device, DeviceKind, InputQueue, MAX_GAMEPADS,
    QUEUE_CAPACITY, RawEvent, RawSource, TOUCH_BUTTON_SLOTS,
};
pub use controls_hint::ControlsHint;
pub use glyph::{GamepadStyle, Glyph};
pub use overrides::{OverrideStore, PlayerOverrides, Slot, TouchSettings};
pub use runtime::{ActionEvent, ActionEventKind, ActionId, ActionValue, InputRuntime};
pub use script::{FingerEvent, ScriptedSource};

/// Stick tilt below this counts as centred (the default dead zone of `Thumbstick1` and
/// `Thumbstick2`, radial and rescaled so the output starts from 0).
pub const STICK_DEADZONE: f32 = 0.18;

/// Everything the game needs from the player for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct FrameInput {
    /// Wish to move relative to the camera: x to the right, y forward, each -1..=1. Longer than 1
    /// counts as 1; a half-tilted stick walks at half speed.
    pub movement: [f32; 2],
    /// Jump is held.
    pub jump: bool,
    /// Run is held.
    pub run: bool,
    /// Camera orbit this frame in radians: yaw (positive turns the view right) and pitch
    /// (positive looks further down).
    pub orbit: [f32; 2],
    /// Camera zoom this frame in studs (positive moves the camera away).
    pub zoom: f32,
}

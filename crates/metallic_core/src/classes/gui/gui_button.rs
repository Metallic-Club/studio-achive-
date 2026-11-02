//! GuiButton: clickable GUI (#79). Roblox's GuiButton derives from GuiObject; here it stays under
//! `Frame`, so a button is also a Frame (design section 3.4).
use crate::classes::prelude::*;

const POINTER: &[Param] = &[Param::new("x", Ty::Number), Param::new("y", Ty::Number)];

static MOUSE_BUTTON_1_DOWN: SignalDesc = SignalDesc::new("MouseButton1Down")
    .params(POINTER)
    .describe("Pressed on the button, by the mouse or a finger, at this point on the screen.");
static MOUSE_BUTTON_1_UP: SignalDesc = SignalDesc::new("MouseButton1Up")
    .params(POINTER)
    .describe("Released on the button, at this point on the screen.");
static MOUSE_BUTTON_1_CLICK: SignalDesc =
    SignalDesc::new("MouseButton1Click").describe("Pressed and released on the button.");
static ACTIVATED: SignalDesc = SignalDesc::new("Activated")
    .params(&[
        Param::new("input", Ty::Any),
        Param::new("clickCount", Ty::Int),
    ])
    .describe("The button was used, right after the click.");

pub static CLASS: ClassDesc = ClassDesc::new("GuiButton", Some("Frame"))
    .flags(ClassFlags::CREATABLE)
    .category("GUI")
    .no_insert()
    .describe("A rectangle that takes clicks.")
    .props(&[
        PropDesc::new("AutoButtonColor", ValueType::Bool, || Value::Bool(true))
            .describe("Changes the button's background while it is pointed at or pressed."),
    ])
    .signals(&[
        &MOUSE_BUTTON_1_DOWN,
        &MOUSE_BUTTON_1_UP,
        &MOUSE_BUTTON_1_CLICK,
        &ACTIVATED,
    ]);

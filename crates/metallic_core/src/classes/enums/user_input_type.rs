//! `Enum.UserInputType`: the kind of device or input an event came from.
use crate::classes::prelude::*;

/// `Enum.UserInputType`.
pub const USER_INPUT_TYPE: EnumDesc = EnumDesc::new(
    "UserInputType",
    &[
        "MouseButton1",
        "MouseButton2",
        "MouseButton3",
        "MouseWheel",
        "MouseMovement",
        "Touch",
        "Keyboard",
        "Focus",
        "Gamepad1",
        "Gamepad2",
        "Gamepad3",
        "Gamepad4",
        "TextInput",
        "None",
    ],
);

/// The enums of this file.
pub static ENUMS: &[EnumDesc] = &[USER_INPUT_TYPE];

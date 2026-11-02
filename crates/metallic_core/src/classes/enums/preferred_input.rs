//! `Enum.PreferredInput`: the three kinds of device that can be the preferred input.
use crate::classes::prelude::*;

/// The enums of this file.
pub static ENUMS: &[EnumDesc] = &[EnumDesc::new(
    "PreferredInput",
    &["KeyboardAndMouse", "Gamepad", "Touch"],
)];

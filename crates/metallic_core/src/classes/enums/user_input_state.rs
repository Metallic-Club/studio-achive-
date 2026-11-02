//! `Enum.UserInputState`: where an input event is in its life.
use crate::classes::prelude::*;

/// The enums of this file.
pub static ENUMS: &[EnumDesc] = &[EnumDesc::new(
    "UserInputState",
    &["Begin", "Change", "End", "Cancel", "None"],
)];

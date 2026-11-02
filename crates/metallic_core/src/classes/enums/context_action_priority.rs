//! `Enum.ContextActionPriority`: Metallic's numbers (`docs/design/input-actions.md` 5.2).
use crate::classes::prelude::*;

/// The enums of this file.
pub static ENUMS: &[EnumDesc] =
    &[
        EnumDesc::new("ContextActionPriority", &["Low", "Medium", "High"])
            .numbers(&[1000, 2000, 3000]),
    ];

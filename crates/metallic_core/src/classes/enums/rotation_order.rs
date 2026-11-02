//! `Enum.RotationOrder`, used by `CFrame.fromEulerAngles`. Names come from [`RotationOrder`].
use crate::cframe::RotationOrder;
use crate::classes::prelude::*;

const NAMES: [&str; 6] = {
    let all = RotationOrder::ALL;
    let mut names = [""; 6];
    let mut i = 0;
    while i < 6 {
        names[i] = all[i].name();
        i += 1;
    }
    names
};

/// The enums of this file.
pub static ENUMS: &[EnumDesc] = &[EnumDesc::new("RotationOrder", &NAMES)];

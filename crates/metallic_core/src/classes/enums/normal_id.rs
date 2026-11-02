//! `Enum.NormalId`, used by `BasePart:Resize`. Names come from [`NormalId`].
use crate::classes::prelude::*;
use crate::transform::NormalId;

const NAMES: [&str; 6] = {
    let all = NormalId::ALL;
    let mut names = [""; 6];
    let mut i = 0;
    while i < 6 {
        names[i] = all[i].name();
        i += 1;
    }
    names
};

/// The enums of this file.
pub static ENUMS: &[EnumDesc] = &[EnumDesc::new("NormalId", &NAMES)];

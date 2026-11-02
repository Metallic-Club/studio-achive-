//! Limits shared with other crates.

/// Largest `Humanoid.Health` and `Humanoid.MaxHealth` (writes above are clamped).
pub const MAX_HUMANOID_HEALTH: f64 = 1e9;
/// Largest `Humanoid.WalkSpeed` in studs per second (writes above are clamped).
pub const MAX_WALK_SPEED: f64 = 1000.0;
/// Largest `Humanoid.JumpPower` in studs per second (writes above are clamped).
pub const MAX_JUMP_POWER: f64 = 1000.0;
/// Largest `Humanoid.MaxSlopeAngle` in degrees, as in Roblox (writes above are clamped).
pub const MAX_SLOPE_ANGLE: f64 = 89.0;

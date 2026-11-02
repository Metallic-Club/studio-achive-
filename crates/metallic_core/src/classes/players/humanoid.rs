//! Humanoid: the walking, jumping, dying part of a character.
//!
//! Tuning is clamped on write (scripts, scene files, engine code) so a huge value cannot send a
//! character to infinity (#272). Non-finite writes are refused by `World::set` and the old value
//! stays.
use crate::classes::prelude::*;

static DIED: SignalDesc = SignalDesc::new("Died").describe("This character died.");

pub static CLASS: ClassDesc = ClassDesc::new("Humanoid", Some("Instance"))
    .flags(ClassFlags::CREATABLE)
    .category("Players")
    .no_insert()
    .describe("Makes a model a character: health, walking and jumping.")
    .props(&[
        num("Health", || Value::Number(100.0), 0.0, MAX_HUMANOID_HEALTH)
            .describe("The character's health now; at 0 it dies."),
        num(
            "MaxHealth",
            || Value::Number(100.0),
            0.0,
            MAX_HUMANOID_HEALTH,
        )
        .describe("The most health the character can have."),
        num("WalkSpeed", || Value::Number(16.0), 0.0, MAX_WALK_SPEED)
            .describe("How fast the character walks, in studs per second."),
        num("JumpPower", || Value::Number(50.0), 0.0, MAX_JUMP_POWER)
            .describe("How hard the character jumps."),
        num(
            "MaxSlopeAngle",
            || Value::Number(89.0),
            0.0,
            MAX_SLOPE_ANGLE,
        )
        .describe("The steepest slope the character can walk up, in degrees."),
    ])
    .signals(&[&DIED]);

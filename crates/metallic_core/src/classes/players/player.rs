//! Player: one person in the game.
use crate::classes::prelude::*;

static CHARACTER_ADDED: SignalDesc = SignalDesc::new("CharacterAdded")
    .params(&[Param::new("character", Ty::Instance(Some("Model")))])
    .describe("This player got a new character.");

pub static CLASS: ClassDesc = ClassDesc::new("Player", Some("Instance"))
    .flags(ClassFlags::CREATABLE)
    .category("Players")
    .no_insert()
    .describe("One person in the game.")
    .props(&[
        PropDesc::new("UserId", ValueType::Number, || Value::Number(0.0))
            .editor(Editor::new().drag(NumberUnit::Integer))
            .describe("The number that identifies this person."),
        PropDesc::new("RespawnLocation", ValueType::Ref, || Value::Ref(None))
            .describe("Where this player appears next time they respawn."),
        PropDesc::new("Character", ValueType::Ref, || Value::Ref(None))
            .describe("The model this player controls."),
    ])
    .signals(&[&CHARACTER_ADDED]);

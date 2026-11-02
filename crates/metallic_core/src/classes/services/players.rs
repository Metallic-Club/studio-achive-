//! Players: the joined players.
use crate::classes::prelude::*;

static PLAYER_ADDED: SignalDesc = SignalDesc::new("PlayerAdded")
    .params(&[Param::new("player", Ty::Instance(Some("Player")))])
    .describe("A player joined the game.");
static PLAYER_REMOVING: SignalDesc = SignalDesc::new("PlayerRemoving")
    .params(&[Param::new("player", Ty::Instance(Some("Player")))])
    .describe("A player is leaving the game.");

pub static CLASS: ClassDesc = ClassDesc::new("Players", Some("Instance"))
    .flags(ClassFlags::SERVICE)
    .category("Services")
    .describe("Holds a Player for everyone in the game.")
    .signals(&[&PLAYER_ADDED, &PLAYER_REMOVING]);

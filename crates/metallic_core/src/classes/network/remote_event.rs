//! RemoteEvent: a reliable message channel.
use crate::classes::prelude::*;

static ON_SERVER_EVENT: SignalDesc = SignalDesc::new("OnServerEvent")
    .params(&[Param::new("player", Ty::Instance(Some("Player")))])
    .variadic(Ty::Any)
    .context(MemberContext::Server)
    .describe("A client called FireServer: the player who sent it, then what it sent.");
static ON_CLIENT_EVENT: SignalDesc = SignalDesc::new("OnClientEvent")
    .variadic(Ty::Any)
    .context(MemberContext::Client)
    .describe("The server called FireClient or FireAllClients: what it sent.");

pub static CLASS: ClassDesc = ClassDesc::new("RemoteEvent", Some("BaseRemoteEvent"))
    .flags(ClassFlags::CREATABLE)
    .category("Network")
    .insert_order(310)
    .describe("Sends messages between the server and clients, reliably and in order.")
    .signals(&[&ON_SERVER_EVENT, &ON_CLIENT_EVENT]);

//! RunService: the frame loop.
use crate::classes::prelude::*;

static HEARTBEAT: SignalDesc = SignalDesc::new("Heartbeat")
    .params(&[Param::new("deltaTime", Ty::Number)])
    .describe("Fires every frame, after inputs and before waiting threads resume.");

pub static CLASS: ClassDesc = ClassDesc::new("RunService", Some("Instance"))
    .flags(ClassFlags::SERVICE)
    .category("Services")
    .describe("The frame loop: Heartbeat and whether this is the server or a client.")
    .signals(&[&HEARTBEAT]);

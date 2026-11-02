//! Camera: the camera `workspace.CurrentCamera` returns.
//!
//! While `CameraType` is `Custom` the game's own camera publishes its pose here every frame; with
//! `Scriptable` (or `Fixed`) the script's `CFrame` and `FieldOfView` are the camera.
use crate::classes::prelude::*;

/// `Custom` is the game's own camera; `Scriptable` and `Fixed` leave the camera to the script's
/// `CFrame` and `FieldOfView`.
const CAMERA_TYPE: EnumDesc = EnumDesc::new("CameraType", &["Custom", "Scriptable", "Fixed"]);

pub static CLASS: ClassDesc = ClassDesc::new("Camera", Some("Instance"))
    .flags(ClassFlags::CREATABLE)
    .category("Base")
    .no_insert()
    .describe("The view a client renders the world from.")
    .enums(&[CAMERA_TYPE])
    // Each client has its own camera: nothing here is sent to a player.
    .props(&[
        PropDesc::new("CFrame", ValueType::CFrame, || {
            Value::CFrame(Box::new(CFrame::IDENTITY))
        })
        .server_only()
        .describe("Where the camera is and which way it looks."),
        num("FieldOfView", || Value::Number(70.0), 1.0, 120.0)
            .server_only()
            .describe("How wide the view is, in degrees."),
        enumerated(
            "CameraType",
            || Value::String("Custom".into()),
            &CAMERA_TYPE,
        )
        .server_only()
        .describe(
            "Custom follows the game's own camera. Scriptable and Fixed leave it to scripts.",
        ),
    ]);

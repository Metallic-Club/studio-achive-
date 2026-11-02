//! ColorGradingEffect: the tone curve.
use crate::classes::prelude::*;

/// `Default` and `Retro` are Roblox's names; the rest are Metallic additions
/// (`docs/research/2026-10-30-lighting.md` 5.4). `Default` follows the Look's curve (AgX Punchy under
/// Realistic); `AgXPunchy` names that curve explicitly (`docs/design/look-presets.md` conflict 14).
const TONEMAPPER_PRESET: EnumDesc = EnumDesc::new(
    "TonemapperPreset",
    &[
        "Default",
        "Retro",
        "AgX",
        "Neutral",
        "ACES",
        "Filmic",
        "AgXPunchy",
    ],
);

pub static CLASS: ClassDesc = ClassDesc::new("ColorGradingEffect", Some("Instance"))
    .flags(ClassFlags::CREATABLE)
    .category("Lighting")
    .no_insert()
    .describe("Chooses the tone curve of the final image.")
    .enums(&[TONEMAPPER_PRESET])
    .props(&[
        PropDesc::new("Enabled", ValueType::Bool, || Value::Bool(true))
            .editor(Editor::section(Section::BEHAVIOUR))
            .describe("Whether the effect is on."),
        enumerated(
            "TonemapperPreset",
            || Value::String("Default".into()),
            &TONEMAPPER_PRESET,
        )
        .describe("The tone curve that turns the scene's light into screen colours."),
    ]);

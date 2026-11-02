//! Lighting service: Roblox names, Metallic defaults and meanings
//! (docs/research/2026-10-30-lighting.md section 5). Presets: `crate::lighting`.
use crate::classes::prelude::*;

/// `Lighting.LightingStyle` values.
const LIGHTING_STYLE: EnumDesc = EnumDesc::new("LightingStyle", &["Realistic", "Soft"]);

pub static CLASS: ClassDesc = ClassDesc::new("Lighting", Some("Instance"))
    .flags(ClassFlags::SERVICE)
    .category("Services")
    .describe("The sun, ambient light, exposure and fog of the world; holds the effect classes.")
    .enums(&[LIGHTING_STYLE])
    .props(&[
        PropDesc::new("ClockTime", ValueType::Number, || Value::Number(14.5))
            .wrapped(0.0, 24.0)
            .describe("The time of day in hours; it places the sun and moon."),
        num("GeographicLatitude", || Value::Number(41.73), -90.0, 90.0)
            .editor(Editor::new().drag(NumberUnit::Degrees))
            .describe("How far north or south the place is, in degrees; it sets the sun's path."),
        num("Brightness", || Value::Number(1.0), 0.0, 10.0).describe("How strong the sunlight is."),
        num("ExposureCompensation", || Value::Number(0.0), -5.0, 5.0)
            .describe("Makes the whole image darker or brighter, in stops."),
        color("Ambient", || Value::Color3(BLACK))
            .describe("Extra light everywhere, indoors and out."),
        color("OutdoorAmbient", || Value::Color3(BLACK))
            .describe("Extra light in places open to the sky."),
        color("ColorShift_Top", || Value::Color3(BLACK)).describe("Tints the sunlight."),
        color("ColorShift_Bottom", || Value::Color3(BLACK))
            .describe("Tints the light bouncing up from the ground."),
        num("EnvironmentDiffuseScale", || Value::Number(1.0), 0.0, 1.0)
            .describe("How much soft light comes from the sky and surroundings."),
        num("EnvironmentSpecularScale", || Value::Number(1.0), 0.0, 1.0)
            .describe("How strongly surfaces reflect the sky and surroundings."),
        PropDesc::new("GlobalShadows", ValueType::Bool, || Value::Bool(true))
            .describe("Whether the sun and moon cast shadows."),
        num("ShadowSoftness", || Value::Number(0.2), 0.0, 1.0)
            .describe("How soft shadow edges are: 0 is sharp."),
        enumerated(
            "LightingStyle",
            || Value::String("Realistic".into()),
            &LIGHTING_STYLE,
        )
        .describe("The overall character of the light: Realistic or Soft."),
        color("FogColor", || {
            Value::Color3(Color3::new(192.0, 192.0, 192.0))
        })
        .describe("The colour of the distance fog."),
        num("FogStart", || Value::Number(0.0), 0.0, 1e7)
            .describe("How far from the camera the fog begins, in studs."),
        num("FogEnd", || Value::Number(100000.0), 0.0, 1e7)
            .describe("How far from the camera the fog is complete, in studs."),
        num("NightAdaptation", || Value::Number(1.0), 0.0, 1.0)
            .describe("How much the view adjusts to the dark and turns blue-grey at night."),
        num("MoonBrightness", || Value::Number(1.0), 0.0, 10.0)
            .describe("How strong the moonlight is."),
        // Histogram metering (#151; lighting doc 3.9 and 5.5). Off: the fixed exposure from
        // the sun elevation. EV100 limits of the meter.
        PropDesc::new("AutoExposure", ValueType::Bool, || Value::Bool(true)).describe(
            "Adjusts exposure to what is on screen, like an eye. Off, it follows the sun.",
        ),
        num("ExposureMin", || Value::Number(-4.0), -10.0, 20.0)
            .describe("The lowest exposure level automatic exposure may pick."),
        num("ExposureMax", || Value::Number(16.0), -10.0, 20.0)
            .describe("The highest exposure level automatic exposure may pick."),
        // EV added only while metering, so a look can match with auto exposure on and off.
        num("AutoExposureBias", || Value::Number(0.0), -5.0, 5.0)
            .describe("Stops added only while automatic exposure is on."),
    ]);

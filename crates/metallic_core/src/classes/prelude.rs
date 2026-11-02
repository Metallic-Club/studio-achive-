//! What a class file imports.
// No class file has a Native method until #465.
#[allow(unused_imports)]
pub use super::call::*;
pub use super::desc::*;
pub use super::lists::*;
pub use crate::cframe::CFrame;
pub use crate::value::{Color3, UDim2, Value, ValueType, Vec3};

/// The default part and frame colour.
pub const GREY: Color3 = Color3::new(163.0, 162.0, 165.0);
/// Black.
pub const BLACK: Color3 = Color3::new(0.0, 0.0, 0.0);
/// White.
pub const WHITE: Color3 = Color3::new(255.0, 255.0, 255.0);

/// A Number clamped to `min..=max`.
pub const fn num(name: &'static str, default: fn() -> Value, min: f64, max: f64) -> PropDesc {
    PropDesc::new(name, ValueType::Number, default).range(min, max)
}

/// A Color3 with components clamped to 0..=255.
pub const fn color(name: &'static str, default: fn() -> Value) -> PropDesc {
    PropDesc::new(name, ValueType::Color3, default).range(0.0, 255.0)
}

/// A String holding an item name of `desc`.
pub const fn enumerated(
    name: &'static str,
    default: fn() -> Value,
    desc: &'static EnumDesc,
) -> PropDesc {
    PropDesc::new(name, ValueType::String, default).enum_of(desc)
}

/// A String restricted to `allowed`.
pub const fn choice(
    name: &'static str,
    default: fn() -> Value,
    allowed: &'static [&'static str],
) -> PropDesc {
    PropDesc::new(name, ValueType::String, default).allowed(allowed)
}

/// `LookOverride` of `BasePart` and `Model` (`crate::look_override`; building-tools spec 12.2).
pub const fn look_override() -> PropDesc {
    use crate::look_override as look;
    choice(
        look::PROPERTY,
        || Value::String(look::INHERIT.into()),
        look::LOOK_OVERRIDES,
    )
    .normalized(look::look_override_id, look::NORMALIZATION_SCHEMA_ID)
    .editor(
        Editor::section(Section::APPEARANCE)
            .label("Style")
            .style_picker(),
    )
    .describe("The look of this object. Inherit takes it from its parent or the place.")
}

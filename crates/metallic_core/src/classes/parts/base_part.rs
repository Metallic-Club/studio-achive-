//! BasePart: everything physical in the world.
use crate::classes::prelude::*;

/// Materials a part may use.
pub const MATERIAL: EnumDesc = EnumDesc::new(
    "Material",
    &[
        "Plastic",
        "SmoothPlastic",
        "Metal",
        "Wood",
        "Concrete",
        "Glass",
        "Grass",
        "Brick",
        "Ice",
        "Sand",
    ],
);

/// Shapes a part may have (`Enum.PartType`, the name scripts know; the property stays `Shape`).
const SHAPE: EnumDesc = EnumDesc::new("PartType", &["Block", "Ball", "Cylinder", "Wedge"]);

pub static CLASS: ClassDesc = ClassDesc::new("BasePart", Some("PVInstance"))
    .flags(ClassFlags::ABSTRACT)
    .category("Parts")
    .describe("A physical object in the world: position, size, look and collision.")
    .enums(&[MATERIAL, SHAPE])
    .props(&[
        PropDesc::new("Position", ValueType::Vector3, || {
            Value::Vector3(Vec3::new(0.0, 0.0, 0.0))
        })
        .editor(Editor::section(Section::TRANSFORM))
        .describe("Where the centre of the part is in the world, in studs."),
        PropDesc::new("Orientation", ValueType::Vector3, || {
            Value::Vector3(Vec3::new(0.0, 0.0, 0.0))
        })
        .editor(Editor::section(Section::TRANSFORM).drag(NumberUnit::Degrees))
        .describe("How the part is turned around each axis, in degrees."),
        PropDesc::new("Size", ValueType::Vector3, || {
            Value::Vector3(Vec3::new(4.0, 1.0, 2.0))
        })
        .editor(Editor::section(Section::TRANSFORM))
        .describe("The width, height and depth of the part, in studs."),
        PropDesc::new("Color", ValueType::Color3, || Value::Color3(GREY))
            .editor(Editor::section(Section::APPEARANCE))
            .describe("The colour of the part."),
        enumerated("Material", || Value::String("Plastic".into()), &MATERIAL)
            .editor(Editor::section(Section::APPEARANCE))
            .describe("What the part is made of: how it looks, how heavy it is and how it grips."),
        PropDesc::new("Anchored", ValueType::Bool, || Value::Bool(false))
            .editor(Editor::section(Section::PHYSICS))
            .describe("An anchored part stays where it is: gravity and collisions do not move it."),
        PropDesc::new("CanCollide", ValueType::Bool, || Value::Bool(true))
            .editor(Editor::section(Section::PHYSICS))
            .describe("Whether characters and other parts bump into this part."),
        PropDesc::new("Transparency", ValueType::Number, || Value::Number(0.0))
            .editor(Editor::section(Section::APPEARANCE).drag(NumberUnit::Fraction))
            .describe("How see-through the part is: 0 is solid, 1 is invisible."),
        enumerated("Shape", || Value::String("Block".into()), &SHAPE)
            .editor(Editor::section(Section::APPEARANCE))
            .describe("The form of the part: block, ball, cylinder or wedge."),
        PropDesc::new("PivotOffset", ValueType::CFrame, || {
            Value::CFrame(Box::new(CFrame::IDENTITY))
        })
        .describe("Where the part's pivot sits, relative to its centre."),
        look_override(),
    ]);

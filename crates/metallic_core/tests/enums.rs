//! Enums in the class registry (#469): declared with a class or under `classes/enums/`, checked
//! when the registry resolves, and invisible to storage: an enum property stays a String.
use metallic_core::classes::{
    ClassDesc, EnumDesc, EnumItemRef, MemberKind, PropDesc, SchemaError, ScriptValue, Ty,
    find_property, registry, resolve, resolve_with_enums,
};
use metallic_core::*;

mod common;
use common::TestDir;

#[test]
fn enums_are_sorted_and_know_their_owner() {
    let enums = registry().enums();
    let names: Vec<&str> = enums.iter().map(|e| e.name()).collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted);
    for (index, resolved) in enums.iter().enumerate() {
        assert_eq!(usize::from(resolved.id.0), index);
    }
    let owner = |name: &str| registry().enum_by_name(name).unwrap().owner;
    assert_eq!(owner("Material"), Some("BasePart"));
    assert_eq!(owner("PartType"), Some("BasePart"));
    assert_eq!(owner("InputActionType"), Some("InputAction"));
    assert_eq!(owner("CameraType"), Some("Camera"));
    assert_eq!(owner("LightingStyle"), Some("Lighting"));
    assert_eq!(owner("TonemapperPreset"), Some("ColorGradingEffect"));
    assert_eq!(owner("KeyCode"), None);
    assert!(registry().enum_by_name("Nope").is_none());
}

/// Each property keeps the values it allowed before #469, now from its enum.
#[test]
fn each_enum_property_allows_exactly_its_items() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        (
            "Part",
            "Material",
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
        ),
        (
            "Part",
            "Shape",
            "PartType",
            &["Block", "Ball", "Cylinder", "Wedge"],
        ),
        (
            "InputAction",
            "Type",
            "InputActionType",
            &[
                "Bool",
                "Direction1D",
                "Direction2D",
                "Direction3D",
                "ViewportPosition",
            ],
        ),
        (
            "InputAction",
            "Trigger",
            "InputTrigger",
            &["Press", "Release", "Hold", "Tap", "DoubleTap", "Repeat"],
        ),
        (
            "InputAction",
            "Role",
            "InputRole",
            &[
                "None",
                "Move",
                "Look",
                "Jump",
                "Run",
                "Primary",
                "Secondary",
                "Interact",
                "Menu",
                "Zoom",
            ],
        ),
        (
            "InputAction",
            "TouchSlot",
            "TouchSlot",
            &["Auto", "Stick", "Look"],
        ),
        (
            "InputAction",
            "TouchLayout",
            "TouchLayout",
            &["Auto", "Custom"],
        ),
        (
            "Lighting",
            "LightingStyle",
            "LightingStyle",
            &["Realistic", "Soft"],
        ),
        (
            "Camera",
            "CameraType",
            "CameraType",
            &["Custom", "Scriptable", "Fixed"],
        ),
        (
            "ColorGradingEffect",
            "TonemapperPreset",
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
        ),
    ];
    for (class, property, enum_name, items) in cases {
        let def = find_property(class, property).unwrap();
        let desc = def
            .enum_type
            .unwrap_or_else(|| panic!("{class}.{property}"));
        assert_eq!(desc.name, *enum_name);
        assert_eq!(desc.items, *items);
        assert_eq!(def.allowed, Some(*items));
        assert_eq!(def.ty, ValueType::String);
    }
}

#[test]
fn a_property_outside_the_enums_stays_a_plain_string() {
    for (class, property) in [
        ("InputBinding", "KeyCode"),
        ("Part", "LookOverride"),
        ("Part", "Name"),
    ] {
        assert!(find_property(class, property).unwrap().enum_type.is_none());
    }
}

#[test]
fn values_default_to_position_and_numbers_override_them() {
    let material = registry().enum_by_name("Material").unwrap().desc;
    assert_eq!(material.value_at(0), 0);
    assert_eq!(material.value_at(2), 2);
    let priority = registry().enum_by_name("ContextActionPriority").unwrap();
    assert_eq!(priority.desc.value_at(1), 2000);
    assert_eq!(priority.item_index("Medium"), Some(1));
}

/// Storage is what it was: a String with the name, refused when it is no item, default not stored.
#[test]
fn storage_stays_the_name() {
    let mut world = World::new_with_services();
    let part = world.create("Part", world.root("Workspace")).unwrap();
    assert_eq!(
        world.get(part, "Material").unwrap(),
        Value::String("Plastic".into())
    );
    world
        .set(part, "Material", Value::String("Metal".into()))
        .unwrap();
    assert_eq!(
        world.get(part, "Material").unwrap(),
        Value::String("Metal".into())
    );
    let refused = world
        .set(part, "Material", Value::String("Lava".into()))
        .unwrap_err()
        .to_string();
    assert!(refused.contains("`Lava` is not one of"), "{refused}");
}

/// The saved text holds plain names, as scenes from before #469 do (`templates/` is the other
/// half: those files were not touched), and it loads and saves to the same bytes.
#[test]
fn a_save_stores_the_name_and_a_load_gives_the_same_scene() {
    let dir = TestDir::new("a_save_stores_the_name");
    let mut world = World::new_with_services();
    let part = world.create("Part", world.root("Workspace")).unwrap();
    world
        .set(part, "Material", Value::String("Glass".into()))
        .unwrap();
    world
        .set(part, "Shape", Value::String("Wedge".into()))
        .unwrap();
    let first = dir.file("first.ron");
    save_scene(&world, &first).unwrap();
    let text = std::fs::read_to_string(&first).unwrap();
    assert!(text.contains(r#"("Material", String("Glass"))"#), "{text}");
    assert!(text.contains(r#"("Shape", String("Wedge"))"#), "{text}");

    let loaded = load_scene(&first).unwrap();
    let workspace = loaded.root("Workspace").unwrap();
    let part = loaded.find_first_child(workspace, "Part").unwrap();
    assert_eq!(
        loaded.get(part, "Material").unwrap(),
        Value::String("Glass".into())
    );
    let second = dir.file("second.ron");
    save_scene(&loaded, &second).unwrap();
    assert_eq!(text, std::fs::read_to_string(&second).unwrap());
}

#[test]
fn templates_load_with_their_material_names() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
    let world = load_scene(&root.join("parkour/main.mscene")).unwrap();
    let mut seen = 0;
    let mut stack = vec![world.root("Workspace").unwrap()];
    while let Some(id) = stack.pop() {
        stack.extend(world.children(id));
        if world.is_a(id, "BasePart") {
            let Value::String(name) = world.get(id, "Material").unwrap() else {
                panic!("Material is not a String");
            };
            assert!(classes::MATERIALS.contains(&name.as_str()), "{name}");
            seen += 1;
        }
    }
    assert!(seen > 0);
}

// ---- Aliases and the resolution checks use fixtures: no built-in enum has an alias yet.

const OLD: EnumDesc =
    EnumDesc::new("Fixture", &["New", "Other"]).aliases(&[("Old", "New"), ("Older", "Other")]);

#[test]
fn an_alias_finds_the_same_item_and_is_what_a_write_stores() {
    static CLASS: ClassDesc = ClassDesc::new("Holder", None)
        .enums(&[OLD])
        .props(&[
            PropDesc::new("P", ValueType::String, || Value::String("New".into())).enum_of(&OLD),
        ]);
    let registry = resolve(&[&CLASS]).unwrap();
    let fixture = registry.enum_by_name("Fixture").unwrap();
    assert_eq!(fixture.owner, Some("Holder"));
    assert_eq!(fixture.item_index("Old"), fixture.item_index("New"));
    assert_eq!(fixture.item_index("Older"), Some(1));
    assert_eq!(fixture.item_index("Nope"), None);

    let (prop, _) = registry
        .class_by_name("Holder")
        .unwrap()
        .property("P")
        .unwrap();
    assert_eq!(prop.stored_form("Old"), Some("New"));
    assert_eq!(prop.stored_form("Nope"), None);
}

#[test]
fn a_normalizer_still_answers_first() {
    let prop = PropDesc::new("P", ValueType::String, || Value::String("New".into()))
        .enum_of(&OLD)
        .normalized(|_| Some("Other"), "fixture");
    assert_eq!(prop.stored_form("Old"), Some("Other"));
}

/// A class with this one enum, for the error cases.
macro_rules! class_with {
    ($desc:expr) => {{
        static CLASS: ClassDesc = ClassDesc::new("Holder", None).enums(&[$desc]);
        resolve(&[&CLASS]).map(|_| ()).unwrap_err()
    }};
}

#[test]
fn a_malformed_enum_is_a_schema_error() {
    assert_eq!(
        class_with!(EnumDesc::new("E", &[])),
        SchemaError::EnumItemCount("E")
    );
    assert_eq!(
        class_with!(EnumDesc::new("E", &["A", "A"])),
        SchemaError::DuplicateEnumItem {
            enum_name: "E",
            item: "A"
        }
    );
    assert_eq!(
        class_with!(EnumDesc::new("E", &["A", "B"]).aliases(&[("A", "B")])),
        SchemaError::DuplicateEnumItem {
            enum_name: "E",
            item: "A"
        }
    );
    assert_eq!(
        class_with!(EnumDesc::new("E", &["A"]).aliases(&[("Old", "Gone")])),
        SchemaError::AliasWithoutItem {
            enum_name: "E",
            alias: "Old"
        }
    );
    assert_eq!(
        class_with!(EnumDesc::new("E", &["A", "B"]).numbers(&[1])),
        SchemaError::BadEnumNumbers("E")
    );
    assert_eq!(
        class_with!(EnumDesc::new("E", &["A", "B"]).numbers(&[1, 1])),
        SchemaError::BadEnumNumbers("E")
    );
}

#[test]
fn two_enums_with_one_name_are_a_schema_error_even_across_owners() {
    static ONE: ClassDesc = ClassDesc::new("One", None).enums(&[EnumDesc::new("E", &["A"])]);
    static TWO: ClassDesc = ClassDesc::new("Two", None).enums(&[EnumDesc::new("E", &["B"])]);
    assert_eq!(
        resolve(&[&ONE, &TWO]).map(|_| ()).unwrap_err(),
        SchemaError::DuplicateEnum("E")
    );
    static SHARED: [EnumDesc; 1] = [EnumDesc::new("E", &["C"])];
    assert_eq!(
        resolve_with_enums(&[&ONE], &[&SHARED])
            .map(|_| ())
            .unwrap_err(),
        SchemaError::DuplicateEnum("E")
    );
}

#[test]
fn an_enum_property_must_use_a_declared_enum_and_default_to_an_item() {
    const E: EnumDesc = EnumDesc::new("E", &["A"]);
    const SAME_NAME: EnumDesc = EnumDesc::new("E", &["B"]);

    static UNDECLARED: ClassDesc =
        ClassDesc::new("Holder", None)
            .props(&[
                PropDesc::new("P", ValueType::String, || Value::String("A".into())).enum_of(&E),
            ]);
    assert_eq!(
        resolve(&[&UNDECLARED]).map(|_| ()).unwrap_err(),
        SchemaError::UndeclaredEnum {
            class: "Holder",
            property: "P",
            enum_name: "E"
        }
    );

    static DIFFERENT: ClassDesc = ClassDesc::new("Holder", None)
        .enums(&[SAME_NAME])
        .props(&[PropDesc::new("P", ValueType::String, || Value::String("A".into())).enum_of(&E)]);
    assert!(matches!(
        resolve(&[&DIFFERENT]).map(|_| ()).unwrap_err(),
        SchemaError::UndeclaredEnum { .. }
    ));

    static BAD_DEFAULT: ClassDesc = ClassDesc::new("Holder", None)
        .enums(&[E])
        .props(&[PropDesc::new("P", ValueType::String, || Value::String("Z".into())).enum_of(&E)]);
    assert_eq!(
        resolve(&[&BAD_DEFAULT]).map(|_| ()).unwrap_err(),
        SchemaError::EnumDefaultNotAnItem {
            class: "Holder",
            property: "P"
        }
    );

    // A subclass's `defaults` entry is checked too.
    static BASE: ClassDesc = ClassDesc::new("Base", None)
        .enums(&[E])
        .props(&[PropDesc::new("P", ValueType::String, || Value::String("A".into())).enum_of(&E)]);
    static SUB: ClassDesc =
        ClassDesc::new("Sub", Some("Base")).defaults(&[("P", || Value::String("Z".into()))]);
    assert_eq!(
        resolve(&[&BASE, &SUB]).map(|_| ()).unwrap_err(),
        SchemaError::EnumDefaultNotAnItem {
            class: "Sub",
            property: "P"
        }
    );
}

#[test]
fn shared_enums_resolve_without_an_owner() {
    static SHARED: [EnumDesc; 1] = [EnumDesc::new("E", &["A"])];
    let registry = resolve_with_enums(&[], &[&SHARED]).unwrap();
    assert_eq!(registry.enum_by_name("E").unwrap().owner, None);
}

// ---- The host interface (#461): a language sees items, storage keeps names.

#[test]
fn a_stored_name_crosses_the_host_interface_as_its_item() {
    let material = find_property("Part", "Material").unwrap();
    let wood = EnumItemRef::new("Material", "Wood");
    assert_eq!(
        material.script_value(&Value::String("Wood".into())),
        ScriptValue::EnumItem(wood)
    );
    assert_eq!(
        material.stored_item(wood),
        Some(Value::String("Wood".into()))
    );
    // Another enum's item and a made-up item are refused; the host reports the error.
    assert_eq!(
        material.stored_item(EnumItemRef::new("Shape", "Ball")),
        None
    );
    assert_eq!(
        material.stored_item(EnumItemRef::new("Material", "Lava")),
        None
    );

    // A property that is not an enum is what it always was.
    let name = find_property("Part", "Name").unwrap();
    assert_eq!(
        name.script_value(&Value::String("Wood".into())),
        ScriptValue::String("Wood".into())
    );
    assert_eq!(name.stored_item(wood), None);
}

#[test]
fn an_item_ref_from_an_alias_is_the_item() {
    assert_eq!(
        OLD.item_ref("Old"),
        Some(EnumItemRef::new("Fixture", "New"))
    );
    assert_eq!(
        OLD.item_ref("New"),
        Some(EnumItemRef::new("Fixture", "New"))
    );
    assert_eq!(OLD.item_ref("Nope"), None);
}

/// The argument check compares an item with `Ty::Enum` by enum name, so a typo in a signature would
/// refuse every value. No resolve check: fixture classes name enums their fixture does not declare.
#[test]
fn every_enum_a_built_in_signature_names_exists() {
    fn check(ty: &Ty, member: &str) {
        match ty {
            Ty::Enum(name) => assert!(
                registry().enum_by_name(name).is_some(),
                "{member} names Enum.{name}"
            ),
            Ty::Array(inner) | Ty::Map(inner) | Ty::Optional(inner) => check(inner, member),
            Ty::Tuple(items) => items.iter().for_each(|item| check(item, member)),
            Ty::Function(signature) => {
                signature.params.iter().for_each(|p| check(p, member));
                check(signature.returns, member);
            }
            _ => {}
        }
    }
    for class in registry().classes() {
        for member in &class.members {
            let name = member.name();
            match member.kind {
                MemberKind::Method { desc, .. } => {
                    desc.params.iter().for_each(|p| check(&p.ty, name));
                    desc.variadic.iter().for_each(|ty| check(ty, name));
                    check(&desc.returns, name);
                }
                MemberKind::Signal { desc, .. } => {
                    desc.params.iter().for_each(|p| check(&p.ty, name));
                }
                MemberKind::Callback { desc, .. } => {
                    desc.params.iter().for_each(|p| check(&p.ty, name));
                    check(&desc.returns, name);
                }
                MemberKind::Property { .. } => {}
            }
        }
    }
}

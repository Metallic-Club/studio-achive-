use metallic_core::classes::{self, Repl};
use metallic_core::replication::{
    instance_visible, property_value, property_visible, registry_hash,
};
use metallic_core::{Value, World};

#[test]
fn indices_cover_inheritance_and_resolve_back_to_the_same_definition() {
    for class in classes::registry_classes() {
        for (index, property) in classes::all_properties(class.name).iter().enumerate() {
            assert_eq!(
                classes::property_index(class.name, property.name),
                Some(index as u16)
            );
            assert!(std::ptr::eq(
                classes::property_by_index(class.name, index as u16).unwrap(),
                *property
            ));
        }
        assert!(classes::property_index(class.name, "NotAProperty").is_none());
        assert!(classes::property_by_index(class.name, u16::MAX).is_none());
    }
    assert!(classes::property_by_index("Missing", 0).is_none());
    assert!(classes::property_index("Missing", "Name").is_none());
    assert_eq!(classes::property_index("Part", "Name"), Some(0));
    assert_eq!(registry_hash(), registry_hash());
    assert_ne!(registry_hash().unwrap(), [0; 32]);
}

#[test]
fn core_only_classes_and_storage_service_keep_scene_values_typed() {
    let mut world = World::new_with_services();
    let storage = world.root("ServerStorage").unwrap();
    assert!(world.is_builtin(storage));
    let module = world.create("ModuleScript", Some(storage)).unwrap();
    world
        .set(module, "Source", Value::String("return 42".into()))
        .unwrap();
    assert_eq!(
        world.get(module, "Name").unwrap(),
        Value::String("ModuleScript".into())
    );
    for class in ["RemoteFunction", "UnreliableRemoteEvent", "PlayerScripts"] {
        let id = world
            .create(class, world.root("ReplicatedStorage"))
            .unwrap();
        assert_eq!(world.class_name(id), Some(class));
    }
    assert_eq!(
        classes::find_property("ModuleScript", "Source")
            .unwrap()
            .repl,
        Repl::Replicated
    );
}

#[test]
fn private_storage_is_class_based_and_reparenting_updates_visibility_immediately() {
    let mut world = World::new_with_services();
    let public = world.root("ReplicatedStorage").unwrap();
    let storage = world.root("ServerStorage").unwrap();
    let folder = world.create("Folder", Some(storage)).unwrap();
    let part = world.create("Part", Some(folder)).unwrap();
    world
        .set(folder, "Name", Value::String("Harmless name".into()))
        .unwrap();
    assert!(!instance_visible(&world, storage, None));
    assert!(!instance_visible(&world, part, None));
    world.set_parent(folder, Some(public)).unwrap();
    assert!(instance_visible(&world, part, None));
    world
        .set_parent(folder, world.root("ServerScriptService"))
        .unwrap();
    assert!(!instance_visible(&world, part, None));
    world.set_parent(folder, None).unwrap();
    assert!(!instance_visible(&world, part, None));
    let misleading = world.create("Folder", Some(public)).unwrap();
    world
        .set(misleading, "Name", Value::String("ServerStorage".into()))
        .unwrap();
    assert!(instance_visible(&world, misleading, None));
}

#[test]
fn server_script_source_is_hidden_without_hiding_client_or_module_source() {
    let mut world = World::new_with_services();
    let public = world.root("Workspace");
    for (class, expected) in [
        ("Script", false),
        ("LocalScript", true),
        ("ModuleScript", true),
    ] {
        let id = world.create(class, public).unwrap();
        world
            .set(id, "Source", Value::String("private source marker".into()))
            .unwrap();
        assert!(instance_visible(&world, id, None));
        assert!(property_visible(&world, id, "Name", None));
        assert_eq!(property_visible(&world, id, "Source", None), expected);
        assert_eq!(
            property_value(&world, id, "Source", None).is_some(),
            expected
        );
        world.set_parent(id, world.root("ServerStorage")).unwrap();
        assert!(!property_visible(&world, id, "Source", None));
    }
}

#[test]
fn private_player_subtrees_require_the_same_owner_at_every_boundary() {
    let mut world = World::new_with_services();
    let players = world.root("Players");
    let alice = world.create("Player", players).unwrap();
    let bob = world.create("Player", players).unwrap();
    for class in ["PlayerGui", "PlayerScripts"] {
        let container = world.create(class, Some(alice)).unwrap();
        let child = world.create("LocalScript", Some(container)).unwrap();
        assert!(instance_visible(&world, child, Some(alice)));
        assert!(!instance_visible(&world, child, Some(bob)));
        assert!(!instance_visible(&world, child, None));
        world.set_parent(container, Some(bob)).unwrap();
        assert!(!instance_visible(&world, child, Some(alice)));
        assert!(instance_visible(&world, child, Some(bob)));
        world
            .set_parent(container, world.root("Workspace"))
            .unwrap();
        assert!(!instance_visible(&world, child, Some(bob)));
    }
    let outer = world.create("PlayerGui", Some(alice)).unwrap();
    let nested_player = world.create("Player", Some(outer)).unwrap();
    let inner = world.create("PlayerScripts", Some(nested_player)).unwrap();
    assert!(!instance_visible(&world, inner, Some(nested_player)));
    assert!(!instance_visible(&world, inner, Some(alice)));
}

#[test]
fn outgoing_references_are_filtered_without_changing_authoritative_state() {
    let mut world = World::new_with_services();
    let players = world.root("Players");
    let alice = world.create("Player", players).unwrap();
    let bob = world.create("Player", players).unwrap();
    let gui = world.create("PlayerGui", Some(alice)).unwrap();
    let private = world.create("Part", Some(gui)).unwrap();
    world
        .set(alice, "Character", Value::Ref(Some(private)))
        .unwrap();
    assert_eq!(
        property_value(&world, alice, "Character", Some(alice)),
        Some(Value::Ref(Some(private)))
    );
    assert_eq!(
        property_value(&world, alice, "Character", Some(bob)),
        Some(Value::Ref(None))
    );
    assert_eq!(
        world.get(alice, "Character").unwrap(),
        Value::Ref(Some(private))
    );
    world.destroy(private);
    assert_eq!(
        property_value(&world, alice, "Character", Some(alice)),
        Some(Value::Ref(None))
    );
    assert!(!instance_visible(&world, private, Some(alice)));
}

#[test]
fn invalid_viewers_and_unknown_properties_fail_closed() {
    let mut world = World::new_with_services();
    let part = world.create("Part", world.root("Workspace")).unwrap();
    assert!(!instance_visible(&world, part, Some(part)));
    let player = world.create("Player", world.root("Players")).unwrap();
    world.destroy(player);
    assert!(!instance_visible(&world, part, Some(player)));
    assert!(!property_visible(&world, part, "Unknown", None));
    assert_eq!(property_value(&world, part, "Unknown", None), None);
    let orphan = world.create("Player", None).unwrap();
    assert!(!instance_visible(&world, part, Some(orphan)));
    // A duplicate service must either be refused by World or remain outside the trusted tree.
    let exposed = world
        .create("Workspace", None)
        .ok()
        .is_some_and(|fake| instance_visible(&world, fake, None));
    assert!(!exposed);
}
#[test]
fn boxed_pivot_values_and_hidden_bookkeeping_are_replicated_as_owned_data() {
    use metallic_core::{CFrame, Vec3};
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let part = world.create("Part", Some(workspace)).unwrap();
    let frame = CFrame::from_position(Vec3::new(1.0, 2.0, 3.0));
    world
        .set(part, "PivotOffset", Value::CFrame(Box::new(frame)))
        .unwrap();
    let Some(Value::CFrame(mut outgoing)) = property_value(&world, part, "PivotOffset", None)
    else {
        panic!("visible pivot value");
    };
    assert_eq!(*outgoing, frame);
    outgoing.position.x = 999.0;
    assert_eq!(
        world.get(part, "PivotOffset").unwrap(),
        Value::CFrame(Box::new(frame))
    );
    let model = world.create("Model", Some(workspace)).unwrap();
    let bookkeeping = metallic_core::classes::all_properties("Model")
        .into_iter()
        .find(|property| property.hidden)
        .unwrap();
    assert!(property_visible(&world, model, bookkeeping.name, None));
    assert_eq!(
        property_value(&world, model, bookkeeping.name, None),
        world.get(model, bookkeeping.name).ok()
    );
}
#[test]
fn lighting_bookkeeping_ranges_preserve_authoritative_values() {
    let mut world = World::new_with_services();
    let lighting = world.root("Lighting").unwrap();
    world
        .set(lighting, "ClockTime", Value::Number(25.0))
        .unwrap();
    assert_eq!(
        property_value(&world, lighting, "ClockTime", None),
        Some(Value::Number(1.0))
    );
    for name in ["Atmosphere", "ColorGradingEffect"] {
        let index = usize::from(classes::class_index(name).unwrap());
        assert_eq!(classes::registry_classes()[index].name, name);
    }
    let classes = classes::registry_classes();
    let lighting_end = classes
        .iter()
        .position(|class| class.name == "ColorGradingEffect")
        .unwrap();
    let storage = classes
        .iter()
        .position(|class| class.name == "ServerStorage")
        .unwrap();
    assert!(storage > lighting_end, "classes are sorted by name");
}

#[test]
fn gui_class_prefix_and_normalized_look_values_match_current_main() {
    for name in [
        "GuiButton",
        "TextButton",
        "ImageButton",
        "ModuleScript",
        "ServerStorage",
    ] {
        let index = usize::from(classes::class_index(name).unwrap());
        assert_eq!(classes::registry_classes()[index].name, name);
    }
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace").unwrap();
    let part = world.create("Part", Some(workspace)).unwrap();
    world
        .set(part, "LookOverride", Value::String("Retro / PS1".into()))
        .unwrap();
    assert_eq!(
        property_value(&world, part, "LookOverride", None),
        Some(Value::String("retro-32".into()))
    );
    let definition = classes::find_property("Part", "LookOverride").unwrap();
    assert_eq!(
        definition.normalize_schema_id,
        Some(metallic_core::look_override::NORMALIZATION_SCHEMA_ID)
    );
    let gui = world.create("TextButton", Some(workspace)).unwrap();
    assert!(property_visible(&world, gui, "Active", None));
    assert!(property_visible(&world, gui, "ZIndex", None));
    assert!(property_visible(&world, gui, "AutoButtonColor", None));
}

/// The golden schema fingerprint. New classes and properties replicate by default, so this is the
/// tripwire: any change to a class, a member (name, kind, signature, context; for a property also
/// type, default, order, range, allowed values, `hidden`, `repl`, normalizer contract) or the class
/// order changes the hash and fails here.
///
/// To update it on purpose: run `cargo run -p metallic_core --example registry_hash`, paste the
/// printed hex below, and say in the PR which schema change moved it (old and new peers then
/// refuse each other with `UpdateRequired`). If the change was not intended, fix the registry.
#[test]
fn registry_hash_is_pinned() {
    const GOLDEN: &str = "535d1a45d404161e9881e3e71c870ddd65a75b9f3d7b8fa686794cfce44b7f27";
    let hash = registry_hash().expect("the built-in registry is well formed");
    let hex: String = hash.iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(
        hex, GOLDEN,
        "the class registry changed; if intended, update GOLDEN (see this test's doc comment)"
    );
}

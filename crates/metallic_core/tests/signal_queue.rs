//! The signal queue and declarations in core (#463): what `World::fire` does before any script host
//! sees it, and that a declaration is a member like any other.

use metallic_core::classes::{
    self, ClassDesc, ClassFlags, MemberKind, Param, SignalDesc, SignalKey, Ty,
};
use metallic_core::{
    ConnectionId, MAX_QUEUED_SIGNALS, QueuedSignal, SignalQueue, Value, Vec3, World,
};

fn world_with_part() -> (World, metallic_core::InstanceId, SignalKey) {
    let mut world = World::new_with_services();
    let workspace = world.root("Workspace");
    let part = world.create("Part", workspace).unwrap();
    let class = classes::registry().class_by_name("Part").unwrap();
    let key = SignalKey {
        instance: part,
        signal: class.member("Touched").unwrap(),
        arg: 0,
    };
    (world, part, key)
}

#[test]
fn a_fire_with_no_subscriber_queues_nothing() {
    let (mut world, part, key) = world_with_part();
    world.fire(part, key.signal, vec![true.into()]);
    assert!(world.signals_mut().take().is_empty());
}

#[test]
fn a_watched_signal_queues_its_payload_in_order() {
    let (mut world, part, key) = world_with_part();
    world.signals_mut().watch(key);
    assert!(world.signal_is_watched(&key));
    world.fire(part, key.signal, vec![1.0.into()]);
    world.fire(part, key.signal, vec![2.0.into()]);
    assert_eq!(
        world.signals_mut().take(),
        [
            QueuedSignal {
                key,
                args: vec![1.0.into()]
            },
            QueuedSignal {
                key,
                args: vec![2.0.into()]
            },
        ]
    );
    assert!(world.signals_mut().take().is_empty(), "taken once");
}

#[test]
fn a_parameterized_key_is_its_own_subscription() {
    let (mut world, part, key) = world_with_part();
    let one = SignalKey { arg: 7, ..key };
    world.signals_mut().watch(one);
    world.fire(part, key.signal, Vec::new());
    assert!(
        world.signals_mut().take().is_empty(),
        "arg 0 is not watched"
    );
    world.fire_key(one, Vec::new());
    assert_eq!(world.signals_mut().take().len(), 1);
}

#[test]
fn a_destroyed_instance_fires_nothing() {
    let (mut world, part, key) = world_with_part();
    world.signals_mut().watch(key);
    world.destroy(part);
    world.fire(part, key.signal, Vec::new());
    assert!(world.signals_mut().take().is_empty());
}

#[test]
fn watching_counts_every_subscriber() {
    let (mut world, _, key) = world_with_part();
    let queue = world.signals_mut();
    queue.watch(key);
    queue.watch(key);
    queue.unwatch(key);
    assert!(queue.is_watched(&key), "the other VM still listens");
    queue.unwatch(key);
    assert!(!queue.is_watched(&key));
    queue.unwatch(key);
    assert!(!queue.is_watched(&key), "one too many is ignored");
}

#[test]
fn a_full_queue_drops_and_says_how_many() {
    let (world, _, key) = world_with_part();
    drop(world);
    let mut queue = SignalQueue::default();
    queue.watch(key);
    for _ in 0..MAX_QUEUED_SIGNALS + 3 {
        queue.fire(key, Vec::new());
    }
    assert_eq!(queue.take_dropped(), 3);
    assert_eq!(queue.take_dropped(), 0);
    assert_eq!(queue.take().len(), MAX_QUEUED_SIGNALS);
}

#[test]
fn a_deep_clone_starts_with_no_subscribers_and_nothing_queued() {
    let (mut world, part, key) = world_with_part();
    world.signals_mut().watch(key);
    world.fire(part, key.signal, Vec::new());
    let mut copy = world.deep_clone();
    assert!(!copy.signal_is_watched(&key));
    assert!(copy.signals_mut().take().is_empty());
}

#[test]
fn connection_identities_are_never_reissued() {
    assert_ne!(ConnectionId::allocate(), ConnectionId::allocate());
}

#[test]
fn property_changes_since_lists_each_real_write_in_order() {
    let (mut world, part, _) = world_with_part();
    let start = world.change_cursor();
    world.set(part, "Transparency", Value::Number(0.5)).unwrap();
    // Same value and refused writes are no change.
    world.set(part, "Transparency", Value::Number(0.5)).unwrap();
    assert!(world.set(part, "Transparency", Value::Bool(true)).is_err());
    let mid = world.change_cursor();
    world
        .set(part, "Position", Value::Vector3(Vec3::new(1.0, 2.0, 3.0)))
        .unwrap();
    world.set(part, "Transparency", Value::Number(0.0)).unwrap();
    // Not a property: a coarse reader sees it, this one does not.
    let child = world.create("Folder", Some(part)).unwrap();
    world.set_parent(child, None).unwrap();

    let names = |from| {
        world
            .property_changes_since(from)
            .unwrap()
            .map(|change| (change.id, change.prop.name))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(start),
        [
            (part, "Transparency"),
            (part, "Position"),
            (part, "Transparency")
        ]
    );
    assert_eq!(names(mid), [(part, "Position"), (part, "Transparency")]);
    let changes: Vec<_> = world.property_changes_since(mid).unwrap().collect();
    assert!(changes[0].at < changes[1].at);
    assert!(changes[0].at > mid);

    let other = World::new_with_services();
    assert!(
        other.property_changes_since(start).is_none(),
        "another world"
    );
    assert_eq!(start.partial_cmp(&other.change_cursor()), None);
}

#[test]
fn property_changes_since_reports_a_cursor_the_journal_no_longer_reaches() {
    let (mut world, part, _) = world_with_part();
    let start = world.change_cursor();
    for i in 0..20_000 {
        world
            .set(part, "Transparency", Value::Number(f64::from(i % 2) / 2.0))
            .unwrap();
    }
    assert!(world.property_changes_since(start).is_none());
}

#[test]
fn every_signal_and_native_method_of_the_built_in_classes_is_described() {
    for class in classes::registry().classes() {
        for member in &class.members {
            let description = match member.kind {
                MemberKind::Signal { desc, .. } => desc.description,
                MemberKind::Method { desc, .. } => desc.description,
                _ => continue,
            };
            assert!(
                !description.is_empty(),
                "{}.{} has no describe(...)",
                class.name(),
                member.name()
            );
        }
    }
}

static ROOT: ClassDesc = ClassDesc::new("Root", None).flags(ClassFlags::ABSTRACT);

#[test]
fn a_signal_is_inherited_with_one_id_and_redeclaring_it_is_an_error() {
    static FIRED: SignalDesc = SignalDesc::new("Fired")
        .params(&[Param::new("how", Ty::Number)])
        .describe("Text.");
    static BASE: ClassDesc = ClassDesc::new("Base", Some("Root")).signals(&[&FIRED]);
    static CHILD: ClassDesc = ClassDesc::new("Child", Some("Base"));
    static SIBLING: ClassDesc = ClassDesc::new("Sibling", Some("Root"));
    let registry = classes::resolve(&[&ROOT, &BASE, &CHILD, &SIBLING]).unwrap();
    let id = |class: &str| registry.class_by_name(class).unwrap().member("Fired");
    assert!(id("Base").is_some());
    assert_eq!(id("Base"), id("Child"));
    assert_eq!(id("Sibling"), None);
    assert_eq!(id("Root"), None);

    static AGAIN: ClassDesc = ClassDesc::new("Again", Some("Base")).signals(&[&FIRED]);
    assert!(classes::resolve(&[&ROOT, &BASE, &AGAIN]).is_err());
}

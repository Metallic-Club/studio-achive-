//! Core schema and visibility rules for a future server-to-client replicator.
//!
//! These functions do not authenticate a connection or send data. The server must supply a
//! viewer Player from its trusted, same-world connection roster. No viewer means public-only
//! observation; it never grants access to a player's private containers.
//! This is the play-client policy, not the separately authorized edit-session snapshot policy.

use crate::classes::{
    self, EnumDesc, Member, MemberContext, MemberKind, MethodImpl, Param, PropAccess, PropDef,
    Registry, Repl, Ty,
};
use crate::{InstanceId, Value, World};
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

pub use crate::classes::SchemaError;

/// Fingerprint of class order, inheritance, flags, every member in id order with its signature and
/// context, effective property defaults and replication/hidden metadata, and every enum in id order
/// with its items, numbers and aliases. Includes declared
/// string-normalization contract ids, numeric clamp/wrap ranges, and boxed CFrames by their 12
/// row-major components, never their allocation address. Descriptions, categories and function
/// addresses are not hashed. All lengths and numeric bits use little endian.
///
/// Protocol peers must compare this hash before interpreting class or property indices. It is a
/// compatibility fingerprint, not an authentication credential or a world-state hash.
///
/// Computed once. Errors: a [`SchemaError`] when the built-in classes do not resolve. The tests
/// `registry_hash_is_pinned` and `every_discovered_class_resolves` keep them valid; a peer that
/// still gets an error must refuse to connect rather than guess.
pub fn registry_hash() -> Result<[u8; 32], SchemaError> {
    static HASH: LazyLock<Result<[u8; 32], SchemaError>> =
        LazyLock::new(|| match classes::registry_error() {
            Some(error) => Err(error),
            None => Ok(hash_registry(classes::registry())),
        });
    *HASH
}

fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_le_bytes());
    hash.update(bytes);
}

fn hash_value(hash: &mut Sha256, value: &Value) {
    field(hash, value.value_type().name().as_bytes());
    let mut number = |v: f64| hash.update(v.to_bits().to_le_bytes());
    match value {
        Value::Nil => {}
        Value::Bool(v) => hash.update([u8::from(*v)]),
        Value::Number(v) => number(*v),
        Value::String(v) => field(hash, v.as_bytes()),
        Value::Vector3(v) => {
            number(v.x);
            number(v.y);
            number(v.z);
        }
        Value::Color3(v) => {
            number(v.r);
            number(v.g);
            number(v.b);
        }
        Value::UDim2(v) => {
            number(v.x_scale);
            number(v.x_offset);
            number(v.y_scale);
            number(v.y_offset);
        }
        Value::CFrame(frame) => {
            for component in frame.components() {
                number(component);
            }
        }
        Value::Ref(v) => {
            hash.update([u8::from(v.is_some())]);
            if let Some(id) = v {
                hash.update(id.to_bits().to_le_bytes());
            }
        }
    }
}

fn optional_field(hash: &mut Sha256, text: Option<&str>) {
    hash.update([u8::from(text.is_some())]);
    if let Some(text) = text {
        field(hash, text.as_bytes());
    }
}

fn hash_context(hash: &mut Sha256, context: MemberContext) {
    hash.update([match context {
        MemberContext::Any => 0,
        MemberContext::Server => 1,
        MemberContext::Client => 2,
    }]);
}

fn hash_ty(hash: &mut Sha256, ty: &Ty) {
    let tag = match ty {
        Ty::Nil => 0,
        Ty::Bool => 1,
        Ty::Int => 2,
        Ty::Number => 3,
        Ty::String => 4,
        Ty::Instance(_) => 5,
        Ty::Vector3 => 6,
        Ty::CFrame => 7,
        Ty::Color3 => 8,
        Ty::UDim2 => 9,
        Ty::Enum(_) => 10,
        Ty::Array(_) => 11,
        Ty::Map(_) => 12,
        Ty::Function(_) => 13,
        Ty::Optional(_) => 14,
        Ty::Tuple(_) => 15,
        Ty::Any => 16,
    };
    hash.update([tag]);
    match ty {
        Ty::Instance(class) => optional_field(hash, *class),
        Ty::Enum(name) => field(hash, name.as_bytes()),
        Ty::Array(inner) | Ty::Map(inner) | Ty::Optional(inner) => hash_ty(hash, inner),
        Ty::Function(signature) => {
            hash_tys(hash, signature.params);
            hash_ty(hash, signature.returns);
        }
        Ty::Tuple(items) => hash_tys(hash, items),
        _ => {}
    }
}

fn hash_tys(hash: &mut Sha256, items: &[Ty]) {
    hash.update((items.len() as u64).to_le_bytes());
    for item in items {
        hash_ty(hash, item);
    }
}

fn hash_params(hash: &mut Sha256, params: &[Param]) {
    hash.update((params.len() as u64).to_le_bytes());
    for param in params {
        field(hash, param.name.as_bytes());
        hash_ty(hash, &param.ty);
        hash.update([u8::from(param.optional)]);
    }
}

fn hash_property(hash: &mut Sha256, property: &PropDef, default: &Value) {
    field(hash, property.ty.name().as_bytes());
    hash.update([match property.repl {
        Repl::Replicated => 0,
        Repl::ServerOnly => 1,
    }]);
    hash.update([u8::from(property.range.is_some())]);
    if let Some(range) = property.range {
        hash.update(range.min.to_bits().to_le_bytes());
        hash.update(range.max.to_bits().to_le_bytes());
        hash.update([u8::from(range.wrap)]);
    }
    // Resolution checked that the normalizer and its contract id come together.
    optional_field(hash, property.normalize_schema_id);
    hash.update([u8::from(property.allowed.is_some())]);
    if let Some(values) = property.allowed {
        hash.update((values.len() as u64).to_le_bytes());
        for value in values {
            field(hash, value.as_bytes());
        }
    }
    hash.update([u8::from(property.readonly)]);
    // #464 adds computed properties as 1 (read-only) and 2 (settable).
    hash.update([match property.access {
        PropAccess::Stored => 0,
    }]);
    hash.update([u8::from(property.saved)]);
    optional_field(hash, property.enum_type.map(|desc| desc.name));
    optional_field(hash, property.deprecated);
    hash_value(hash, default);
}

fn hash_member(hash: &mut Sha256, member: &Member, owner: &str, defaults: &[Value]) {
    hash.update([match member.kind {
        MemberKind::Property { .. } => 0,
        MemberKind::Method { .. } => 1,
        MemberKind::Signal { .. } => 2,
        MemberKind::Callback { .. } => 3,
    }]);
    field(hash, member.name().as_bytes());
    let aliases = member.aliases();
    hash.update((aliases.len() as u64).to_le_bytes());
    for alias in aliases {
        field(hash, alias.as_bytes());
    }
    field(hash, owner.as_bytes());
    hash_context(hash, member.context);
    hash.update([u8::from(member.hidden)]);
    match member.kind {
        MemberKind::Property { desc, index } => {
            hash_property(hash, desc, &defaults[usize::from(index.0)]);
        }
        MemberKind::Method { desc, .. } => {
            hash_params(hash, desc.params);
            hash.update([u8::from(desc.variadic.is_some())]);
            if let Some(variadic) = &desc.variadic {
                hash_ty(hash, variadic);
            }
            hash_ty(hash, &desc.returns);
            hash.update([u8::from(desc.yields)]);
            // Where the body lives, never which function: an address differs per build.
            hash.update([match desc.implementation {
                MethodImpl::Native(_) => 0,
                MethodImpl::Host => 1,
                MethodImpl::Luau => 2,
            }]);
            optional_field(hash, desc.deprecated);
        }
        MemberKind::Signal { desc, .. } => {
            hash_params(hash, desc.params);
            hash.update([u8::from(desc.parameterized)]);
            optional_field(hash, desc.deprecated);
        }
        MemberKind::Callback { desc, .. } => {
            hash_params(hash, desc.params);
            hash_ty(hash, &desc.returns);
        }
    }
}

/// Who declares an enum and its description are not hashed: moving it changes no peer.
fn hash_enum(hash: &mut Sha256, desc: &EnumDesc) {
    field(hash, desc.name.as_bytes());
    hash.update((desc.items.len() as u64).to_le_bytes());
    for (index, item) in desc.items.iter().enumerate() {
        field(hash, item.as_bytes());
        hash.update(desc.value_at(index).to_le_bytes());
    }
    hash.update((desc.aliases.len() as u64).to_le_bytes());
    for (alias, item) in desc.aliases {
        field(hash, alias.as_bytes());
        field(hash, item.as_bytes());
    }
}

fn hash_registry(registry: &Registry) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"metallic-registry-v5\0");
    hash.update((registry.classes().len() as u64).to_le_bytes());
    for class in registry.classes() {
        field(&mut hash, class.name().as_bytes());
        optional_field(&mut hash, class.desc.superclass);
        hash.update([class.desc.flags.bits()]);
        optional_field(&mut hash, class.desc.deprecated_by);
        hash_context(&mut hash, class.desc.context);
        hash.update((class.members.len() as u64).to_le_bytes());
        for member in &class.members {
            let owner = registry.resolved(member.owner).name();
            hash_member(&mut hash, member, owner, &class.defaults);
        }
    }
    hash.update((registry.enums().len() as u64).to_le_bytes());
    for resolved in registry.enums() {
        hash_enum(&mut hash, resolved.desc);
    }
    hash.finalize().into()
}

fn owner_of(world: &World, container: InstanceId) -> Option<InstanceId> {
    let mut current = world.parent(container);
    while let Some(id) = current {
        if world.class_name(id)? == "Player" {
            return Some(id);
        }
        current = world.parent(id);
    }
    None
}

/// Whether an instance belongs in the viewer's replicated tree.
///
/// ServerStorage/ServerScriptService subtrees, orphan roots and another player's private
/// containers are excluded. Privacy is class-based, so renaming a container cannot expose it.
/// The query is uncached: a reparent is reflected immediately. Invalid viewers fail closed.
pub fn instance_visible(world: &World, id: InstanceId, viewer: Option<InstanceId>) -> bool {
    if viewer.is_some_and(|player| {
        world.class_name(player) != Some("Player")
            || !world.root("Players").is_some_and(|players| {
                world.is_builtin(players) && world.parent(player) == Some(players)
            })
    }) {
        return false;
    }
    let mut current = Some(id);
    while let Some(node) = current {
        let Some(class) = world.class_name(node) else {
            return false;
        };
        if classes::is_subclass(class, "ServerStorage")
            || classes::is_subclass(class, "ServerScriptService")
        {
            return false;
        }
        if (classes::is_subclass(class, "PlayerGui")
            || classes::is_subclass(class, "PlayerScripts"))
            && (viewer.is_none() || owner_of(world, node) != viewer)
        {
            return false;
        }
        current = world.parent(node);
        if current.is_none() {
            return world.is_builtin(node)
                && matches!(
                    class,
                    "Workspace"
                        | "ReplicatedStorage"
                        | "Players"
                        | "Lighting"
                        | "StarterGui"
                        | "StarterPlayer"
                );
        }
    }
    false
}

fn property_allowed(class: &str, property: &PropDef) -> bool {
    property.repl == Repl::Replicated
        && !(property.name == "Source"
            && classes::is_subclass(class, "Script")
            && !classes::is_subclass(class, "LocalScript"))
}

/// Whether a property may be sent. A server Script's Source is private even under Workspace;
/// LocalScript and ModuleScript source remains eligible in visible containers.
pub fn property_visible(
    world: &World,
    id: InstanceId,
    property: &str,
    viewer: Option<InstanceId>,
) -> bool {
    if !instance_visible(world, id, viewer) {
        return false;
    }
    let Some(class) = world.class_name(id) else {
        return false;
    };
    classes::find_property(class, property).is_some_and(|def| property_allowed(class, def))
}

/// Copies one outgoing property after applying visibility. Properties excluded by replication/instance visibility return None;
/// references to invisible/destroyed targets become Ref(None). This does not mutate the world.
pub fn property_value(
    world: &World,
    id: InstanceId,
    property: &str,
    viewer: Option<InstanceId>,
) -> Option<Value> {
    if !property_visible(world, id, property, viewer) {
        return None;
    }
    let value = world.get(id, property).ok()?;
    Some(match value {
        Value::Ref(Some(target)) if !instance_visible(world, target, viewer) => Value::Ref(None),
        value => value,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ValueType;
    use crate::classes::ClassDef;

    /// Resolves and hashes a fixture set. Fixtures are built at run time, and resolved classes
    /// need `'static` descriptors, so they leak; these are tests.
    fn hash_schema(classes: &[ClassDef]) -> Result<[u8; 32], SchemaError> {
        let classes: Vec<&'static ClassDef> = classes
            .iter()
            .map(|class| &*Box::leak(Box::new(*class)))
            .collect();
        classes::resolve(&classes).map(|registry| hash_registry(&registry))
    }

    fn ok_hash(classes: &[ClassDef]) -> [u8; 32] {
        hash_schema(classes).unwrap()
    }

    fn base(prop: PropDef) -> ClassDef {
        ClassDef::new("Fixture", None).props(Box::leak(vec![prop].into_boxed_slice()))
    }

    fn prop() -> PropDef {
        PropDef::new("Flag", ValueType::Bool, || Value::Bool(false))
    }

    #[test]
    fn fingerprint_covers_property_metadata_defaults_order_and_class_identity() {
        let original = ok_hash(&[base(prop())]);
        assert_eq!(original, ok_hash(&[base(prop())]));
        // Independent fixture for version 5's explicit byte encoding: the stream written by hand
        // with `printf` and piped through `sha256sum`, no Rust involved.
        assert_eq!(
            original,
            [
                0xcc, 0xc9, 0x95, 0x37, 0x29, 0x42, 0x9e, 0x63, 0xa5, 0xdc, 0x71, 0xf0, 0x11, 0x56,
                0x82, 0xea, 0x83, 0x8c, 0xa0, 0x8e, 0xa6, 0x33, 0x4d, 0x36, 0x56, 0x34, 0x9d, 0xcc,
                0xc4, 0xff, 0x69, 0x52
            ]
        );
        let mut changed = prop();
        changed.repl = Repl::ServerOnly;
        assert_ne!(original, ok_hash(&[base(changed)]));
        let mut changed = prop();
        changed.hidden = true;
        assert_ne!(original, ok_hash(&[base(changed)]));
        let mut changed = prop();
        changed.default = || Value::Bool(true);
        assert_ne!(original, ok_hash(&[base(changed)]));
        let mut changed = prop();
        changed.ty = ValueType::String;
        assert_ne!(original, ok_hash(&[base(changed)]));
        let mut changed = prop();
        changed.allowed = Some(&["A", "B"]);
        assert_ne!(original, ok_hash(&[base(changed)]));
        let mut changed = prop();
        changed.name = "Other";
        assert_ne!(original, ok_hash(&[base(changed)]));
        assert_ne!(
            original,
            ok_hash(&[base(prop()), ClassDef::new("Added", None)])
        );
        let mut changed = base(prop());
        changed.name = "Changed";
        assert_ne!(original, ok_hash(&[changed]));
    }

    #[test]
    fn fingerprint_covers_enums_but_not_their_owner_or_description() {
        use crate::classes::EnumDesc;
        fn hash(list: &'static [EnumDesc]) -> [u8; 32] {
            let shared: &'static [EnumDesc] = list;
            hash_registry(&classes::resolve_with_enums(&[], &[shared]).unwrap())
        }
        static PLAIN: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "B"])];
        let plain = hash(&PLAIN);

        static RENAMED: [EnumDesc; 1] = [EnumDesc::new("F", &["A", "B"])];
        static ITEM: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "C"])];
        static ORDER: [EnumDesc; 1] = [EnumDesc::new("E", &["B", "A"])];
        static NUMBERS: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "B"]).numbers(&[5, 6])];
        static ALIAS: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "B"]).aliases(&[("Old", "A")])];
        for changed in [&RENAMED, &ITEM, &ORDER, &NUMBERS, &ALIAS] {
            assert_ne!(plain, hash(changed), "{changed:?}");
        }

        // The default numbers written out are the same enum.
        static SAME_NUMBERS: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "B"]).numbers(&[0, 1])];
        assert_eq!(plain, hash(&SAME_NUMBERS));
        static DESCRIBED: [EnumDesc; 1] = [EnumDesc::new("E", &["A", "B"]).describe("Text.")];
        assert_eq!(plain, hash(&DESCRIBED));
        static OWNER: ClassDef = ClassDef::new("Owner", None).enums(&PLAIN);
        static NO_ENUM: ClassDef = ClassDef::new("Owner", None);
        assert_eq!(
            hash_registry(&classes::resolve(&[&OWNER]).unwrap()),
            hash_registry(&classes::resolve_with_enums(&[&NO_ENUM], &[&PLAIN]).unwrap()),
        );
    }

    #[test]
    fn fingerprint_covers_flags_and_signatures_but_not_descriptions() {
        use crate::classes::{ClassFlags, MemberContext, MethodDesc, Param, SignalDesc, Ty};
        static SIGNAL: SignalDesc = SignalDesc::new("Fired");
        static SIGNAL_WITH_PAYLOAD: SignalDesc =
            SignalDesc::new("Fired").params(&[Param::new("amount", Ty::Number)]);
        static DESCRIBED_SIGNAL: SignalDesc = SignalDesc::new("Fired").describe("Text.");
        static PLAIN: [&SignalDesc; 1] = [&SIGNAL];
        static PAYLOAD: [&SignalDesc; 1] = [&SIGNAL_WITH_PAYLOAD];
        static DESCRIBED: [&SignalDesc; 1] = [&DESCRIBED_SIGNAL];
        let method = MethodDesc::host("Do");
        let class = |methods: &'static [MethodDesc], signals: &'static [&'static SignalDesc]| {
            ClassDef::new("Fixture", None)
                .methods(methods)
                .signals(signals)
        };
        let leak = |method| -> &'static [MethodDesc] { Box::leak(vec![method].into_boxed_slice()) };
        let original = ok_hash(&[class(leak(method), &PLAIN)]);
        assert_eq!(original, ok_hash(&[class(leak(method), &PLAIN)]));
        for changed in [
            method.returns(Ty::Bool),
            method.params(const { &[Param::new("x", Ty::Number)] }),
            method.params(const { &[Param::new("x", Ty::Number).optional()] }),
            method.params(const { &[Param::new("y", Ty::Number)] }),
            method.context(MemberContext::Server),
            MethodDesc {
                yields: true,
                ..method
            },
            MethodDesc {
                variadic: Some(Ty::Any),
                ..method
            },
            MethodDesc {
                implementation: crate::classes::MethodImpl::Luau,
                ..method
            },
        ] {
            assert_ne!(original, ok_hash(&[class(leak(changed), &PLAIN)]));
        }
        assert_ne!(original, ok_hash(&[class(leak(method), &PAYLOAD)]));
        assert_ne!(
            original,
            ok_hash(&[class(leak(method), &PLAIN).flags(ClassFlags::ABSTRACT)])
        );
        // A changed tooltip must not split peers.
        assert_eq!(
            original,
            ok_hash(&[class(leak(method.describe("Text.")), &DESCRIBED)
                .describe("Text.")
                .category("Text")])
        );
    }

    #[test]
    fn fingerprint_covers_where_a_method_lives_but_not_which_function() {
        use crate::classes::{ApiError, CallOutcome, MethodCall, MethodDesc, ScriptValue};
        fn one(_: &mut MethodCall<'_>) -> Result<CallOutcome, ApiError> {
            Ok(ScriptValue::Nil.into())
        }
        fn other(_: &mut MethodCall<'_>) -> Result<CallOutcome, ApiError> {
            Ok(true.into())
        }
        static NATIVE: [MethodDesc; 1] = [MethodDesc::native("Do", one)];
        static OTHER_BODY: [MethodDesc; 1] = [MethodDesc::native("Do", other)];
        static HOST: [MethodDesc; 1] = [MethodDesc::host("Do")];
        let hash = |methods| ok_hash(&[ClassDef::new("Fixture", None).methods(methods)]);
        assert_ne!(hash(&NATIVE), hash(&HOST));
        assert_eq!(hash(&NATIVE), hash(&OTHER_BODY));
    }

    #[test]
    fn normalization_contract_ids_are_hashed_without_executing_callbacks() {
        fn callback(_: &str) -> Option<&'static str> {
            panic!("schema hashing must not execute normalization");
        }
        let schema = |id: Option<&'static str>| {
            let mut definition = prop();
            definition.ty = ValueType::String;
            definition.default = || Value::String("stored".into());
            definition.normalize = id.map(|_| callback as fn(&str) -> Option<&'static str>);
            definition.normalize_schema_id = id;
            ok_hash(&[base(definition)])
        };
        let v1 = schema(Some("fixture-normalization/v1"));
        assert_eq!(v1, schema(Some("fixture-normalization/v1")));
        assert_ne!(v1, schema(None));
        assert_ne!(v1, schema(Some("fixture-normalization/v2")));
        for class in classes::registry_classes() {
            for definition in class.props {
                assert_eq!(
                    definition.normalize.is_some(),
                    definition.normalize_schema_id.is_some()
                );
            }
        }
    }

    #[test]
    fn unversioned_normalizer_fails_closed() {
        let mut definition = prop();
        definition.normalize = Some(|_| Some("stored"));
        assert_eq!(
            hash_schema(&[base(definition)]),
            Err(SchemaError::UnpairedNormalizer {
                class: "Fixture",
                property: "Flag"
            })
        );
    }

    #[test]
    fn orphan_normalization_id_fails_closed() {
        let mut definition = prop();
        definition.normalize_schema_id = Some("orphan/v1");
        assert_eq!(
            hash_schema(&[base(definition)]),
            Err(SchemaError::UnpairedNormalizer {
                class: "Fixture",
                property: "Flag"
            })
        );
    }

    #[test]
    fn empty_normalization_id_fails_closed() {
        let mut definition = prop();
        definition.normalize = Some(|_| Some("stored"));
        definition.normalize_schema_id = Some("");
        assert_eq!(
            hash_schema(&[base(definition)]),
            Err(SchemaError::EmptyNormalizationId {
                class: "Fixture",
                property: "Flag"
            })
        );
    }

    #[test]
    fn malformed_inheritance_is_an_error_not_a_panic() {
        let orphan = ClassDef::new("Orphan", Some("Missing"));
        assert_eq!(
            hash_schema(&[orphan]),
            Err(SchemaError::UnknownSuperclass {
                class: "Orphan",
                superclass: "Missing"
            })
        );
        let a = ClassDef::new("A", Some("B"));
        let b = ClassDef::new("B", Some("A"));
        assert_eq!(
            hash_schema(&[a, b]),
            Err(SchemaError::InheritanceCycle("A"))
        );
    }

    #[test]
    fn numeric_range_bounds_wrap_and_presence_are_fingerprinted() {
        use crate::classes::NumRange;
        let schema = |range| {
            let mut definition = prop();
            definition.ty = ValueType::Number;
            definition.default = || Value::Number(0.0);
            definition.range = range;
            ok_hash(&[base(definition)])
        };
        let range = NumRange {
            min: -1.0,
            max: 1.0,
            wrap: false,
        };
        let original = schema(Some(range));
        assert_ne!(schema(None), original);
        assert_ne!(schema(Some(NumRange { min: -2.0, ..range })), original);
        assert_ne!(schema(Some(NumRange { max: 2.0, ..range })), original);
        assert_ne!(
            schema(Some(NumRange {
                wrap: true,
                ..range
            })),
            original
        );
        assert_ne!(
            schema(Some(NumRange { min: 0.0, ..range })),
            schema(Some(NumRange { min: -0.0, ..range }))
        );
    }

    #[test]
    fn boxed_cframe_hash_covers_all_components_and_numeric_bits() {
        use crate::{CFrame, Vec3};
        let digest = |frame: CFrame| -> [u8; 32] {
            let mut hash = Sha256::new();
            hash_value(&mut hash, &Value::CFrame(Box::new(frame)));
            hash.finalize().into()
        };
        let frame = CFrame::from_parts(
            Vec3::new(1.25, -2.5, 3.75),
            [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
        );
        // Independent Python SHA-256 over type tag plus 12 row-major LE doubles.
        assert_eq!(
            digest(frame),
            [
                0xde, 0x81, 0x8e, 0x00, 0xee, 0x3e, 0x96, 0xe8, 0x1f, 0x1b, 0x70, 0xc4, 0x27, 0x96,
                0xd2, 0x4a, 0x76, 0xb9, 0xcb, 0x0b, 0xcb, 0x7f, 0x0f, 0xf0, 0x7e, 0x42, 0x38, 0xfe,
                0xb8, 0x83, 0xac, 0x79
            ]
        );
        for index in 0..12 {
            let mut components = frame.components();
            components[index] += 0.25;
            let changed = CFrame::from_parts(
                Vec3::new(components[0], components[1], components[2]),
                [
                    [components[3], components[4], components[5]],
                    [components[6], components[7], components[8]],
                    [components[9], components[10], components[11]],
                ],
            );
            assert_ne!(digest(frame), digest(changed), "component {index}");
        }
        let mut negative_zero = CFrame::IDENTITY;
        negative_zero.position.x = -0.0;
        assert_ne!(digest(CFrame::IDENTITY), digest(negative_zero));
    }

    #[test]
    fn server_only_metadata_is_enforced_before_source_special_cases() {
        let mut def = prop();
        def.repl = Repl::ServerOnly;
        assert!(!property_allowed("Part", &def));
        assert!(!property_allowed("LocalScript", &def));
        def.repl = Repl::Replicated;
        def.hidden = true;
        assert!(property_allowed("Part", &def)); // UI/Luau hidden is not confidential.
    }

    #[test]
    fn adding_or_reordering_properties_and_changing_inheritance_changes_hash() {
        let single = ok_hash(&[base(prop())]);
        let mut extra = prop();
        extra.name = "Extra";
        let pair =
            ClassDef::new("Fixture", None).props(Box::leak(vec![prop(), extra].into_boxed_slice()));
        let pair_hash = ok_hash(&[pair]);
        assert_ne!(single, pair_hash);
        let mut extra = prop();
        extra.name = "Extra";
        let reversed =
            ClassDef::new("Fixture", None).props(Box::leak(vec![extra, prop()].into_boxed_slice()));
        assert_ne!(pair_hash, ok_hash(&[reversed]));
        let schema = |parent| {
            [
                base(prop()),
                ClassDef::new("OtherBase", None),
                ClassDef::new("Derived", Some(parent)),
            ]
        };
        assert_ne!(ok_hash(&schema("Fixture")), ok_hash(&schema("OtherBase")));
    }
}

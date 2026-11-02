//! The class registry: every class scripts and scene files see, its superclass, its flags and its
//! members. Design: `docs/design/class-system.md`.
//!
//! One file per class under `src/classes/<group>/<class>.rs` exports `pub static CLASS: ClassDesc`
//! ([`ClassDesc`]). `build.rs` discovers those files; [`resolve()`] flattens them once into
//! [`registry`]. Lookups then use a [`ClassId`] and a per-class name table and never walk the
//! superclass chain. The functions below ([`class_def`], [`find_property`], [`is_subclass`],
//! [`all_properties`], ...) are thin wrappers over it, kept so callers can migrate later.
//!
//! Methods use a language-neutral calling convention ([`MethodCall`], [`ScriptValue`],
//! [`CallOutcome`], [`ApiError`]; `call.rs` has "How to write a method"), and every language binding
//! goes through one interface ([`HostApi`]). This crate knows no scripting language.
//!
//! Signals are members too ([`SignalDesc`], "How to add a signal" in `ARCHITECTURE.md`): keyed by
//! [`SignalKey`], fired with [`World::fire`](crate::World::fire) or [`MethodCall::fire`], and held in
//! the world's [`SignalQueue`](crate::SignalQueue) until the script host's next step.
//!
//! # Adding a class
//!
//! Create `src/classes/<group>/<class>.rs` with a `pub static CLASS: ClassDesc` and nothing else
//! changes: the folder is for people, inheritance comes from the superclass name. Run
//! `cargo test -p metallic_core`: the resolution checks run, and `registry_hash_is_pinned` tells
//! you to update the golden hash on purpose. `cargo fmt` does not follow discovered modules, so
//! format new files with `rustfmt --edition 2024 --check <file>` (CI checks the folder).
//!
//! # Adding an enum
//!
//! Write a `const` [`EnumDesc`] in the class file that owns it, list it in that class's
//! [`enums`](ClassDesc::enums) and declare the property with `enumerated(name, default, &ENUM)`.
//! An enum no one class owns goes in `src/classes/enums/<name>.rs` with a `pub static ENUMS`. The
//! property stays a String holding the item name, and `Enum.<name>` appears in Luau.
//!
//! Studio reads this registry too: [`Registry::insertable`] is its Insert lists and
//! [`PropDesc::editor`] says how the Properties panel shows a property. Neither is in
//! `registry_hash`.
use crate::value::Value;
use std::sync::LazyLock;

mod call;
mod desc;
mod host_api;
mod lists;
pub(crate) mod prelude;
mod resolve;

pub use call::{
    ApiError, ApiErrorKind, CallContext, CallOutcome, EnumItemRef, FunctionRef, MemberCategory,
    MethodCall, MethodFn, ScriptValue, Side, SignalId, SignalKey, TypeName, UNNAMED_INSTANCE,
    WaitRequest, check_arguments, conforms, property_signal_arg,
};
pub use desc::{
    ArgCheck, CallbackDesc, ClassDesc, ClassFlags, DefaultOverride, Editor, EnumDesc, FunctionSig,
    InsertEntry, MemberContext, MethodDesc, MethodImpl, NumRange, NumberUnit, Param, PropAccess,
    PropDesc, Repl, Section, SignalDesc, Ty, VisibleWhen, Widget,
};
pub use host_api::{
    Handle, HostApi, HostCall, HostMethodError, HostMethodFn, HostMethods, MethodDispatch,
    instance_class, member_of,
};
pub use lists::{MAX_HUMANOID_HEALTH, MAX_JUMP_POWER, MAX_SLOPE_ANGLE, MAX_WALK_SPEED};
pub use resolve::{
    ClassId, DeclId, EnumId, Member, MemberId, MemberKind, PropertyIndex, Registry, ResolvedClass,
    ResolvedEnum, SchemaError, resolve, resolve_with_enums,
};

/// The discovered class files and the `ALL_CLASSES` list `build.rs` generates.
#[allow(missing_docs)]
mod discovered {
    include!(concat!(env!("OUT_DIR"), "/classes_generated.rs"));
}

pub use discovered::enums::key_code::KEY_CODE;
pub use discovered::enums::user_input_type::USER_INPUT_TYPE;

/// The item names of `Enum.Material`, for crates with one table row per material.
pub const MATERIALS: &[&str] = discovered::parts::base_part::MATERIAL.items;

/// The name [`PropDesc`] had before #460; callers migrate later.
pub type PropDef = PropDesc;
/// The name [`ClassDesc`] had before #460; callers migrate later.
pub type ClassDef = ClassDesc;

/// Built once. When the built-in classes do not resolve, the registry is empty, so lookups fail
/// closed, and [`registry_error`] (and so `registry_hash`) gives the reason. A test keeps the
/// built-in classes valid.
static REGISTRY: LazyLock<(Registry, Option<SchemaError>)> =
    LazyLock::new(
        || match resolve_with_enums(discovered::ALL_CLASSES, discovered::ALL_ENUMS) {
            Ok(registry) => (registry, None),
            Err(error) => (Registry::empty(), Some(error)),
        },
    );

/// The resolved built-in classes.
#[inline]
pub fn registry() -> &'static Registry {
    &REGISTRY.0
}

/// Why the built-in classes did not resolve, if they did not.
pub fn registry_error() -> Option<SchemaError> {
    REGISTRY.1
}

/// Looks up a class by name.
pub fn class_def(name: &str) -> Option<&'static ClassDef> {
    Some(registry().class_by_name(name)?.desc)
}

/// The class's [`ClassId`] as a number, the wire class index.
///
/// Indices are meaningful only between peers with an equal
/// [`registry_hash`](crate::replication::registry_hash). [`class_at`] is the inverse.
pub fn class_index(name: &str) -> Option<u16> {
    Some(registry().class_id(name)?.0)
}

/// The class at a registry index (see [`class_index`]), or `None` past the end.
pub fn class_at(index: u16) -> Option<&'static ClassDef> {
    Some(registry().class(ClassId(index))?.desc)
}

/// True when `class` is `of` or inherits from it.
pub fn is_subclass(class: &str, of: &str) -> bool {
    registry()
        .class_by_name(class)
        .is_some_and(|resolved| resolved.is_a_named(of))
}

/// All properties of a class including inherited ones, base class first.
pub fn all_properties(class: &str) -> Vec<&'static PropDef> {
    registry()
        .class_by_name(class)
        .map_or_else(Vec::new, |resolved| resolved.props.clone())
}

/// Finds a property (own or inherited) of a class. Allocates nothing.
pub fn find_property(class: &str, property: &str) -> Option<&'static PropDef> {
    Some(registry().class_by_name(class)?.property(property)?.0)
}

/// Borrows the shared default for a property; `Name` defaults to the concrete class name.
///
/// The immutable registry initializes once on the first call. Subsequent successful reads do
/// not allocate or clone, and the returned values remain valid for the life of the process.
/// Unknown classes or properties return `None`.
pub fn default_value_ref(class: &str, property: &str) -> Option<&'static Value> {
    registry().class_by_name(class)?.default_of(property)
}

/// An owned default value; `Name` defaults to the concrete class name.
///
/// Use [`default_value_ref`] to inspect defaults without cloning string values.
pub fn default_value(class: &str, property: &str) -> Option<Value> {
    default_value_ref(class, property).cloned()
}

/// Classes in [`ClassId`] order (sorted by name). Protocol fingerprints include this order.
pub fn registry_classes() -> &'static [&'static ClassDef] {
    registry().descs()
}

/// The zero-based property index within a class, including inherited properties.
///
/// Indices are meaningful only with an equal replication registry hash. Unknown classes and
/// properties return None. Allocates nothing.
pub fn property_index(class: &str, property: &str) -> Option<u16> {
    Some(registry().class_by_name(class)?.property(property)?.1.0)
}

/// Resolves a class-relative protocol index. Unknown classes and indices return None.
pub fn property_by_index(class: &str, index: u16) -> Option<&'static PropDef> {
    registry()
        .class_by_name(class)?
        .props
        .get(usize::from(index))
        .copied()
}

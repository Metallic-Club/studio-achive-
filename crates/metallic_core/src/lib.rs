//! Core data model of the Metallic Engine: a Roblox-style Instance tree.
//!
//! A [`World`] owns every instance of one game. Each instance has a class (see
//! [`classes`]: one file per class, resolved once into flat tables), a name, typed properties
//! ([`Value`]) whose defaults come from the class registry, one optional parent and an ordered
//! list of children. Instances are addressed
//! by [`InstanceId`], a generational handle that stays invalid after the instance is
//! destroyed. [`World::get_ref`] borrows values and shared defaults without cloning;
//! [`World::get`] retains the owned-value interface. Scenes are saved to and loaded from a diff-friendly text file
//! ([`save_scene`], [`load_scene`]). Saves replace the destination only after a temporary file
//! has been written and synced; nesting is limited to [`MAX_SCENE_DEPTH`] instance levels.
//! [`World::set`] rejects non-finite numeric components before changing state, recording a
//! journal entry, or dispatching a property-change signal.
//! [`World::changes_since`] reports deduplicated instance invalidations;
//! [`World::changes_since_detailed`] shares its cursor/history and reports ordered [`ChangeKind`]
//! notices with static property tokens. Neither reader consumes the journal. Detailed notices
//! retain no historic values and do not replace a durable editing operation log.
//! Attribute and tag changes are coarse-only for now: `changes_since` reports the id, the detailed
//! reader has no entry for them (read the attribute and tag logs instead).
//! Class methods use a language-neutral calling convention and one host interface, in [`classes`]:
//! this crate depends on no scripting language.
#![warn(missing_docs)]

mod cframe;
pub mod classes;
mod instance;
pub mod lighting;
pub mod look_override;
pub mod replication;
mod scene_file;
mod signal;
pub mod transform;
mod value;

pub use cframe::{CFrame, RotationOrder};
pub use instance::edit;
pub use instance::{
    AttributeChange, AttributeChanges, AttributeCursor, Change, ChangeCursor, ChangeKind, Changes,
    DetailedChanges, InstanceId, MAX_ATTRIBUTE_NAME, MAX_ATTRIBUTE_STRING, MAX_ATTRIBUTES,
    MAX_EXTRAS_BYTES, MAX_TAG, MAX_TAGS, PropertyChange, TagChange, TagChanges, TagCursor, World,
    check_attribute_name, check_attribute_value, check_tag,
};
pub use scene_file::{MAX_SCENE_DEPTH, load_scene, save_scene};
pub use signal::{ConnectionId, MAX_QUEUED_SIGNALS, QueuedSignal, Signal, SignalQueue};
pub use value::{Color3, UDim2, Value, ValueType, Vec3, orientation_to_quat, quat_to_orientation};

/// Errors returned by the core crate.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CoreError {
    /// The class name is not in the class registry.
    #[error("unknown class `{0}`")]
    UnknownClass(String),
    /// The class is abstract: it exists for inheritance and `IsA`, never as an instance.
    #[error("class `{0}` is abstract")]
    AbstractClass(String),
    /// The class has no property with this name.
    #[error("class `{class}` has no property `{property}`")]
    UnknownProperty {
        /// Class of the instance.
        class: String,
        /// Requested property.
        property: String,
    },
    /// The value has the wrong type for the property.
    #[error("property `{property}` expects {expected}, got {found}")]
    TypeMismatch {
        /// Property name.
        property: String,
        /// Expected type name.
        expected: String,
        /// Type name of the value given.
        found: String,
    },
    /// The value has the right type but is not allowed (for example an unknown material or a
    /// non-finite numeric component).
    #[error("invalid value for `{property}`: {reason}")]
    InvalidValue {
        /// Property name.
        property: String,
        /// Why the value was refused.
        reason: String,
    },
    /// The instance id is stale (destroyed) or was never valid.
    #[error("instance does not exist (destroyed or invalid id)")]
    InvalidInstance,
    /// Reparenting would make an instance its own ancestor.
    #[error("reparenting would create a cycle")]
    Cycle,
    /// A file could not be read or written.
    #[error("file error: {0}")]
    Io(String),
    /// A scene file could not be parsed or does not describe a valid world.
    #[error("scene error: {0}")]
    Scene(String),
}

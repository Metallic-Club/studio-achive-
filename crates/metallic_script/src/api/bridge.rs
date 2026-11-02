//! The methods the registry does not declare yet, so scripts cannot tell the generic binding
//! apart from the old tables while #465-#467 move them into class files.
//!
//! [`BRIDGED_METHODS`] says which class has each of them. It is resolved to [`ClassId`]s once per
//! VM, and a lookup compares only names of the right length, so nothing here allocates or scans
//! the whole table. The list can only shrink: a method declared in a class file must leave it, and
//! a test fails when it does not.
//!
//! The bodies stay in `api/*.rs`; each module's `methods()` hands them over by name. A `Host`
//! method on [`NOT_MIGRATED_HOST_METHODS`] has a body here too, with no owner: the registry finds
//! it, and the binding takes its function from [`Bridge::function`].

use metallic_core::classes::{ClassId, Registry, ResolvedClass};
use mlua::Function;

use super::host_methods::NOT_MIGRATED_HOST_METHODS;
use super::lua_error;

/// `(class, methods)`: each method exists on that class and every subclass.
pub(crate) const BRIDGED_METHODS: &[(&str, &[&str])] = &[
    (
        "Instance",
        &[
            "FindFirstChild",
            "GetChildren",
            "GetDescendants",
            "IsA",
            "Destroy",
            "Clone",
            "WaitForChild",
            "SetAttribute",
            "GetAttribute",
            "GetAttributes",
            "GetAttributeChangedSignal",
            "AddTag",
            "RemoveTag",
            "HasTag",
            "GetTags",
        ],
    ),
    ("DataModel", &["GetService"]),
    ("Players", &["GetPlayers", "GetPlayerFromCharacter"]),
    ("RunService", &["IsServer", "IsClient"]),
    ("Humanoid", &["TakeDamage"]),
    (
        "RemoteEvent",
        &["FireServer", "FireClient", "FireAllClients"],
    ),
    (
        "Camera",
        &[
            "ViewportPointToRay",
            "ScreenPointToRay",
            "WorldToViewportPoint",
            "WorldToScreenPoint",
        ],
    ),
    ("InputAction", &["GetState", "GetHoldProgress"]),
    ("BasePart", &["Resize"]),
    (
        "Model",
        &["ScaleTo", "GetScale", "GetBoundingBox", "GetExtentsSize"],
    ),
];

struct Entry {
    name: &'static str,
    /// `None` for a method the registry finds itself: on no class through this table.
    owner: Option<ClassId>,
    function: Function,
}

/// One VM's bridged methods, grouped by name length.
pub(crate) struct Bridge {
    by_length: Vec<Vec<Entry>>,
}

impl Bridge {
    /// Resolves the table against `registry` and pairs it with the bodies. A body nothing owns or
    /// a listed method with no body is a setup error, so the two cannot drift apart.
    pub(crate) fn new(
        registry: &Registry,
        bodies: Vec<(&'static str, Function)>,
    ) -> mlua::Result<Bridge> {
        let owner_of = |name: &str| {
            BRIDGED_METHODS
                .iter()
                .find(|(_, methods)| methods.contains(&name))
                .map(|(class, _)| *class)
        };
        for (_, methods) in BRIDGED_METHODS {
            for method in *methods {
                if !bodies.iter().any(|(name, _)| name == method) {
                    return Err(lua_error(format!("bridged method {method} has no body")));
                }
            }
        }
        let mut by_length: Vec<Vec<Entry>> = Vec::new();
        for (name, function) in bodies {
            let owner = match owner_of(name) {
                Some(class) => Some(registry.class_id(class).ok_or_else(|| {
                    lua_error(format!("bridged method {name} names unknown class {class}"))
                })?),
                None if NOT_MIGRATED_HOST_METHODS.iter().any(|(_, m)| *m == name) => None,
                None => return Err(lua_error(format!("method {name} has no bridge entry"))),
            };
            if by_length.len() <= name.len() {
                by_length.resize_with(name.len() + 1, Vec::new);
            }
            by_length[name.len()].push(Entry {
                name,
                owner,
                function,
            });
        }
        Ok(Bridge { by_length })
    }

    #[inline]
    fn entry(&self, name: &str) -> Option<&Entry> {
        self.by_length
            .get(name.len())?
            .iter()
            .find(|entry| entry.name == name)
    }

    /// The bridged method `name` of `class`, if that class has it.
    #[inline]
    pub(crate) fn method(&self, class: &ResolvedClass, name: &str) -> Option<&Function> {
        let entry = self.entry(name)?;
        class.is_a(entry.owner?).then_some(&entry.function)
    }

    /// The body named `name`, whichever class has it.
    pub(crate) fn function(&self, name: &str) -> Option<&Function> {
        self.entry(name).map(|entry| &entry.function)
    }
}

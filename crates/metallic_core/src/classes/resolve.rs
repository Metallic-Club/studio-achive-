//! Resolution: every [`ClassDesc`] plus its superclass chain becomes one flat [`ResolvedClass`].
//!
//! Lookups never walk the chain afterwards. Rules (`docs/design/class-system.md` section 3.3):
//! a class has every member of its chain; an inherited member keeps its [`MemberId`] and
//! [`PropertyIndex`] in every subclass (the prefix rule); redeclaring one is an error without
//! `overrides()`; `defaults` and `hide` change an inherited member without redeclaring it.
//! A malformed set of classes gives a typed [`SchemaError`], never a panic.
use super::desc::{CallbackDesc, SignalDesc};
use super::desc::{
    ClassDesc, ClassFlags, EnumDesc, InsertEntry, MemberContext, MethodDesc, PropDesc,
};
use crate::value::Value;
use std::collections::HashSet;

/// A class's position in the registry: classes sorted by name.
///
/// Never persist one: ids change when a class is added. Scenes save names and peers compare
/// [`registry_hash`](crate::replication::registry_hash) first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClassId(pub u16);

/// A class-relative index into [`ResolvedClass::members`]. By the prefix rule, an inherited
/// member has the same id in the class that declares it and in every subclass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemberId(pub u16);

/// A class-relative index counting properties only: the wire and storage index. Same prefix rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PropertyIndex(pub u16);

/// A dense registry-wide index of method, signal and callback declarations; bindings use it to
/// cache one function object per declaration. An override has its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DeclId(pub u32);

/// An enum's position in the registry: enums sorted by name. Never persist one, like [`ClassId`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EnumId(pub u16);

/// What a member is, with its effective descriptor.
#[derive(Clone, Copy)]
pub enum MemberKind {
    /// A property. `desc` carries any `defaults` and `hide` applied along the chain.
    Property {
        /// The effective descriptor.
        desc: &'static PropDesc,
        /// Its index among the class's properties.
        index: PropertyIndex,
    },
    /// A method.
    Method {
        /// The nearest declaration.
        desc: &'static MethodDesc,
        /// That declaration's id.
        decl: DeclId,
    },
    /// A signal.
    Signal {
        /// The declaration.
        desc: &'static SignalDesc,
        /// That declaration's id.
        decl: DeclId,
    },
    /// A callback.
    Callback {
        /// The declaration.
        desc: &'static CallbackDesc,
        /// That declaration's id.
        decl: DeclId,
    },
}

/// One entry of a class's flattened member table.
#[derive(Clone, Copy)]
pub struct Member {
    /// What it is.
    pub kind: MemberKind,
    /// The class whose declaration this is (the nearest one).
    pub owner: ClassId,
    /// Who may use it, with the class default applied.
    pub context: MemberContext,
    /// Hidden from scripts, Studio and docs by `hide` or by the descriptor.
    pub hidden: bool,
}

impl Member {
    /// The member's name.
    pub fn name(&self) -> &'static str {
        match self.kind {
            MemberKind::Property { desc, .. } => desc.name,
            MemberKind::Method { desc, .. } => desc.name,
            MemberKind::Signal { desc, .. } => desc.name,
            MemberKind::Callback { desc, .. } => desc.name,
        }
    }

    /// Old names that still resolve to this member.
    pub fn aliases(&self) -> &'static [&'static str] {
        match self.kind {
            MemberKind::Property { desc, .. } => desc.aliases,
            _ => &[],
        }
    }
}

/// A lookup table of static names, grouped by name length, so a lookup only compares names of the
/// right length: no hashing, no scan of the whole table, no allocation. Measured against a linear
/// scan and a hash map with a fast hasher in `docs/agents/460-class-registry.md`.
struct NameTable<V> {
    /// `by_length[len]` holds the entries whose name has `len` bytes.
    by_length: Vec<Vec<(&'static str, V)>>,
}

impl<V: Copy> NameTable<V> {
    fn new(entries: Vec<(&'static str, V)>) -> NameTable<V> {
        let mut by_length: Vec<Vec<(&'static str, V)>> = Vec::new();
        for (name, value) in entries {
            if by_length.len() <= name.len() {
                by_length.resize_with(name.len() + 1, Vec::new);
            }
            by_length[name.len()].push((name, value));
        }
        NameTable { by_length }
    }

    #[inline]
    fn get(&self, name: &str) -> Option<V> {
        self.by_length
            .get(name.len())?
            .iter()
            .find(|(entry, _)| *entry == name)
            .map(|(_, value)| *value)
    }
}

/// One class, flattened.
pub struct ResolvedClass {
    /// Its id.
    pub id: ClassId,
    /// Its descriptor.
    pub desc: &'static ClassDesc,
    /// Self first, then superclasses up to the root.
    pub chain: Vec<ClassId>,
    /// `chain` as names, so `IsA(name)` needs no class lookup of the name.
    chain_names: Vec<&'static str>,
    /// Every member, base first. See [`MemberId`].
    pub members: Vec<Member>,
    /// Every property, base first. See [`PropertyIndex`].
    pub props: Vec<&'static PropDesc>,
    /// One default per property; `Name` already holds the class name.
    pub defaults: Vec<Value>,
    by_name: NameTable<MemberId>,
}

impl ResolvedClass {
    /// The class name.
    #[inline]
    pub fn name(&self) -> &'static str {
        self.desc.name
    }

    /// The member with this name or alias. Allocates nothing, on a hit or a miss.
    #[inline]
    pub fn member(&self, name: &str) -> Option<MemberId> {
        self.by_name.get(name)
    }

    /// The property with this name or alias, and its index. `None` for other kinds of member.
    #[inline]
    pub fn property(&self, name: &str) -> Option<(&'static PropDesc, PropertyIndex)> {
        let id = self.member(name)?;
        match self.members.get(usize::from(id.0))?.kind {
            MemberKind::Property { desc, index } => Some((desc, index)),
            _ => None,
        }
    }

    /// The default of the property with this name or alias.
    #[inline]
    pub fn default_of(&self, name: &str) -> Option<&Value> {
        let (_, index) = self.property(name)?;
        self.defaults.get(usize::from(index.0))
    }

    /// True when this class is `of` or inherits from it.
    #[inline]
    pub fn is_a(&self, of: ClassId) -> bool {
        self.chain.contains(&of)
    }

    /// True when this class is the class named `of` or inherits from it.
    #[inline]
    pub fn is_a_named(&self, of: &str) -> bool {
        self.chain_names.contains(&of)
    }
}

/// One enum, with a lookup table of its item names and aliases.
pub struct ResolvedEnum {
    /// Its id.
    pub id: EnumId,
    /// Its descriptor.
    pub desc: &'static EnumDesc,
    /// The class that declares it; `None` for an enum under `classes/enums/`.
    pub owner: Option<&'static str>,
    by_name: NameTable<u16>,
}

impl ResolvedEnum {
    /// The enum's name.
    #[inline]
    pub fn name(&self) -> &'static str {
        self.desc.name
    }

    /// The index in [`EnumDesc::items`] of the item with this name or alias. Allocates nothing.
    #[inline]
    pub fn item_index(&self, name: &str) -> Option<u16> {
        self.by_name.get(name)
    }
}

/// Every class and enum, resolved. Immutable once built.
pub struct Registry {
    classes: Vec<ResolvedClass>,
    descs: Vec<&'static ClassDesc>,
    by_name: NameTable<ClassId>,
    enums: Vec<ResolvedEnum>,
    enums_by_name: NameTable<EnumId>,
}

impl std::fmt::Debug for Registry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list()
            .entries(self.descs.iter().map(|class| class.name))
            .finish()
    }
}

impl Registry {
    /// A registry with no classes: what lookups see when the built-in classes do not resolve.
    pub(super) fn empty() -> Registry {
        Registry {
            classes: Vec::new(),
            descs: Vec::new(),
            by_name: NameTable::new(Vec::new()),
            enums: Vec::new(),
            enums_by_name: NameTable::new(Vec::new()),
        }
    }

    /// Every enum, in [`EnumId`] order.
    pub fn enums(&self) -> &[ResolvedEnum] {
        &self.enums
    }

    /// The enum with this name. Allocates nothing.
    #[inline]
    pub fn enum_by_name(&self, name: &str) -> Option<&ResolvedEnum> {
        self.enums.get(usize::from(self.enums_by_name.get(name)?.0))
    }

    /// Every class, in [`ClassId`] order.
    pub fn classes(&self) -> &[ResolvedClass] {
        &self.classes
    }

    /// Every descriptor, in [`ClassId`] order.
    pub fn descs(&self) -> &[&'static ClassDesc] {
        &self.descs
    }

    /// The class with this id.
    pub fn class(&self, id: ClassId) -> Option<&ResolvedClass> {
        self.classes.get(usize::from(id.0))
    }

    /// The id of the class with this name.
    #[inline]
    pub fn class_id(&self, name: &str) -> Option<ClassId> {
        self.by_name.get(name)
    }

    /// The class with this name.
    #[inline]
    pub fn class_by_name(&self, name: &str) -> Option<&ResolvedClass> {
        self.classes.get(usize::from(self.class_id(name)?.0))
    }

    /// The class with an id this registry handed out.
    #[inline]
    pub(crate) fn resolved(&self, id: ClassId) -> &ResolvedClass {
        &self.classes[usize::from(id.0)]
    }

    /// What the Studio Insert lists offer: `CREATABLE` classes not marked
    /// [`InsertEntry::Hidden`], with each category's classes together. A category sits where its
    /// lowest [`InsertEntry::Listed`] number puts it; ties go by name.
    pub fn insertable(&self) -> Vec<&'static ClassDesc> {
        let listed: Vec<(u16, &'static ClassDesc)> = self
            .descs
            .iter()
            .filter(|class| class.is_creatable())
            .filter_map(|&class| match class.insert {
                InsertEntry::Listed(order) => Some((order, class)),
                InsertEntry::Hidden => None,
            })
            .collect();
        let first_of = |category: &str| {
            listed
                .iter()
                .filter(|(_, class)| class.category == category)
                .map(|(order, _)| *order)
                .min()
        };
        let mut sorted: Vec<_> = listed
            .iter()
            .map(|&(order, class)| {
                let key = (first_of(class.category), class.category, order, class.name);
                (key, class)
            })
            .collect();
        sorted.sort_by_key(|&(key, _)| key);
        sorted.into_iter().map(|(_, class)| class).collect()
    }
}

/// Why a set of classes does not resolve. Each variant names the class (and member) at fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SchemaError {
    /// Two classes have the same name.
    #[error("class {0} is declared twice")]
    DuplicateClass(&'static str),
    /// More classes than a `u16` id can address.
    #[error("the registry has more than 65,536 classes")]
    ClassIdOverflow,
    /// `CREATABLE` and `SERVICE` together, or `ABSTRACT` with either.
    #[error("class {0} has conflicting flags")]
    ConflictingFlags(&'static str),
    /// A class names a superclass that is not in the registry.
    #[error("class {class} names unregistered superclass {superclass}")]
    UnknownSuperclass {
        /// The class with the bad superclass.
        class: &'static str,
        /// The missing superclass name.
        superclass: &'static str,
    },
    /// Following superclasses from this class never reaches a root.
    #[error("class {0} has an inheritance cycle")]
    InheritanceCycle(&'static str),
    /// The class has more properties (inherited ones included) than a `u16` index can address.
    #[error("class {0} has more than 65,536 properties")]
    PropertyIndexOverflow(&'static str),
    /// The class has more members (inherited ones included) than a `u16` id can address.
    #[error("class {0} has more than 65,536 members")]
    MemberIdOverflow(&'static str),
    /// Two members of one class share a name or alias, across all member kinds.
    #[error("class {class} declares member {member} twice")]
    DuplicateMember {
        /// The class.
        class: &'static str,
        /// The repeated name.
        member: &'static str,
    },
    /// A class declares a member an ancestor has, without saying so.
    #[error("class {class} redeclares inherited member {member} (add overrides())")]
    RedeclaredMember {
        /// The redeclaring class.
        class: &'static str,
        /// The member.
        member: &'static str,
    },
    /// `overrides()` on a member no ancestor has.
    #[error("class {class} overrides {member}, which it does not inherit")]
    OverrideWithoutBase {
        /// The overriding class.
        class: &'static str,
        /// The member.
        member: &'static str,
    },
    /// An override changes the kind, a method's signature or a property's type.
    #[error("class {class} overrides {member} with a different kind, signature or type")]
    OverrideChangesShape {
        /// The overriding class.
        class: &'static str,
        /// The member.
        member: &'static str,
    },
    /// An override allows more contexts than what it replaces.
    #[error("class {class} widens the context of {member}; an override may only narrow it")]
    OverrideWidensContext {
        /// The overriding class.
        class: &'static str,
        /// The member.
        member: &'static str,
    },
    /// `defaults` names something that is not an inherited property.
    #[error("class {class} sets a default for {property}, which is not an inherited property")]
    DefaultWithoutProperty {
        /// The class.
        class: &'static str,
        /// The name in `defaults`.
        property: &'static str,
    },
    /// `hide` names something that is not an inherited member.
    #[error("class {class} hides {member}, which it does not inherit")]
    HideWithoutMember {
        /// The class.
        class: &'static str,
        /// The name in `hide`.
        member: &'static str,
    },
    /// A property's `visible_when` reads a name that is not a property of the class.
    #[error("property {class}.{property} is visible when {reads}, which is not a property")]
    VisibleWhenWithoutProperty {
        /// The class.
        class: &'static str,
        /// The property with the condition.
        property: &'static str,
        /// The name the condition reads.
        reads: &'static str,
    },
    /// A property has a normalizer without a contract id, or a contract id without a normalizer.
    #[error("property {class}.{property} must declare normalize and normalize_schema_id together")]
    UnpairedNormalizer {
        /// The class declaring the property.
        class: &'static str,
        /// The property.
        property: &'static str,
    },
    /// A property's normalization contract id is empty.
    #[error("property {class}.{property} has an empty normalization schema id")]
    EmptyNormalizationId {
        /// The class declaring the property.
        class: &'static str,
        /// The property.
        property: &'static str,
    },
    /// More enums than a `u16` id can address.
    #[error("the registry has more than 65,536 enums")]
    EnumIdOverflow,
    /// Two enums have the same name.
    #[error("enum {0} is declared twice")]
    DuplicateEnum(&'static str),
    /// An enum has no items, or more than a `u16` index can address.
    #[error("enum {0} must have between 1 and 65,536 items")]
    EnumItemCount(&'static str),
    /// An item name or alias appears twice in one enum.
    #[error("enum {enum_name} declares {item} twice")]
    DuplicateEnumItem {
        /// The enum.
        enum_name: &'static str,
        /// The repeated name.
        item: &'static str,
    },
    /// `numbers` does not have one distinct value per item.
    #[error("enum {0} must give each item its own number")]
    BadEnumNumbers(&'static str),
    /// An alias stands for something that is not an item.
    #[error("enum {enum_name} has alias {alias} for an item it does not have")]
    AliasWithoutItem {
        /// The enum.
        enum_name: &'static str,
        /// The alias.
        alias: &'static str,
    },
    /// A property's enum is not in the registry, or differs from the one that is.
    #[error("property {class}.{property} uses enum {enum_name}, which is not declared")]
    UndeclaredEnum {
        /// The class.
        class: &'static str,
        /// The property.
        property: &'static str,
        /// The enum.
        enum_name: &'static str,
    },
    /// An enum property's default is not an item name.
    #[error("property {class}.{property} defaults to something that is not an item of its enum")]
    EnumDefaultNotAnItem {
        /// The class.
        class: &'static str,
        /// The property.
        property: &'static str,
    },
}

/// Resolves a set of classes: sorts them by name, checks every rule, flattens every class.
///
/// Discovery hands the built-in classes to this once ([`registry`](super::registry)). It is
/// public so a test can resolve a fixture set, and for creator-defined classes later.
pub fn resolve(descs: &[&'static ClassDesc]) -> Result<Registry, SchemaError> {
    resolve_with_enums(descs, &[])
}

/// Like [`resolve`], with the enums no class owns: the `pub static ENUMS` lists `build.rs` finds.
pub fn resolve_with_enums(
    descs: &[&'static ClassDesc],
    shared_enums: &[&'static [EnumDesc]],
) -> Result<Registry, SchemaError> {
    let mut sorted = descs.to_vec();
    sorted.sort_by_key(|class| class.name);
    if sorted.len() > usize::from(u16::MAX) + 1 {
        return Err(SchemaError::ClassIdOverflow);
    }
    let mut entries = Vec::with_capacity(sorted.len());
    let mut decl_starts = Vec::with_capacity(sorted.len());
    let mut next_decl = 0;
    let mut previous = None;
    for (index, class) in sorted.iter().enumerate() {
        check_flags(class)?;
        if previous == Some(class.name) {
            return Err(SchemaError::DuplicateClass(class.name));
        }
        previous = Some(class.name);
        entries.push((class.name, ClassId(index as u16)));
        decl_starts.push(next_decl);
        next_decl += (class.methods.len() + class.signals.len() + class.callbacks.len()) as u32;
    }
    let by_name = NameTable::new(entries);
    let chains = sorted
        .iter()
        .enumerate()
        .map(|(index, class)| chain_of(class, ClassId(index as u16), &sorted, &by_name))
        .collect::<Result<Vec<_>, _>>()?;

    // A chain is longer than its superclass's, so this visits every superclass first.
    let mut order: Vec<usize> = (0..sorted.len()).collect();
    order.sort_by_key(|&index| chains[index].len());
    let mut resolved: Vec<Option<ResolvedClass>> = sorted.iter().map(|_| None).collect();
    for index in order {
        let parent = chains[index]
            .get(1)
            .and_then(|id| resolved[usize::from(id.0)].as_ref());
        let chain = &chains[index];
        let class = resolve_class(sorted[index], chain, parent, decl_starts[index], &sorted)?;
        resolved[index] = Some(class);
    }
    let enums = resolve_enums(&sorted, shared_enums)?;
    let enums_by_name = NameTable::new(enums.iter().map(|e| (e.name(), e.id)).collect());
    let registry = Registry {
        classes: resolved.into_iter().flatten().collect(),
        descs: sorted,
        by_name,
        enums,
        enums_by_name,
    };
    check_enum_properties(&registry)?;
    Ok(registry)
}

/// Every enum, sorted by name, with its own rules checked.
fn resolve_enums(
    classes: &[&'static ClassDesc],
    shared: &[&'static [EnumDesc]],
) -> Result<Vec<ResolvedEnum>, SchemaError> {
    let owned = classes
        .iter()
        .flat_map(|class| class.enums.iter().map(|desc| (desc, Some(class.name))));
    let unowned = shared
        .iter()
        .flat_map(|list| list.iter().map(|desc| (desc, None)));
    let mut all: Vec<(&'static EnumDesc, Option<&'static str>)> = owned.chain(unowned).collect();
    all.sort_by_key(|(desc, _)| desc.name);
    if all.len() > usize::from(u16::MAX) + 1 {
        return Err(SchemaError::EnumIdOverflow);
    }
    let mut enums = Vec::with_capacity(all.len());
    let mut previous = None;
    for (index, (desc, owner)) in all.into_iter().enumerate() {
        if previous == Some(desc.name) {
            return Err(SchemaError::DuplicateEnum(desc.name));
        }
        previous = Some(desc.name);
        enums.push(ResolvedEnum {
            id: EnumId(index as u16),
            desc,
            owner,
            by_name: enum_name_table(desc)?,
        });
    }
    Ok(enums)
}

/// The name and alias table of one enum. Every name must be unique across it.
fn enum_name_table(desc: &'static EnumDesc) -> Result<NameTable<u16>, SchemaError> {
    let count = desc.items.len();
    if count == 0 || count > usize::from(u16::MAX) + 1 {
        return Err(SchemaError::EnumItemCount(desc.name));
    }
    if let Some(numbers) = desc.numbers {
        let distinct = numbers.iter().collect::<HashSet<_>>().len();
        if numbers.len() != count || distinct != count {
            return Err(SchemaError::BadEnumNumbers(desc.name));
        }
    }
    let mut seen = HashSet::with_capacity(count + desc.aliases.len());
    let mut entries = Vec::with_capacity(count + desc.aliases.len());
    let duplicate = |item| SchemaError::DuplicateEnumItem {
        enum_name: desc.name,
        item,
    };
    for (index, item) in desc.items.iter().enumerate() {
        if !seen.insert(*item) {
            return Err(duplicate(item));
        }
        entries.push((*item, index as u16));
    }
    for (alias, item) in desc.aliases {
        if !seen.insert(*alias) {
            return Err(duplicate(alias));
        }
        let Some(index) = desc.items.iter().position(|name| name == item) else {
            return Err(SchemaError::AliasWithoutItem {
                enum_name: desc.name,
                alias,
            });
        };
        entries.push((*alias, index as u16));
    }
    Ok(NameTable::new(entries))
}

/// Every enum property's enum is in the registry and its default is an item.
fn check_enum_properties(registry: &Registry) -> Result<(), SchemaError> {
    for class in registry.classes() {
        for (prop, default) in class.props.iter().zip(&class.defaults) {
            let Some(desc) = prop.enum_type else {
                continue;
            };
            let declared = registry.enum_by_name(desc.name);
            if declared.is_none_or(|e| e.desc != desc) {
                return Err(SchemaError::UndeclaredEnum {
                    class: class.name(),
                    property: prop.name,
                    enum_name: desc.name,
                });
            }
            if !matches!(default, Value::String(s) if desc.items.contains(&s.as_str())) {
                return Err(SchemaError::EnumDefaultNotAnItem {
                    class: class.name(),
                    property: prop.name,
                });
            }
        }
    }
    Ok(())
}

fn check_flags(class: &ClassDesc) -> Result<(), SchemaError> {
    let creatable = class.flags.contains(ClassFlags::CREATABLE);
    let service = class.flags.contains(ClassFlags::SERVICE);
    let abstract_ = class.flags.contains(ClassFlags::ABSTRACT);
    if (creatable && service) || (abstract_ && (creatable || service)) {
        return Err(SchemaError::ConflictingFlags(class.name));
    }
    Ok(())
}

fn chain_of(
    class: &'static ClassDesc,
    id: ClassId,
    sorted: &[&'static ClassDesc],
    by_name: &NameTable<ClassId>,
) -> Result<Vec<ClassId>, SchemaError> {
    let mut chain = vec![id];
    let mut current = class;
    while let Some(superclass) = current.superclass {
        let Some(next) = by_name.get(superclass) else {
            return Err(SchemaError::UnknownSuperclass {
                class: current.name,
                superclass,
            });
        };
        if chain.len() >= sorted.len() {
            return Err(SchemaError::InheritanceCycle(class.name));
        }
        chain.push(next);
        current = sorted[usize::from(next.0)];
    }
    Ok(chain)
}

/// The member tables of one class while it is being built.
struct Table {
    class: &'static ClassDesc,
    id: ClassId,
    members: Vec<Member>,
    props: Vec<&'static PropDesc>,
    own: HashSet<&'static str>,
}

impl Table {
    /// Where an own declaration goes: a new entry, or the inherited one it overrides.
    fn place(
        &mut self,
        parent: Option<&ResolvedClass>,
        name: &'static str,
        overrides: bool,
    ) -> Result<Option<usize>, SchemaError> {
        let class = self.class.name;
        if !self.own.insert(name) {
            return Err(SchemaError::DuplicateMember {
                class,
                member: name,
            });
        }
        let inherited = parent.and_then(|p| p.member(name));
        match (inherited, overrides) {
            (None, true) => Err(SchemaError::OverrideWithoutBase {
                class,
                member: name,
            }),
            (Some(_), false) => Err(SchemaError::RedeclaredMember {
                class,
                member: name,
            }),
            (None, false) => {
                if u16::try_from(self.members.len()).is_err() {
                    return Err(SchemaError::MemberIdOverflow(class));
                }
                Ok(None)
            }
            (Some(id), true) => Ok(Some(usize::from(id.0))),
        }
    }

    fn shape_error(&self, member: &'static str) -> SchemaError {
        SchemaError::OverrideChangesShape {
            class: self.class.name,
            member,
        }
    }

    fn check_context(
        &self,
        old: &Member,
        context: MemberContext,
        member: &'static str,
    ) -> Result<(), SchemaError> {
        if context.within(old.context) {
            return Ok(());
        }
        Err(SchemaError::OverrideWidensContext {
            class: self.class.name,
            member,
        })
    }

    fn set_or_push(&mut self, slot: Option<usize>, kind: MemberKind, context: MemberContext) {
        let hidden_by_chain = slot.is_some_and(|index| self.members[index].hidden);
        let hidden =
            hidden_by_chain || matches!(kind, MemberKind::Property { desc, .. } if desc.hidden);
        let member = Member {
            kind,
            owner: self.id,
            context,
            hidden,
        };
        match slot {
            Some(index) => self.members[index] = member,
            None => self.members.push(member),
        }
    }

    fn declare_property(
        &mut self,
        parent: Option<&ResolvedClass>,
        prop: &'static PropDesc,
    ) -> Result<(), SchemaError> {
        check_normalizer(self.class.name, prop)?;
        let context = prop.context.unwrap_or(self.class.context);
        let slot = self.place(parent, prop.name, prop.overrides)?;
        let (desc, index) = match slot {
            Some(slot_index) => {
                let old = self.members[slot_index];
                let index = match old.kind {
                    MemberKind::Property { desc, index } if desc.ty == prop.ty => index,
                    _ => return Err(self.shape_error(prop.name)),
                };
                self.check_context(&old, context, prop.name)?;
                // A member hidden further up stays hidden through an override.
                let desc = if old.hidden && !prop.hidden {
                    leak(PropDesc {
                        hidden: true,
                        ..*prop
                    })
                } else {
                    prop
                };
                self.props[usize::from(index.0)] = desc;
                (desc, index)
            }
            None => {
                let Ok(index) = u16::try_from(self.props.len()) else {
                    return Err(SchemaError::PropertyIndexOverflow(self.class.name));
                };
                self.props.push(prop);
                (prop, PropertyIndex(index))
            }
        };
        self.set_or_push(slot, MemberKind::Property { desc, index }, context);
        Ok(())
    }

    fn declare_method(
        &mut self,
        parent: Option<&ResolvedClass>,
        method: &'static MethodDesc,
        decl: DeclId,
    ) -> Result<(), SchemaError> {
        let context = method.context.unwrap_or(self.class.context);
        let slot = self.place(parent, method.name, method.overrides)?;
        if let Some(index) = slot {
            let old = self.members[index];
            if !matches!(old.kind, MemberKind::Method { desc, .. } if desc.same_signature(method)) {
                return Err(self.shape_error(method.name));
            }
            self.check_context(&old, context, method.name)?;
        }
        let kind = MemberKind::Method { desc: method, decl };
        self.set_or_push(slot, kind, context);
        Ok(())
    }

    fn apply_default(
        &mut self,
        parent: Option<&ResolvedClass>,
        name: &'static str,
        default: fn() -> Value,
    ) -> Result<(), SchemaError> {
        let missing = SchemaError::DefaultWithoutProperty {
            class: self.class.name,
            property: name,
        };
        let id = parent.and_then(|p| p.member(name)).ok_or(missing)?;
        let member = &mut self.members[usize::from(id.0)];
        let MemberKind::Property { desc, index } = member.kind else {
            return Err(missing);
        };
        let desc = leak(PropDesc { default, ..*desc });
        member.kind = MemberKind::Property { desc, index };
        self.props[usize::from(index.0)] = desc;
        Ok(())
    }

    fn apply_hide(
        &mut self,
        parent: Option<&ResolvedClass>,
        name: &'static str,
    ) -> Result<(), SchemaError> {
        let id = parent
            .and_then(|p| p.member(name))
            .ok_or(SchemaError::HideWithoutMember {
                class: self.class.name,
                member: name,
            })?;
        let member = &mut self.members[usize::from(id.0)];
        member.hidden = true;
        if let MemberKind::Property { desc, index } = member.kind {
            let desc = leak(PropDesc {
                hidden: true,
                ..*desc
            });
            member.kind = MemberKind::Property { desc, index };
            self.props[usize::from(index.0)] = desc;
        }
        Ok(())
    }
}

/// A changed copy of a descriptor. Resolution runs once per process, so this is bounded by the
/// `defaults` and `hide` entries of the classes.
fn leak(desc: PropDesc) -> &'static PropDesc {
    Box::leak(Box::new(desc))
}

fn check_normalizer(class: &'static str, prop: &PropDesc) -> Result<(), SchemaError> {
    match (prop.normalize.is_some(), prop.normalize_schema_id) {
        (false, None) => Ok(()),
        (true, Some("")) => Err(SchemaError::EmptyNormalizationId {
            class,
            property: prop.name,
        }),
        (true, Some(_)) => Ok(()),
        _ => Err(SchemaError::UnpairedNormalizer {
            class,
            property: prop.name,
        }),
    }
}

fn resolve_class(
    class: &'static ClassDesc,
    chain: &[ClassId],
    parent: Option<&ResolvedClass>,
    decl_start: u32,
    sorted: &[&'static ClassDesc],
) -> Result<ResolvedClass, SchemaError> {
    let id = chain[0];
    let mut table = Table {
        class,
        id,
        members: parent.map_or_else(Vec::new, |p| p.members.clone()),
        props: parent.map_or_else(Vec::new, |p| p.props.clone()),
        own: HashSet::new(),
    };
    let mut next_decl = decl_start;
    let mut decl = || {
        let current = DeclId(next_decl);
        next_decl += 1;
        current
    };
    for prop in class.props {
        table.declare_property(parent, prop)?;
    }
    for method in class.methods {
        table.declare_method(parent, method, decl())?;
    }
    for signal in class.signals {
        // Signals and callbacks have no `overrides()`: redeclaring one is always an error.
        let slot = table.place(parent, signal.name, false)?;
        let kind = MemberKind::Signal {
            desc: signal,
            decl: decl(),
        };
        table.set_or_push(slot, kind, signal.context.unwrap_or(class.context));
    }
    for callback in class.callbacks {
        let slot = table.place(parent, callback.name, false)?;
        let kind = MemberKind::Callback {
            desc: callback,
            decl: decl(),
        };
        table.set_or_push(slot, kind, callback.context.unwrap_or(class.context));
    }
    for (name, default) in class.defaults {
        table.apply_default(parent, name, *default)?;
    }
    for name in class.hide {
        table.apply_hide(parent, name)?;
    }

    let chain_names: Vec<&'static str> = chain
        .iter()
        .map(|ancestor| sorted[usize::from(ancestor.0)].name)
        .collect();
    let by_name = name_table(class.name, &table.members)?;
    for prop in &table.props {
        if let Some(reads) = prop.editor.visible_when.map(|when| when.property())
            && !table.props.iter().any(|other| other.name == reads)
        {
            return Err(SchemaError::VisibleWhenWithoutProperty {
                class: class.name,
                property: prop.name,
                reads,
            });
        }
    }
    let name_is_custom = chain
        .iter()
        .any(|ancestor| has_name_default(sorted[usize::from(ancestor.0)]));
    let defaults = table
        .props
        .iter()
        .map(|prop| {
            if prop.name == NAME_PROPERTY && !name_is_custom {
                Value::String(class.name.to_owned())
            } else {
                (prop.default)()
            }
        })
        .collect();
    Ok(ResolvedClass {
        id,
        desc: class,
        chain: chain.to_vec(),
        chain_names,
        members: table.members,
        props: table.props,
        defaults,
        by_name,
    })
}

/// `Name` defaults to the concrete class name unless a class in the chain sets it in `defaults`.
const NAME_PROPERTY: &str = "Name";

fn has_name_default(class: &ClassDesc) -> bool {
    class
        .defaults
        .iter()
        .any(|(name, _)| *name == NAME_PROPERTY)
}

/// The lookup table of one class. Every name and alias must be unique across it.
fn name_table(class: &'static str, members: &[Member]) -> Result<NameTable<MemberId>, SchemaError> {
    let mut entries = Vec::with_capacity(members.len());
    let mut seen = HashSet::with_capacity(members.len());
    for (index, member) in members.iter().enumerate() {
        let id = MemberId(index as u16);
        for name in std::iter::once(member.name()).chain(member.aliases().iter().copied()) {
            if !seen.insert(name) {
                return Err(SchemaError::DuplicateMember {
                    class,
                    member: name,
                });
            }
            entries.push((name, id));
        }
    }
    Ok(NameTable::new(entries))
}

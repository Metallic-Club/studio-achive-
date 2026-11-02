//! The descriptors a class file writes: [`ClassDesc`] and its member descriptors.
//!
//! Everything here is plain `const` data with nothing specific to one scripting language.
//! [`resolve`](super::resolve()) turns these into flat per-class tables.
use super::call::{EnumItemRef, MethodFn, ScriptValue};
use crate::value::{Color3, Value, ValueType};

/// What a class is allowed to be. See `docs/design/class-system.md` section 3.1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClassFlags(u8);

impl ClassFlags {
    /// No flag: exists, but `Instance.new` refuses it (a missing `CREATABLE` fails closed).
    pub const NONE: ClassFlags = ClassFlags(0);
    /// Cannot be created; exists for inheritance and `IsA`.
    pub const ABSTRACT: ClassFlags = ClassFlags(1);
    /// `Instance.new` and the Insert menu may create it.
    pub const CREATABLE: ClassFlags = ClassFlags(1 << 1);
    /// Exactly one per game, reached by `GetService`; not creatable.
    pub const SERVICE: ClassFlags = ClassFlags(1 << 2);
    /// Instances of this class are never sent to clients. Meaning only: instance and owner
    /// visibility stays in [`crate::replication`].
    pub const NOT_REPLICATED: ClassFlags = ClassFlags(1 << 3);
    /// Scripts are warned on use; [`ClassDesc::deprecated_by`] names the replacement.
    pub const DEPRECATED: ClassFlags = ClassFlags(1 << 4);

    /// Both sets of flags.
    pub const fn with(self, other: ClassFlags) -> ClassFlags {
        ClassFlags(self.0 | other.0)
    }

    /// True when every flag of `other` is set.
    pub const fn contains(self, other: ClassFlags) -> bool {
        self.0 & other.0 == other.0
    }

    /// The flags as one byte, for the registry fingerprint.
    pub const fn bits(self) -> u8 {
        self.0
    }
}

/// Which side may use a member. Enforcement arrives with #468; the declaration is hashed now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberContext {
    /// Server and client.
    Any,
    /// Server scripts only.
    Server,
    /// Client scripts only.
    Client,
}

impl MemberContext {
    /// True when this context allows no more than `other`: an override may narrow, never widen.
    pub const fn within(self, other: MemberContext) -> bool {
        matches!(
            (self, other),
            (_, MemberContext::Any)
                | (MemberContext::Server, MemberContext::Server)
                | (MemberContext::Client, MemberContext::Client)
        )
    }
}

/// Registry-level eligibility for a property sent from server to client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repl {
    /// Eligible for replication, subject to instance and owner visibility rules.
    Replicated,
    /// Never sent to a player.
    ServerOnly,
}

/// A numeric property's sane range: values outside are clamped (or wrapped, for a time of day).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NumRange {
    /// Smallest value.
    pub min: f64,
    /// Largest value (exclusive when `wrap`).
    pub max: f64,
    /// Wrap around instead of clamping (`ClockTime` 25 is 1).
    pub wrap: bool,
}

impl NumRange {
    /// `x` brought into the range. `x` must be finite.
    pub fn apply_number(self, x: f64) -> f64 {
        if self.wrap {
            let span = self.max - self.min;
            let w = self.min + (x - self.min).rem_euclid(span);
            // rem_euclid can round up to `span` for tiny negative inputs.
            if w >= self.max { self.min } else { w }
        } else {
            x.clamp(self.min, self.max)
        }
    }

    /// `value` brought into the range: Numbers and Color3 components; other types unchanged.
    pub fn apply(self, value: Value) -> Value {
        match value {
            Value::Number(n) => Value::Number(self.apply_number(n)),
            Value::Color3(c) => Value::Color3(Color3::new(
                self.apply_number(c.r),
                self.apply_number(c.g),
                self.apply_number(c.b),
            )),
            other => other,
        }
    }
}

/// A set of named values: `Enum.<name>.<item>` in Luau, and an enum in every generated language.
///
/// Declared next to the class that owns it ([`ClassDesc::enums`]), or under `classes/enums/` when
/// no one class does. A property of this type stores the item name as a String, so scene files
/// and the replication and Studio paths see plain names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumDesc {
    /// The name scripts use. Unique in the registry.
    pub name: &'static str,
    /// Item names. A property's `allowed` is this same slice.
    pub items: &'static [&'static str],
    /// One `Value` per item; `None` numbers the items from 0 in order.
    pub numbers: Option<&'static [i64]>,
    /// `(old name, item)`: old names that still resolve to that item, in scripts and scene files.
    pub aliases: &'static [(&'static str, &'static str)],
    /// One or two sentences for tooltips and the reference. Not hashed.
    pub description: &'static str,
}

impl EnumDesc {
    /// An enum whose items are numbered from 0.
    pub const fn new(name: &'static str, items: &'static [&'static str]) -> EnumDesc {
        EnumDesc {
            name,
            items,
            numbers: None,
            aliases: &[],
            description: "",
        }
    }

    /// Gives each item its own `Value`, in `items` order.
    pub const fn numbers(mut self, numbers: &'static [i64]) -> EnumDesc {
        self.numbers = Some(numbers);
        self
    }

    /// Old names that still resolve to an item: `(old name, item)`.
    pub const fn aliases(mut self, aliases: &'static [(&'static str, &'static str)]) -> EnumDesc {
        self.aliases = aliases;
        self
    }

    /// Text for tooltips and the reference.
    pub const fn describe(mut self, description: &'static str) -> EnumDesc {
        self.description = description;
        self
    }

    /// The `Value` of the item at `index`, which must be in range.
    pub fn value_at(&self, index: usize) -> i64 {
        match self.numbers {
            Some(numbers) => numbers[index],
            None => index as i64,
        }
    }

    /// The item with this name or alias, as it crosses the host interface.
    pub fn item_ref(&self, name: &str) -> Option<EnumItemRef> {
        let item = self.items.iter().find(|item| **item == name).copied();
        let item = item.or_else(|| self.alias_target(name))?;
        Some(EnumItemRef::new(self.name, item))
    }

    /// The item an old name stands for.
    pub fn alias_target(&self, old: &str) -> Option<&'static str> {
        self.aliases
            .iter()
            .find(|(name, _)| *name == old)
            .map(|(_, item)| *item)
    }
}

/// How a property's value is held. `Computed { get, set }` arrives with #464.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PropAccess {
    /// The World stores the value.
    Stored,
}

/// A titled group of rows in the Studio Properties panel. A class file may use its own: sections
/// sort by `order`, then by name, so `Section::new("Beacon", 50)` lands between Data and Appearance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Section {
    /// The title.
    pub name: &'static str,
    /// Lower comes first.
    pub order: u16,
}

impl Section {
    /// The default: what a class is, its own settings.
    pub const DATA: Section = Section::new("Data", 0);
    /// How it looks.
    pub const APPEARANCE: Section = Section::new("Appearance", 100);
    /// Where it is and how big.
    pub const TRANSFORM: Section = Section::new("Transform", 200);
    /// How it collides and moves.
    pub const PHYSICS: Section = Section::new("Physics", 300);
    /// Whether it runs.
    pub const BEHAVIOUR: Section = Section::new("Behaviour", 400);

    /// A section with this title and place.
    pub const fn new(name: &'static str, order: u16) -> Section {
        Section { name, order }
    }
}

/// What a number measures, which decides how Studio shows it and how fast it drags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumberUnit {
    /// Lengths and other measures with no unit of their own.
    Studs,
    /// Angles.
    Degrees,
    /// A 0-1 fraction.
    Fraction,
    /// A whole number.
    Integer,
}

/// The control a property gets in the Studio Properties panel.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Widget {
    /// Chosen from the value: checkbox, number field, text field, colour swatch, or a dropdown when
    /// the property has `allowed`. A number with a 0-1 range shows as a fraction.
    Auto,
    /// A number slider between these values.
    Slider {
        /// The left end.
        min: f64,
        /// The right end.
        max: f64,
    },
    /// Number fields that scrub on drag, in this unit (one field, or one per axis of a Vector3).
    Drag(NumberUnit),
    /// A text box several lines tall.
    Multiline,
    /// A text field holding an asset. Studio has no asset picker yet, so it looks like `Auto`.
    Asset,
    /// A dropdown of these values, for a String whose writes are not limited to them. A property
    /// with `allowed` needs no widget: `Auto` already gives it a dropdown.
    Enum(&'static [&'static str]),
    /// A button that opens the text in the script editor.
    Script,
    /// A button that opens the style picker.
    StylePicker,
}

/// When a property's row shows. It is checked against the same class's other properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisibleWhen {
    /// This Bool property has this value.
    Is(&'static str, bool),
    /// This String property has one of these values.
    OneOf(&'static str, &'static [&'static str]),
}

impl VisibleWhen {
    /// The property the condition reads.
    pub const fn property(&self) -> &'static str {
        match *self {
            VisibleWhen::Is(property, _) | VisibleWhen::OneOf(property, _) => property,
        }
    }

    /// True when `value`, the current value of [`Self::property`], meets the condition. A missing
    /// value or one of another type does not.
    pub fn holds(&self, value: Option<&Value>) -> bool {
        match (*self, value) {
            (VisibleWhen::Is(_, want), Some(Value::Bool(have))) => want == *have,
            (VisibleWhen::OneOf(_, values), Some(Value::String(have))) => {
                values.contains(&have.as_str())
            }
            _ => false,
        }
    }
}

/// How the Studio Properties panel shows a property. Display only, so it is not part of
/// `registry_hash` and two peers may differ in it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Editor {
    /// The section its row goes in.
    pub section: Section,
    /// Lower comes first within the section; equal keeps declaration order, base class first.
    pub order: u16,
    /// Shown instead of the property name.
    pub label: Option<&'static str>,
    /// The control.
    pub widget: Widget,
    /// The row only shows while this holds.
    pub visible_when: Option<VisibleWhen>,
    /// Shown but not editable in Studio. Scripts are a separate matter: see [`PropDesc::readonly`].
    pub readonly: bool,
}

impl Editor {
    /// In [`Section::DATA`], named by the property, with the [`Widget::Auto`] control.
    pub const fn new() -> Editor {
        Editor::section(Section::DATA)
    }

    /// The same defaults, in this section.
    pub const fn section(section: Section) -> Editor {
        Editor {
            section,
            order: 0,
            label: None,
            widget: Widget::Auto,
            visible_when: None,
            readonly: false,
        }
    }

    /// Places the row within its section.
    pub const fn order(mut self, order: u16) -> Editor {
        self.order = order;
        self
    }

    /// Shows this instead of the property name.
    pub const fn label(mut self, label: &'static str) -> Editor {
        self.label = Some(label);
        self
    }

    /// A slider between `min` and `max`.
    pub const fn slider(mut self, min: f64, max: f64) -> Editor {
        self.widget = Widget::Slider { min, max };
        self
    }

    /// Number fields in this unit.
    pub const fn drag(mut self, unit: NumberUnit) -> Editor {
        self.widget = Widget::Drag(unit);
        self
    }

    /// A text box several lines tall.
    pub const fn multiline(mut self) -> Editor {
        self.widget = Widget::Multiline;
        self
    }

    /// An asset field.
    pub const fn asset(mut self) -> Editor {
        self.widget = Widget::Asset;
        self
    }

    /// A dropdown of these values.
    pub const fn enumerated(mut self, values: &'static [&'static str]) -> Editor {
        self.widget = Widget::Enum(values);
        self
    }

    /// A button that opens the script editor.
    pub const fn script(mut self) -> Editor {
        self.widget = Widget::Script;
        self
    }

    /// A button that opens the style picker.
    pub const fn style_picker(mut self) -> Editor {
        self.widget = Widget::StylePicker;
        self
    }

    /// Shows the row only while `condition` holds.
    pub const fn visible_when(mut self, condition: VisibleWhen) -> Editor {
        self.visible_when = Some(condition);
        self
    }

    /// Shown but not editable in Studio.
    pub const fn readonly(mut self) -> Editor {
        self.readonly = true;
        self
    }
}

impl Default for Editor {
    fn default() -> Editor {
        Editor::new()
    }
}

/// Whether and where the Studio Insert lists offer a [`ClassFlags::CREATABLE`] class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InsertEntry {
    /// Offered. Lower comes first. A category sits where its lowest number puts it, so its classes
    /// stay together. The default sorts last, by name.
    Listed(u16),
    /// `Instance.new` can make it, but the lists do not offer it.
    Hidden,
}

/// Definition of one property of a class.
#[derive(Clone, Copy)]
pub struct PropDesc {
    /// Property name.
    pub name: &'static str,
    /// Type that values must have.
    pub ty: ValueType,
    /// Builds an owned default; the registry calls this once per class while it resolves.
    pub default: fn() -> Value,
    /// When set, a String value must be one of these.
    pub allowed: Option<&'static [&'static str]>,
    /// Engine bookkeeping that Luau and the Properties panel do not show (for example the scale
    /// behind `Model:GetScale`). Saved and cloned like any other property.
    pub hidden: bool,
    /// Replication eligibility, subject to container/source/owner policy.
    pub repl: Repl,
    /// When set, Number values and every Color3 component are brought into this range on
    /// write (and so on load and from scripts).
    pub range: Option<NumRange>,
    /// When set, a String write that is not in `allowed` is passed through this first: it
    /// returns the stored form (for example a display name to an id), or `None` to refuse.
    pub normalize: Option<fn(&str) -> Option<&'static str>>,
    /// Stable normalization-contract identifier. Bump it when accepted aliases or rules change.
    /// Present exactly when `normalize` is present; protocol hashes never encode function addresses.
    pub normalize_schema_id: Option<&'static str>,
    /// Scripts cannot write it; the engine can. Enforced by the generic binding (#462).
    pub readonly: bool,
    /// How the value is held.
    pub access: PropAccess,
    /// Written to scene files.
    pub saved: bool,
    /// Old names that still resolve to this member.
    pub aliases: &'static [&'static str],
    /// The enum this String property holds an item name of. Set with [`PropDesc::enum_of`], which
    /// also derives `allowed` from it. Stored values stay plain names; bindings show the item.
    pub enum_type: Option<&'static EnumDesc>,
    /// Replaces an inherited property of the same name and type.
    pub overrides: bool,
    /// Who may use it; `None` takes the class default.
    pub context: Option<MemberContext>,
    /// Deprecated in favour of this member (empty when there is no replacement).
    pub deprecated: Option<&'static str>,
    /// One or two sentences for tooltips and the reference. Not hashed.
    pub description: &'static str,
    /// How the Studio Properties panel shows it. Not hashed.
    pub editor: Editor,
}

impl PropDesc {
    /// A stored, replicated, saved property.
    pub const fn new(name: &'static str, ty: ValueType, default: fn() -> Value) -> PropDesc {
        PropDesc {
            name,
            ty,
            default,
            allowed: None,
            hidden: false,
            repl: Repl::Replicated,
            range: None,
            normalize: None,
            normalize_schema_id: None,
            readonly: false,
            access: PropAccess::Stored,
            saved: true,
            aliases: &[],
            enum_type: None,
            overrides: false,
            context: None,
            deprecated: None,
            description: "",
            editor: Editor::new(),
        }
    }

    /// Clamps Number values and Color3 components into `min..=max` on write.
    pub const fn range(mut self, min: f64, max: f64) -> PropDesc {
        self.range = Some(NumRange {
            min,
            max,
            wrap: false,
        });
        self
    }

    /// Wraps Number values into `min..max` on write.
    pub const fn wrapped(mut self, min: f64, max: f64) -> PropDesc {
        self.range = Some(NumRange {
            min,
            max,
            wrap: true,
        });
        self
    }

    /// Restricts a String to these values.
    pub const fn allowed(mut self, allowed: &'static [&'static str]) -> PropDesc {
        self.allowed = Some(allowed);
        self
    }

    /// Holds an item name of `desc`: `allowed` is its items, and its aliases are accepted on write.
    pub const fn enum_of(mut self, desc: &'static EnumDesc) -> PropDesc {
        self.allowed = Some(desc.items);
        self.enum_type = Some(desc);
        self
    }

    /// Marks engine bookkeeping that scripts and the Properties panel do not show.
    pub const fn hidden(mut self) -> PropDesc {
        self.hidden = true;
        self
    }

    /// Never sent to a player.
    pub const fn server_only(mut self) -> PropDesc {
        self.repl = Repl::ServerOnly;
        self
    }

    /// Passes String writes outside `allowed` through `normalize`; `schema_id` names its contract.
    pub const fn normalized(
        mut self,
        normalize: fn(&str) -> Option<&'static str>,
        schema_id: &'static str,
    ) -> PropDesc {
        self.normalize = Some(normalize);
        self.normalize_schema_id = Some(schema_id);
        self
    }

    /// Replaces an inherited property of the same name and type.
    pub const fn overrides(mut self) -> PropDesc {
        self.overrides = true;
        self
    }

    /// Old names that still resolve to this member.
    pub const fn aliases(mut self, aliases: &'static [&'static str]) -> PropDesc {
        self.aliases = aliases;
        self
    }

    /// Who may use it, instead of the class default.
    pub const fn context(mut self, context: MemberContext) -> PropDesc {
        self.context = Some(context);
        self
    }

    /// Text for tooltips and the reference.
    pub const fn describe(mut self, description: &'static str) -> PropDesc {
        self.description = description;
        self
    }

    /// A stored value as every language's script sees it: an enum property's name becomes its item.
    /// `HostApi::get_property` goes through this, so storage can stay a String.
    #[inline]
    pub fn script_value(&self, stored: &Value) -> ScriptValue {
        if let (Some(desc), Value::String(name)) = (self.enum_type, stored)
            && let Some(item) = desc.item_ref(name)
        {
            return ScriptValue::EnumItem(item);
        }
        ScriptValue::from(stored)
    }

    /// What a script's item stores on this property: its name, or `None` when it is not an item of
    /// this property's enum. `HostApi::set_property` goes through this; a name needs no conversion.
    pub fn stored_item(&self, item: EnumItemRef) -> Option<Value> {
        let desc = self
            .enum_type
            .filter(|desc| desc.name == item.enum_name())?;
        let name = desc.items.iter().find(|name| **name == item.item())?;
        Some(Value::String((*name).to_owned()))
    }

    /// What a String write outside `allowed` is stored as: the normalizer's answer, or the item an
    /// enum alias stands for. `None` refuses the write.
    pub fn stored_form(&self, text: &str) -> Option<&'static str> {
        match self.normalize {
            Some(normalize) => normalize(text),
            None => self.enum_type?.alias_target(text),
        }
    }

    /// How the Studio Properties panel shows it.
    pub const fn editor(mut self, editor: Editor) -> PropDesc {
        self.editor = editor;
        self
    }
}

/// A language-neutral type in a method, signal or callback signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ty {
    /// No value.
    Nil,
    /// A boolean.
    Bool,
    /// A whole number.
    Int,
    /// A float.
    Number,
    /// A string.
    String,
    /// An instance, optionally of a class.
    Instance(Option<&'static str>),
    /// A `Vector3`.
    Vector3,
    /// A `CFrame`.
    CFrame,
    /// A `Color3`.
    Color3,
    /// A `UDim2`.
    UDim2,
    /// An item of the named enum.
    Enum(&'static str),
    /// A list.
    Array(&'static Ty),
    /// A string-keyed map.
    Map(&'static Ty),
    /// A function value (signal handlers, callbacks).
    Function(&'static FunctionSig),
    /// The type or nil.
    Optional(&'static Ty),
    /// Several values (returns only).
    Tuple(&'static [Ty]),
    /// A signal object, as `GetPropertyChangedSignal` returns (returns only).
    Signal,
    /// Any value.
    Any,
}

/// The shape of a function value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FunctionSig {
    /// Parameter types.
    pub params: &'static [Ty],
    /// Result type.
    pub returns: &'static Ty,
}

/// What an accessor names as its expected function type when no parameter declares one.
pub(super) static ANY_FUNCTION: FunctionSig = FunctionSig {
    params: &[],
    returns: &Ty::Any,
};

/// How a parameter's type is checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArgCheck {
    /// The binding checks the type from the signature.
    Auto,
    /// The method checks it and keeps its own error text (migration only, design section 5.5).
    Manual,
}

/// One parameter of a method, signal or callback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Param {
    /// Parameter name, for docs and generated bindings.
    pub name: &'static str,
    /// Declared type.
    pub ty: Ty,
    /// May be left out.
    pub optional: bool,
    /// Who checks the type.
    pub check: ArgCheck,
}

impl Param {
    /// A required parameter the binding checks.
    pub const fn new(name: &'static str, ty: Ty) -> Param {
        Param {
            name,
            ty,
            optional: false,
            check: ArgCheck::Auto,
        }
    }

    /// May be left out.
    pub const fn optional(mut self) -> Param {
        self.optional = true;
        self
    }

    /// The method checks the type itself.
    pub const fn manual(mut self) -> Param {
        self.check = ArgCheck::Manual;
        self
    }
}

/// Where a method's body lives.
#[derive(Debug, Clone, Copy)]
pub enum MethodImpl {
    /// In the class file; needs only the world.
    Native(MethodFn),
    /// In `metallic_script`, because it needs the script host.
    Host,
    /// In a Luau snippet next to the class file (#470).
    Luau,
}

impl PartialEq for MethodImpl {
    fn eq(&self, other: &MethodImpl) -> bool {
        match (self, other) {
            (MethodImpl::Native(a), MethodImpl::Native(b)) => std::ptr::fn_addr_eq(*a, *b),
            (MethodImpl::Host, MethodImpl::Host) | (MethodImpl::Luau, MethodImpl::Luau) => true,
            _ => false,
        }
    }
}

impl Eq for MethodImpl {}

/// One method a class declares. No script call goes through this until #462-#465: the Luau binding
/// still dispatches its own table, so declaring a method here changes no script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodDesc {
    /// Method name.
    pub name: &'static str,
    /// Parameters.
    pub params: &'static [Param],
    /// Type of trailing `...` arguments, when it takes them.
    pub variadic: Option<Ty>,
    /// Result type.
    pub returns: Ty,
    /// The call may park the calling thread.
    pub yields: bool,
    /// Where the body lives.
    pub implementation: MethodImpl,
    /// Replaces an inherited method of the same name and signature.
    pub overrides: bool,
    /// Who may call it; `None` takes the class default.
    pub context: Option<MemberContext>,
    /// Deprecated in favour of this member (empty when there is no replacement).
    pub deprecated: Option<&'static str>,
    /// Text for tooltips and the reference. Not hashed.
    pub description: &'static str,
}

impl MethodDesc {
    /// A method implemented in `metallic_script`, returning nothing and taking no arguments.
    pub const fn host(name: &'static str) -> MethodDesc {
        MethodDesc {
            name,
            params: &[],
            variadic: None,
            returns: Ty::Nil,
            yields: false,
            implementation: MethodImpl::Host,
            overrides: false,
            context: None,
            deprecated: None,
            description: "",
        }
    }

    /// A method implemented in the class file, returning nothing and taking no arguments.
    pub const fn native(name: &'static str, body: MethodFn) -> MethodDesc {
        let mut method = MethodDesc::host(name);
        method.implementation = MethodImpl::Native(body);
        method
    }

    /// Declares the parameters.
    pub const fn params(mut self, params: &'static [Param]) -> MethodDesc {
        self.params = params;
        self
    }

    /// Declares the result type.
    pub const fn returns(mut self, returns: Ty) -> MethodDesc {
        self.returns = returns;
        self
    }

    /// Takes any number of trailing arguments of this type.
    pub const fn variadic(mut self, ty: Ty) -> MethodDesc {
        self.variadic = Some(ty);
        self
    }

    /// May park the calling thread by returning `CallOutcome::Wait`.
    pub const fn yields(mut self) -> MethodDesc {
        self.yields = true;
        self
    }

    /// Replaces an inherited method of the same name and signature.
    pub const fn overrides(mut self) -> MethodDesc {
        self.overrides = true;
        self
    }

    /// Who may call it, instead of the class default.
    pub const fn context(mut self, context: MemberContext) -> MethodDesc {
        self.context = Some(context);
        self
    }

    /// Text for tooltips and the reference.
    pub const fn describe(mut self, description: &'static str) -> MethodDesc {
        self.description = description;
        self
    }

    /// True when `other` has the same parameter types, result and yielding.
    pub fn same_signature(&self, other: &MethodDesc) -> bool {
        self.variadic == other.variadic
            && self.returns == other.returns
            && self.yields == other.yields
            && self.params.len() == other.params.len()
            && self
                .params
                .iter()
                .zip(other.params)
                .all(|(a, b)| a.ty == b.ty && a.optional == b.optional)
    }
}

/// One signal a class declares: inherited like any member, fired with
/// [`World::fire`](crate::World::fire) or [`MethodCall::fire`](super::MethodCall::fire), connected
/// through the host interface. See "How to add a signal" in `ARCHITECTURE.md`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalDesc {
    /// Signal name.
    pub name: &'static str,
    /// The payload.
    pub params: &'static [Param],
    /// Type of further values after `params`, when the payload is open-ended.
    pub variadic: Option<Ty>,
    /// Who may connect; `None` takes the class default.
    pub context: Option<MemberContext>,
    /// Keyed by an argument (`GetAttributeChangedSignal(name)`).
    pub parameterized: bool,
    /// Deprecated in favour of this member (empty when there is no replacement).
    pub deprecated: Option<&'static str>,
    /// Text for tooltips and the reference. Not hashed.
    pub description: &'static str,
}

impl SignalDesc {
    /// A signal with no payload.
    pub const fn new(name: &'static str) -> SignalDesc {
        SignalDesc {
            name,
            params: &[],
            variadic: None,
            context: None,
            parameterized: false,
            deprecated: None,
            description: "",
        }
    }

    /// Declares the payload.
    pub const fn params(mut self, params: &'static [Param]) -> SignalDesc {
        self.params = params;
        self
    }

    /// The payload goes on with any number of values of this type.
    pub const fn variadic(mut self, ty: Ty) -> SignalDesc {
        self.variadic = Some(ty);
        self
    }

    /// Who may connect, instead of the class default.
    pub const fn context(mut self, context: MemberContext) -> SignalDesc {
        self.context = Some(context);
        self
    }

    /// Also keyed by an argument, as `GetPropertyChangedSignal(name)` is to `Changed`.
    pub const fn parameterized(mut self) -> SignalDesc {
        self.parameterized = true;
        self
    }

    /// Text for tooltips and the reference.
    pub const fn describe(mut self, description: &'static str) -> SignalDesc {
        self.description = description;
        self
    }
}

/// A member a script assigns a function to, such as `RemoteFunction.OnServerInvoke`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallbackDesc {
    /// Callback name.
    pub name: &'static str,
    /// What the function receives.
    pub params: &'static [Param],
    /// What the function returns.
    pub returns: Ty,
    /// Who may set it; `None` takes the class default.
    pub context: Option<MemberContext>,
    /// Text for tooltips and the reference. Not hashed.
    pub description: &'static str,
}

impl CallbackDesc {
    /// A callback that takes nothing and returns nothing.
    pub const fn new(name: &'static str) -> CallbackDesc {
        CallbackDesc {
            name,
            params: &[],
            returns: Ty::Nil,
            context: None,
            description: "",
        }
    }

    /// Text for tooltips and the reference.
    pub const fn describe(mut self, description: &'static str) -> CallbackDesc {
        self.description = description;
        self
    }
}

/// A new default for an inherited property: its name and what builds the value.
pub type DefaultOverride = (&'static str, fn() -> Value);

/// Definition of one class: what its file exports as `pub static CLASS`.
#[derive(Clone, Copy)]
pub struct ClassDesc {
    /// The class name scripts and scene files use. Unique.
    pub name: &'static str,
    /// By name, so class files never import each other. `None` only for `Instance`.
    pub superclass: Option<&'static str>,
    /// What the class is allowed to be.
    pub flags: ClassFlags,
    /// Insert menu and docs grouping. Not hashed.
    pub category: &'static str,
    /// Its place in the Studio Insert lists, when `CREATABLE`. Not hashed.
    pub insert: InsertEntry,
    /// Draw it with the icon of this class instead of working one out from its superclass. Not
    /// hashed.
    pub icon_like: Option<&'static str>,
    /// One or two sentences for tooltips and the reference. Not hashed.
    pub description: &'static str,
    /// The class that replaces this one, with [`ClassFlags::DEPRECATED`].
    pub deprecated_by: Option<&'static str>,
    /// Default context for this class's own members.
    pub context: MemberContext,
    /// Properties this class declares (not inherited ones).
    pub props: &'static [PropDesc],
    /// Methods this class declares.
    pub methods: &'static [MethodDesc],
    /// Signals this class declares, by reference so the class file can name them.
    pub signals: &'static [&'static SignalDesc],
    /// Callbacks this class declares.
    pub callbacks: &'static [CallbackDesc],
    /// New defaults for inherited properties, for this class and its subclasses.
    pub defaults: &'static [DefaultOverride],
    /// Inherited members hidden from scripts, Studio and docs for this class and its subclasses.
    pub hide: &'static [&'static str],
    /// Enums this class owns.
    pub enums: &'static [EnumDesc],
}

impl ClassDesc {
    /// A class with no flags and no members.
    pub const fn new(name: &'static str, superclass: Option<&'static str>) -> ClassDesc {
        ClassDesc {
            name,
            superclass,
            flags: ClassFlags::NONE,
            category: "",
            insert: InsertEntry::Listed(u16::MAX),
            icon_like: None,
            description: "",
            deprecated_by: None,
            context: MemberContext::Any,
            props: &[],
            methods: &[],
            signals: &[],
            callbacks: &[],
            defaults: &[],
            hide: &[],
            enums: &[],
        }
    }

    /// Sets the flags.
    pub const fn flags(mut self, flags: ClassFlags) -> ClassDesc {
        self.flags = flags;
        self
    }

    /// Sets the docs and Insert menu grouping.
    pub const fn category(mut self, category: &'static str) -> ClassDesc {
        self.category = category;
        self
    }

    /// Places it in the Studio Insert lists. See [`InsertEntry::Listed`].
    pub const fn insert_order(mut self, order: u16) -> ClassDesc {
        self.insert = InsertEntry::Listed(order);
        self
    }

    /// Leaves it out of the Studio Insert lists; `Instance.new` can still make it.
    pub const fn no_insert(mut self) -> ClassDesc {
        self.insert = InsertEntry::Hidden;
        self
    }

    /// Draws it with the icon of `class`.
    pub const fn icon_like(mut self, class: &'static str) -> ClassDesc {
        self.icon_like = Some(class);
        self
    }

    /// Sets the description.
    pub const fn describe(mut self, description: &'static str) -> ClassDesc {
        self.description = description;
        self
    }

    /// Declares this class's properties.
    pub const fn props(mut self, props: &'static [PropDesc]) -> ClassDesc {
        self.props = props;
        self
    }

    /// Declares this class's methods.
    pub const fn methods(mut self, methods: &'static [MethodDesc]) -> ClassDesc {
        self.methods = methods;
        self
    }

    /// Declares this class's signals.
    pub const fn signals(mut self, signals: &'static [&'static SignalDesc]) -> ClassDesc {
        self.signals = signals;
        self
    }

    /// Declares this class's callbacks.
    pub const fn callbacks(mut self, callbacks: &'static [CallbackDesc]) -> ClassDesc {
        self.callbacks = callbacks;
        self
    }

    /// Changes the defaults of inherited properties.
    pub const fn defaults(mut self, defaults: &'static [DefaultOverride]) -> ClassDesc {
        self.defaults = defaults;
        self
    }

    /// Hides inherited members.
    pub const fn hide(mut self, hide: &'static [&'static str]) -> ClassDesc {
        self.hide = hide;
        self
    }

    /// Declares the enums this class owns.
    pub const fn enums(mut self, enums: &'static [EnumDesc]) -> ClassDesc {
        self.enums = enums;
        self
    }

    /// True when `Instance.new` may create this class.
    pub const fn is_creatable(&self) -> bool {
        self.flags.contains(ClassFlags::CREATABLE)
    }
}

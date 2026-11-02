//! The calling convention of class methods, with nothing specific to one scripting language.
//!
//! A method is a plain Rust function over [`MethodCall`]. It reads [`ScriptValue`] arguments that
//! the host already checked against the declared signature, and returns a [`CallOutcome`]: a value
//! or, for a yielding method, a [`WaitRequest`]. Failures are a structured [`ApiError`] with one
//! formatter here, so every language prints the same message. Design:
//! `docs/design/class-system.md` section 5.
//!
//! # How to write a method
//!
//! Write the function, then declare it in the class file's `.methods(&[...])`:
//!
//! ```
//! use metallic_core::classes::{ApiError, CallOutcome, MethodCall, MethodDesc, Param, Ty};
//! use metallic_core::Value;
//!
//! fn set_lit(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
//!     // Checked against the signature already; this `?` cannot fire for an Auto parameter.
//!     let lit = call.opt_bool(0)?.unwrap_or(true);
//!     call.world.set(call.this, "Lit", Value::Bool(lit))?;
//!     Ok(lit.into())
//! }
//!
//! static SET_LIT: MethodDesc = MethodDesc::native("SetLit", set_lit)
//!     .params(&[Param::new("lit", Ty::Bool).optional()])
//!     .returns(Ty::Bool)
//!     .describe("Lights or darkens the beacon; lights it when left out.");
//! # assert_eq!(SET_LIT.name, "SetLit");
//! ```
//!
//! A method that needs the script host (fires a remote, calls back into script code, reads host
//! state) is declared with [`MethodDesc::host`] and implemented in `metallic_script`; see
//! [`HostMethods`](super::HostMethods). A method that parks its caller is declared with
//! [`MethodDesc::yields`] and returns [`CallOutcome::Wait`].
use super::desc::{ANY_FUNCTION, ArgCheck, MethodDesc, Ty};
use super::resolve::MemberId;
use crate::cframe::CFrame;
use crate::value::{Color3, UDim2, Value, ValueType, Vec3};
use crate::{CoreError, InstanceId, World};
use std::borrow::Cow;
use std::fmt;

/// A reference to script code a binding owns (a callback or a signal handler). Opaque to core.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionRef(pub u64);

/// An item of an enum.
///
/// Held by name until `EnumDesc` gives enums ids (#469). The fields are private so that change
/// reaches no caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EnumItemRef {
    enum_name: &'static str,
    item: &'static str,
}

impl EnumItemRef {
    /// The item `item` of the enum `enum_name`.
    pub const fn new(enum_name: &'static str, item: &'static str) -> EnumItemRef {
        EnumItemRef { enum_name, item }
    }

    /// The enum's name.
    pub const fn enum_name(self) -> &'static str {
        self.enum_name
    }

    /// The item's name.
    pub const fn item(self) -> &'static str {
        self.item
    }
}

/// The value that crosses the host interface, in every language.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptValue {
    /// No value.
    Nil,
    /// A boolean.
    Bool(bool),
    /// A whole number from a typed language, or a whole Luau number.
    Int(i64),
    /// A float.
    Number(f64),
    /// A string.
    String(Box<str>),
    /// A handle; the host checks liveness on use.
    Instance(InstanceId),
    /// A `Vector3`.
    Vector3(Vec3),
    /// A `CFrame`, boxed as in [`Value`].
    CFrame(Box<CFrame>),
    /// A `Color3`.
    Color3(Color3),
    /// A `UDim2`.
    UDim2(UDim2),
    /// An item of an enum.
    EnumItem(EnumItemRef),
    /// A list.
    Array(Vec<ScriptValue>),
    /// Key and value pairs.
    Map(Vec<(ScriptValue, ScriptValue)>),
    /// Script code the binding owns.
    Function(FunctionRef),
    /// Several results.
    Tuple(Vec<ScriptValue>),
}

/// 2^63: the first `f64` above the `i64` range, and the magnitude of its lowest value.
const I64_BOUND: f64 = 9_223_372_036_854_775_808.0;

impl ScriptValue {
    /// The boolean, if this is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ScriptValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// The whole number: an `Int`, or a `Number` that is whole and in range.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            ScriptValue::Int(i) => Some(*i),
            ScriptValue::Number(n) if n.fract() == 0.0 && *n >= -I64_BOUND && *n < I64_BOUND => {
                Some(*n as i64)
            }
            _ => None,
        }
    }

    /// The number: a `Number`, or an `Int` (accepted wherever a `Number` is declared).
    pub fn as_number(&self) -> Option<f64> {
        match self {
            ScriptValue::Number(n) => Some(*n),
            ScriptValue::Int(i) => Some(*i as f64),
            _ => None,
        }
    }

    /// The string, if this is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            ScriptValue::String(s) => Some(s),
            _ => None,
        }
    }

    /// The handle, if this is one. It may be stale.
    pub fn as_instance(&self) -> Option<InstanceId> {
        match self {
            ScriptValue::Instance(id) => Some(*id),
            _ => None,
        }
    }

    /// The `Vector3`, if this is one.
    pub fn as_vector3(&self) -> Option<Vec3> {
        match self {
            ScriptValue::Vector3(v) => Some(*v),
            _ => None,
        }
    }

    /// The `CFrame`, if this is one.
    pub fn as_cframe(&self) -> Option<CFrame> {
        match self {
            ScriptValue::CFrame(c) => Some(**c),
            _ => None,
        }
    }

    /// The `Color3`, if this is one.
    pub fn as_color3(&self) -> Option<Color3> {
        match self {
            ScriptValue::Color3(c) => Some(*c),
            _ => None,
        }
    }

    /// The `UDim2`, if this is one.
    pub fn as_udim2(&self) -> Option<UDim2> {
        match self {
            ScriptValue::UDim2(u) => Some(*u),
            _ => None,
        }
    }

    /// The enum item, if this is one.
    pub fn as_enum_item(&self) -> Option<EnumItemRef> {
        match self {
            ScriptValue::EnumItem(item) => Some(*item),
            _ => None,
        }
    }

    /// The list, if this is one.
    pub fn as_array(&self) -> Option<&[ScriptValue]> {
        match self {
            ScriptValue::Array(items) => Some(items),
            _ => None,
        }
    }

    /// The key and value pairs, if this is a map.
    pub fn as_map(&self) -> Option<&[(ScriptValue, ScriptValue)]> {
        match self {
            ScriptValue::Map(entries) => Some(entries),
            _ => None,
        }
    }

    /// The function reference, if this is one.
    pub fn as_function(&self) -> Option<FunctionRef> {
        match self {
            ScriptValue::Function(f) => Some(*f),
            _ => None,
        }
    }

    /// This value as a stored property of type `ty`, or `None` when it is not one.
    ///
    /// Nil is the empty [`Value::Ref`]; an `Int` is a [`Value::Number`]. The range and
    /// allowed-value checks stay in [`World::set`].
    pub fn to_value(&self, ty: ValueType) -> Option<Value> {
        Some(match (self, ty) {
            (ScriptValue::Nil, ValueType::Nil) => Value::Nil,
            (ScriptValue::Nil, ValueType::Ref) => Value::Ref(None),
            (ScriptValue::Bool(b), ValueType::Bool) => Value::Bool(*b),
            (ScriptValue::Number(_) | ScriptValue::Int(_), ValueType::Number) => {
                Value::Number(self.as_number()?)
            }
            (ScriptValue::String(s), ValueType::String) => Value::String(s.as_ref().to_owned()),
            (ScriptValue::Vector3(v), ValueType::Vector3) => Value::Vector3(*v),
            (ScriptValue::Color3(c), ValueType::Color3) => Value::Color3(*c),
            (ScriptValue::UDim2(u), ValueType::UDim2) => Value::UDim2(*u),
            (ScriptValue::Instance(id), ValueType::Ref) => Value::Ref(Some(*id)),
            (ScriptValue::CFrame(c), ValueType::CFrame) => Value::CFrame(c.clone()),
            _ => return None,
        })
    }

    /// Like [`ScriptValue::to_value`], moving what it holds, so a string or `CFrame` that came
    /// from a [`Value`] goes back with no allocation.
    pub fn into_value(self, ty: ValueType) -> Option<Value> {
        Some(match (self, ty) {
            (ScriptValue::Bool(b), ValueType::Bool) => Value::Bool(b),
            (ScriptValue::Number(n), ValueType::Number) => Value::Number(n),
            (ScriptValue::String(s), ValueType::String) => Value::String(s.into_string()),
            (ScriptValue::CFrame(c), ValueType::CFrame) => Value::CFrame(c),
            (other, ty) => return other.to_value(ty),
        })
    }

    /// What this value is, for an error message.
    pub fn type_name(&self, world: &World) -> TypeName {
        match self {
            ScriptValue::Nil => TypeName::Nil,
            ScriptValue::Bool(_) => TypeName::Bool,
            // Both are `number`: a Luau number must not change its name with its value.
            ScriptValue::Int(_) | ScriptValue::Number(_) => TypeName::Number,
            ScriptValue::String(_) => TypeName::String,
            ScriptValue::Instance(id) => TypeName::Instance(world.class_name(*id)),
            ScriptValue::Vector3(_) => TypeName::Vector3,
            ScriptValue::CFrame(_) => TypeName::CFrame,
            ScriptValue::Color3(_) => TypeName::Color3,
            ScriptValue::UDim2(_) => TypeName::UDim2,
            ScriptValue::EnumItem(item) => TypeName::Enum(item.enum_name()),
            ScriptValue::Array(_) => TypeName::Array,
            ScriptValue::Map(_) => TypeName::Map,
            ScriptValue::Function(_) => TypeName::Function,
            ScriptValue::Tuple(_) => TypeName::Tuple,
        }
    }
}

impl From<&Value> for ScriptValue {
    fn from(value: &Value) -> ScriptValue {
        match value {
            Value::Nil | Value::Ref(None) => ScriptValue::Nil,
            Value::Bool(b) => ScriptValue::Bool(*b),
            Value::Number(n) => ScriptValue::Number(*n),
            Value::String(s) => ScriptValue::String(s.as_str().into()),
            Value::Vector3(v) => ScriptValue::Vector3(*v),
            Value::Color3(c) => ScriptValue::Color3(*c),
            Value::UDim2(u) => ScriptValue::UDim2(*u),
            Value::Ref(Some(id)) => ScriptValue::Instance(*id),
            Value::CFrame(c) => ScriptValue::CFrame(c.clone()),
        }
    }
}

impl From<Value> for ScriptValue {
    fn from(value: Value) -> ScriptValue {
        match value {
            Value::Nil | Value::Ref(None) => ScriptValue::Nil,
            Value::Bool(b) => ScriptValue::Bool(b),
            Value::Number(n) => ScriptValue::Number(n),
            Value::String(s) => ScriptValue::String(s.into_boxed_str()),
            Value::Vector3(v) => ScriptValue::Vector3(v),
            Value::Color3(c) => ScriptValue::Color3(c),
            Value::UDim2(u) => ScriptValue::UDim2(u),
            Value::Ref(Some(id)) => ScriptValue::Instance(id),
            Value::CFrame(c) => ScriptValue::CFrame(c),
        }
    }
}

impl From<bool> for ScriptValue {
    fn from(value: bool) -> ScriptValue {
        ScriptValue::Bool(value)
    }
}

impl From<f64> for ScriptValue {
    fn from(value: f64) -> ScriptValue {
        ScriptValue::Number(value)
    }
}

impl From<&str> for ScriptValue {
    fn from(value: &str) -> ScriptValue {
        ScriptValue::String(value.into())
    }
}

impl From<InstanceId> for ScriptValue {
    fn from(value: InstanceId) -> ScriptValue {
        ScriptValue::Instance(value)
    }
}

impl From<CFrame> for ScriptValue {
    fn from(value: CFrame) -> ScriptValue {
        ScriptValue::CFrame(Box::new(value))
    }
}

/// True when `value` is a valid `ty`. A destroyed instance is no instance; an `Int` is a `Number`;
/// a `Number` is an `Int` only when whole and in range.
pub fn conforms(world: &World, ty: &Ty, value: &ScriptValue) -> bool {
    match ty {
        Ty::Any => true,
        Ty::Nil => matches!(value, ScriptValue::Nil),
        Ty::Bool => value.as_bool().is_some(),
        Ty::Int => value.as_int().is_some(),
        Ty::Number => value.as_number().is_some(),
        Ty::String => value.as_str().is_some(),
        Ty::Instance(class) => value.as_instance().is_some_and(|id| match class {
            Some(class) => world.is_a(id, class),
            None => world.is_alive(id),
        }),
        Ty::Vector3 => value.as_vector3().is_some(),
        Ty::CFrame => matches!(value, ScriptValue::CFrame(_)),
        Ty::Color3 => value.as_color3().is_some(),
        Ty::UDim2 => value.as_udim2().is_some(),
        Ty::Enum(name) => value
            .as_enum_item()
            .is_some_and(|item| item.enum_name() == *name),
        Ty::Array(inner) => value
            .as_array()
            .is_some_and(|items| items.iter().all(|item| conforms(world, inner, item))),
        Ty::Map(inner) => value.as_map().is_some_and(|entries| {
            entries
                .iter()
                .all(|(key, item)| key.as_str().is_some() && conforms(world, inner, item))
        }),
        Ty::Function(_) => value.as_function().is_some(),
        Ty::Optional(inner) => matches!(value, ScriptValue::Nil) || conforms(world, inner, value),
        Ty::Tuple(types) => match value {
            ScriptValue::Tuple(items) => {
                items.len() == types.len()
                    && types
                        .iter()
                        .zip(items)
                        .all(|(ty, item)| conforms(world, ty, item))
            }
            _ => false,
        },
    }
}

/// Why a value is not a `ty`: a stale handle where an instance belongs is
/// [`ApiErrorKind::Destroyed`], anything else [`ApiErrorKind::BadArgument`].
fn check_value(world: &World, index: usize, ty: &Ty, value: &ScriptValue) -> Option<ApiErrorKind> {
    if conforms(world, ty, value) {
        return None;
    }
    let wants_instance = match ty {
        Ty::Instance(_) => true,
        Ty::Optional(inner) => matches!(inner, Ty::Instance(_)),
        _ => false,
    };
    Some(match value {
        ScriptValue::Instance(id) if wants_instance && !world.is_alive(*id) => {
            ApiErrorKind::Destroyed {
                name: UNNAMED_INSTANCE.to_owned(),
            }
        }
        _ => ApiErrorKind::BadArgument {
            index: position(index),
            expected: *ty,
            got: value.type_name(world),
        },
    })
}

/// Checks arguments against a declared signature: the count (a missing required parameter, too
/// many when not variadic), then each type, left to right. `class` is the class of the instance
/// the method is called on; errors name it.
///
/// A [`ArgCheck::Manual`] parameter is skipped entirely, missing or not: its method keeps its own
/// text for both. Nil counts as left out for an optional parameter.
pub fn check_arguments(
    world: &World,
    class: &'static str,
    desc: &MethodDesc,
    args: &[ScriptValue],
) -> Result<(), ApiError> {
    let failed = |kind| Err(ApiError::new(class, desc.name, kind));
    for (index, param) in desc.params.iter().enumerate().skip(args.len()) {
        if !param.optional && param.check == ArgCheck::Auto {
            return failed(ApiErrorKind::MissingArgument {
                index: position(index),
                expected: param.ty,
            });
        }
    }
    if desc.variadic.is_none() && args.len() > desc.params.len() {
        return failed(ApiErrorKind::TooManyArguments {
            max: desc.params.len() as u32,
            got: args.len() as u32,
        });
    }
    for (index, (param, value)) in desc.params.iter().zip(args).enumerate() {
        let left_out = param.optional && matches!(value, ScriptValue::Nil);
        if param.check == ArgCheck::Auto
            && !left_out
            && let Some(kind) = check_value(world, index, &param.ty, value)
        {
            return failed(kind);
        }
    }
    if let Some(ty) = &desc.variadic {
        for (index, value) in args.iter().enumerate().skip(desc.params.len()) {
            if let Some(kind) = check_value(world, index, ty, value) {
                return failed(kind);
            }
        }
    }
    Ok(())
}

/// A zero-based index as the one-based position a message prints.
fn position(index: usize) -> u32 {
    index as u32 + 1
}

/// Which side a call runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// A server script.
    Server,
    /// A client script.
    Client,
}

/// Who is calling. Member contexts are enforced from this with #468.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CallContext {
    /// The side the call runs on.
    pub side: Side,
    /// The calling script, for errors and deprecation warnings.
    pub script: Option<InstanceId>,
    /// The local player, on a client.
    pub local_player: Option<InstanceId>,
}

/// What a signal is keyed by. Design section 6; #463 fires and delivers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SignalKey {
    /// The instance the signal belongs to.
    pub instance: InstanceId,
    /// The signal.
    pub signal: MemberId,
    /// 0, or an interned name id for a parameterized signal.
    pub arg: u32,
}

/// What a parked thread waits for. The host resumes it with a [`ScriptValue`] when this is
/// satisfied, or with [`ScriptValue::Nil`] on timeout.
#[derive(Debug, Clone, PartialEq)]
pub enum WaitRequest {
    /// A child with this name, as in `WaitForChild`.
    Child {
        /// Whose child.
        parent: InstanceId,
        /// The name it must have.
        name: Box<str>,
        /// Give up after this many seconds.
        timeout: Option<f64>,
    },
    /// The next firing of a signal.
    Signal {
        /// Which signal.
        key: SignalKey,
    },
    /// The reply to a request, as in `RemoteFunction:InvokeServer`.
    Reply {
        /// The request.
        request: u64,
    },
    /// This many seconds.
    Seconds(f64),
}

/// How a method finished. Yielding is a return value here, so every language can yield.
#[derive(Debug, Clone, PartialEq)]
pub enum CallOutcome {
    /// Done; these are the results.
    Return(ScriptValue),
    /// Park the calling thread. Only a method declared with [`MethodDesc::yields`] may return this.
    Wait(WaitRequest),
}

impl<T: Into<ScriptValue>> From<T> for CallOutcome {
    fn from(value: T) -> CallOutcome {
        CallOutcome::Return(value.into())
    }
}

/// A method body in `metallic_core`.
pub type MethodFn = fn(&mut MethodCall<'_>) -> Result<CallOutcome, ApiError>;

/// One call of a method.
///
/// The typed accessors take a zero-based argument index. They cannot fail on an
/// [`ArgCheck::Auto`] parameter, because the host already checked it; they still return `Result`
/// so a method never panics. A [`ArgCheck::Manual`] parameter is read with [`MethodCall::arg`].
/// Strings and lists borrow the argument buffer, not the call, so `call.world` stays usable.
pub struct MethodCall<'a> {
    /// The world the call runs in.
    pub world: &'a mut World,
    /// The instance the method is called on.
    pub this: InstanceId,
    /// The arguments, without `this`. Borrowed so a call allocates no list.
    pub args: &'a [ScriptValue],
    /// Who is calling.
    pub context: &'a CallContext,
    class: &'static str,
    desc: &'static MethodDesc,
}

/// [`MethodCall::arg`] of an argument that was left out.
static NIL: ScriptValue = ScriptValue::Nil;

macro_rules! accessors {
    ($($name:ident, $opt:ident, $as:path, $ty:expr, $out:ty, $what:literal;)*) => {
        $(
            #[doc = concat!("Argument `i` as ", $what, ".")]
            pub fn $name(&self, i: usize) -> Result<$out, ApiError> {
                self.required(i, $ty, $as)
            }

            #[doc = concat!("Argument `i` as ", $what, ", or `None` when left out or nil.")]
            pub fn $opt(&self, i: usize) -> Result<Option<$out>, ApiError> {
                self.optional(i, $ty, $as)
            }
        )*
    };
}

impl<'a> MethodCall<'a> {
    /// A call of `desc` on `this`, an instance of class `class`. The host builds this after
    /// [`check_arguments`].
    pub fn new(
        world: &'a mut World,
        this: InstanceId,
        class: &'static str,
        desc: &'static MethodDesc,
        args: &'a [ScriptValue],
        context: &'a CallContext,
    ) -> MethodCall<'a> {
        MethodCall {
            world,
            this,
            args,
            context,
            class,
            desc,
        }
    }

    /// The class of the instance the method is called on.
    pub fn class_name(&self) -> &'static str {
        self.class
    }

    /// The declaration being called.
    pub fn method(&self) -> &'static MethodDesc {
        self.desc
    }

    /// Argument `i` as it came, nil when left out.
    pub fn arg(&self, i: usize) -> &'a ScriptValue {
        self.args.get(i).unwrap_or(&NIL)
    }

    fn required<T>(
        &self,
        i: usize,
        ty: Ty,
        decode: impl FnOnce(&'a ScriptValue) -> Option<T>,
    ) -> Result<T, ApiError> {
        // The declared type, so an error names `Enum.Material` and not the accessor's guess.
        let expected = self.desc.params.get(i).map_or(ty, |param| param.ty);
        let index = position(i);
        let failed = |kind| ApiError::new(self.class, self.desc.name, kind);
        let Some(value) = self.args.get(i) else {
            return Err(failed(ApiErrorKind::MissingArgument { index, expected }));
        };
        decode(value).ok_or_else(|| {
            failed(ApiErrorKind::BadArgument {
                index,
                expected,
                got: value.type_name(self.world),
            })
        })
    }

    fn optional<T>(
        &self,
        i: usize,
        ty: Ty,
        decode: impl FnOnce(&'a ScriptValue) -> Option<T>,
    ) -> Result<Option<T>, ApiError> {
        match self.arg(i) {
            ScriptValue::Nil => Ok(None),
            _ => self.required(i, ty, decode).map(Some),
        }
    }

    accessors! {
        bool, opt_bool, ScriptValue::as_bool, Ty::Bool, bool, "a boolean";
        int, opt_int, ScriptValue::as_int, Ty::Int, i64, "a whole number";
        number, opt_number, ScriptValue::as_number, Ty::Number, f64, "a number";
        string, opt_string, ScriptValue::as_str, Ty::String, &'a str, "a string";
        instance, opt_instance, ScriptValue::as_instance, Ty::Instance(None), InstanceId,
            "an instance handle";
        vector3, opt_vector3, ScriptValue::as_vector3, Ty::Vector3, Vec3, "a `Vector3`";
        cframe, opt_cframe, ScriptValue::as_cframe, Ty::CFrame, CFrame, "a `CFrame`";
        color3, opt_color3, ScriptValue::as_color3, Ty::Color3, Color3, "a `Color3`";
        udim2, opt_udim2, ScriptValue::as_udim2, Ty::UDim2, UDim2, "a `UDim2`";
        enum_item, opt_enum_item, ScriptValue::as_enum_item, Ty::Enum("?"), EnumItemRef,
            "an enum item";
        array, opt_array, ScriptValue::as_array, Ty::Array(&Ty::Any), &'a [ScriptValue], "a list";
        map, opt_map, ScriptValue::as_map, Ty::Map(&Ty::Any), &'a [(ScriptValue, ScriptValue)],
            "key and value pairs";
        function, opt_function, ScriptValue::as_function, Ty::Function(&ANY_FUNCTION), FunctionRef,
            "a function reference";
    }
}

/// What a value is, in an error message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypeName {
    /// `nil`
    Nil,
    /// `boolean`
    Bool,
    /// `number`, whole or not.
    Number,
    /// `string`
    String,
    /// The instance's class; `None` for a destroyed one.
    Instance(Option<&'static str>),
    /// `Vector3`
    Vector3,
    /// `CFrame`
    CFrame,
    /// `Color3`
    Color3,
    /// `UDim2`
    UDim2,
    /// `Enum.<name>`
    Enum(&'static str),
    /// `array`
    Array,
    /// `map`
    Map,
    /// `function`
    Function,
    /// `tuple`
    Tuple,
}

impl fmt::Display for TypeName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            TypeName::Nil => "nil",
            TypeName::Bool => "boolean",
            TypeName::Number => "number",
            TypeName::String => "string",
            TypeName::Instance(class) => class.unwrap_or(UNNAMED_INSTANCE),
            TypeName::Vector3 => "Vector3",
            TypeName::CFrame => "CFrame",
            TypeName::Color3 => "Color3",
            TypeName::UDim2 => "UDim2",
            TypeName::Enum(name) => return write!(f, "Enum.{name}"),
            TypeName::Array => "array",
            TypeName::Map => "map",
            TypeName::Function => "function",
            TypeName::Tuple => "tuple",
        };
        f.write_str(name)
    }
}

impl fmt::Display for Ty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ty::Nil => f.write_str("nil"),
            Ty::Bool => f.write_str("boolean"),
            Ty::Int => f.write_str("integer"),
            Ty::Number => f.write_str("number"),
            Ty::String => f.write_str("string"),
            Ty::Instance(class) => f.write_str(class.unwrap_or(UNNAMED_INSTANCE)),
            Ty::Vector3 => f.write_str("Vector3"),
            Ty::CFrame => f.write_str("CFrame"),
            Ty::Color3 => f.write_str("Color3"),
            Ty::UDim2 => f.write_str("UDim2"),
            Ty::Enum(name) => write!(f, "Enum.{name}"),
            Ty::Array(inner) => write!(f, "array of {inner}"),
            Ty::Map(inner) => write!(f, "map of {inner}"),
            Ty::Function(_) => f.write_str("function"),
            Ty::Optional(inner) => write!(f, "{inner} or nil"),
            Ty::Tuple(_) => f.write_str("tuple"),
            Ty::Any => f.write_str("any"),
        }
    }
}

/// What core calls an instance whose name it no longer knows. A host that kept a name replaces it.
pub const UNNAMED_INSTANCE: &str = "Instance";

/// A kind of member, where a message depends on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberCategory {
    /// A property.
    Property,
    /// A method.
    Method,
    /// A signal.
    Signal,
    /// A callback.
    Callback,
}

/// What went wrong in an [`ApiError`].
#[derive(Debug, Clone, PartialEq)]
pub enum ApiErrorKind {
    /// An argument has the wrong type.
    BadArgument {
        /// One-based position.
        index: u32,
        /// The declared type.
        expected: Ty,
        /// What came.
        got: TypeName,
    },
    /// A required argument is missing.
    MissingArgument {
        /// One-based position.
        index: u32,
        /// The declared type.
        expected: Ty,
    },
    /// More arguments than a method without trailing `...` takes.
    TooManyArguments {
        /// How many it takes.
        max: u32,
        /// How many came.
        got: u32,
    },
    /// The class has no such member, or hides it.
    NotAMember,
    /// Scripts cannot write this member.
    ReadOnly,
    /// A callback member was read.
    CallbackRead,
    /// The member belongs to the other side.
    WrongContext {
        /// The side that may use it.
        required: Side,
        /// What the member is.
        member_kind: MemberCategory,
    },
    /// `Instance.new` may not create this class.
    NotCreatable {
        /// The name that was asked for, which need not be a class.
        class: Cow<'static, str>,
    },
    /// The instance is destroyed or the handle is stale.
    Destroyed {
        /// The best name known; see [`UNNAMED_INSTANCE`].
        name: String,
    },
    /// The world refused.
    Core(CoreError),
    /// Text a method chose.
    Message(Cow<'static, str>),
}

/// A failed member access, in every language. Each language wraps the [`Display`](fmt::Display)
/// text in its own error and keeps these fields; Luau keeps wrapping it in
/// `<script name>:<line>: <message>`.
///
/// The texts existing tests assert are kept as they are during the migration (design section 5.5).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{}", self.text())]
pub struct ApiError {
    /// The class of the instance, not the class that declares the member (`Part:Resize`). Empty
    /// until the host fills it, for an error a method body raised with `?`.
    pub class: &'static str,
    /// The member. Not always static: a script can ask for any name.
    pub member: Cow<'static, str>,
    /// What went wrong.
    pub kind: ApiErrorKind,
}

impl ApiError {
    /// An error about a member of a class.
    pub fn new(
        class: &'static str,
        member: impl Into<Cow<'static, str>>,
        kind: ApiErrorKind,
    ) -> ApiError {
        ApiError {
            class,
            member: member.into(),
            kind,
        }
    }

    /// An error with text a method chose, printed exactly as given.
    pub fn message(text: impl Into<Cow<'static, str>>) -> ApiError {
        ApiError::new("", "", ApiErrorKind::Message(text.into()))
    }

    /// Fills in the class and member where a method body left them empty.
    pub fn within(mut self, class: &'static str, member: &'static str) -> ApiError {
        if self.class.is_empty() {
            self.class = class;
        }
        if self.member.is_empty() {
            self.member = Cow::Borrowed(member);
        }
        self
    }

    /// The one formatter.
    fn text(&self) -> String {
        let (class, member) = (self.class, &self.member);
        match &self.kind {
            ApiErrorKind::BadArgument {
                index,
                expected,
                got,
            } => format!("{class}:{member}: argument #{index} expected {expected}, got {got}"),
            ApiErrorKind::MissingArgument { index, expected } => {
                format!("{class}:{member}: argument #{index} expected {expected}, got nothing")
            }
            ApiErrorKind::TooManyArguments { max, got } => {
                let plural = if *max == 1 { "" } else { "s" };
                format!("{class}:{member}: expected at most {max} argument{plural}, got {got}")
            }
            ApiErrorKind::NotAMember => format!("{member} is not a valid member of {class}"),
            ApiErrorKind::ReadOnly => format!("{member} is read-only"),
            ApiErrorKind::CallbackRead => format!("{member} is a callback; it can only be set"),
            ApiErrorKind::WrongContext {
                required,
                member_kind,
            } => {
                let verb = match member_kind {
                    MemberCategory::Method => "called",
                    _ => "used",
                };
                let side = match required {
                    Side::Server => "server",
                    Side::Client => "client",
                };
                format!("{member} can only be {verb} from the {side}")
            }
            ApiErrorKind::NotCreatable { class } => {
                format!("Unable to create an Instance of type \"{class}\"")
            }
            ApiErrorKind::Destroyed { name } => format!("{name} has been destroyed"),
            ApiErrorKind::Core(error) => error.to_string(),
            ApiErrorKind::Message(text) => text.to_string(),
        }
    }
}

impl From<CoreError> for ApiError {
    /// So `call.world.set(..)?` works in a method; the host then fills in the class and member.
    fn from(error: CoreError) -> ApiError {
        ApiError::new("", "", ApiErrorKind::Core(error))
    }
}

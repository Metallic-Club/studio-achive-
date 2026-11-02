//! The host interface: one trait addressed by ids, used by every language binding, and the table
//! of methods the script host implements. Design: `docs/design/class-system.md` sections 5.6, 13.2.
//!
//! A binding calls [`HostApi`]. Its `call_method` goes through [`MethodDispatch`], which holds the
//! order of the rules once, so no binding can skip one.
use super::call::{ApiError, ApiErrorKind, CallContext, CallOutcome, MethodCall, ScriptValue};
use super::call::{FunctionRef, UNNAMED_INSTANCE, check_arguments};
use super::desc::MethodImpl;
use super::resolve::{ClassId, DeclId, Member, MemberId, MemberKind, Registry, ResolvedClass};
use crate::{ConnectionId, InstanceId, World};

/// An instance as a binding holds it. Across a language boundary it is the `u64` of
/// [`InstanceId::to_bits`]; the binding converts at that edge. It carries no class and no pointer.
pub type Handle = InstanceId;

/// What every language binding calls. The first implementation is the Luau binding (#462), the
/// next the WASM one (#474).
///
/// `class` is the class that declares the member, which is what a generated wrapper knows. By the
/// prefix rule the member has the same id in `class` and in the instance's own class. An
/// implementation checks, in this order: the handle is live and `is_a(h, class)`, the member in
/// the instance's own table ([`member_of`]), the member's context (#468), the arguments, then runs
/// it.
pub trait HostApi {
    /// The class with this name.
    fn resolve_class(&self, name: &str) -> Option<ClassId>;
    /// The member of `class` with this name or alias.
    fn resolve_member(&self, class: ClassId, name: &str) -> Option<MemberId>;
    /// The class of a live instance.
    fn class_of(&self, h: Handle) -> Result<ClassId, ApiError>;
    /// True when the instance is `class` or inherits from it.
    fn is_a(&self, h: Handle, class: ClassId) -> Result<bool, ApiError>;
    /// Reads a property.
    fn get_property(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
    ) -> Result<ScriptValue, ApiError>;
    /// Writes a property.
    fn set_property(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        v: ScriptValue,
    ) -> Result<(), ApiError>;
    /// Calls a method. [`CallOutcome::Wait`] asks the host to park the calling thread.
    fn call_method(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        args: &[ScriptValue],
    ) -> Result<CallOutcome, ApiError>;
    /// Connects a handler to a signal; `arg` selects a parameterized signal's key, otherwise 0.
    fn connect_signal(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        arg: u32,
        f: FunctionRef,
    ) -> Result<ConnectionId, ApiError>;
    /// Removes a connection. A connection already gone is fine.
    fn disconnect(&mut self, c: ConnectionId);
    /// Sets or clears a callback.
    fn set_callback(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        f: Option<FunctionRef>,
    ) -> Result<(), ApiError>;
    /// Creates an instance of a `CREATABLE` class.
    fn new_instance(&mut self, class: ClassId, parent: Option<Handle>) -> Result<Handle, ApiError>;
}

/// The class of a live instance in `registry`. World nodes hold ids of the built-in registry, so
/// that one needs no lookup; any other (a test's fixture) is matched by class name.
#[inline]
pub fn instance_class<'r>(
    registry: &'r Registry,
    world: &World,
    h: Handle,
) -> Option<&'r ResolvedClass> {
    let id = world.class_id(h)?;
    if std::ptr::eq(registry, super::registry()) {
        registry.class(id)
    } else {
        registry.class_by_name(super::registry().class(id)?.name())
    }
}

/// The member a binding addressed, looked up in the instance's own class: this is where `hide`
/// and overrides apply. Gives the instance's class with it.
///
/// A destroyed or stale handle is [`ApiErrorKind::Destroyed`]; an instance that is not a `class`,
/// an unknown id or a hidden member is [`ApiErrorKind::NotAMember`], naming the instance's class.
pub fn member_of<'r>(
    registry: &'r Registry,
    world: &World,
    h: Handle,
    class: ClassId,
    m: MemberId,
) -> Result<(&'r ResolvedClass, &'r Member), ApiError> {
    let Some(own) = instance_class(registry, world, h) else {
        let kind = ApiErrorKind::Destroyed {
            name: UNNAMED_INSTANCE.to_owned(),
        };
        return Err(ApiError::new("", "", kind));
    };
    let index = usize::from(m.0);
    let member = own.members.get(index).filter(|_| own.is_a(class));
    match member {
        Some(member) if !member.hidden => Ok((own, member)),
        _ => {
            let name = registry
                .class(class)
                .and_then(|declaring| declaring.members.get(index))
                .map_or("member", Member::name);
            Err(ApiError::new(own.name(), name, ApiErrorKind::NotAMember))
        }
    }
}

/// A host-implemented method's call: a [`MethodCall`] plus the host's own state `S`.
pub struct HostCall<'a, S> {
    /// The call.
    pub call: MethodCall<'a>,
    /// What only the host has: threads, remotes, the joined players.
    pub state: &'a mut S,
}

/// A method body the script host provides. `metallic_script` names its `S`; a WASM binding calls
/// the same functions.
pub type HostMethodFn<S> = fn(&mut HostCall<'_, S>) -> Result<CallOutcome, ApiError>;

/// Why the table of host methods does not match the declarations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum HostMethodError {
    /// The named class does not itself declare a `Host` method with this name.
    #[error("{class}.{method} is not a Host method declared by that class")]
    NotDeclared {
        /// The class named.
        class: &'static str,
        /// The method named.
        method: &'static str,
    },
    /// Two implementations for one declaration.
    #[error("{class}.{method} is implemented twice")]
    ImplementedTwice {
        /// The declaring class.
        class: &'static str,
        /// The method.
        method: &'static str,
    },
    /// A declared `Host` method has no implementation.
    #[error("{class}.{method} is declared Host but has no implementation")]
    Missing {
        /// The declaring class.
        class: &'static str,
        /// The method.
        method: &'static str,
    },
    /// An entry of the not-yet-migrated list is implemented or no longer declared: remove it.
    #[error("{class}.{method} is on the not-yet-migrated list but needs no entry")]
    NeedlessEntry {
        /// The class named.
        class: &'static str,
        /// The method named.
        method: &'static str,
    },
}

struct HostEntry<S> {
    decl: DeclId,
    class: &'static str,
    method: &'static str,
    implementation: Option<HostMethodFn<S>>,
}

/// One slot per declared `Host` method of a registry, filled by [`HostMethods::implement`].
///
/// Build it once: it walks every member of every class.
pub struct HostMethods<S> {
    /// Sorted by `decl`.
    entries: Vec<HostEntry<S>>,
}

impl<S> HostMethods<S> {
    /// An empty slot for each `Host` method `registry` declares.
    pub fn new(registry: &Registry) -> HostMethods<S> {
        let mut entries = Vec::new();
        for class in registry.classes() {
            for member in &class.members {
                if member.owner != class.id {
                    continue;
                }
                if let MemberKind::Method { desc, decl } = member.kind
                    && desc.implementation == MethodImpl::Host
                {
                    entries.push(HostEntry {
                        decl,
                        class: class.name(),
                        method: desc.name,
                        implementation: None,
                    });
                }
            }
        }
        entries.sort_by_key(|entry| entry.decl);
        HostMethods { entries }
    }

    /// Registers the body of `class.method`, resolved to its declaration once. `class` is the
    /// class that declares it, not a subclass. An override has its own declaration and its own
    /// body.
    pub fn implement(
        &mut self,
        class: &'static str,
        method: &'static str,
        implementation: HostMethodFn<S>,
    ) -> Result<(), HostMethodError> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.class == class && entry.method == method)
            .ok_or(HostMethodError::NotDeclared { class, method })?;
        if entry.implementation.is_some() {
            return Err(HostMethodError::ImplementedTwice { class, method });
        }
        entry.implementation = Some(implementation);
        Ok(())
    }

    /// The body of a declaration.
    pub fn get(&self, decl: DeclId) -> Option<HostMethodFn<S>> {
        let index = self
            .entries
            .binary_search_by_key(&decl, |entry| entry.decl)
            .ok()?;
        self.entries[index].implementation
    }

    /// The completeness rule: every `Host` method has a body, apart from those in `not_migrated`
    /// (`(class, method)` still dispatched by the old tables), and every entry there is still
    /// needed, so the list can only shrink. [`HostMethods::implement`] already refused a body that
    /// names no declared `Host` method.
    pub fn check_complete(
        &self,
        not_migrated: &[(&'static str, &'static str)],
    ) -> Result<(), HostMethodError> {
        for entry in &self.entries {
            if entry.implementation.is_none()
                && !not_migrated.contains(&(entry.class, entry.method))
            {
                return Err(HostMethodError::Missing {
                    class: entry.class,
                    method: entry.method,
                });
            }
        }
        for &(class, method) in not_migrated {
            let missing = self.entries.iter().any(|entry| {
                entry.class == class && entry.method == method && entry.implementation.is_none()
            });
            if !missing {
                return Err(HostMethodError::NeedlessEntry { class, method });
            }
        }
        Ok(())
    }
}

/// Everything one method call needs, and the one order of its rules.
///
/// In a binding, `registry` is [`classes::registry`](super::registry); a test passes a resolved
/// fixture set whose class names match real classes, so fixture methods run on real instances.
pub struct MethodDispatch<'a, S> {
    /// Where the instance's class is looked up, by name.
    pub registry: &'a Registry,
    /// The bodies of `Host` methods.
    pub methods: &'a HostMethods<S>,
    /// The world the call runs in.
    pub world: &'a mut World,
    /// The host's own state, for `Host` methods.
    pub state: &'a mut S,
    /// Who is calling.
    pub context: &'a CallContext,
}

impl<S> MethodDispatch<'_, S> {
    /// [`HostApi::call_method`]: finds the member in the instance's own class, checks the
    /// arguments and runs the nearest declaration.
    pub fn call(
        &mut self,
        h: Handle,
        class: ClassId,
        m: MemberId,
        args: &[ScriptValue],
    ) -> Result<CallOutcome, ApiError> {
        let (own, member) = member_of(self.registry, self.world, h, class, m)?;
        let (class_name, name) = (own.name(), member.name());
        let MemberKind::Method { desc, decl } = member.kind else {
            let text = format!("{name} is not a method");
            return Err(ApiError::message(text).within(class_name, name));
        };
        // #468 checks `member.context` against `self.context.side` here, before the arguments.
        check_arguments(self.world, class_name, desc, args)?;
        let mut call = MethodCall::new(&mut *self.world, h, class_name, desc, args, self.context);
        let result = match desc.implementation {
            MethodImpl::Native(body) => body(&mut call),
            MethodImpl::Host => match self.methods.get(decl) {
                Some(body) => body(&mut HostCall {
                    call,
                    state: &mut *self.state,
                }),
                None => Err(ApiError::message(format!(
                    "{name} has no host implementation"
                ))),
            },
            MethodImpl::Luau => Err(ApiError::message(format!(
                "{name} is implemented in Luau; the binding calls it"
            ))),
        };
        let outcome = result.map_err(|error| error.within(class_name, name))?;
        if matches!(outcome, CallOutcome::Wait(_)) && !desc.yields {
            let text = format!("{name} waited but is not declared as yielding");
            return Err(ApiError::message(text).within(class_name, name));
        }
        Ok(outcome)
    }
}

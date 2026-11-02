//! The methods core declares `Host` and this crate implements: they fire remotes, park threads,
//! call back into script code or read host state. Design: `docs/design/class-system.md` section 5.6.
//!
//! The Luau binding dispatches through this table (`api/host_api.rs`). Methods move here from the
//! old tables with #465-#467; until then `api/bridge.rs` runs them. `tests/host_methods.rs` holds
//! the completeness rule.
use std::rc::Rc;
use std::sync::LazyLock;

use metallic_core::classes::{self, HostMethodError, HostMethods};

use super::ApiState;

/// What only the script host has: the state of the VM the call runs in.
pub struct HostState {
    // Read by the first method body that moves here (#465-#467).
    #[allow(dead_code)]
    pub(crate) api: Rc<ApiState>,
}

/// One call of a method this crate implements.
///
/// `call.world` is the world, borrowed for the call: going through the state's `world()` instead
/// answers "the world is busy".
pub type HostCall<'a> = classes::HostCall<'a, HostState>;

/// A method body this crate implements. A WASM binding will call the same functions.
pub type HostMethodFn = classes::HostMethodFn<HostState>;

/// `Host` methods still dispatched by the old tables, as `(class, method)`. An entry leaves when
/// its method gets a body here or becomes `Native`; the completeness test fails on a stale one.
pub const NOT_MIGRATED_HOST_METHODS: &[(&str, &str)] = &[
    // `api/pivot.rs` until #465.
    ("PVInstance", "GetPivot"),
    ("PVInstance", "PivotTo"),
];

/// Every `Host` method body of this crate, registered by the name of its declaration.
pub fn host_methods() -> Result<HostMethods<HostState>, HostMethodError> {
    // #465-#467 add `methods.implement("RemoteEvent", "FireServer", fire_server)?;` here.
    Ok(HostMethods::new(classes::registry()))
}

/// [`host_methods`], built once per process: it walks every member of every class.
pub(crate) static SHARED: LazyLock<Result<HostMethods<HostState>, HostMethodError>> =
    LazyLock::new(host_methods);

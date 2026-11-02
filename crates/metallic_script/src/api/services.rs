//! `game`, `game:GetService`, and the members of the services and of `Humanoid`:
//! `Players`, `RunService`, `Humanoid:TakeDamage`.

use std::rc::Rc;

use metallic_core::Value as CoreValue;
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, UserData, UserDataFields, UserDataMethods, UserDataRef,
    Value,
};

use super::errors;
use super::instance::LuaInstance;
use super::{ApiState, lua_error};
use crate::vm::Context;

/// The services `game:GetService` knows (the builtin roots of `World::new_with_services`).
const SERVICES: &[&str] = &[
    "Workspace",
    "Players",
    "ReplicatedStorage",
    "ServerScriptService",
    "StarterPlayer",
    "StarterGui",
    "Lighting",
    "RunService",
    "UserInputService",
    "ContextActionService",
    "CollectionService",
];

/// True when `class` is a name `GetService` knows (#466 reads this from the registry).
fn is_service(class: &str) -> bool {
    SERVICES.contains(&class)
}

/// True for a service that is a host object rather than a registry class, which `Instance.new`
/// cannot find in the registry. Goes away when #466 and #467 make them classes.
pub(crate) fn is_host_object(state: &ApiState, name: &str) -> bool {
    super::input::service(state, name).is_some() || super::tags::service(state, name).is_some()
}

/// The Luau `DataModel` (`game`).
struct LuaGame {
    state: Rc<ApiState>,
}

impl LuaGame {
    fn service(&self, lua: &Lua, name: &str) -> mlua::Result<Value> {
        if !is_service(name) {
            return Err(lua_error(format!("'{name}' is not a valid Service name")));
        }
        if let Some(service) = super::input::service(&self.state, name) {
            return Ok(service);
        }
        if let Some(service) = super::tags::service(&self.state, name) {
            return Ok(service);
        }
        let id = self.state.world()?.root(name);
        self.state.wrap_opt(lua, id)
    }
}

impl UserData for LuaGame {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Instance");
        // `game.Workspace` and friends, plus `game.GetService` for `game:GetService(...)`.
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(lua, |lua, (this, name): (UserDataRef<LuaGame>, String)| {
                if name == "GetService" {
                    return Ok(this
                        .state
                        .bridge
                        .function("GetService")
                        .map_or(Value::Nil, |f| Value::Function(f.clone())));
                }
                this.service(lua, &name)
                    .map_err(|_| lua_error(format!("{name} is not a valid member of DataModel")))
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| Ok("Game".to_string()));
    }
}

/// The global `game`.
pub(crate) fn game(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<AnyUserData> {
    lua.create_userdata(LuaGame {
        state: state.clone(),
    })
}

/// The service and `Humanoid` methods, by name (see `api/bridge.rs`).
pub(crate) fn methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    let get_players = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        this.expect_class("Players", "GetPlayers")?;
        let state = &this.state;
        let ids: Vec<_> = {
            let world = state.world()?;
            let joined = state.shared.joined.borrow();
            world
                .children(this.id)
                .iter()
                .copied()
                .filter(|&c| world.is_a(c, "Player") && joined.contains(&c))
                .collect()
        };
        let list = lua.create_table()?;
        for (i, id) in ids.into_iter().enumerate() {
            list.set(i + 1, state.wrap(lua, id)?)?;
        }
        Ok(list)
    })?;

    let player_from_character =
        lua.create_function(|lua, (this, model): (UserDataRef<LuaInstance>, Value)| {
            this.expect_class("Players", "GetPlayerFromCharacter")?;
            let model = match &model {
                Value::UserData(ud) => ud.borrow::<LuaInstance>().ok().map(|m| m.id),
                _ => None,
            };
            let Some(model) = model else {
                return Ok(Value::Nil);
            };
            let found = {
                let world = this.state.world()?;
                world.children(this.id).iter().copied().find(|&p| {
                    world.is_a(p, "Player")
                        && matches!(world.get(p, "Character"), Ok(CoreValue::Ref(Some(c))) if c == model)
                })
            };
            this.state.wrap_opt(lua, found)
        })?;

    let is_server = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("RunService", "IsServer")?;
        Ok(this.state.context == Context::Server)
    })?;
    let is_client = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("RunService", "IsClient")?;
        Ok(this.state.context == Context::Client)
    })?;

    // Only lowers Health (never below 0); death itself is detected by the character controller.
    let take_damage =
        lua.create_function(|_, (this, amount): (UserDataRef<LuaInstance>, f64)| {
            this.expect_class("Humanoid", "TakeDamage")?;
            if !amount.is_finite() {
                return Err(lua_error("TakeDamage expects a finite number"));
            }
            let mut world = this.state.world_mut()?;
            let Ok(CoreValue::Number(health)) = world.get(this.id, "Health") else {
                return Ok(());
            };
            if health > 0.0 && amount > 0.0 {
                world
                    .set(
                        this.id,
                        "Health",
                        CoreValue::Number((health - amount).max(0.0)),
                    )
                    .map_err(lua_error)?;
            }
            Ok(())
        })?;

    let get_service =
        lua.create_function(|lua, (this, name): (UserDataRef<LuaGame>, String)| {
            this.service(lua, &name)
        })?;

    Ok(vec![
        ("GetService", get_service),
        ("GetPlayers", get_players),
        ("GetPlayerFromCharacter", player_from_character),
        ("IsServer", is_server),
        ("IsClient", is_client),
        ("TakeDamage", take_damage),
    ])
}

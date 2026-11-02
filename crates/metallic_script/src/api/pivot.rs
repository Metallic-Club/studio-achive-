//! Pivot, size and scale methods of BasePart and Model (Roblox's `PVInstance` API).
//!
//! The rules live in [`metallic_core::transform`]; these functions check arguments and convert.

use metallic_core::transform::{self, NormalId};
use mlua::{Function, Lua, UserDataRef, Value};

use super::cframe::{LuaCFrame, cframe};
use super::datatypes::LuaVector3;
use super::enums::enum_item;
use super::instance::LuaInstance;
use super::lua_error;

fn core_error(e: metallic_core::CoreError) -> mlua::Error {
    lua_error(e)
}

/// Errors unless `this` is a BasePart or a Model.
fn expect_pv(this: &LuaInstance, method: &str) -> mlua::Result<()> {
    this.expect_class("BasePart", method)
        .or_else(|_| this.expect_class("Model", method))
}

/// A face given as `Enum.NormalId.X`, its number (0 to 5) or its name.
fn normal_id(v: &Value) -> mlua::Result<NormalId> {
    let by_name = |name: &str| NormalId::ALL.into_iter().find(|n| n.name() == name);
    let found = match v {
        Value::Integer(i) => usize::try_from(*i)
            .ok()
            .and_then(|i| NormalId::ALL.get(i).copied()),
        Value::Number(n) if n.fract() == 0.0 && *n >= 0.0 => {
            NormalId::ALL.get(*n as usize).copied()
        }
        Value::String(s) => by_name(&s.to_string_lossy()),
        other => enum_item(other)
            .filter(|(e, _, _)| *e == "NormalId")
            .and_then(|(_, name, _)| by_name(name)),
    };
    found.ok_or_else(|| lua_error("Resize: expected an Enum.NormalId"))
}

/// The methods, by name. Which classes have them is in `api/bridge.rs`.
pub(crate) fn methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    let get_pivot = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        expect_pv(&this, "GetPivot")?;
        let cf = transform::get_pivot(&*this.state.world()?, this.id).map_err(core_error)?;
        Ok(LuaCFrame(cf))
    })?;
    let pivot_to =
        lua.create_function(|_, (this, target): (UserDataRef<LuaInstance>, Value)| {
            expect_pv(&this, "PivotTo")?;
            let target = cframe(&target).ok_or_else(|| lua_error("PivotTo: expected a CFrame"))?;
            transform::pivot_to(&mut *this.state.world_mut()?, this.id, target).map_err(core_error)
        })?;
    let resize = lua.create_function(
        |_, (this, face, delta): (UserDataRef<LuaInstance>, Value, f64)| {
            this.expect_class("BasePart", "Resize")?;
            let face = normal_id(&face)?;
            transform::resize(&mut *this.state.world_mut()?, this.id, face, delta)
                .map_err(core_error)
        },
    )?;
    let scale_to = lua.create_function(|_, (this, scale): (UserDataRef<LuaInstance>, f64)| {
        this.expect_class("Model", "ScaleTo")?;
        transform::scale_to(&mut *this.state.world_mut()?, this.id, scale).map_err(core_error)
    })?;
    let get_scale = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("Model", "GetScale")?;
        Ok(transform::model_scale(&*this.state.world()?, this.id))
    })?;
    let bounding_box = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("Model", "GetBoundingBox")?;
        let (cf, size) =
            transform::bounding_box(&*this.state.world()?, this.id).map_err(core_error)?;
        Ok((LuaCFrame(cf), LuaVector3(size)))
    })?;
    let extents = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("Model", "GetExtentsSize")?;
        let (_, size) =
            transform::bounding_box(&*this.state.world()?, this.id).map_err(core_error)?;
        Ok(LuaVector3(size))
    })?;
    Ok(vec![
        ("GetPivot", get_pivot),
        ("PivotTo", pivot_to),
        ("Resize", resize),
        ("ScaleTo", scale_to),
        ("GetScale", get_scale),
        ("GetBoundingBox", bounding_box),
        ("GetExtentsSize", extents),
    ])
}

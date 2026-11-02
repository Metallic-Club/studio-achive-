//! `Vector3`, `Vector2`, `Color3` and `UDim2` as Luau userdata, and the conversion between Luau
//! values and the world's property [`Value`](metallic_core::Value)s.

use std::rc::Rc;

use metallic_core::{Color3, UDim2, Value as CoreValue, ValueType, Vec3};
use mlua::{Lua, MetaMethod, Table, UserData, UserDataFields, UserDataMethods, UserDataRef, Value};

use super::cframe::{LuaCFrame, cframe};
use super::errors;
use super::instance::LuaInstance;
use super::{ApiState, lua_error};

/// Luau `Vector3` (f64 components).
#[derive(Clone, Copy)]
pub(crate) struct LuaVector3(pub(crate) Vec3);

/// Luau `Vector2` (f64 components). Not a property type; scripts, remotes and input use it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) struct LuaVector2(pub(crate) [f64; 2]);

impl LuaVector2 {
    fn magnitude(self) -> f64 {
        self.0[0].hypot(self.0[1])
    }
}

/// Luau `Color3`.
#[derive(Clone, Copy)]
pub(crate) struct LuaColor3(pub(crate) Color3);

/// Luau `UDim2`.
#[derive(Clone, Copy)]
pub(crate) struct LuaUDim2(pub(crate) UDim2);

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => Some(*n),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

pub(crate) fn vector(v: &Value) -> Option<Vec3> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaVector3>().ok().map(|v| v.0),
        _ => None,
    }
}

pub(crate) fn vector2(v: &Value) -> Option<[f64; 2]> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaVector2>().ok().map(|v| v.0),
        _ => None,
    }
}

pub(crate) fn color(v: &Value) -> Option<Color3> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaColor3>().ok().map(|v| v.0),
        _ => None,
    }
}

pub(crate) fn udim2(v: &Value) -> Option<UDim2> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaUDim2>().ok().map(|v| v.0),
        _ => None,
    }
}

impl UserData for LuaVector3 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Vector3");
        fields.add_meta_field_with(MetaMethod::Add, |lua| {
            errors::guarded_fn(
                lua,
                |_, (a, b): (UserDataRef<LuaVector3>, UserDataRef<LuaVector3>)| {
                    Ok(LuaVector3(a.0 + b.0))
                },
            )
        });
        fields.add_meta_field_with(MetaMethod::Sub, |lua| {
            errors::guarded_fn(
                lua,
                |_, (a, b): (UserDataRef<LuaVector3>, UserDataRef<LuaVector3>)| {
                    Ok(LuaVector3(a.0 - b.0))
                },
            )
        });
        fields.add_meta_field_with(MetaMethod::Mul, |lua| {
            errors::guarded_fn(lua, |_, (a, b): (Value, Value)| {
                match (vector(&a), vector(&b), number(&a), number(&b)) {
                    (Some(v), _, _, Some(k)) | (_, Some(v), Some(k), _) => Ok(LuaVector3(v * k)),
                    (Some(a), Some(b), _, _) => {
                        Ok(LuaVector3(Vec3::new(a.x * b.x, a.y * b.y, a.z * b.z)))
                    }
                    _ => Err(lua_error(
                        "Vector3 can only be multiplied by a number or a Vector3",
                    )),
                }
            })
        });
        fields.add_meta_field_with(MetaMethod::Div, |lua| {
            errors::guarded_fn(lua, |_, (a, k): (UserDataRef<LuaVector3>, f64)| {
                Ok(LuaVector3(a.0 * (1.0 / k)))
            })
        });
        fields.add_field_method_get("X", |_, this| Ok(this.0.x));
        fields.add_field_method_get("Y", |_, this| Ok(this.0.y));
        fields.add_field_method_get("Z", |_, this| Ok(this.0.z));
        fields.add_field_method_get("Magnitude", |_, this| Ok(this.0.magnitude()));
        fields.add_field_method_get("Unit", |_, this| Ok(LuaVector3(this.0.unit())));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Unm, |_, this, ()| Ok(LuaVector3(this.0 * -1.0)));
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!((vector(&a), vector(&b)), (Some(a), Some(b)) if a == b))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("{}, {}, {}", this.0.x, this.0.y, this.0.z))
        });
    }
}

impl UserData for LuaVector2 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Vector2");
        fields.add_meta_field_with(MetaMethod::Add, |lua| {
            errors::guarded_fn(
                lua,
                |_, (a, b): (UserDataRef<LuaVector2>, UserDataRef<LuaVector2>)| {
                    Ok(LuaVector2([a.0[0] + b.0[0], a.0[1] + b.0[1]]))
                },
            )
        });
        fields.add_meta_field_with(MetaMethod::Sub, |lua| {
            errors::guarded_fn(
                lua,
                |_, (a, b): (UserDataRef<LuaVector2>, UserDataRef<LuaVector2>)| {
                    Ok(LuaVector2([a.0[0] - b.0[0], a.0[1] - b.0[1]]))
                },
            )
        });
        fields.add_meta_field_with(MetaMethod::Mul, |lua| {
            errors::guarded_fn(lua, |_, (a, b): (Value, Value)| {
                match (vector2(&a), vector2(&b), number(&a), number(&b)) {
                    (Some(v), _, _, Some(k)) | (_, Some(v), Some(k), _) => {
                        Ok(LuaVector2([v[0] * k, v[1] * k]))
                    }
                    (Some(a), Some(b), _, _) => Ok(LuaVector2([a[0] * b[0], a[1] * b[1]])),
                    _ => Err(lua_error(
                        "Vector2 can only be multiplied by a number or a Vector2",
                    )),
                }
            })
        });
        fields.add_meta_field_with(MetaMethod::Div, |lua| {
            errors::guarded_fn(lua, |_, (a, b): (Value, Value)| {
                match (vector2(&a), vector2(&b), number(&b)) {
                    (Some(v), _, Some(k)) => Ok(LuaVector2([v[0] / k, v[1] / k])),
                    (Some(a), Some(b), _) => Ok(LuaVector2([a[0] / b[0], a[1] / b[1]])),
                    _ => Err(lua_error(
                        "Vector2 can only be divided by a number or a Vector2",
                    )),
                }
            })
        });
        fields.add_field_method_get("X", |_, this| Ok(this.0[0]));
        fields.add_field_method_get("Y", |_, this| Ok(this.0[1]));
        fields.add_field_method_get("Magnitude", |_, this| Ok(this.magnitude()));
        fields.add_field_method_get("Unit", |_, this| {
            let m = this.magnitude();
            Ok(LuaVector2(if m == 0.0 {
                this.0
            } else {
                [this.0[0] / m, this.0[1] / m]
            }))
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::Unm, |_, this, ()| {
            Ok(LuaVector2([-this.0[0], -this.0[1]]))
        });
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!((vector2(&a), vector2(&b)), (Some(a), Some(b)) if a == b))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("{}, {}", this.0[0], this.0[1]))
        });
    }
}

impl UserData for LuaColor3 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Color3");
        fields.add_field_method_get("R", |_, this| Ok(this.0.r / 255.0));
        fields.add_field_method_get("G", |_, this| Ok(this.0.g / 255.0));
        fields.add_field_method_get("B", |_, this| Ok(this.0.b / 255.0));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!((color(&a), color(&b)), (Some(a), Some(b)) if a == b))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "{}, {}, {}",
                this.0.r / 255.0,
                this.0.g / 255.0,
                this.0.b / 255.0
            ))
        });
    }
}

impl UserData for LuaUDim2 {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "UDim2");
        // `X` and `Y` are read-only tables `{Scale, Offset}`.
        fields.add_field_method_get("X", |lua, this| axis(lua, this.0.x_scale, this.0.x_offset));
        fields.add_field_method_get("Y", |lua, this| axis(lua, this.0.y_scale, this.0.y_offset));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!((udim2(&a), udim2(&b)), (Some(a), Some(b)) if a == b))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let u = this.0;
            Ok(format!(
                "{{{}, {}}}, {{{}, {}}}",
                u.x_scale, u.x_offset, u.y_scale, u.y_offset
            ))
        });
    }
}

fn axis(lua: &Lua, scale: f64, offset: f64) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set("Scale", scale)?;
    t.set("Offset", offset)?;
    t.set_readonly(true);
    Ok(t)
}

/// The constructor tables `Vector3`, `Color3` and `UDim2` (read-only).
pub(crate) fn constructors(lua: &Lua) -> mlua::Result<Vec<(&'static str, Table)>> {
    let vector3 = lua.create_table()?;
    vector3.set(
        "new",
        errors::guarded_fn(
            lua,
            |_, (x, y, z): (Option<f64>, Option<f64>, Option<f64>)| {
                Ok(LuaVector3(Vec3::new(
                    x.unwrap_or(0.0),
                    y.unwrap_or(0.0),
                    z.unwrap_or(0.0),
                )))
            },
        )?,
    )?;
    let vector2 = lua.create_table()?;
    vector2.set(
        "new",
        errors::guarded_fn(lua, |_, (x, y): (Option<f64>, Option<f64>)| {
            Ok(LuaVector2([x.unwrap_or(0.0), y.unwrap_or(0.0)]))
        })?,
    )?;
    let color3 = lua.create_table()?;
    color3.set(
        "fromRGB",
        errors::guarded_fn(
            lua,
            |_, (r, g, b): (Option<f64>, Option<f64>, Option<f64>)| {
                let channel = |c: Option<f64>| c.unwrap_or(0.0).clamp(0.0, 255.0).round();
                Ok(LuaColor3(Color3::new(channel(r), channel(g), channel(b))))
            },
        )?,
    )?;
    let udim2 = lua.create_table()?;
    udim2.set(
        "new",
        errors::guarded_fn(
            lua,
            |_, (xs, xo, ys, yo): (Option<f64>, Option<f64>, Option<f64>, Option<f64>)| {
                Ok(LuaUDim2(UDim2::new(
                    xs.unwrap_or(0.0),
                    xo.unwrap_or(0.0),
                    ys.unwrap_or(0.0),
                    yo.unwrap_or(0.0),
                )))
            },
        )?,
    )?;
    for t in [&vector3, &vector2, &color3, &udim2] {
        t.set_readonly(true);
    }
    Ok(vec![
        ("Vector3", vector3),
        ("Vector2", vector2),
        ("Color3", color3),
        ("UDim2", udim2),
    ])
}

impl ApiState {
    /// Converts a property value to Luau. Instance references become userdata (or nil).
    pub(crate) fn value_to_lua(
        self: &Rc<Self>,
        lua: &Lua,
        value: CoreValue,
    ) -> mlua::Result<Value> {
        Ok(match value {
            CoreValue::Nil | CoreValue::Ref(None) => Value::Nil,
            CoreValue::Bool(b) => Value::Boolean(b),
            CoreValue::Number(n) => Value::Number(n),
            CoreValue::String(s) => Value::String(lua.create_string(s)?),
            CoreValue::Vector3(v) => Value::UserData(lua.create_userdata(LuaVector3(v))?),
            CoreValue::Color3(c) => Value::UserData(lua.create_userdata(LuaColor3(c))?),
            CoreValue::UDim2(u) => Value::UserData(lua.create_userdata(LuaUDim2(u))?),
            CoreValue::Ref(Some(id)) => Value::UserData(self.wrap(lua, id)?),
            CoreValue::CFrame(cf) => Value::UserData(lua.create_userdata(LuaCFrame(*cf))?),
        })
    }
}

/// Converts a Luau value to a property value of type `ty`; errors name `prop`. Numbers
/// and vectors must be finite.
pub(crate) fn lua_to_value(value: &Value, ty: ValueType, prop: &str) -> mlua::Result<CoreValue> {
    let converted = match (ty, value) {
        (ValueType::Bool, Value::Boolean(b)) => Some(CoreValue::Bool(*b)),
        (ValueType::Number, v) => number(v).map(CoreValue::Number),
        (ValueType::String, Value::String(s)) => Some(CoreValue::String(
            String::from_utf8_lossy(&s.as_bytes()).into_owned(),
        )),
        // Enum properties do not come here (`enums::enum_write`). This is a free-form String named
        // like an enum, which keeps its item's name: `binding.KeyCode = Enum.KeyCode.Space`.
        (ValueType::String, v) if super::enums::enum_item(v).is_some_and(|(e, _, _)| e == prop) => {
            super::enums::enum_item(v).map(|(_, name, _)| CoreValue::String(name.to_string()))
        }
        (ValueType::Vector3, v) => vector(v).map(CoreValue::Vector3),
        (ValueType::Color3, v) => color(v).map(CoreValue::Color3),
        (ValueType::UDim2, v) => udim2(v).map(CoreValue::UDim2),
        (ValueType::CFrame, v) => cframe(v).map(|cf| CoreValue::CFrame(Box::new(cf))),
        (ValueType::Ref, Value::Nil) => Some(CoreValue::Ref(None)),
        (ValueType::Ref, Value::UserData(ud)) => ud
            .borrow::<LuaInstance>()
            .ok()
            .map(|i| CoreValue::Ref(Some(i.id))),
        _ => None,
    };
    let converted = converted.ok_or_else(|| {
        lua_error(format!(
            "invalid value for property {prop}: expected {}, got {}",
            ty.name(),
            match value {
                Value::Integer(_) | Value::Number(_) => "number",
                other => other.type_name(),
            }
        ))
    })?;
    let finite = match &converted {
        CoreValue::Number(n) => n.is_finite(),
        CoreValue::Vector3(v) => v.x.is_finite() && v.y.is_finite() && v.z.is_finite(),
        CoreValue::UDim2(u) => {
            u.x_scale.is_finite()
                && u.x_offset.is_finite()
                && u.y_scale.is_finite()
                && u.y_offset.is_finite()
        }
        CoreValue::CFrame(cf) => cf.is_finite(),
        _ => true,
    };
    if !finite {
        return Err(lua_error(format!("property {prop} must be finite")));
    }
    // Stored CFrames keep an orthonormal rotation (Roblox does the same), so a pivot set from
    // a scaled or skewed matrix still round-trips through PivotTo and GetPivot.
    if let CoreValue::CFrame(cf) = &converted {
        let cf = metallic_core::transform::orthonormal(**cf, prop).map_err(|_| {
            lua_error(format!(
                "invalid value for property {prop}: the rotation is degenerate"
            ))
        })?;
        return Ok(CoreValue::CFrame(Box::new(cf)));
    }
    Ok(converted)
}

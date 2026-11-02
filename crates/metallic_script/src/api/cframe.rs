//! Luau `CFrame`: constructors, fields, methods and operators with Roblox names.
//!
//! The math is [`metallic_core::CFrame`]; this module only converts arguments and results.

use metallic_core::{CFrame, RotationOrder, Vec3};
use mlua::{
    Lua, MetaMethod, MultiValue, Table, UserData, UserDataFields, UserDataMethods, UserDataRef,
    Value, Variadic,
};

use super::datatypes::{LuaVector3, vector};
use super::enums::enum_item;
use super::errors;
use super::lua_error;

/// Luau `CFrame`.
#[derive(Clone, Copy)]
pub(crate) struct LuaCFrame(pub(crate) CFrame);

/// The `CFrame` inside a Luau value, if it is one.
pub(crate) fn cframe(v: &Value) -> Option<CFrame> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaCFrame>().ok().map(|c| c.0),
        _ => None,
    }
}

pub(crate) use metallic_core::transform::{part_cframe, set_part_cframe};

fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => Some(*n),
        Value::Integer(i) => Some(*i as f64),
        _ => None,
    }
}

fn vec3(v: Vec3) -> LuaVector3 {
    LuaVector3(v)
}

/// An optional `Enum.RotationOrder` argument; nil means XYZ.
fn rotation_order(v: Option<&Value>) -> mlua::Result<RotationOrder> {
    match v {
        None | Some(Value::Nil) => Ok(RotationOrder::Xyz),
        Some(v) => enum_item(v)
            .filter(|(e, _, _)| *e == "RotationOrder")
            .and_then(|(_, name, _)| RotationOrder::ALL.into_iter().find(|o| o.name() == name))
            .ok_or_else(|| lua_error("expected an Enum.RotationOrder")),
    }
}

fn euler_args(args: &[Option<f64>]) -> (f64, f64, f64) {
    let at = |i: usize| args.get(i).copied().flatten().unwrap_or(0.0);
    (at(0), at(1), at(2))
}

/// `CFrame.new(x, y, z, R00, R01, R02, R10, R11, R12, R20, R21, R22)`, re-orthonormalised like a
/// stored CFrame ([`CFrame::try_orthonormalize`]: an orthonormal matrix is kept bit for bit, a
/// scaled or skewed one keeps its right axis), so a script reads back what a property stores.
/// A finite matrix that cannot be orthonormalised (a zero column, parallel columns) gets the
/// identity rotation at the given position: Roblox does not document that case. A non-finite
/// matrix is kept as given, so assigning it to a property still errors.
fn twelve_numbers(n: &[f64]) -> CFrame {
    let cf = CFrame::from_parts(
        Vec3::new(n[0], n[1], n[2]),
        [[n[3], n[4], n[5]], [n[6], n[7], n[8]], [n[9], n[10], n[11]]],
    );
    match cf.try_orthonormalize() {
        Some(fixed) => fixed,
        None if cf.is_finite() => CFrame::from_position(cf.position),
        None => cf,
    }
}

/// `CFrame.new(...)` with each Roblox overload.
fn new(args: &[Value]) -> mlua::Result<CFrame> {
    let numbers: Option<Vec<f64>> = args.iter().map(number).collect();
    match (args.len(), numbers) {
        (0, _) => Ok(CFrame::IDENTITY),
        (3, Some(n)) => Ok(CFrame::from_position(Vec3::new(n[0], n[1], n[2]))),
        (7, Some(n)) => Ok(CFrame::from_quaternion(
            Vec3::new(n[0], n[1], n[2]),
            [n[3], n[4], n[5], n[6]],
        )),
        (12, Some(n)) => Ok(twelve_numbers(&n)),
        (1, _) => vector(&args[0])
            .map(CFrame::from_position)
            .ok_or_else(|| lua_error("CFrame.new: expected a Vector3")),
        (2, _) => match (vector(&args[0]), vector(&args[1])) {
            (Some(at), Some(target)) => Ok(CFrame::look_at(at, target, Vec3::new(0.0, 1.0, 0.0))),
            _ => Err(lua_error("CFrame.new: expected two Vector3s")),
        },
        _ => Err(lua_error(
            "CFrame.new: expected (), (Vector3), (Vector3, Vector3), 3, 7 or 12 numbers",
        )),
    }
}

fn need_vector(v: &Value, what: &str) -> mlua::Result<Vec3> {
    vector(v).ok_or_else(|| lua_error(format!("{what}: expected a Vector3")))
}

/// The read-only global `CFrame` table.
pub(crate) fn constructor(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    t.set(
        "new",
        errors::guarded_fn(lua, |_, args: Variadic<Value>| Ok(LuaCFrame(new(&args)?)))?,
    )?;
    t.set("identity", LuaCFrame(CFrame::IDENTITY))?;
    t.set(
        "lookAt",
        errors::guarded_fn(lua, |_, (at, target, up): (Value, Value, Option<Value>)| {
            let up = match up {
                None | Some(Value::Nil) => Vec3::new(0.0, 1.0, 0.0),
                Some(v) => need_vector(&v, "CFrame.lookAt")?,
            };
            Ok(LuaCFrame(CFrame::look_at(
                need_vector(&at, "CFrame.lookAt")?,
                need_vector(&target, "CFrame.lookAt")?,
                up,
            )))
        })?,
    )?;
    let xyz = errors::guarded_fn(
        lua,
        |_, (x, y, z): (Option<f64>, Option<f64>, Option<f64>)| {
            let (x, y, z) = euler_args(&[x, y, z]);
            Ok(LuaCFrame(CFrame::from_euler_xyz(x, y, z)))
        },
    )?;
    t.set("Angles", xyz.clone())?;
    t.set("fromEulerAnglesXYZ", xyz)?;
    let yxz = errors::guarded_fn(
        lua,
        |_, (x, y, z): (Option<f64>, Option<f64>, Option<f64>)| {
            let (x, y, z) = euler_args(&[x, y, z]);
            Ok(LuaCFrame(CFrame::from_euler_yxz(x, y, z)))
        },
    )?;
    t.set("fromEulerAnglesYXZ", yxz.clone())?;
    t.set("fromOrientation", yxz)?;
    t.set(
        "fromEulerAngles",
        errors::guarded_fn(
            lua,
            |_, (x, y, z, order): (Option<f64>, Option<f64>, Option<f64>, Option<Value>)| {
                let (x, y, z) = euler_args(&[x, y, z]);
                let order = rotation_order(order.as_ref())?;
                Ok(LuaCFrame(CFrame::from_euler(x, y, z, order)))
            },
        )?,
    )?;
    t.set(
        "fromAxisAngle",
        errors::guarded_fn(lua, |_, (axis, angle): (Value, Option<f64>)| {
            let axis = need_vector(&axis, "CFrame.fromAxisAngle")?;
            Ok(LuaCFrame(CFrame::from_axis_angle(
                axis,
                angle.unwrap_or(0.0),
            )))
        })?,
    )?;
    t.set(
        "fromMatrix",
        errors::guarded_fn(
            lua,
            |_, (pos, vx, vy, vz): (Value, Value, Value, Option<Value>)| {
                let what = "CFrame.fromMatrix";
                let (pos, vx, vy) = (
                    need_vector(&pos, what)?,
                    need_vector(&vx, what)?,
                    need_vector(&vy, what)?,
                );
                let vz = match vz {
                    None | Some(Value::Nil) => Vec3::new(
                        vx.y * vy.z - vx.z * vy.y,
                        vx.z * vy.x - vx.x * vy.z,
                        vx.x * vy.y - vx.y * vy.x,
                    )
                    .unit(),
                    Some(v) => need_vector(&v, what)?,
                };
                Ok(LuaCFrame(CFrame::from_axes(pos, vx, vy, vz)))
            },
        )?,
    )?;
    t.set_readonly(true);
    Ok(t)
}

fn need_cframe(v: &Value, what: &str) -> mlua::Result<CFrame> {
    cframe(v).ok_or_else(|| lua_error(format!("{what}: expected a CFrame")))
}

/// Applies `f` to every argument after `self` and returns the results as a tuple, like
/// Roblox's variadic `ToWorldSpace` family.
fn map_tuple<T, R>(
    lua: &Lua,
    args: &[Value],
    read: impl Fn(&Value) -> Option<T>,
    what: &str,
    f: impl Fn(T) -> R,
) -> mlua::Result<MultiValue>
where
    R: UserData + 'static,
{
    let mut out = MultiValue::new();
    for a in args {
        let v = read(a).ok_or_else(|| lua_error(format!("{what}: wrong argument type")))?;
        out.push_back(Value::UserData(lua.create_userdata(f(v))?));
    }
    Ok(out)
}

/// The methods of a CFrame (`cf:Inverse()` and so on), guarded so errors read like Luau's.
fn methods(lua: &Lua) -> mlua::Result<Table> {
    let t = lua.create_table()?;
    let this = |v: &Value, what: &str| need_cframe(v, what);
    t.set(
        "Inverse",
        errors::guarded_fn(lua, move |_, s: Value| {
            Ok(LuaCFrame(this(&s, "Inverse")?.inverse()))
        })?,
    )?;
    t.set(
        "Lerp",
        errors::guarded_fn(lua, move |_, (s, goal, alpha): (Value, Value, f64)| {
            let goal = need_cframe(&goal, "Lerp")?;
            Ok(LuaCFrame(this(&s, "Lerp")?.lerp(goal, alpha)))
        })?,
    )?;
    t.set(
        "Orthonormalize",
        errors::guarded_fn(lua, move |_, s: Value| {
            Ok(LuaCFrame(this(&s, "Orthonormalize")?.orthonormalize()))
        })?,
    )?;
    t.set(
        "ToWorldSpace",
        errors::guarded_fn(lua, move |lua, (s, rest): (Value, Variadic<Value>)| {
            let cf = this(&s, "ToWorldSpace")?;
            map_tuple(lua, &rest, cframe, "ToWorldSpace", |o| {
                LuaCFrame(cf.to_world(o))
            })
        })?,
    )?;
    t.set(
        "ToObjectSpace",
        errors::guarded_fn(lua, move |lua, (s, rest): (Value, Variadic<Value>)| {
            let cf = this(&s, "ToObjectSpace")?;
            map_tuple(lua, &rest, cframe, "ToObjectSpace", |o| {
                LuaCFrame(cf.to_object(o))
            })
        })?,
    )?;
    type VecFn = fn(CFrame, Vec3) -> Vec3;
    let vector_methods: [(&str, VecFn); 4] = [
        ("PointToWorldSpace", CFrame::point_to_world),
        ("PointToObjectSpace", CFrame::point_to_object),
        ("VectorToWorldSpace", CFrame::vector_to_world),
        ("VectorToObjectSpace", CFrame::vector_to_object),
    ];
    for (name, f) in vector_methods {
        t.set(
            name,
            errors::guarded_fn(lua, move |lua, (s, rest): (Value, Variadic<Value>)| {
                let cf = this(&s, name)?;
                map_tuple(lua, &rest, vector, name, |v| vec3(f(cf, v)))
            })?,
        )?;
    }
    let components = errors::guarded_fn(lua, move |_, s: Value| {
        let c = this(&s, "GetComponents")?.components();
        Ok(Variadic::from_iter(c))
    })?;
    t.set("GetComponents", components.clone())?;
    t.set("components", components)?;
    let euler = |lua: &Lua, order: Option<RotationOrder>, name: &'static str| {
        errors::guarded_fn(lua, move |_, (s, o): (Value, Option<Value>)| {
            let order = match order {
                Some(order) => order,
                None => rotation_order(o.as_ref())?,
            };
            Ok(this(&s, name)?.to_euler(order))
        })
    };
    t.set(
        "ToEulerAnglesXYZ",
        euler(lua, Some(RotationOrder::Xyz), "ToEulerAnglesXYZ")?,
    )?;
    t.set(
        "ToEulerAnglesYXZ",
        euler(lua, Some(RotationOrder::Yxz), "ToEulerAnglesYXZ")?,
    )?;
    t.set(
        "ToOrientation",
        euler(lua, Some(RotationOrder::Yxz), "ToOrientation")?,
    )?;
    t.set("ToEulerAngles", euler(lua, None, "ToEulerAngles")?)?;
    t.set(
        "ToAxisAngle",
        errors::guarded_fn(lua, move |_, s: Value| {
            let (axis, angle) = this(&s, "ToAxisAngle")?.to_axis_angle();
            Ok((vec3(axis), angle))
        })?,
    )?;
    t.set(
        "FuzzyEq",
        errors::guarded_fn(
            lua,
            move |_, (s, other, eps): (Value, Value, Option<f64>)| {
                let a = this(&s, "FuzzyEq")?.components();
                let b = need_cframe(&other, "FuzzyEq")?.components();
                let eps = eps.unwrap_or(1e-5);
                Ok(a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps))
            },
        )?,
    )?;
    t.set_readonly(true);
    Ok(t)
}

/// A field of a CFrame, or `None` when `key` is not a field.
fn field(cf: CFrame, key: &str) -> Option<Field> {
    Some(match key {
        "Position" | "p" => Field::Vector(cf.position),
        "X" => Field::Number(cf.position.x),
        "Y" => Field::Number(cf.position.y),
        "Z" => Field::Number(cf.position.z),
        "Rotation" => Field::CFrame(cf.rotation_only()),
        "LookVector" => Field::Vector(cf.look_vector()),
        "RightVector" | "XVector" => Field::Vector(cf.right_vector()),
        "UpVector" | "YVector" => Field::Vector(cf.up_vector()),
        "ZVector" => Field::Vector(cf.back_vector()),
        _ => return None,
    })
}

enum Field {
    Number(f64),
    Vector(Vec3),
    CFrame(CFrame),
}

impl UserData for LuaCFrame {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "CFrame");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            let methods = methods(lua)?;
            errors::guarded_fn(
                lua,
                move |lua, (this, key): (UserDataRef<LuaCFrame>, Value)| {
                    let key = match &key {
                        Value::String(s) => s.to_string_lossy(),
                        other => other.type_name().to_string(),
                    };
                    match field(this.0, &key) {
                        Some(Field::Number(n)) => Ok(Value::Number(n)),
                        Some(Field::Vector(v)) => {
                            Ok(Value::UserData(lua.create_userdata(vec3(v))?))
                        }
                        Some(Field::CFrame(c)) => {
                            Ok(Value::UserData(lua.create_userdata(LuaCFrame(c))?))
                        }
                        None => match methods.raw_get::<Value>(key.as_str())? {
                            Value::Function(f) => Ok(Value::Function(f)),
                            _ => Err(lua_error(format!("{key} is not a valid member of CFrame"))),
                        },
                    }
                },
            )
        });
        fields.add_meta_field_with(MetaMethod::NewIndex, |lua| {
            errors::guarded_fn(lua, |_, (_, key): (Value, Value)| {
                let key = match &key {
                    Value::String(s) => s.to_string_lossy(),
                    other => other.type_name().to_string(),
                };
                Err::<(), _>(lua_error(format!("{key} cannot be assigned to")))
            })
        });
        fields.add_meta_field_with(MetaMethod::Mul, |lua| {
            errors::guarded_fn(lua, |lua, (a, b): (UserDataRef<LuaCFrame>, Value)| {
                if let Some(o) = cframe(&b) {
                    return Ok(Value::UserData(lua.create_userdata(LuaCFrame(a.0 * o))?));
                }
                if let Some(v) = vector(&b) {
                    return Ok(Value::UserData(lua.create_userdata(vec3(a.0 * v))?));
                }
                Err(lua_error(
                    "CFrame can only be multiplied by a CFrame or a Vector3",
                ))
            })
        });
        fields.add_meta_field_with(MetaMethod::Add, |lua| {
            errors::guarded_fn(lua, |_, (a, b): (UserDataRef<LuaCFrame>, Value)| {
                let v = vector(&b)
                    .ok_or_else(|| lua_error("only a Vector3 can be added to a CFrame"))?;
                Ok(LuaCFrame(a.0.translated(v)))
            })
        });
        fields.add_meta_field_with(MetaMethod::Sub, |lua| {
            errors::guarded_fn(lua, |_, (a, b): (UserDataRef<LuaCFrame>, Value)| {
                let v = vector(&b)
                    .ok_or_else(|| lua_error("only a Vector3 can be subtracted from a CFrame"))?;
                Ok(LuaCFrame(a.0.translated(v * -1.0)))
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!((cframe(&a), cframe(&b)), (Some(a), Some(b)) if a == b))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            let parts: Vec<String> = this.0.components().iter().map(f64::to_string).collect();
            Ok(parts.join(", "))
        });
    }
}

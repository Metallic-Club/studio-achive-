//! Luau values and the language-neutral [`ScriptValue`]: the edge of the host interface.
//!
//! Script functions cross as a [`FunctionRef`] into this VM's [`ScriptFunctions`].

use std::cell::RefCell;
use std::rc::Rc;

use metallic_core::classes::{FunctionRef, ScriptValue};
use mlua::{Function, Lua, Table, Value};

use super::cframe::{LuaCFrame, cframe};
use super::datatypes::{LuaColor3, LuaUDim2, LuaVector3, color, udim2, vector};
use super::enums::enum_item_ref;
use super::instance::LuaInstance;
use super::{ApiState, lua_error};

/// How deep a table argument may nest. A table that holds itself ends here, with an error.
const MAX_TABLE_DEPTH: usize = 32;

/// 2^63: the first number above the `Int` range, and the magnitude of its lowest value.
const I64_BOUND: f64 = 9_223_372_036_854_775_808.0;

/// The functions of this VM that a [`FunctionRef`] currently names: those of the calls in progress,
/// as a stack, so a nested call's references end before its caller's. A reference is good for the
/// call it came with; what outlives that (a callback) keeps the function itself.
#[derive(Default)]
pub(crate) struct ScriptFunctions {
    live: RefCell<Vec<Function>>,
}

impl ScriptFunctions {
    pub(crate) fn add(&self, function: Function) -> FunctionRef {
        let mut live = self.live.borrow_mut();
        live.push(function);
        FunctionRef(live.len() as u64 - 1)
    }

    pub(crate) fn get(&self, reference: FunctionRef) -> Option<Function> {
        let index = usize::try_from(reference.0).ok()?;
        self.live.borrow().get(index).cloned()
    }

    /// Where a call's references start.
    pub(crate) fn mark(&self) -> usize {
        self.live.borrow().len()
    }

    /// Ends the references made since `mark`, even by an argument that failed to convert halfway.
    pub(crate) fn release_since(&self, mark: usize) {
        self.live.borrow_mut().truncate(mark);
    }
}

/// A number as design 5.1 says: whole and in range is an `Int`, anything else a `Number`. Negative
/// zero stays a `Number`, or it would come back as plain zero.
fn number(n: f64) -> ScriptValue {
    if n.fract() == 0.0
        && (-I64_BOUND..I64_BOUND).contains(&n)
        && !(n == 0.0 && n.is_sign_negative())
    {
        ScriptValue::Int(n as i64)
    } else {
        ScriptValue::Number(n)
    }
}

/// A Luau value as a [`ScriptValue`]. A function gets a reference in `state`'s table; the caller
/// ends it with [`ScriptFunctions::release_since`].
pub(crate) fn lua_to_script(state: &ApiState, value: &Value) -> mlua::Result<ScriptValue> {
    to_script(state, value, 0)
}

fn to_script(state: &ApiState, value: &Value, depth: usize) -> mlua::Result<ScriptValue> {
    Ok(match value {
        Value::Nil => ScriptValue::Nil,
        Value::Boolean(b) => ScriptValue::Bool(*b),
        Value::Integer(i) => ScriptValue::Int(*i),
        Value::Number(n) => number(*n),
        Value::String(s) => ScriptValue::String(match s.to_str() {
            Ok(text) => text.as_ref().into(),
            // Invalid UTF-8 is replaced, as a property write does.
            Err(_) => String::from_utf8_lossy(&s.as_bytes()).into(),
        }),
        Value::Function(f) => ScriptValue::Function(state.functions.add(f.clone())),
        Value::Table(table) => table_to_script(state, table, depth)?,
        Value::UserData(ud) => {
            if let Ok(instance) = ud.borrow::<LuaInstance>() {
                ScriptValue::Instance(instance.id)
            } else if let Some(v) = vector(value) {
                ScriptValue::Vector3(v)
            } else if let Some(cf) = cframe(value) {
                cf.into()
            } else if let Some(c) = color(value) {
                ScriptValue::Color3(c)
            } else if let Some(u) = udim2(value) {
                ScriptValue::UDim2(u)
            } else if let Some(item) = enum_item_ref(value) {
                ScriptValue::EnumItem(item)
            } else {
                return Err(unsupported(value));
            }
        }
        other => return Err(unsupported(other)),
    })
}

fn unsupported(value: &Value) -> mlua::Error {
    lua_error(format!(
        "a {} cannot be passed to an Instance method",
        value.type_name()
    ))
}

/// A table is a list when its keys are exactly 1 to its length, otherwise a map.
fn table_to_script(state: &ApiState, table: &Table, depth: usize) -> mlua::Result<ScriptValue> {
    if depth >= MAX_TABLE_DEPTH {
        return Err(lua_error("a table argument is nested too deeply"));
    }
    let mut pairs = Vec::new();
    for pair in table.pairs::<Value, Value>() {
        pairs.push(pair?);
    }
    let length = table.raw_len();
    let is_list = length == pairs.len()
        && pairs.iter().all(|(key, _)| match key {
            Value::Integer(i) => usize::try_from(*i).is_ok_and(|i| (1..=length).contains(&i)),
            Value::Number(n) => n.fract() == 0.0 && *n >= 1.0 && *n <= length as f64,
            _ => false,
        });
    if is_list {
        let mut items = vec![ScriptValue::Nil; length];
        for (key, item) in &pairs {
            let index = match key {
                Value::Integer(i) => *i as usize,
                Value::Number(n) => *n as usize,
                _ => continue,
            };
            items[index - 1] = to_script(state, item, depth + 1)?;
        }
        return Ok(ScriptValue::Array(items));
    }
    let entries = pairs
        .iter()
        .map(|(key, item)| {
            Ok((
                to_script(state, key, depth + 1)?,
                to_script(state, item, depth + 1)?,
            ))
        })
        .collect::<mlua::Result<Vec<_>>>()?;
    Ok(ScriptValue::Map(entries))
}

impl ApiState {
    /// A host value as a Luau value. An instance becomes its one userdata (or nil when gone from
    /// a property), a list and a map become tables; [`ScriptValue::Tuple`] is a list here, since
    /// only the top level of a result can be several values (`api/host_api.rs`).
    pub(crate) fn script_to_lua(
        self: &Rc<Self>,
        lua: &Lua,
        value: ScriptValue,
    ) -> mlua::Result<Value> {
        Ok(match value {
            ScriptValue::Nil => Value::Nil,
            ScriptValue::Bool(b) => Value::Boolean(b),
            ScriptValue::Int(i) => Value::Number(i as f64),
            ScriptValue::Number(n) => Value::Number(n),
            ScriptValue::String(s) => Value::String(lua.create_string(&*s)?),
            ScriptValue::Instance(id) => Value::UserData(self.wrap(lua, id)?),
            ScriptValue::Vector3(v) => Value::UserData(lua.create_userdata(LuaVector3(v))?),
            ScriptValue::CFrame(cf) => Value::UserData(lua.create_userdata(LuaCFrame(*cf))?),
            ScriptValue::Color3(c) => Value::UserData(lua.create_userdata(LuaColor3(c))?),
            ScriptValue::UDim2(u) => Value::UserData(lua.create_userdata(LuaUDim2(u))?),
            ScriptValue::EnumItem(item) => self.enum_ref_value(item),
            ScriptValue::Function(reference) => match self.functions.get(reference) {
                Some(function) => Value::Function(function),
                None => Value::Nil,
            },
            ScriptValue::Array(items) | ScriptValue::Tuple(items) => {
                let table = lua.create_table_with_capacity(items.len(), 0)?;
                for (index, item) in items.into_iter().enumerate() {
                    table.raw_set(index + 1, self.script_to_lua(lua, item)?)?;
                }
                Value::Table(table)
            }
            ScriptValue::Map(entries) => {
                let table = lua.create_table_with_capacity(0, entries.len())?;
                for (key, item) in entries {
                    let key = self.script_to_lua(lua, key)?;
                    table.raw_set(key, self.script_to_lua(lua, item)?)?;
                }
                Value::Table(table)
            }
        })
    }
}

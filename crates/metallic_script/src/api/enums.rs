//! The `Enum` global and `EnumItem`, built from the class registry
//! (`docs/design/class-system.md` section 9).
//!
//! An enum property stays a String in core, holding the item name, so scene files, Studio and
//! replication see names. A read crosses the host interface as `ScriptValue::EnumItem`
//! (`PropDesc::script_value`); this file gives it its Luau object, and a write takes the item or its
//! name. `part.Material == "Metal"` is false because
//! Luau does not call `__eq` across types; scripts compare with the item or with `.Name`.

use metallic_core::Value as CoreValue;
use metallic_core::classes::{self, EnumDesc, EnumItemRef, Registry};
use mlua::{AnyUserData, Lua, MetaMethod, Table, UserData, UserDataFields, UserDataMethods, Value};

use super::{ApiState, errors, lua_error};

/// A value of an `Enum`.
pub(crate) struct LuaEnumItem {
    item: EnumItemRef,
    value: i64,
}

impl UserData for LuaEnumItem {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "EnumItem");
        fields.add_field_method_get("Name", |_, this| Ok(this.item.item()));
        fields.add_field_method_get("Value", |_, this| Ok(this.value));
        fields.add_field_function_get("EnumType", |_, this: AnyUserData| {
            this.user_value::<Table>()
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(matches!(
                (enum_item_ref(&a), enum_item_ref(&b)),
                (Some(a), Some(b)) if a == b
            ))
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!(
                "Enum.{}.{}",
                this.item.enum_name(),
                this.item.item()
            ))
        });
    }
}

/// `(enum, item, value)` of an `EnumItem` value.
pub(crate) fn enum_item(v: &Value) -> Option<(&'static str, &'static str, i64)> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<LuaEnumItem>()
            .ok()
            .map(|i| (i.item.enum_name(), i.item.item(), i.value)),
        _ => None,
    }
}

/// An `EnumItem` value as it crosses the host interface. [`ApiState::enum_ref_value`] is the inverse.
pub(crate) fn enum_item_ref(v: &Value) -> Option<EnumItemRef> {
    match v {
        Value::UserData(ud) => ud.borrow::<LuaEnumItem>().ok().map(|i| i.item),
        _ => None,
    }
}

/// The `Enum` table of one VM and Rust-side access to its items.
pub(crate) struct Enums {
    pub(crate) table: Table,
    registry: &'static Registry,
    /// One object per item: by enum id, then by index in the enum's items.
    items: Vec<Vec<AnyUserData>>,
}

impl Enums {
    pub(crate) fn install(lua: &Lua) -> mlua::Result<Enums> {
        Enums::build(lua, classes::registry())
    }

    fn build(lua: &Lua, registry: &'static Registry) -> mlua::Result<Enums> {
        let table = lua.create_table()?;
        let mut items = Vec::with_capacity(registry.enums().len());
        for resolved in registry.enums() {
            let desc = resolved.desc;
            let sub = lua.create_table()?;
            let mut objects = Vec::with_capacity(desc.items.len());
            for (index, name) in desc.items.iter().enumerate() {
                let ud = lua.create_userdata(LuaEnumItem {
                    item: EnumItemRef::new(desc.name, name),
                    value: desc.value_at(index),
                })?;
                // A user value, so the item keeps its enum table alive without a strong Rust ref.
                ud.set_user_value(sub.clone())?;
                sub.raw_set(*name, ud.clone())?;
                objects.push(ud);
            }
            for (alias, _) in desc.aliases {
                let index = resolved.item_index(alias).map(usize::from);
                if let Some(ud) = index.and_then(|index| objects.get(index)) {
                    sub.raw_set(*alias, ud.clone())?;
                }
            }
            strict(lua, &sub, format!("Enum.{}", desc.name), desc.name)?;
            table.raw_set(desc.name, sub)?;
            items.push(objects);
        }
        strict(lua, &table, "Enum".to_string(), "Enums")?;
        Ok(Enums {
            table,
            registry,
            items,
        })
    }

    /// The item with this name or alias, or nil.
    pub(crate) fn item(&self, enum_name: &str, name: &str) -> Value {
        self.registry
            .enum_by_name(enum_name)
            .and_then(|resolved| {
                let index = usize::from(resolved.item_index(name)?);
                self.items.get(usize::from(resolved.id.0))?.get(index)
            })
            .map_or(Value::Nil, |ud| Value::UserData(ud.clone()))
    }
}

/// Makes `table` read-only, turns a missing member into an error and gives it a printed name.
fn strict(lua: &Lua, table: &Table, label: String, printed: &'static str) -> mlua::Result<()> {
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        errors::guarded_fn(lua, move |_, (_, key): (Value, Value)| {
            Err::<Value, _>(lua_error(format!(
                "{} is not a valid member of {label}",
                match &key {
                    Value::String(s) => s.to_string_lossy(),
                    other => other.type_name().to_string(),
                }
            )))
        })?,
    )?;
    meta.set(
        "__tostring",
        lua.create_function(move |_, _: Value| Ok(printed))?,
    )?;
    table.set_metatable(Some(meta))?;
    table.set_readonly(true);
    Ok(())
}

impl ApiState {
    pub(crate) fn enum_value(&self, enum_name: &str, name: &str) -> Value {
        self.enums.item(enum_name, name)
    }

    /// This VM's object for an item from another VM or from the host interface.
    pub(crate) fn enum_ref_value(&self, item: EnumItemRef) -> Value {
        self.enums.item(item.enum_name(), item.item())
    }
}

/// What a script's write to an enum property stores: the name, from an item of that enum or from
/// a string. `World::set` then checks a string against the items and aliases.
pub(crate) fn enum_write(value: &Value, desc: &EnumDesc, prop: &str) -> mlua::Result<CoreValue> {
    if let Value::String(s) = value {
        let name = String::from_utf8_lossy(&s.as_bytes()).into_owned();
        return Ok(CoreValue::String(name));
    }
    let expected = desc.name;
    match enum_item(value) {
        Some((enum_name, name, _)) if enum_name == expected => {
            Ok(CoreValue::String(name.to_string()))
        }
        Some((enum_name, name, _)) => Err(lua_error(format!(
            "invalid value for property {prop}: expected Enum.{expected}, got Enum.{enum_name}.{name}"
        ))),
        None => Err(lua_error(format!(
            "invalid value for property {prop}: expected Enum.{expected} or an item name, got {}",
            match value {
                Value::Integer(_) | Value::Number(_) => "number",
                other => other.type_name(),
            }
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{HostShared, install};
    use crate::{Context, OutputSink, ScriptVm};
    use metallic_core::World;
    use metallic_core::classes::{HostApi, ScriptValue, resolve_with_enums};
    use std::cell::RefCell;
    use std::rc::Rc;

    struct Silent;

    impl OutputSink for Silent {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    /// What another language's binding sees: items out, items or names in, names stored.
    #[test]
    fn the_host_interface_gives_and_takes_items() {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let part = {
            let mut world = world.borrow_mut();
            let parent = world.root("Workspace");
            world.create("Part", parent).unwrap()
        };
        let output = Rc::new(Silent);
        let vm = ScriptVm::with_output(Context::Server, output.clone()).unwrap();
        let shared = Rc::new(HostShared::default());
        let state = install(&vm, world.clone(), shared, output).unwrap();
        let mut host = state.host();
        let class = host.class_of(part).unwrap();
        let material = host.resolve_member(class, "Material").unwrap();
        let item = |name| ScriptValue::EnumItem(EnumItemRef::new("Material", name));

        assert_eq!(
            host.get_property(part, class, material).unwrap(),
            item("Plastic")
        );
        host.set_property(part, class, material, item("Metal"))
            .unwrap();
        assert_eq!(
            world.borrow().get(part, "Material").unwrap(),
            CoreValue::String("Metal".into())
        );
        host.set_property(part, class, material, ScriptValue::String("Glass".into()))
            .unwrap();
        assert_eq!(
            host.get_property(part, class, material).unwrap(),
            item("Glass")
        );

        let wrong = ScriptValue::EnumItem(EnumItemRef::new("PartType", "Ball"));
        let error = host
            .set_property(part, class, material, wrong)
            .unwrap_err()
            .to_string();
        assert!(error.contains("expected Enum.Material"), "{error}");
        let made_up = host
            .set_property(part, class, material, item("Lava"))
            .unwrap_err()
            .to_string();
        assert!(made_up.contains("expected Enum.Material"), "{made_up}");
        assert_eq!(
            host.get_property(part, class, material).unwrap(),
            item("Glass")
        );
        vm.shutdown();
    }

    /// A fixture registry: no built-in enum has an alias yet.
    #[test]
    fn an_alias_is_the_same_item_object_in_scripts() {
        static SHARED: [EnumDesc; 1] =
            [EnumDesc::new("Fixture", &["New", "Other"]).aliases(&[("Old", "New")])];
        let registry = Box::leak(Box::new(resolve_with_enums(&[], &[&SHARED]).unwrap()));
        let lua = Lua::new();
        errors::install(&lua).unwrap();
        let enums = Enums::build(&lua, registry).unwrap();
        lua.globals().set("Enum", enums.table.clone()).unwrap();
        let same: bool = lua
            .load(
                "return rawequal(Enum.Fixture.Old, Enum.Fixture.New)
                    and Enum.Fixture.Old.Name == 'New'
                    and Enum.Fixture.Old.EnumType == Enum.Fixture",
            )
            .eval()
            .unwrap();
        assert!(same);
        let item = enums.item("Fixture", "Old");
        assert_eq!(enum_item(&item).map(|i| i.1), Some("New"));
        assert!(matches!(enums.item("Fixture", "Nope"), Value::Nil));
        assert!(matches!(enums.item("Nope", "New"), Value::Nil));
    }
}

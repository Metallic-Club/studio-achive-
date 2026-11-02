//! Enums in Luau (#469): the `Enum` global comes from the class registry, enum properties read as
//! `EnumItem`, writes take the item or its name, and core keeps storing the name.

mod common;

use common::Rig;
use metallic_core::classes::{self, registry};
use metallic_core::{Value, load_scene, save_scene};

/// Runs `source` as a server script against a workspace part named `Brick`.
fn run(source: &str) -> (Rig, Vec<String>) {
    let mut rig = Rig::new();
    rig.add("Part", "Brick", rig.root("Workspace"));
    rig.server_script("S", &format!("local part = workspace.Brick\n{source}"));
    rig.host.start().unwrap();
    let lines = rig.lines();
    (rig, lines)
}

#[test]
fn an_item_and_its_name_both_write_and_core_stores_the_name() {
    let (rig, lines) = run(r#"
        part.Material = Enum.Material.Metal
        print(part.Material.Name)
        part.Material = "Glass"
        print(part.Material.Name)
        part.Shape = Enum.PartType.Ball
        "#);
    assert_eq!(lines, ["print:Metal", "print:Glass"]);
    let world = rig.world.borrow();
    let part = world
        .find_first_child(world.root("Workspace").unwrap(), "Brick")
        .unwrap();
    assert_eq!(
        world.get(part, "Material").unwrap(),
        Value::String("Glass".into()),
        "core keeps the name, as Studio, replication and scene files expect"
    );
    assert_eq!(
        world.get(part, "Shape").unwrap(),
        Value::String("Ball".into())
    );
}

#[test]
fn a_read_gives_the_item_and_compares_equal_to_it() {
    let (_, lines) = run(r#"
        print(part.Material == Enum.Material.Plastic, typeof(part.Material))
        part.Material = "Metal"
        print(part.Material == Enum.Material.Metal, part.Material ~= Enum.Material.Glass)
        print(rawequal(part.Material, Enum.Material.Metal))
        "#);
    assert_eq!(
        lines,
        ["print:true EnumItem", "print:true true", "print:true"]
    );
}

/// What a script that used to hold a string notices; `docs/luau-api.md` says the same.
#[test]
fn what_a_script_that_read_a_string_now_sees() {
    let (_, lines) = run(r#"
        part.Material = "Metal"
        print(part.Material)
        print(part.Material == "Metal", part.Material.Name == "Metal")
        print(tostring(part.Material), typeof(part.Material), type(part.Material))
        print(pcall(function() return "is " .. part.Material end))
        print("is " .. part.Material.Name)
        print(pcall(function() part.Name = part.Material end))
        print(pcall(string.lower, part.Material))
        "#);
    assert_eq!(lines[0], "print:Enum.Material.Metal");
    assert_eq!(lines[1], "print:false true");
    assert_eq!(lines[2], "print:Enum.Material.Metal EnumItem userdata");
    assert!(
        lines[3].starts_with("print:false") && lines[3].contains("concatenate"),
        "{lines:?}"
    );
    assert_eq!(lines[4], "print:is Metal");
    assert!(
        lines[5].starts_with("print:false") && lines[5].contains("property Name"),
        "{lines:?}"
    );
    assert!(lines[6].starts_with("print:false"), "{lines:?}");
}

#[test]
fn enum_item_has_name_value_and_enum_type() {
    let (_, lines) = run(r#"
        local metal = Enum.Material.Metal
        print(metal.Name, metal.Value, typeof(metal))
        print(metal.EnumType == Enum.Material, tostring(metal.EnumType))
        print(metal.EnumType.Metal == metal, Enum.PartType.Wedge.Value)
        print(Enum.ContextActionPriority.High.Value, tostring(Enum))
        print(pcall(function() metal.Name = "x" end))
        print(pcall(function() Enum.Material.Metal = 1 end))
        "#);
    assert_eq!(lines[0], "print:Metal 2 EnumItem");
    assert_eq!(lines[1], "print:true Material");
    assert_eq!(lines[2], "print:true 3");
    assert_eq!(lines[3], "print:3000 Enums");
    assert!(lines[4].starts_with("print:false"), "{lines:?}");
    assert!(lines[5].starts_with("print:false"), "{lines:?}");
}

#[test]
fn a_bad_name_item_or_type_is_a_clear_error_and_changes_nothing() {
    let (_, lines) = run(r#"
        part.Material = "Metal"
        print(pcall(function() part.Material = "Lava" end))
        print(pcall(function() part.Material = Enum.PartType.Ball end))
        print(pcall(function() part.Material = 5 end))
        print(pcall(function() part.Material = nil end))
        print(pcall(function() part.Material = part end))
        print(pcall(function() return Enum.Material.Lava end))
        print(pcall(function() return Enum.Nope end))
        print(part.Material.Name)
        "#);
    let has = |line: usize, parts: &[&str]| {
        assert!(lines[line].starts_with("print:false"), "{lines:?}");
        for part in parts {
            assert!(lines[line].contains(part), "{part}: {lines:?}");
        }
    };
    has(0, &["`Material`", "`Lava`", "Metal"]);
    has(
        1,
        &["property Material", "Enum.Material", "Enum.PartType.Ball"],
    );
    has(2, &["property Material", "Enum.Material", "got number"]);
    has(3, &["property Material", "Enum.Material", "got nil"]);
    has(4, &["property Material", "Enum.Material", "got userdata"]);
    has(5, &["Lava is not a valid member of Enum.Material"]);
    has(6, &["Nope is not a valid member of Enum"]);
    assert_eq!(lines[7], "print:Metal");
}

/// Every property the registry says is an enum, on every class that can be made, including the
/// default value: no property is left reading a string, and `Enum.<name>` exists for each.
#[test]
fn every_enum_property_of_every_class_reads_as_its_item() {
    let mut checks = String::new();
    let mut expected = Vec::new();
    for class in registry().classes() {
        if !class.desc.is_creatable() {
            continue;
        }
        for prop in class.props.iter().filter(|prop| !prop.hidden) {
            let Some(desc) = prop.enum_type else {
                continue;
            };
            let (class, name) = (class.name(), prop.name);
            let default = classes::default_value(class, name).unwrap();
            let Value::String(default) = default else {
                panic!("{class}.{name} is not stored as a String");
            };
            let last = desc.items[desc.items.len() - 1];
            checks.push_str(&format!(
                r#"do
                    local i = Instance.new("{class}")
                    local d = i.{name}
                    i.{name} = Enum.{e}.{last}
                    local a = i.{name}
                    i.{name} = "{default}"
                    print(typeof(d), tostring(d), a == Enum.{e}.{last}, i.{name} == d)
                end
                "#,
                e = desc.name
            ));
            expected.push(format!(
                "print:EnumItem Enum.{}.{default} true true",
                desc.name
            ));
        }
    }
    assert!(expected.len() >= 9, "{expected:?}");
    let (_, lines) = run(&checks);
    assert_eq!(lines, expected);
}

#[test]
fn the_enum_global_lists_every_registry_enum_and_only_those() {
    let mut checks = String::from("local count = 0\nfor _ in pairs(Enum) do count += 1 end\n");
    checks.push_str("print(count)\n");
    for resolved in registry().enums() {
        for (index, item) in resolved.desc.items.iter().enumerate() {
            let value = resolved.desc.value_at(index);
            checks.push_str(&format!(
                "assert(Enum.{e}.{item}.Name == \"{item}\" and Enum.{e}.{item}.Value == {value})\n",
                e = resolved.name()
            ));
        }
    }
    let (_, lines) = run(&checks);
    assert_eq!(lines, [format!("print:{}", registry().enums().len())]);
}

#[test]
fn input_binding_key_code_stays_a_string_that_takes_an_item() {
    let (_, lines) = run(r#"
        local binding = Instance.new("InputBinding")
        binding.KeyCode = Enum.KeyCode.Space
        print(binding.KeyCode, typeof(binding.KeyCode))
        binding.KeyCode = "W"
        print(binding.KeyCode)
        "#);
    assert_eq!(lines, ["print:Space string", "print:W"]);
}

#[test]
fn an_item_read_from_a_property_goes_through_a_remote() {
    let mut rig = Rig::new();
    rig.add("RemoteEvent", "Ping", rig.root("ReplicatedStorage"));
    rig.add("Part", "Brick", rig.root("Workspace"));
    rig.server_script(
        "Server",
        r#"
        game:GetService("ReplicatedStorage").Ping.OnServerEvent:Connect(function(_, a, t)
            print(typeof(a), a == Enum.Material.Plastic, t.inner == Enum.PartType.Ball)
        end)
        "#,
    );
    rig.local_script(
        "Client",
        r#"
        local ping = game:GetService("ReplicatedStorage"):WaitForChild("Ping")
        ping:FireServer(workspace.Brick.Material, { inner = Enum.PartType.Ball })
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    assert_eq!(rig.lines(), ["print:EnumItem true true"]);
}

/// Attributes have no item type and core's `Value` must not gain one, so this one errors.
#[test]
fn an_attribute_refuses_an_item_and_keeps_the_name() {
    let (_, lines) = run(r#"
        print(pcall(function() part:SetAttribute("M", part.Material) end))
        part:SetAttribute("M", part.Material.Name)
        print(part:GetAttribute("M"))
        part.Material = part:GetAttribute("M")
        print(part.Material == Enum.Material.Plastic)
        "#);
    assert!(
        lines[0].starts_with("print:false") && lines[0].contains("attributes cannot hold"),
        "{lines:?}"
    );
    assert_eq!(lines[1], "print:Plastic");
    assert_eq!(lines[2], "print:true");
}

#[test]
fn a_save_after_an_item_write_stores_the_name_and_loads_back_as_the_item() {
    let (rig, lines) = run("part.Material = Enum.Material.Brick");
    assert!(lines.is_empty(), "{lines:?}");
    let dir = std::env::temp_dir().join(format!("metallic_enums_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("saved.mscene");
    save_scene(&rig.world.borrow(), &path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(r#"("Material", String("Brick"))"#), "{text}");

    let loaded = load_scene(&path).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    let world = std::rc::Rc::new(std::cell::RefCell::new(loaded));
    let out = std::rc::Rc::new(common::Capture::default());
    let mut host = metallic_script::ScriptHost::new(world.clone(), out.clone());
    host.set_resume_budget(common::TEST_RESUME_BUDGET);
    let mut rig = Rig { world, host, out };
    rig.server_script(
        "S",
        "print(workspace.Brick.Material == Enum.Material.Brick)",
    );
    rig.host.start().unwrap();
    assert_eq!(rig.lines(), ["print:true"]);
}

#[test]
fn a_service_property_reads_as_its_item_too() {
    let (_, lines) = run(r#"
        local lighting = game:GetService("Lighting")
        print(lighting.LightingStyle == Enum.LightingStyle.Realistic)
        lighting.LightingStyle = Enum.LightingStyle.Soft
        print(lighting.LightingStyle)
        "#);
    assert_eq!(lines, ["print:true", "print:Enum.LightingStyle.Soft"]);
}

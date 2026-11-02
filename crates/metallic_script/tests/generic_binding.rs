//! The generic Instance binding (#462): member lookup through the registry and `HostApi`.
//!
//! The first half uses the real classes and the public host. The second half binds a fixture
//! registry, so a member is added with no change to the binding.

mod common;
mod fixture_classes;

use std::collections::BTreeMap;

use common::Rig;
use fixture_classes::{Fixture, decl_of};
use metallic_core::classes::registry;
use metallic_script::Context;

fn printed(rig: &mut Rig, source: &str) -> Vec<String> {
    rig.server_script("S", source);
    rig.host.start().expect("host starts");
    rig.lines()
}

#[test]
fn a_superclass_method_works_on_every_subclass() {
    let creatable: Vec<_> = registry()
        .classes()
        .iter()
        .filter(|class| class.desc.is_creatable())
        .collect();
    let classes = creatable
        .iter()
        .map(|class| format!("{:?}", class.name()))
        .collect::<Vec<_>>()
        .join(", ");
    let under = |base: &str| creatable.iter().filter(|c| c.is_a_named(base)).count();
    // `GetPivot` is declared on PVInstance, `Resize` on BasePart, `FindFirstChild` on Instance.
    let expected = format!(
        "print:{} {} {}",
        creatable.len(),
        under("PVInstance"),
        under("BasePart")
    );
    let mut rig = Rig::new();
    let lines = printed(
        &mut rig,
        &format!(
            r#"
            local counts = {{ any = 0, pv = 0, part = 0 }}
            for _, class in {{ {classes} }} do
                local object = Instance.new(class)
                assert(object:FindFirstChild("Nope") == nil, class)
                counts.any += 1
                if object:IsA("PVInstance") then
                    assert(object:GetPivot() ~= nil, class)
                    counts.pv += 1
                end
                if object:IsA("BasePart") then
                    object:Resize(Enum.NormalId.Top, 1)
                    counts.part += 1
                end
            end
            print(counts.any, counts.pv, counts.part)
            "#
        ),
    );
    assert_eq!(lines, [expected]);
    assert!(under("BasePart") >= 2, "Part and its subclasses");
}

#[test]
fn a_member_of_a_sibling_class_is_not_visible() {
    let mut rig = Rig::new();
    let lines = printed(
        &mut rig,
        r#"
        local function text(object, member)
            local ok, message = pcall(function() return object[member] end)
            assert(not ok, member)
            return (string.gsub(message, "^.-:%d+: ", ""))
        end
        local part, model, folder = Instance.new("Part"), Instance.new("Model"), Instance.new("Folder")
        print(text(part, "ScaleTo"))
        print(text(part, "WorldPivot"))
        print(text(model, "Resize"))
        print(text(model, "Transparency"))
        print(text(model, "Touched"))
        print(text(folder, "GetPivot"))
        "#,
    );
    assert_eq!(
        lines,
        [
            "print:ScaleTo is not a valid member of Part",
            "print:WorldPivot is not a valid member of Part",
            "print:Resize is not a valid member of Model",
            "print:Transparency is not a valid member of Model",
            "print:Touched is not a valid member of Model",
            "print:GetPivot is not a valid member of Folder",
        ]
    );
}

#[test]
fn is_a_pv_instance_follows_the_hierarchy() {
    let mut rig = Rig::new();
    let lines = printed(
        &mut rig,
        r#"
        for _, class in { "Part", "SpawnLocation", "Model", "Folder", "Script" } do
            print(class, Instance.new(class):IsA("PVInstance"))
        end
        print(workspace:IsA("PVInstance"))
        "#,
    );
    assert_eq!(
        lines,
        [
            "print:Part true",
            "print:SpawnLocation true",
            "print:Model true",
            "print:Folder false",
            "print:Script false",
            "print:false",
        ]
    );
}

#[test]
fn a_child_named_like_a_member_does_not_shadow_it() {
    let mut rig = Rig::new();
    let lines = printed(
        &mut rig,
        r#"
        local part = Instance.new("Part")
        local function child(name)
            local kid = Instance.new("Folder")
            kid.Name = name
            kid.Parent = part
        end
        -- A method, a property, an event and a method of another class, then a plain name.
        child("IsA")
        child("Transparency")
        child("Touched")
        child("GetPlayers")
        child("Plain")
        print(type(part.IsA), part:IsA("Part"))
        print(type(part.Transparency))
        print(tostring(part.Touched))
        print(type(part.GetPlayers), part.GetPlayers.Name)
        print(part.Plain.Name)
        "#,
    );
    assert_eq!(
        lines,
        [
            "print:function true",
            "print:number",
            "print:RBXScriptSignal",
            "print:userdata GetPlayers",
            "print:Plain",
        ]
    );
}

#[test]
fn a_method_is_one_function_per_vm() {
    let mut rig = Rig::new();
    let lines = printed(
        &mut rig,
        r#"
        local part, model = Instance.new("Part"), Instance.new("Model")
        print(part.IsA == part.IsA, part.IsA == model.IsA, part.GetPivot == model.GetPivot)
        print(part.IsA == part.Clone)
        "#,
    );
    assert_eq!(lines, ["print:true true true", "print:false"]);
}

/// Class-name string literals `api/instance.rs` may still hold, with how many of each. The arms
/// belong to the issues named; each removal lowers a count, so this list only shrinks.
const CLASS_LITERALS_LEFT: &[(&str, usize, &str)] = &[
    // The userdata's `__type`, what `typeof` and error texts call it: not a class check. Stays.
    ("Instance", 1, "stays"),
    // `CFrame` read and write, `Size` clamp: #464. `Touched`, `TouchEnded`: #463.
    ("BasePart", 5, "#463, #464"),
    // `WorldPivot` read and write: #464.
    ("Model", 2, "#464"),
    // `LocalPlayer`: #464. `PlayerAdded`, `PlayerRemoving`: #463.
    ("Players", 3, "#463, #464"),
    // `CurrentCamera` read and write: computed like the others (#464).
    ("Workspace", 2, "#464"),
    // `ViewportSize` read and write: computed like the others (#464).
    ("Camera", 2, "#464"),
    // Events: #463.
    ("Humanoid", 1, "#463"),
    ("Player", 1, "#463"),
    ("RunService", 1, "#463"),
    ("RemoteEvent", 2, "#463"),
    ("InputAction", 4, "#463"),
];

/// Every string literal in Rust source, comments and char literals left out.
fn string_literals(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'r' if matches!(bytes.get(i + 1), Some(b'"' | b'#'))
                && !bytes[i.saturating_sub(1)].is_ascii_alphanumeric() =>
            {
                let hashes = bytes[i + 1..].iter().take_while(|b| **b == b'#').count();
                let start = i + 1 + hashes + 1;
                let close = format!("\"{}", "#".repeat(hashes));
                let length = source[start..].find(&close).expect("raw string ends");
                found.push(source[start..start + length].to_owned());
                i = start + length + close.len();
            }
            b'"' => {
                let start = i + 1;
                let mut end = start;
                while bytes[end] != b'"' {
                    end += if bytes[end] == b'\\' { 2 } else { 1 };
                }
                found.push(source[start..end].to_owned());
                i = end + 1;
            }
            // `'"'` and friends; a lifetime has no closing quote right after one character.
            b'\'' if bytes.get(i + 2) == Some(&b'\'') => i += 3,
            b'\'' if bytes.get(i + 1) == Some(&b'\\') && bytes.get(i + 3) == Some(&b'\'') => i += 4,
            _ => i += 1,
        }
    }
    found
}

#[test]
fn the_literal_scanner_sees_strings_and_skips_the_rest() {
    let source = "// \"Skipped\"\nlet a = \"One\"; let b = '\"'; let c: &'static str = r#\"T\"w\"o\"#; \"\\\"3\"";
    assert_eq!(string_literals(source), ["One", "T\"w\"o", "\\\"3"]);
}

#[test]
fn instance_rs_holds_no_class_names_beyond_the_shrinking_list() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/api/instance.rs");
    let source = std::fs::read_to_string(path).expect("instance.rs is readable");
    let mut counts = BTreeMap::new();
    for literal in string_literals(&source) {
        if registry().class_by_name(&literal).is_some() {
            *counts.entry(literal).or_insert(0_usize) += 1;
        }
    }
    let allowed: BTreeMap<String, usize> = CLASS_LITERALS_LEFT
        .iter()
        .map(|(class, count, _)| (class.to_string(), *count))
        .collect();
    // Equal, not at most: a removed arm must lower its count, so nothing can come back unseen.
    assert_eq!(
        counts, allowed,
        "api/instance.rs names classes (left) against CLASS_LITERALS_LEFT (right). New members go \
         in class files, not here; after moving an arm out, lower its count."
    );
}

// From here on: a fixture registry.

#[test]
fn a_fixture_method_is_callable_with_no_binding_change() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        print(Instance.new("Part"):Greet())
        print(Instance.new("Model"):Greet())
        print(workspace:Greet())
        print(Instance.new("Folder"):Greet())
        print(Instance.new("Part"):AddOne(41))
        print(Instance.new("Part").Greet(Instance.new("Model")))
        "#,
    );
    assert_eq!(
        lines,
        [
            "hello from a Part",
            "hello from a Model",
            "hello from a Workspace",
            "a Folder's own hello",
            "42",
            "hello from a Model",
        ]
    );
}

#[test]
fn a_fixture_method_of_a_superclass_works_on_every_subclass() {
    let classes = registry()
        .classes()
        .iter()
        .filter(|class| class.desc.is_creatable() && class.name() != "Atmosphere")
        .map(|class| class.name())
        .collect::<Vec<_>>();
    let list = classes
        .iter()
        .map(|class| format!("{class:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    let mut fixture = Fixture::server();
    let lines = fixture.run(&format!(
        r#"
        local passed = 0
        for _, class in {{ {list} }} do
            assert(Instance.new(class):AddOne(1) == 2, class)
            passed += 1
        end
        print(passed)
        "#
    ));
    assert_eq!(lines, [classes.len().to_string()]);
}

#[test]
fn a_fixture_member_of_a_sibling_class_is_not_visible() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        print(Instance.new("Part"):PartOnly())
        print(pcall(function() return Instance.new("Model"):PartOnly() end))
        print(pcall(function() return Instance.new("Model").Pinged end))
        print(pcall(function() return Instance.new("Part").PartOnly(Instance.new("Model")) end))
        -- Hidden on Folder, kept on everything else; then a child of that name is what it finds.
        print(Instance.new("Part"):Secret())
        local folder = Instance.new("Folder")
        print(pcall(function() return folder.Secret end))
        local kid = Instance.new("Part")
        kid.Name = "Secret"
        kid.Parent = folder
        print(folder.Secret == kid)
        print(pcall(function() folder.Greet = 1 end))
        "#,
    );
    assert_eq!(
        lines,
        [
            "only parts have this",
            "false Test:3: PartOnly is not a valid member of Model",
            "false Test:4: Pinged is not a valid member of Model",
            "false Test:5: PartOnly is not a valid member of Model",
            "not for folders",
            "false Test:9: Secret is not a valid member of Folder",
            "true",
            "false Test:14: Greet is read-only",
        ]
    );
}

#[test]
fn lookup_finds_members_before_children_of_any_kind() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        for _, name in { "Transparency", "Greet", "Pinged", "OnPing", "Plain" } do
            local kid = Instance.new("Folder")
            kid.Name = name
            kid.Parent = part
        end
        print(type(part.Transparency))
        print(type(part.Greet))
        print(tostring(part.Pinged))
        print(pcall(function() return part.OnPing end))
        print(part.Plain.Name)
        "#,
    );
    assert_eq!(
        lines,
        [
            "number",
            "function",
            "RBXScriptSignal",
            "false Test:11: OnPing is a callback; it can only be set",
            "Plain",
        ]
    );
}

#[test]
fn a_method_closure_is_cached_per_declaration_and_per_vm() {
    // Both VMs stay alive to the end, so no address can be reused between them.
    let fixtures = [Fixture::server(), Fixture::server()];
    let mut pointers = Vec::new();
    for fixture in &fixtures {
        let wrap = |class| {
            let id = fixture.world.borrow_mut().create(class, None).unwrap();
            fixture.api.wrap(id).unwrap()
        };
        let (part, model, folder) = (wrap("Part"), wrap("Model"), wrap("Folder"));
        let read = fixture.function("return function(object) return object.Greet end");
        let pointer = |object: &mlua::AnyUserData| {
            let function: mlua::Function = read.call(object.clone()).unwrap();
            function.to_pointer()
        };
        // The same declaration on two classes, and asked for again.
        assert_eq!(pointer(&part), pointer(&part));
        assert_eq!(pointer(&part), pointer(&model));
        // An override is its own declaration.
        assert_ne!(pointer(&part), pointer(&folder));
        pointers.push(pointer(&part));
    }
    assert_ne!(pointers[0], pointers[1], "each VM has its own function");
}

#[test]
fn arguments_are_checked_from_the_signature() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        print(pcall(function() return part:AddOne("x") end))
        print(pcall(function() return part:AddOne() end))
        print(pcall(function() return part:AddOne(1, 2) end))
        print(pcall(function() return part:Nothing(1) end))
        print(pcall(function() return part:TakesPart(workspace) end))
        print(pcall(function() return part:TakesPart(5) end))
        print(pcall(function() return part:Fails() end))
        print(pcall(function() return part.AddOne(5, 1) end))
        print(pcall(function() return part.AddOne() end))
        print(pcall(function() return workspace.PartOnly end))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:3: Part:AddOne: argument #1 expected number, got string",
            "false Test:4: Part:AddOne: argument #1 expected number, got nothing",
            "false Test:5: Part:AddOne: expected at most 1 argument, got 2",
            "false Test:6: Part:Nothing: expected at most 0 arguments, got 1",
            "false Test:7: Part:TakesPart: argument #1 expected Part, got Workspace",
            "false Test:8: Part:TakesPart: argument #1 expected Part, got number",
            "false Test:9: a method's own text",
            "false Test:10: bad argument #1: error converting Lua number to userdata (expected \
             userdata of type 'LuaInstance')",
            "false Test:11: bad argument #1: error converting Lua nil to userdata (expected \
             userdata of type 'LuaInstance')",
            "false Test:12: PartOnly is not a valid member of Workspace",
        ]
    );
}

#[test]
fn a_destroyed_instance_is_named_by_what_the_binding_last_saw() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        part.Name = "Doomed"
        local method = part.AddOne
        local other = Instance.new("Part")
        other.Name = "Argument"
        local takes = other.TakesPart
        local gone = Instance.new("Part")
        gone.Name = "Gone"
        gone:Destroy()
        print(pcall(takes, other, gone))
        part:Destroy()
        print(pcall(method, part, 1))
        print(pcall(function() return part:AddOne(1) end))
        print(pcall(function() return part.Name end))
        print(pcall(function() part.Name = "x" end))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:11: Gone has been destroyed",
            "false Test:13: Doomed has been destroyed",
            "false Test:14: Doomed has been destroyed",
            "false Test:15: Doomed has been destroyed",
            "false Test:16: Doomed has been destroyed",
        ]
    );
}

#[test]
fn a_method_body_sees_the_world_and_a_write_keeps_the_name_it_knows() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r##"
        local part = Instance.new("Part")
        print(select("#", part:Rename("Renamed")))
        print(part.Name)
        print(select("#", part:Nothing()))
        print(select("#", part:Greet()))
        "##,
    );
    assert_eq!(lines, ["0", "Renamed", "0", "1"]);
}

#[test]
fn values_cross_the_host_interface_and_come_back() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r##"
        local part = Instance.new("Part")
        print(select("#", part:Echo()))
        print(select("#", part:Echo(nil, nil)))
        print(part:Echo(nil, 1, 1.5, -0, true, "text", "\255x"))
        local v, cf, c, u, i, e = part:Echo(Vector3.new(1, 2, 3), CFrame.new(4, 5, 6),
            Color3.fromRGB(7, 8, 9), UDim2.new(1, 2, 3, 4), part, Enum.NormalId.Top)
        print(v, cf.Position, c, u, i == part, e == Enum.NormalId.Top)
        print(1 / select(1, part:Echo(-0.0)))
        local called = part:Echo(function() return "called" end)
        print(called())
        local list = part:Echo({ 1, "two", { 3 } })
        print(#list, list[1], list[2], list[3][1])
        local map = part:Echo({ a = 1, [2] = "b" })
        print(map.a, map[2], #map)
        print(select("#", part:Echo(1, 2, 3, 4, 5, 6, 7, 8)))
        print(part:Echo(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12))
        print(pcall(function() return part:Echo(Vector2.new(1, 2)) end))
        print(pcall(function() return part:Echo(coroutine.create(print)) end))
        local loop = {}
        loop.self = loop
        print(pcall(function() return part:Echo(loop) end))
        print(part:AddOne(0.5), part:AddOne(1e300))
        "##,
    );
    assert_eq!(
        lines,
        [
            "1",
            "2",
            "nil 1 1.5 -0 true text \u{fffd}x",
            "1, 2, 3 4, 5, 6 0.027450980392156862, 0.03137254901960784, 0.03529411764705882 \
             {1, 2}, {3, 4} true true",
            "-inf",
            "called",
            "3 1 two 3",
            "1 b 0",
            "8",
            "1 2 3 4 5 6 7 8 9 10 11 12",
            "false Test:18: a userdata cannot be passed to an Instance method",
            "false Test:19: a thread cannot be passed to an Instance method",
            "false Test:22: a table argument is nested too deeply",
            "1.5 1e+300",
        ]
    );
}

#[test]
fn the_function_references_of_a_call_end_with_it() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        local f = function() end
        for _ = 1, 3 do
            assert(part:Echo(f) == f)
            assert(pcall(function() return part:Echo(f, Vector2.new()) end) == false)
            assert(pcall(function() return part:Echo({ f, Vector2.new() }) end) == false)
        end
        print("ok")
        "#,
    );
    assert_eq!(lines, ["ok"]);
    assert_eq!(fixture.api.functions_in_use(), 0);
}

#[test]
fn a_yielding_method_parks_its_thread_and_is_resumed_with_the_answer() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part", workspace)
        task.spawn(function()
            print("found", part:WaitKid("Late").Name)
        end)
        task.spawn(function()
            print("timed out", part:WaitKid("Never", 1))
        end)
        local kid = Instance.new("Part", part)
        kid.Name = "Now"
        print("at once", part:WaitKid("Now") == kid)
        print("parked")
        "#,
    );
    assert_eq!(lines, ["at once true", "parked"]);
    fixture.api.wake_waiters(0.5);
    assert_eq!(fixture.take_output(), Vec::<String>::new());
    {
        let mut world = fixture.world.borrow_mut();
        let workspace = world.root("Workspace").unwrap();
        let part = world.children(workspace)[0];
        let late = world.create("Folder", Some(part)).unwrap();
        world
            .set(late, "Name", metallic_core::Value::String("Late".into()))
            .unwrap();
    }
    fixture.api.wake_waiters(0.75);
    assert_eq!(fixture.take_output(), ["found Late"]);
    fixture.api.wake_waiters(2.0);
    assert_eq!(fixture.take_output(), ["timed out nil"]);
}

#[test]
fn a_yielding_method_can_sleep_and_refuses_what_nothing_can_resume() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        task.spawn(function()
            print("slept", part:Nap(1))
        end)
        print(pcall(function() return part:WaitSignal() end))
        print(pcall(function() return part:WaitsUndeclared(1) end))
        print("waiting")
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:6: waiting for a signal arrives with #463",
            "false Test:7: WaitsUndeclared waited but is not declared as yielding",
            "waiting",
        ]
    );
    fixture.vm.step(0.5);
    assert_eq!(fixture.take_output(), Vec::<String>::new());
    fixture.vm.step(2.0);
    assert_eq!(fixture.take_output(), ["slept nil"]);
}

#[test]
fn a_host_method_runs_with_the_vm_side_and_a_missing_body_says_so() {
    for (context, side) in [
        (Context::Server, "the server"),
        (Context::Client, "a client"),
    ] {
        let mut fixture = Fixture::new(context);
        let lines = fixture.run(
            r#"
            local part = Instance.new("Part")
            print(part:WhichSide())
            print(pcall(function() return part:NoBody() end))
            "#,
        );
        assert_eq!(
            lines,
            [side, "false Test:4: NoBody has no host implementation",]
        );
    }
}

#[test]
fn a_host_method_with_a_body_stops_asking_the_bridge() {
    // The fixture gives `GetPivot` a body that returns nothing and leaves `PivotTo` to the bridge.
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r##"
        local part = Instance.new("Part")
        print(part:GetPivot())
        print(pcall(function() return part:PivotTo(1) end))
        part:PivotTo(CFrame.new(1, 2, 3))
        print(part.Position)
        "##,
    );
    assert_eq!(
        lines,
        ["nil", "false Test:4: PivotTo: expected a CFrame", "1, 2, 3"]
    );
}

#[test]
fn a_callback_can_only_be_set_and_the_host_keeps_it() {
    let mut fixture = Fixture::server();
    let id = fixture
        .world
        .borrow_mut()
        .create("Part", None)
        .expect("create");
    let decl = decl_of("Part", "OnPing");
    let part = fixture.api.wrap(id).unwrap();
    let set = fixture.function("return function(part, f) part.OnPing = f end");
    assert!(fixture.api.callback(id, decl).is_none());
    set.call::<()>((part.clone(), fixture.function("return print")))
        .unwrap();
    assert!(fixture.api.callback(id, decl).is_some());
    set.call::<()>((part.clone(), mlua::Value::Nil)).unwrap();
    assert!(fixture.api.callback(id, decl).is_none());
    set.call::<()>((part.clone(), fixture.function("return print")))
        .unwrap();

    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        print(pcall(function() part.OnPing = 5 end))
        print(pcall(function() return part.OnPing end))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:3: invalid value for callback OnPing: expected function, got integer",
            "false Test:4: OnPing is a callback; it can only be set",
        ]
    );

    fixture.world.borrow_mut().destroy(id);
    fixture.api.prune();
    assert!(fixture.api.callback(id, decl).is_none());
}

#[test]
fn a_declared_signal_connects_and_is_read_only() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        local connection = part.Pinged:Connect(function() end)
        print(connection.Connected)
        connection:Disconnect()
        print(connection.Connected)
        print(pcall(function() part.Pinged = 1 end))
        "#,
    );
    assert_eq!(
        lines,
        ["true", "false", "false Test:7: Pinged is read-only"]
    );
}

#[test]
fn instance_new_reads_the_flag_from_the_registry_it_binds() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        print(pcall(Instance.new, "Atmosphere"))
        print(Instance.new("Part").ClassName)
        print(pcall(Instance.new, "Nope", 5))
        print(pcall(Instance.new, "UserInputService", 5))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:2: Unable to create an Instance of type \"Atmosphere\"",
            "Part",
            "false Test:4: Instance.new: parent must be an Instance",
            "false Test:5: Unable to create an Instance of type \"UserInputService\"",
        ]
    );
}

#[test]
fn a_namecall_on_something_not_callable_says_what_the_vm_says() {
    let mut fixture = Fixture::server();
    let lines = fixture.run(
        r#"
        local part = Instance.new("Part")
        local kid = Instance.new("Folder", part)
        kid.Name = "Kid"
        print(pcall(function() return part:Name() end))
        print(pcall(function() return part:Transparency() end))
        print(pcall(function() return part:Anchored() end))
        print(pcall(function() return part:Position() end))
        print(pcall(function() return part:Kid() end))
        print(pcall(function() return part:Touched() end))
        print(pcall(function() return part:Nope() end))
        print(pcall(function() return part["Name"](part) end))
        "#,
    );
    assert_eq!(
        lines,
        [
            "false Test:5: attempt to call a string value",
            "false Test:6: attempt to call a number value",
            "false Test:7: attempt to call a boolean value",
            "false Test:8: attempt to call a Vector3 value",
            "false Test:9: attempt to call a Instance value",
            "false Test:10: attempt to call a RBXScriptSignal value",
            "false Test:11: Nope is not a valid member of Part",
            "false Test:12: attempt to call a string value",
        ]
    );
}

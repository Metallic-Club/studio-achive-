//! `Instance.Changed` and `Instance:GetPropertyChangedSignal(name)` (#410, #463). `Parent` and `CFrame`
//! are not class properties until #464, so they are not valid names yet (#437).

mod common;

use common::Rig;
use metallic_core::{InstanceId, Value, Vec3};

fn prints(rig: &Rig) -> Vec<String> {
    rig.lines()
        .into_iter()
        .filter_map(|l| l.strip_prefix("print:").map(str::to_string))
        .collect()
}

fn errors(rig: &Rig) -> Vec<String> {
    rig.lines()
        .into_iter()
        .filter(|l| l.starts_with("error:") || l.starts_with("warn:"))
        .collect()
}

fn set(rig: &Rig, id: InstanceId, prop: &str, value: Value) {
    rig.world.borrow_mut().set(id, prop, value).unwrap();
}

/// A rig with `workspace.Brick` (a Part).
fn rig_with_brick() -> (Rig, InstanceId) {
    let rig = Rig::new();
    let brick = rig.add("Part", "Brick", rig.root("Workspace"));
    (rig, brick)
}

#[test]
fn property_changed_signal_fires_on_real_change() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r##"
        local brick = workspace.Brick
        local signal = brick:GetPropertyChangedSignal("Transparency")
        print(typeof(signal), tostring(signal))
        signal:Connect(function(...)
            print("changed", select("#", ...), brick.Transparency)
        end)
        brick.Transparency = 0.25
        "##,
    );
    rig.host.start().unwrap();
    assert_eq!(prints(&rig), ["RBXScriptSignal RBXScriptSignal"]);
    let t = rig.run(0.0, 1);
    assert_eq!(&prints(&rig)[1..], ["changed 0 0.25"], "a Luau write fires");
    // Other properties do not fire this signal.
    set(&rig, brick, "Anchored", Value::Bool(true));
    set(&rig, brick, "Transparency", Value::Number(0.5));
    rig.run(t, 1);
    assert_eq!(&prints(&rig)[1..], ["changed 0 0.25", "changed 0 0.5"]);
    assert!(errors(&rig).is_empty(), "{:?}", rig.lines());
}

#[test]
fn same_value_write_does_not_fire() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        brick:GetPropertyChangedSignal("Transparency"):Connect(function() print("prop") end)
        brick.Changed:Connect(function(name) print("changed", name) end)
        brick.Transparency = brick.Transparency
        brick.Name = "Brick"
        "#,
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 2);
    set(&rig, brick, "Transparency", Value::Number(0.0));
    set(&rig, brick, "Name", Value::String("Brick".into()));
    let t = rig.run(t, 2);
    assert!(prints(&rig).is_empty(), "{:?}", rig.lines());
    // A -> B -> A within one step is two real changes, like Roblox.
    set(&rig, brick, "Transparency", Value::Number(0.5));
    set(&rig, brick, "Transparency", Value::Number(0.0));
    rig.run(t, 1);
    assert_eq!(
        prints(&rig),
        [
            "prop",
            "changed Transparency",
            "prop",
            "changed Transparency"
        ]
    );
}

#[test]
fn changed_passes_the_property_name() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        brick.Changed:Connect(function(name, extra)
            print(name, type(name), extra)
        end)
        brick.Name = "Renamed"
        brick.Anchored = true
        "#,
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 1);
    set(
        &rig,
        brick,
        "Position",
        Value::Vector3(Vec3::new(0.0, 5.0, 0.0)),
    );
    rig.run(t, 1);
    assert_eq!(
        prints(&rig),
        [
            "Name string nil",
            "Anchored string nil",
            "Position string nil"
        ]
    );
}

#[test]
fn hidden_property_does_not_fire_changed() {
    let mut rig = Rig::new();
    let model = rig.add("Model", "Box", rig.root("Workspace"));
    rig.server_script(
        "Watch",
        r#"
        local box = workspace.Box
        box.Changed:Connect(function(name) print("changed", name) end)
        for _, name in { "ScaleFactor", "WorldPivotSet" } do
            local ok, err = pcall(function() return box:GetPropertyChangedSignal(name) end)
            print(ok, err)
        end
        "#,
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 1);
    set(&rig, model, "ScaleFactor", Value::Number(2.0));
    set(&rig, model, "WorldPivotSet", Value::Bool(true));
    let t = rig.run(t, 1);
    set(&rig, model, "Name", Value::String("Crate".into()));
    rig.run(t, 1);
    let out = prints(&rig);
    assert_eq!(out.len(), 3, "{out:?}");
    assert!(
        out[0].starts_with("false")
            && out[0].ends_with("ScaleFactor is not a valid property name.")
    );
    assert!(
        out[1].starts_with("false")
            && out[1].ends_with("WorldPivotSet is not a valid property name.")
    );
    assert_eq!(out[2], "changed Name", "only the visible change: {out:?}");
}

#[test]
fn unknown_property_name_errors_like_roblox() {
    let (mut rig, _) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        print(pcall(brick.GetPropertyChangedSignal, brick, "Nope"))
        print(pcall(brick.GetPropertyChangedSignal, brick, "transparency"))
        print(pcall(brick.GetPropertyChangedSignal, brick, "Parent"))
        brick:GetPropertyChangedSignal("Bogus")
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 1);
    let out = prints(&rig);
    assert_eq!(out.len(), 3, "{:?}", rig.lines());
    assert!(
        out[0].starts_with("false ") && out[0].ends_with(":3: Nope is not a valid property name."),
        "{out:?}"
    );
    assert!(
        out[1].ends_with("transparency is not a valid property name."),
        "{out:?}"
    );
    assert!(
        out[2].ends_with("Parent is not a valid property name."),
        "{out:?}"
    );
    let errs = errors(&rig);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert!(
        errs[0].ends_with(":6: Bogus is not a valid property name."),
        "{errs:?}"
    );
}

#[test]
fn write_inside_handler_fires_on_next_dispatch() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        local n = 0
        brick:GetPropertyChangedSignal("Transparency"):Connect(function()
            n += 1
            print("fired", n)
            -- Changes the watched property again: fires next step, never recursively.
            brick.Transparency = if brick.Transparency < 0.5 then 0.75 else 0.25
        end)
        "#,
    );
    rig.host.start().unwrap();
    set(&rig, brick, "Transparency", Value::Number(0.1));
    let mut t = 0.0;
    for step in 1..=4 {
        t = rig.run(t, 1);
        assert_eq!(
            prints(&rig).len(),
            step,
            "one dispatch per step: {:?}",
            rig.lines()
        );
    }
    assert_eq!(prints(&rig), ["fired 1", "fired 2", "fired 3", "fired 4"]);
    assert!(errors(&rig).is_empty(), "{:?}", rig.lines());
}

#[test]
fn rust_side_write_fires_the_signal() {
    let (mut rig, brick) = rig_with_brick();
    rig.local_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        brick:GetPropertyChangedSignal("Position"):Connect(function()
            print("client sees", brick.Position.Y)
        end)
        "#,
    );
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        brick:GetPropertyChangedSignal("Position"):Connect(function()
            print("server sees", brick.Position.Y)
        end)
        "#,
    );
    rig.host.start().unwrap();
    // Like physics or the character controller moving a part between steps.
    set(
        &rig,
        brick,
        "Position",
        Value::Vector3(Vec3::new(0.0, 7.0, 0.0)),
    );
    let t = rig.run(0.0, 1);
    let mut out = prints(&rig);
    out.sort();
    assert_eq!(out, ["client sees 7", "server sees 7"], "each VM once");
    rig.run(t, 3);
    assert_eq!(prints(&rig).len(), 2, "nothing more without a change");
}

#[test]
fn changes_made_before_watching_do_not_fire() {
    let (mut rig, _) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        brick.Transparency = 0.5
        brick.Changed:Connect(function(name) print("changed", name) end)
        brick.Anchored = true
        "#,
    );
    rig.host.start().unwrap();
    rig.run(0.0, 2);
    assert_eq!(prints(&rig), ["changed Anchored"]);
}

#[test]
fn destroyed_instance_drops_connections() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        local a = brick.Changed:Connect(function(name) print("old changed", name) end)
        local b = brick:GetPropertyChangedSignal("Transparency"):Connect(function() print("old prop") end)
        game:GetService("RunService").Heartbeat:Connect(function()
            print("connected", a.Connected, b.Connected)
        end)
        "#,
    );
    rig.host.start().unwrap();
    let t = rig.run(0.0, 1);
    assert_eq!(prints(&rig), ["connected true true"]);
    // Changed and then destroyed before the next step: nothing fires for it.
    set(&rig, brick, "Transparency", Value::Number(0.5));
    rig.world.borrow_mut().destroy(brick);
    // A new part most likely reuses the slot; the old connections must never see it.
    let fresh = rig.add("Part", "Brick", rig.root("Workspace"));
    assert_eq!(
        fresh.to_bits() & 0xffff_ffff,
        brick.to_bits() & 0xffff_ffff,
        "the slot is reused"
    );
    set(&rig, fresh, "Transparency", Value::Number(0.75));
    let t = rig.run(t, 1);
    set(&rig, fresh, "Transparency", Value::Number(0.25));
    rig.run(t, 1);
    let out = prints(&rig);
    assert!(
        out.iter().all(|l| !l.starts_with("old")),
        "no old connection fired: {out:?}"
    );
    assert_eq!(out.last().unwrap(), "connected false false", "{out:?}");
}

#[test]
fn disconnect_stops_the_signal() {
    let (mut rig, brick) = rig_with_brick();
    rig.server_script(
        "Watch",
        r#"
        local brick = workspace.Brick
        local prop, changed
        prop = brick:GetPropertyChangedSignal("Transparency"):Connect(function()
            print("prop", prop.Connected)
            prop:Disconnect()
        end)
        changed = brick.Changed:Connect(function(name)
            print("changed", name)
            changed:Disconnect()
            print("after", changed.Connected)
        end)
        "#,
    );
    rig.host.start().unwrap();
    set(&rig, brick, "Transparency", Value::Number(0.5));
    let t = rig.run(0.0, 1);
    set(&rig, brick, "Transparency", Value::Number(0.25));
    set(&rig, brick, "Name", Value::String("Other".into()));
    rig.run(t, 2);
    assert_eq!(
        prints(&rig),
        ["prop true", "changed Transparency", "after false"]
    );
}

#[test]
fn unwatched_properties_cost_nothing() {
    let mut rig = Rig::new();
    let ws = rig.root("Workspace");
    let parts: Vec<InstanceId> = (0..200)
        .map(|i| rig.add("Part", &format!("P{i}"), ws))
        .collect();
    rig.server_script(
        "Busy",
        r#"
        game:GetService("RunService").Heartbeat:Connect(function() end)
        workspace.P0.Touched:Connect(function() end)
        "#,
    );
    rig.host.start().unwrap();
    let mut t = 0.0;
    for round in 0..5 {
        for &p in &parts {
            set(
                &rig,
                p,
                "Transparency",
                Value::Number(f64::from(round) / 10.0 + 0.05),
            );
        }
        t = rig.run(t, 1);
    }
    assert_eq!(
        rig.host.property_changes_scanned(),
        0,
        "no watcher: the journal is never read for property signals"
    );

    // Watching one part costs one look per journal entry, and only while it lives.
    rig.server_script(
        "Watch",
        "workspace.P7.Changed:Connect(function(name) print(name) end)",
    );
    t = rig.run(t, 1);
    for &p in &parts {
        set(&rig, p, "Transparency", Value::Number(0.9));
    }
    t = rig.run(t, 1);
    assert_eq!(prints(&rig), ["Transparency"]);
    let scanned = rig.host.property_changes_scanned();
    assert!((200..400).contains(&scanned), "{scanned}");
    rig.world.borrow_mut().destroy(parts[7]);
    t = rig.run(t, 1);
    let after_destroy = rig.host.property_changes_scanned();
    for &p in &parts[8..] {
        set(&rig, p, "Transparency", Value::Number(0.1));
    }
    rig.run(t, 3);
    assert_eq!(
        rig.host.property_changes_scanned(),
        after_destroy,
        "the destroyed part's watch is gone, so nothing is read any more"
    );
}

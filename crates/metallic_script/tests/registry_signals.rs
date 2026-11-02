//! Signals as class members (#463): inherited, keyed by the registry, fired with no binding change.
//!
//! The first half uses the real classes. The second half binds a registry whose signal, method and
//! property are declared only here, so the binding cannot know them. `World` only creates real
//! classes, so the fixture keeps their names.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;

use metallic_core::classes::{
    self, ApiErrorKind, CallOutcome, ClassDesc, FunctionRef, HostMethods, MemberContext, MemberId,
    MemberKind, MethodCall, MethodDesc, Param, PropDesc, Registry, ResolvedClass, ScriptValue,
    SignalDesc, SignalKey, Ty, WaitRequest,
};
use metallic_core::{CFrame, Color3, InstanceId, UDim2, Value, ValueType, Vec3, World};
use metallic_script::{Context, FixtureApi, HostState, OutputSink, ScriptHost, ScriptVm};

#[derive(Default)]
struct Capture(RefCell<Vec<String>>);

impl OutputSink for Capture {
    fn print(&self, line: &str) {
        self.0.borrow_mut().push(line.to_string());
    }
    fn warn(&self, line: &str) {
        self.0.borrow_mut().push(format!("warn: {line}"));
    }
    fn error(&self, line: &str) {
        self.0.borrow_mut().push(format!("error: {line}"));
    }
}

// ---- The real classes --------------------------------------------------------------------------

/// Runs `source` in a server script of a fresh host and gives back what it printed.
fn on_host(source: &str) -> Vec<String> {
    let world = Rc::new(RefCell::new(World::new_with_services()));
    let out = Rc::new(Capture::default());
    let mut host = ScriptHost::new(world.clone(), out.clone());
    host.set_resume_budget(std::time::Duration::from_secs(2));
    {
        let mut world = world.borrow_mut();
        let services = world.root("ServerScriptService").unwrap();
        let script = world.create("Script", Some(services)).unwrap();
        world
            .set(script, "Source", Value::String(source.into()))
            .unwrap();
    }
    host.start().unwrap();
    out.0.take()
}

fn creatable_under(base: &str) -> Vec<&'static ResolvedClass> {
    classes::registry()
        .classes()
        .iter()
        .filter(|class| class.desc.is_creatable() && class.is_a_named(base))
        .collect()
}

#[test]
fn a_signal_declared_on_a_superclass_connects_on_every_subclass() {
    for (base, signal, expected_at_least) in [
        ("BasePart", "Touched", 3),
        ("BasePart", "TouchEnded", 3),
        ("Frame", "MouseEnter", 5),
        ("GuiButton", "Activated", 3),
        ("Instance", "AttributeChanged", 20),
        ("Instance", "Changed", 20),
    ] {
        let classes = creatable_under(base);
        assert!(classes.len() >= expected_at_least, "{base}");
        let list = classes
            .iter()
            .map(|class| format!("{:?}", class.name()))
            .collect::<Vec<_>>()
            .join(", ");
        let lines = on_host(&format!(
            r##"
            local connected = 0
            for _, class in {{ {list} }} do
                local connection = Instance.new(class).{signal}:Connect(function() end)
                assert(connection.Connected, class)
                connected += 1
            end
            print(connected)
            "##
        ));
        assert_eq!(lines, [classes.len().to_string()], "{base}.{signal}");
    }
}

#[test]
fn a_signal_of_a_sibling_class_is_not_visible() {
    let lines = on_host(
        r##"
        local function text(class, signal)
            local ok, message = pcall(function() return Instance.new(class)[signal] end)
            assert(not ok, signal)
            return (string.gsub(message, "^.-:%d+: ", ""))
        end
        print(text("Model", "Touched"))
        print(text("Folder", "Died"))
        print(text("Part", "CharacterAdded"))
        print(text("TextLabel", "Activated"))
        print(text("UnreliableRemoteEvent", "OnServerEvent"))
        "##,
    );
    assert_eq!(
        lines,
        [
            "Touched is not a valid member of Model",
            "Died is not a valid member of Folder",
            "CharacterAdded is not a valid member of Part",
            "Activated is not a valid member of TextLabel",
            "OnServerEvent is not a valid member of UnreliableRemoteEvent",
        ]
    );
}

#[test]
fn an_undeclared_signal_gives_the_registry_error() {
    let lines = on_host(
        r##"
        print(pcall(function() return Instance.new("Part").Nope:Connect(print) end))
        print(pcall(function() return Instance.new("Part"):GetPropertyChangedSignal("Nope") end))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false ServerScriptService.Script:2: Nope is not a valid member of Part",
            "false ServerScriptService.Script:3: Nope is not a valid property name.",
        ]
    );
}

#[test]
fn every_signal_scripts_have_is_declared_where_they_see_it() {
    let declared = |class: &str, signal: &str| {
        let class = classes::registry().class_by_name(class).unwrap();
        let id = class.member(signal).unwrap();
        match class.members[usize::from(id.0)].kind {
            MemberKind::Signal { desc, .. } => {
                assert!(!desc.description.is_empty(), "{signal} has no describe");
                desc
            }
            _ => panic!("{signal} is not a signal"),
        }
    };
    for (class, signal) in [
        ("Instance", "AttributeChanged"),
        ("Instance", "Changed"),
        ("BasePart", "Touched"),
        ("BasePart", "TouchEnded"),
        ("Humanoid", "Died"),
        ("Player", "CharacterAdded"),
        ("Players", "PlayerAdded"),
        ("Players", "PlayerRemoving"),
        ("RunService", "Heartbeat"),
        ("RemoteEvent", "OnServerEvent"),
        ("RemoteEvent", "OnClientEvent"),
        ("InputAction", "Pressed"),
        ("InputAction", "Released"),
        ("InputAction", "Triggered"),
        ("InputAction", "StateChanged"),
        ("Frame", "MouseEnter"),
        ("Frame", "MouseLeave"),
        ("Frame", "InputBegan"),
        ("Frame", "InputChanged"),
        ("Frame", "InputEnded"),
        ("GuiButton", "MouseButton1Down"),
        ("GuiButton", "MouseButton1Up"),
        ("GuiButton", "MouseButton1Click"),
        ("GuiButton", "Activated"),
    ] {
        declared(class, signal);
    }
    assert_eq!(
        declared("BasePart", "Touched").params[0].ty,
        Ty::Instance(Some("BasePart"))
    );
    let server = declared("RemoteEvent", "OnServerEvent");
    assert_eq!(server.context, Some(MemberContext::Server));
    assert_eq!(server.variadic, Some(Ty::Any));
    assert_eq!(
        declared("RemoteEvent", "OnClientEvent").context,
        Some(MemberContext::Client)
    );
    assert!(declared("Instance", "Changed").parameterized);
    assert!(declared("Instance", "AttributeChanged").parameterized);
    assert!(!declared("Humanoid", "Died").parameterized);
}

// ---- A registry the binding has never heard of -------------------------------------------------

static BUMPED: SignalDesc = SignalDesc::new("Bumped")
    .params(&[Param::new("howHard", Ty::Number)])
    .describe("Declared only in this file.");
static OFFSIDE: SignalDesc = SignalDesc::new("Offside")
    .context(MemberContext::Client)
    .describe("Only clients may connect.");

fn bump(call: &mut MethodCall) -> Result<CallOutcome, classes::ApiError> {
    let how_hard = call.number(0)?;
    call.fire("Bumped", vec![ScriptValue::Number(how_hard)])?;
    Ok(ScriptValue::Nil.into())
}

fn bump_wrong(call: &mut MethodCall) -> Result<CallOutcome, classes::ApiError> {
    call.fire("Name", Vec::new())?;
    Ok(ScriptValue::Nil.into())
}

fn wait_bumped(call: &mut MethodCall) -> Result<CallOutcome, classes::ApiError> {
    let key = call.signal("Bumped", 0)?;
    Ok(CallOutcome::Wait(WaitRequest::Signal { key }))
}

fn leak<T: Copy>(lists: &[&[T]]) -> &'static [T] {
    Vec::leak(lists.concat())
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let descs: Vec<&'static ClassDesc> = classes::registry()
        .descs()
        .iter()
        .map(|real| -> &'static ClassDesc {
            let changed = match real.name {
                // Every member id below moves, which the binding must not notice.
                "Instance" => ClassDesc {
                    methods: leak(&[
                        &[MethodDesc::native("WedgeOne", bump_wrong).describe("Shifts ids.")],
                        real.methods,
                    ]),
                    ..**real
                },
                "BasePart" => ClassDesc {
                    props: leak(&[
                        real.props,
                        &[
                            PropDesc::new("Beacon", ValueType::Number, || Value::Number(0.0))
                                .describe("Declared only in this file."),
                        ],
                    ]),
                    methods: leak(&[
                        real.methods,
                        &[
                            MethodDesc::native("Bump", bump)
                                .params(const { &[Param::new("howHard", Ty::Number)] })
                                .describe("Fires Bumped."),
                            MethodDesc::native("WaitBumped", wait_bumped)
                                .returns(Ty::Any)
                                .yields()
                                .describe("Waits for Bumped."),
                        ],
                    ]),
                    signals: leak(&[real.signals, &[&BUMPED, &OFFSIDE]]),
                    ..**real
                },
                "Folder" => ClassDesc {
                    hide: &["AttributeChanged"],
                    ..**real
                },
                _ => return real,
            };
            Box::leak(Box::new(changed))
        })
        .collect();
    classes::resolve(&descs).expect("the fixture classes resolve")
});

static METHODS: LazyLock<HostMethods<HostState>> = LazyLock::new(|| HostMethods::new(&REGISTRY));

struct Fixture {
    vm: RefCell<ScriptVm>,
    world: Rc<RefCell<World>>,
    api: FixtureApi,
    out: Rc<Capture>,
}

impl Fixture {
    fn new(context: Context) -> Fixture {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let out = Rc::new(Capture::default());
        let vm = ScriptVm::with_output(context, out.clone()).expect("VM");
        let api = FixtureApi::install(&vm, world.clone(), out.clone(), &REGISTRY, &METHODS)
            .expect("the API installs over the fixture");
        Fixture {
            vm: RefCell::new(vm),
            world,
            api,
            out,
        }
    }

    fn run(&self, source: &str) -> Vec<String> {
        self.vm
            .borrow_mut()
            .run_script("Test", source)
            .unwrap_or_else(|e| panic!("script failed: {}", e.message));
        self.out.0.take()
    }

    /// What arrives once the signals are delivered, as a step does.
    fn deliver(&self) -> Vec<String> {
        self.api.deliver_signals();
        self.out.0.take()
    }

    fn function(&self, source: &str) -> mlua::Function {
        self.vm
            .borrow()
            .lua()
            .load(source)
            .set_name("=Test")
            .eval()
            .expect("the source compiles to a function")
    }

    fn part(&self) -> InstanceId {
        let mut world = self.world.borrow_mut();
        let workspace = world.root("Workspace");
        let part = world.create("Part", workspace).unwrap();
        world
            .set(part, "Name", Value::String("Bumper".into()))
            .unwrap();
        part
    }
}

fn signal_of(class: &str, signal: &str) -> (classes::ClassId, MemberId) {
    let class = REGISTRY.class_by_name(class).unwrap();
    (class.id, class.member(signal).unwrap())
}

#[test]
fn a_fixture_signal_is_connected_and_fired_with_no_binding_change() {
    let fixture = Fixture::new(Context::Server);
    fixture.part();
    let lines = fixture.run(
        r##"
        local part = workspace.Bumper
        print(typeof(part.Bumped), tostring(part.Bumped))
        part.Bumped:Connect(function(...) print("bumped", select("#", ...), ...) end)
        part.Bumped:Once(function(n) print("once", n) end)
        task.spawn(function() print("waited", part.Bumped:Wait()) end)
        part:Bump(3)
        print("deferred")
        "##,
    );
    assert_eq!(lines, ["RBXScriptSignal RBXScriptSignal", "deferred"]);
    assert_eq!(
        fixture.deliver(),
        ["bumped 1 3", "once 3", "waited 3"],
        "handlers, then waiters"
    );
    assert_eq!(fixture.deliver(), Vec::<String>::new(), "delivered once");
    fixture.run("workspace.Bumper:Bump(4)");
    assert_eq!(fixture.deliver(), ["bumped 1 4"]);
}

#[test]
fn a_fixture_signal_is_inherited_and_invisible_on_a_sibling() {
    let fixture = Fixture::new(Context::Server);
    let lines = fixture.run(
        r##"
        for _, class in { "Part", "SpawnLocation", "Checkpoint" } do
            local object = Instance.new(class, workspace)
            object.Bumped:Connect(function(n) print(class, n) end)
            object:Bump(1)
        end
        print(pcall(function() return Instance.new("Model").Bumped end))
        print(pcall(function() return workspace.Bumped end))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false Test:7: Bumped is not a valid member of Model",
            "false Test:8: Bumped is not a valid member of Workspace",
        ]
    );
    assert_eq!(
        fixture.deliver(),
        ["Part 1", "SpawnLocation 1", "Checkpoint 1"]
    );
}

#[test]
fn a_fire_nobody_listens_to_queues_nothing() {
    let fixture = Fixture::new(Context::Server);
    let part = fixture.part();
    fixture.run(
        r##"
        local part = workspace.Bumper
        part:Bump(1)
        local connection = part.Bumped:Connect(function() end)
        part:Bump(2)
        connection:Disconnect()
        "##,
    );
    let (_, bumped) = signal_of("Part", "Bumped");
    let key = SignalKey {
        instance: part,
        signal: bumped,
        arg: 0,
    };
    assert_eq!(fixture.world.borrow_mut().signals_mut().take().len(), 1);
    // The world learns of the last Disconnect at the end of the step.
    assert!(fixture.api.watched(&key));
    fixture.api.prune();
    assert!(!fixture.api.watched(&key));
    fixture.run("workspace.Bumper:Bump(3)");
    assert!(fixture.world.borrow_mut().signals_mut().take().is_empty());
}

#[test]
fn firing_a_member_that_is_no_signal_is_the_registry_error() {
    let fixture = Fixture::new(Context::Server);
    let lines = fixture.run(r##"print(pcall(function() Instance.new("Part"):WedgeOne() end))"##);
    assert_eq!(lines, ["false Test:1: Name is not a valid member of Part"]);
}

#[test]
fn a_fixture_method_can_wait_for_a_signal() {
    let fixture = Fixture::new(Context::Server);
    fixture.part();
    let lines = fixture.run(
        r##"
        task.spawn(function() print("resumed", workspace.Bumper:WaitBumped()) end)
        print("parked")
        "##,
    );
    assert_eq!(lines, ["parked"]);
    assert_eq!(fixture.deliver(), Vec::<String>::new());
    fixture.run("workspace.Bumper:Bump(9)");
    assert_eq!(fixture.deliver(), ["resumed 9"]);
    fixture.run("workspace.Bumper:Bump(10)");
    assert_eq!(
        fixture.deliver(),
        Vec::<String>::new(),
        "one fire resumes it"
    );
}

#[test]
fn connect_signal_and_disconnect_work_through_the_host_interface() {
    let fixture = Fixture::new(Context::Server);
    let part = fixture.part();
    let handler = fixture.function(r##"return function(n) print("handler", n) end"##);
    let (class, bumped) = signal_of("BasePart", "Bumped");
    let connection = fixture
        .api
        .connect_signal(part, class, bumped, 0, handler)
        .expect("connects");
    // The reference ended with the call; the connection keeps the function.
    assert_eq!(fixture.api.functions_in_use(), 0);
    fixture.run("workspace.Bumper:Bump(1)");
    assert_eq!(fixture.deliver(), ["handler 1"]);
    fixture.api.disconnect(connection);
    fixture.run("workspace.Bumper:Bump(2)");
    assert_eq!(fixture.deliver(), Vec::<String>::new());
    // A connection already gone is fine.
    fixture.api.disconnect(connection);
}

#[test]
fn connect_signal_checks_every_rule() {
    let fixture = Fixture::new(Context::Server);
    let part = fixture.part();
    let (base_part, bumped) = signal_of("BasePart", "Bumped");
    let (_, name) = signal_of("Instance", "Name");
    let (_, offside) = signal_of("BasePart", "Offside");
    let (_, changed) = signal_of("Instance", "Changed");
    let handler = fixture.function("return print");
    let connect = |h, class, m, arg| {
        fixture
            .api
            .connect_signal(h, class, m, arg, handler.clone())
            .map(|_| ())
            .map_err(|error| error.to_string())
    };
    // A member that is no signal, an id no member has, a class the instance is not.
    assert_eq!(
        connect(part, base_part, name, 0).unwrap_err(),
        "Name is not a valid member of Part"
    );
    assert!(connect(part, base_part, MemberId(60_000), 0).is_err());
    let model = fixture.world.borrow_mut().create("Model", None).unwrap();
    assert_eq!(
        connect(model, base_part, bumped, 0).unwrap_err(),
        "Bumped is not a valid member of Model"
    );
    // The signal's context, below every language.
    assert_eq!(
        connect(part, base_part, offside, 0).unwrap_err(),
        "Offside can only be used from the client"
    );
    // A key only a parameterized signal has.
    assert_eq!(
        connect(part, base_part, bumped, 7).unwrap_err(),
        "Bumped takes no argument"
    );
    assert!(connect(part, base_part, changed, 7).is_ok());
    // A reference that names nothing.
    assert_eq!(
        fixture
            .api
            .connect_signal_ref(part, base_part, bumped, 0, FunctionRef(999))
            .unwrap_err()
            .to_string(),
        "that function reference has ended"
    );
    // A destroyed handle.
    fixture.world.borrow_mut().destroy(part);
    let error = fixture
        .api
        .connect_signal(part, base_part, bumped, 0, handler.clone())
        .unwrap_err();
    assert!(matches!(error.kind, ApiErrorKind::Destroyed { .. }));
    assert_eq!(fixture.api.functions_in_use(), 0);
}

#[test]
fn a_signal_of_the_other_side_is_refused_when_read() {
    let fixture = Fixture::new(Context::Server);
    fixture.part();
    let lines = fixture.run(
        r##"
        print(pcall(function() return workspace.Bumper.Offside end))
        print(pcall(function() return Instance.new("RemoteEvent").OnClientEvent end))
        print(tostring(Instance.new("RemoteEvent").OnServerEvent))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false Test:2: Offside can only be used from the client",
            "false Test:3: OnClientEvent can only be used from the client",
            "RBXScriptSignal",
        ]
    );
    let client = Fixture::new(Context::Client);
    let lines = client.run(
        r##"
        print(pcall(function() return Instance.new("RemoteEvent").OnServerEvent end))
        print(tostring(Instance.new("RemoteEvent").OnClientEvent))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false Test:2: OnServerEvent can only be used from the server",
            "RBXScriptSignal",
        ]
    );
}

#[test]
fn a_hidden_signal_is_not_a_member() {
    let fixture = Fixture::new(Context::Server);
    let lines = fixture.run(
        r##"
        print(pcall(function() return Instance.new("Folder").AttributeChanged end))
        print(tostring(Instance.new("Model").AttributeChanged))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false Test:2: AttributeChanged is not a valid member of Folder",
            "RBXScriptSignal",
        ]
    );
}

/// A value of the property's type that differs from `current`, if one can be made for it.
fn another_value(world: &mut World, desc: &PropDesc, current: &Value) -> Option<Value> {
    Some(match current {
        Value::Bool(b) => Value::Bool(!b),
        Value::Number(n) => match desc.range {
            Some(range) if *n >= range.max => Value::Number(range.min),
            Some(range) => Value::Number(range.max.min(n + 1.0)),
            None => Value::Number(n + 1.0),
        },
        Value::String(s) => match desc.allowed {
            Some(allowed) => {
                Value::String((*allowed.iter().find(|item| **item != s.as_str())?).to_string())
            }
            None => Value::String(format!("{s}x")),
        },
        Value::Vector3(v) => Value::Vector3(Vec3::new(v.x + 1.0, v.y + 1.0, v.z + 1.0)),
        Value::Color3(c) => Value::Color3(Color3::new(if c.r > 0.5 { 0.0 } else { 1.0 }, c.g, c.b)),
        Value::UDim2(_) => Value::UDim2(UDim2::new(0.25, 1.0, 0.75, 2.0)),
        Value::CFrame(_) => {
            Value::CFrame(Box::new(CFrame::from_position(Vec3::new(7.0, 8.0, 9.0))))
        }
        Value::Ref(_) => Value::Ref(Some(world.create("Folder", None).ok()?)),
        Value::Nil => return None,
    })
}

#[test]
fn get_property_changed_signal_works_for_every_property_with_no_extra_code() {
    let fixture = Fixture::new(Context::Server);
    let connect = fixture.function(
        r##"
        return function(object, label, name)
            object:GetPropertyChangedSignal(name):Connect(function(...)
                print(label, select("#", ...))
            end)
        end
        "##,
    );
    let mut fired = 0;
    let mut unchanged = Vec::new();
    for class in REGISTRY.classes() {
        if !class.desc.is_creatable() {
            continue;
        }
        for member in &class.members {
            let MemberKind::Property { desc, .. } = member.kind else {
                continue;
            };
            // Declared only here: the world cannot store it, so the next test covers it.
            if member.hidden || desc.name == "Beacon" {
                continue;
            }
            let id = fixture
                .world
                .borrow_mut()
                .create(class.name(), None)
                .unwrap();
            let label = format!("{}.{}", class.name(), desc.name);
            connect
                .call::<()>((fixture.api.wrap(id).unwrap(), label.as_str(), desc.name))
                .unwrap();
            let current = fixture.world.borrow().get(id, desc.name).unwrap();
            let changed = {
                let mut world = fixture.world.borrow_mut();
                another_value(&mut world, desc, &current)
                    .and_then(|value| world.set(id, desc.name, value).ok())
                    .is_some()
                    && world.get(id, desc.name).unwrap() != current
            };
            let lines = fixture.deliver();
            if changed {
                assert_eq!(lines, [format!("{label} 0")]);
                fired += 1;
            } else {
                assert_eq!(lines, Vec::<String>::new(), "{label}");
                unchanged.push(label);
            }
        }
    }
    assert!(
        unchanged.is_empty(),
        "{fired} fired, no write changed {unchanged:?}"
    );
}

#[test]
fn a_property_only_a_fixture_declares_gets_its_signal_with_no_extra_code() {
    let fixture = Fixture::new(Context::Server);
    let part = fixture.part();
    let lines = fixture.run(
        r##"
        local part = workspace.Bumper
        part:GetPropertyChangedSignal("Beacon"):Connect(function() print("beacon") end)
        part:GetPropertyChangedSignal("Name"):Connect(function() print("name") end)
        print(pcall(function() return workspace:GetPropertyChangedSignal("Beacon") end))
        "##,
    );
    assert_eq!(
        lines,
        ["false Test:5: Beacon is not a valid property name."]
    );
    let (_, changed) = signal_of("Part", "Changed");
    let (_, beacon) = signal_of("Part", "Beacon");
    fixture.world.borrow_mut().fire_key(
        SignalKey {
            instance: part,
            signal: changed,
            arg: classes::property_signal_arg(beacon),
        },
        Vec::new(),
    );
    assert_eq!(fixture.deliver(), ["beacon"]);
}

#[test]
fn a_method_or_signal_cannot_be_written_and_a_signal_is_not_a_function() {
    let fixture = Fixture::new(Context::Server);
    let lines = fixture.run(
        r##"
        local part = Instance.new("Part")
        print(pcall(function() part.Bumped = 1 end))
        print(pcall(function() part.Changed = 1 end))
        print(pcall(function() part.GetPropertyChangedSignal = 1 end))
        print(pcall(function() return part:Bumped() end))
        print(pcall(function() return part:GetPropertyChangedSignal() end))
        print(pcall(function() return part:GetPropertyChangedSignal(5) end))
        print(pcall(function() return part:GetPropertyChangedSignal("Name", 1) end))
        "##,
    );
    assert_eq!(
        lines,
        [
            "false Test:3: Bumped is read-only",
            "false Test:4: Changed is read-only",
            "false Test:5: GetPropertyChangedSignal is read-only",
            "false Test:6: attempt to call a RBXScriptSignal value",
            "false Test:7: Part:GetPropertyChangedSignal: argument #1 expected string, got nothing",
            "false Test:8: Part:GetPropertyChangedSignal: argument #1 expected string, got number",
            "false Test:9: Part:GetPropertyChangedSignal: expected at most 1 argument, got 2",
        ]
    );
}

#[test]
fn a_stopped_host_tells_the_world_it_listens_to_nothing() {
    let world = Rc::new(RefCell::new(World::new_with_services()));
    let out = Rc::new(Capture::default());
    let mut host = ScriptHost::new_server(world.clone(), out.clone());
    let (part, key) = {
        let mut world = world.borrow_mut();
        let workspace = world.root("Workspace");
        let part = world.create("Part", workspace).unwrap();
        world
            .set(part, "Name", Value::String("Pad".into()))
            .unwrap();
        let services = world.root("ServerScriptService").unwrap();
        let script = world.create("Script", Some(services)).unwrap();
        world
            .set(
                script,
                "Source",
                Value::String("workspace.Pad.Touched:Connect(print)".into()),
            )
            .unwrap();
        let class = classes::registry().class_by_name("Part").unwrap();
        let key = SignalKey {
            instance: part,
            signal: class.member("Touched").unwrap(),
            arg: 0,
        };
        (part, key)
    };
    host.start().unwrap();
    assert!(world.borrow().signal_is_watched(&key));
    // Core code firing a real signal reaches scripts through the next step.
    world.borrow_mut().fire(
        part,
        key.signal,
        vec![ScriptValue::String("from core".into())],
    );
    host.step(1.0, 0.1);
    assert_eq!(out.0.take(), ["from core"]);
    host.stop();
    assert!(!world.borrow().signal_is_watched(&key));
}

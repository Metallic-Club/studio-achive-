//! A fixture registry: every real class, plus members the real classes do not have. The binding
//! gets it with no change, which is the point. `World` only creates real classes, so the fixture
//! keeps their names.
#![allow(dead_code)]

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::LazyLock;

use metallic_core::classes::{
    self, ApiError, CallOutcome, CallbackDesc, ClassDesc, ClassFlags, DeclId, HostMethods,
    MemberKind, MethodCall, MethodDesc, Param, Registry, ScriptValue, Side, SignalDesc, SignalKey,
    Ty, WaitRequest,
};
use metallic_core::{Value, World};
use metallic_script::{Context, FixtureApi, HostCall, HostState, OutputSink, ScriptVm};

fn greet(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(format!("hello from a {}", call.class_name())
        .as_str()
        .into())
}

fn folder_greet(_: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok("a Folder's own hello".into())
}

fn add_one(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok((call.number(0)? + 1.0).into())
}

fn nothing(_: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(ScriptValue::Nil.into())
}

fn echo(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(ScriptValue::Tuple(call.args.to_vec()).into())
}

fn rename(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    let name = call.string(0)?.to_owned();
    call.world.set(call.this, "Name", Value::String(name))?;
    Ok(ScriptValue::Nil.into())
}

fn part_only(_: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok("only parts have this".into())
}

fn takes_part(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(call.instance(0)?.into())
}

fn fails(_: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Err(ApiError::message("a method's own text"))
}

fn wait_kid(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    let name = call.string(0)?;
    if let Some(found) = call.world.find_first_child(call.this, name) {
        return Ok(found.into());
    }
    Ok(CallOutcome::Wait(WaitRequest::Child {
        parent: call.this,
        name: name.into(),
        timeout: call.opt_number(1)?,
    }))
}

fn nap(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(CallOutcome::Wait(WaitRequest::Seconds(call.number(0)?)))
}

fn wait_signal(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok(CallOutcome::Wait(WaitRequest::Signal {
        key: SignalKey {
            instance: call.this,
            signal: classes::MemberId(0),
            arg: 0,
        },
    }))
}

fn secret(_: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    Ok("not for folders".into())
}

static INSTANCE_METHODS: [MethodDesc; 11] = [
    MethodDesc::native("Greet", greet).returns(Ty::String),
    MethodDesc::native("AddOne", add_one)
        .params(&[Param::new("x", Ty::Number)])
        .returns(Ty::Number),
    MethodDesc::native("Nothing", nothing),
    MethodDesc::native("Echo", echo)
        .variadic(Ty::Any)
        .returns(Ty::Any),
    MethodDesc::native("Rename", rename).params(&[Param::new("name", Ty::String)]),
    MethodDesc::native("TakesPart", takes_part)
        .params(&[Param::new("part", Ty::Instance(Some("Part")))])
        .returns(Ty::Instance(None)),
    MethodDesc::native("Fails", fails),
    MethodDesc::native("WaitKid", wait_kid)
        .params(&[
            Param::new("name", Ty::String),
            Param::new("timeout", Ty::Number).optional(),
        ])
        .returns(Ty::Any)
        .yields(),
    MethodDesc::native("Nap", nap)
        .params(&[Param::new("seconds", Ty::Number)])
        .yields(),
    MethodDesc::native("WaitSignal", wait_signal).yields(),
    MethodDesc::native("WaitsUndeclared", nap)
        .params(&[Param::new("seconds", Ty::Number)])
        .returns(Ty::Any),
];

static INSTANCE_MORE: [MethodDesc; 3] = [
    MethodDesc::native("Secret", secret).returns(Ty::String),
    MethodDesc::host("WhichSide").returns(Ty::String),
    MethodDesc::host("NoBody"),
];

static PART_METHODS: [MethodDesc; 1] =
    [MethodDesc::native("PartOnly", part_only).returns(Ty::String)];
static FOLDER_METHODS: [MethodDesc; 1] = [MethodDesc::native("Greet", folder_greet)
    .returns(Ty::String)
    .overrides()];
static PINGED: SignalDesc = SignalDesc::new("Pinged");
static PART_SIGNALS: [&SignalDesc; 1] = [&PINGED];
static PART_CALLBACKS: [CallbackDesc; 1] = [CallbackDesc::new("OnPing")];

fn which_side(call: &mut HostCall<'_>) -> Result<CallOutcome, ApiError> {
    Ok(match call.call.context.side {
        Side::Server => "the server".into(),
        Side::Client => "a client".into(),
    })
}

fn fixture_pivot(_: &mut HostCall<'_>) -> Result<CallOutcome, ApiError> {
    Ok(ScriptValue::Nil.into())
}

static ALL_METHODS: LazyLock<Vec<MethodDesc>> = LazyLock::new(|| {
    INSTANCE_METHODS
        .iter()
        .chain(&INSTANCE_MORE)
        .copied()
        .collect()
});

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let descs: Vec<&'static ClassDesc> = classes::registry()
        .descs()
        .iter()
        .map(|real| -> &'static ClassDesc {
            let changed = match real.name {
                "Instance" => ClassDesc {
                    methods: ALL_METHODS.as_slice(),
                    ..**real
                },
                "Part" => ClassDesc {
                    methods: &PART_METHODS,
                    signals: &PART_SIGNALS,
                    callbacks: &PART_CALLBACKS,
                    ..**real
                },
                "Folder" => ClassDesc {
                    methods: &FOLDER_METHODS,
                    hide: &["Secret"],
                    ..**real
                },
                // Creatable in the real registry: the binding must read the flag from this one.
                "Atmosphere" => ClassDesc {
                    flags: ClassFlags::NONE,
                    ..**real
                },
                _ => return real,
            };
            Box::leak(Box::new(changed))
        })
        .collect();
    classes::resolve(&descs).expect("the fixture classes resolve")
});

static METHODS: LazyLock<HostMethods<HostState>> = LazyLock::new(|| {
    let mut methods = HostMethods::new(&REGISTRY);
    methods
        .implement("Instance", "WhichSide", which_side)
        .unwrap();
    // A body arriving is all a migration needs: the bridge stops being asked.
    methods
        .implement("PVInstance", "GetPivot", fixture_pivot)
        .unwrap();
    methods
});

/// The fixture registry.
pub fn registry() -> &'static Registry {
    &REGISTRY
}

/// The declaration of the method, signal or callback `member` of `class`.
pub fn decl_of(class: &str, member: &str) -> DeclId {
    let class = REGISTRY.class_by_name(class).expect("fixture class");
    let id = class.member(member).expect("fixture member");
    match class.members[usize::from(id.0)].kind {
        MemberKind::Method { decl, .. }
        | MemberKind::Signal { decl, .. }
        | MemberKind::Callback { decl, .. } => decl,
        MemberKind::Property { .. } => panic!("{member} is a property"),
    }
}

/// Keeps what scripts print.
#[derive(Default)]
pub struct Capture(RefCell<Vec<String>>);

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

/// One VM over the fixture registry.
pub struct Fixture {
    pub vm: ScriptVm,
    pub world: Rc<RefCell<World>>,
    pub api: FixtureApi,
    pub out: Rc<Capture>,
}

impl Fixture {
    pub fn new(context: Context) -> Fixture {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let out = Rc::new(Capture::default());
        metallic_script::use_test_budgets_on_this_thread();
        let vm = ScriptVm::with_output(context, out.clone()).expect("VM");
        let api = FixtureApi::install(&vm, world.clone(), out.clone(), &REGISTRY, &METHODS)
            .expect("the API installs over the fixture");
        Fixture {
            vm,
            world,
            api,
            out,
        }
    }

    pub fn server() -> Fixture {
        Fixture::new(Context::Server)
    }

    /// Runs a script; what it printed comes back.
    pub fn run(&mut self, source: &str) -> Vec<String> {
        self.vm
            .run_script("Test", source)
            .unwrap_or_else(|e| panic!("script failed: {}", e.message));
        std::mem::take(&mut *self.out.0.borrow_mut())
    }

    /// What scripts printed since the last call.
    pub fn take_output(&self) -> Vec<String> {
        std::mem::take(&mut *self.out.0.borrow_mut())
    }

    /// A Luau function compiled from `source`, which must return one.
    pub fn function(&self, source: &str) -> mlua::Function {
        self.vm
            .lua()
            .load(source)
            .set_name("=Test")
            .eval()
            .expect("the source compiles to a function")
    }
}

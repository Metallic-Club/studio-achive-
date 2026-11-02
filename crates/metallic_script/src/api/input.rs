//! Input in Luau (docs/design/input-actions.md section 5): the `Enum` table, `InputAction`
//! members, `ContextActionService` and the last-input members of `UserInputService`.
//!
//! The input runtime lives in a crate above this one, so the host (the owner of a
//! `ScriptHost`) pushes what happened: [`InputActionEvent`]s, per-frame action states, the last
//! input type and the device flags. Events wait in a queue and the client VM delivers at most
//! [`MAX_ACTION_EVENTS_PER_STEP`] of them per `step`, oldest event time first.
//!
//! `BindAction` creates a hidden `InputContext` (one per priority) under the local player, so
//! the runtime sees a bound action like any other action. Each handler is wrapped in a small
//! Luau function that tells the host whether it returned `Sink` or `Pass` ([`BoundResult`]).

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use metallic_core::classes::{INPUT_ACTION_TYPES, INPUT_TRIGGERS, TOUCH_SLOTS};
use metallic_core::{InstanceId, Value as CoreValue, Vec3, World};
use mlua::{
    AnyUserData, Function, Lua, MetaMethod, Table, UserData, UserDataFields, UserDataMethods,
    UserDataRef, Value, Variadic,
};

use super::datatypes::{LuaUDim2, LuaVector2, LuaVector3, udim2};
use super::errors;
use super::signals::{EventKind, LuaSignal};
use super::{ApiState, lua_error};
use crate::vm::Context;

/// Most action events one `step` delivers; the rest wait for the next one.
pub const MAX_ACTION_EVENTS_PER_STEP: usize = 16;
/// Most action events kept waiting; beyond this the oldest are dropped.
const MAX_QUEUED_ACTION_EVENTS: usize = 1024;
/// Most actions one script may have bound at a time.
pub const MAX_BOUND_ACTIONS_PER_SCRIPT: usize = 256;
/// Most `Sink`/`Pass` results kept for the host.
const MAX_BOUND_RESULTS: usize = 1024;

const LOCAL_ONLY: &str = "InputAction can only be used from a LocalScript";
const CAS_LOCAL_ONLY: &str = "ContextActionService can only be used from a LocalScript";

/// Names of `Enum.KeyCode`: `Unknown`, then the controls of the input runtime (a test in
/// `metallic_runtime` keeps the two lists equal).
pub const KEY_CODES: &[&str] = &[
    "Unknown",
    "A",
    "B",
    "C",
    "D",
    "E",
    "F",
    "G",
    "H",
    "I",
    "J",
    "K",
    "L",
    "M",
    "N",
    "O",
    "P",
    "Q",
    "R",
    "S",
    "T",
    "U",
    "V",
    "W",
    "X",
    "Y",
    "Z",
    "Zero",
    "One",
    "Two",
    "Three",
    "Four",
    "Five",
    "Six",
    "Seven",
    "Eight",
    "Nine",
    "F1",
    "F2",
    "F3",
    "F4",
    "F5",
    "F6",
    "F7",
    "F8",
    "F9",
    "F10",
    "F11",
    "F12",
    "Space",
    "Return",
    "Tab",
    "Backspace",
    "Escape",
    "LeftShift",
    "RightShift",
    "LeftControl",
    "RightControl",
    "LeftAlt",
    "RightAlt",
    "Up",
    "Down",
    "Left",
    "Right",
    "Home",
    "End",
    "PageUp",
    "PageDown",
    "Insert",
    "Delete",
    "Comma",
    "Period",
    "Slash",
    "Semicolon",
    "Quote",
    "LeftBracket",
    "RightBracket",
    "BackSlash",
    "Minus",
    "Equals",
    "Backquote",
    "MouseButton1",
    "MouseButton2",
    "MouseButton3",
    "MouseWheel",
    "MouseDelta",
    "MousePosition",
    "ButtonA",
    "ButtonB",
    "ButtonX",
    "ButtonY",
    "ButtonL1",
    "ButtonR1",
    "ButtonL2",
    "ButtonR2",
    "ButtonL3",
    "ButtonR3",
    "ButtonSelect",
    "ButtonStart",
    "DPadUp",
    "DPadDown",
    "DPadLeft",
    "DPadRight",
    "Thumbstick1",
    "Thumbstick2",
    "TouchStick",
    "TouchLook",
    "TouchButton",
    "TouchButton1",
    "TouchButton2",
    "TouchButton3",
    "TouchButton4",
    "TouchButton5",
    "TouchButton6",
    "TouchButton7",
];

/// First and last keyboard key of [`KEY_CODES`] (what `UserInputType.Keyboard` binds).
const KEYBOARD_RANGE: (&str, &str) = ("A", "Backquote");
/// First and last gamepad button of [`KEY_CODES`] bound by `UserInputType.Gamepad1` to `4`.
const GAMEPAD_RANGE: (&str, &str) = ("ButtonA", "DPadRight");

/// Names of `Enum.UserInputType`.
pub const USER_INPUT_TYPES: &[&str] = &[
    "MouseButton1",
    "MouseButton2",
    "MouseButton3",
    "MouseWheel",
    "MouseMovement",
    "Touch",
    "Keyboard",
    "Focus",
    "Gamepad1",
    "Gamepad2",
    "Gamepad3",
    "Gamepad4",
    "TextInput",
    "None",
];

const USER_INPUT_STATES: &[&str] = &["Begin", "Change", "End", "Cancel", "None"];
const CONTEXT_ACTION_RESULTS: &[&str] = &["Sink", "Pass"];
const PREFERRED_INPUTS: &[&str] = &["KeyboardAndMouse", "Gamepad", "Touch"];
/// `(name, value)`: Metallic's numbers (spec 5.2).
const CONTEXT_ACTION_PRIORITIES: &[(&str, i64)] =
    &[("Low", 1000), ("Medium", 2000), ("High", 3000)];

/// The value of an action as the host reports it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputValue {
    /// A Bool action.
    Bool(bool),
    /// A Direction1D action.
    Axis1(f32),
    /// A Direction2D or ViewportPosition action (a `Vector2` in Luau).
    Axis2([f32; 2]),
    /// A Direction3D action (a `Vector3` in Luau).
    Axis3([f32; 3]),
}

/// What happened to an action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEventKind {
    /// The raw press edge.
    Pressed,
    /// The raw release edge.
    Released,
    /// The action's trigger fired.
    Triggered,
    /// The value changed.
    StateChanged,
}

/// One event of one `InputAction` instance, for the client scripts.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputActionEvent {
    /// The `InputAction` instance.
    pub action: InstanceId,
    /// What happened.
    pub kind: InputEventKind,
    /// The action's value right after.
    pub value: InputValue,
    /// Event time in seconds; events are delivered oldest first.
    pub time: f64,
}

/// The three kinds of device that can be the preferred input (`Enum.PreferredInput`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputDevice {
    /// Keyboard and mouse.
    KeyboardMouse,
    /// Any gamepad.
    Gamepad,
    /// The touch screen.
    Touch,
}

impl InputDevice {
    fn enum_name(self) -> &'static str {
        match self {
            InputDevice::KeyboardMouse => "KeyboardAndMouse",
            InputDevice::Gamepad => "Gamepad",
            InputDevice::Touch => "Touch",
        }
    }
}

/// What a `ContextActionService` handler answered for one event (spec 3.7).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BoundResult {
    /// The hidden `InputAction` of the bound action.
    pub action: InstanceId,
    /// Host time of the step that ran the handler.
    pub time: f64,
    /// True for `Sink` or no return value, false for `Pass`. A handler that errors or yields
    /// before returning records nothing.
    pub sink: bool,
}

/// One on-screen touch button as the host's touch overlay placed it, for
/// `ContextActionService:GetButton` (docs/design/input-actions.md section 7).
#[derive(Debug, Clone, PartialEq)]
pub struct TouchButtonInfo {
    /// The `InputAction` the button presses.
    pub action: InstanceId,
    /// Top-left corner in dp from the top-left of the screen.
    pub position: [f32; 2],
    /// Width and height in dp.
    pub size: [f32; 2],
    /// The text under the button.
    pub title: String,
    /// The button's image (`TouchImage`); empty when it draws an icon.
    pub image: String,
    /// The overlay is showing (the last input was touch).
    pub visible: bool,
}

/// What the scripts may read about devices.
pub(crate) struct InputInfo {
    pub(crate) last: &'static str,
    pub(crate) preferred: InputDevice,
    pub(crate) touch: bool,
    pub(crate) keyboard: bool,
    pub(crate) gamepad: bool,
}

impl Default for InputInfo {
    fn default() -> Self {
        InputInfo {
            last: "None",
            preferred: InputDevice::KeyboardMouse,
            touch: false,
            keyboard: true,
            gamepad: false,
        }
    }
}

/// Input state shared by the two VMs of a host.
#[derive(Default)]
pub(crate) struct InputShared {
    pub(crate) events: RefCell<Vec<InputActionEvent>>,
    pub(crate) dropped: Cell<u64>,
    pub(crate) states: RefCell<HashMap<InstanceId, (InputValue, f32)>>,
    pub(crate) info: RefCell<InputInfo>,
    /// `LastInputTypeChanged` waiting for the next step.
    pub(crate) type_changes: RefCell<Vec<&'static str>>,
    pub(crate) results: RefCell<Vec<BoundResult>>,
    /// The touch overlay's buttons, as the host last reported them.
    pub(crate) touch_buttons: RefCell<Vec<TouchButtonInfo>>,
}

impl InputShared {
    pub(crate) fn clear(&self) {
        self.touch_buttons.borrow_mut().clear();
        self.events.borrow_mut().clear();
        self.states.borrow_mut().clear();
        self.type_changes.borrow_mut().clear();
        self.results.borrow_mut().clear();
        *self.info.borrow_mut() = InputInfo::default();
    }

    pub(crate) fn push_event(&self, event: InputActionEvent) {
        let mut events = self.events.borrow_mut();
        if events.len() >= MAX_QUEUED_ACTION_EVENTS {
            events.remove(0);
            self.dropped.set(self.dropped.get() + 1);
        }
        events.push(event);
    }

    pub(crate) fn set_last_input_type(&self, name: &str) -> bool {
        let Some(&name) = USER_INPUT_TYPES.iter().find(|n| **n == name) else {
            return false;
        };
        let mut info = self.info.borrow_mut();
        if info.last != name {
            info.last = name;
            self.type_changes.borrow_mut().push(name);
        }
        true
    }

    pub(crate) fn set_preferred(&self, device: InputDevice) {
        self.info.borrow_mut().preferred = device;
    }

    pub(crate) fn set_devices(&self, touch: bool, keyboard: bool, gamepad: bool) {
        let mut info = self.info.borrow_mut();
        info.touch = touch;
        info.keyboard = keyboard;
        info.gamepad = gamepad;
    }
}

// ---------------------------------------------------------------------------------------------
// Enums

/// A value of an `Enum`.
pub(crate) struct LuaEnumItem {
    enum_name: &'static str,
    name: &'static str,
    value: i64,
}

impl UserData for LuaEnumItem {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "EnumItem");
        fields.add_field_method_get("Name", |_, this| Ok(this.name));
        fields.add_field_method_get("Value", |_, this| Ok(this.value));
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_function(MetaMethod::Eq, |_, (a, b): (Value, Value)| {
            Ok(match (enum_item(&a), enum_item(&b)) {
                (Some(a), Some(b)) => a.0 == b.0 && a.1 == b.1,
                _ => false,
            })
        });
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| {
            Ok(format!("Enum.{}.{}", this.enum_name, this.name))
        });
    }
}

/// `(enum, item, value)` of an `EnumItem` value.
pub(crate) fn enum_item(v: &Value) -> Option<(&'static str, &'static str, i64)> {
    match v {
        Value::UserData(ud) => ud
            .borrow::<LuaEnumItem>()
            .ok()
            .map(|i| (i.enum_name, i.name, i.value)),
        _ => None,
    }
}

/// The `Enum` table and Rust-side access to its items.
/// `Enum.RotationOrder` item names (used by `CFrame.fromEulerAngles`).
static ROTATION_ORDERS: [&str; 6] = {
    let all = metallic_core::RotationOrder::ALL;
    let mut names = [""; 6];
    let mut i = 0;
    while i < 6 {
        names[i] = all[i].name();
        i += 1;
    }
    names
};

/// `Enum.NormalId` item names (used by `BasePart:Resize`).
static NORMAL_IDS: [&str; 6] = {
    let all = metallic_core::transform::NormalId::ALL;
    let mut names = [""; 6];
    let mut i = 0;
    while i < 6 {
        names[i] = all[i].name();
        i += 1;
    }
    names
};

pub(crate) struct Enums {
    pub(crate) table: Table,
    items: HashMap<(&'static str, &'static str), AnyUserData>,
}

impl Enums {
    pub(crate) fn install(lua: &Lua) -> mlua::Result<Enums> {
        let named = |names: &'static [&'static str]| -> Vec<(&'static str, i64)> {
            names
                .iter()
                .enumerate()
                .map(|(i, n)| (*n, i as i64))
                .collect()
        };
        let all: Vec<(&'static str, Vec<(&'static str, i64)>)> = vec![
            ("KeyCode", named(KEY_CODES)),
            ("UserInputType", named(USER_INPUT_TYPES)),
            ("UserInputState", named(USER_INPUT_STATES)),
            ("InputActionType", named(INPUT_ACTION_TYPES)),
            ("InputTrigger", named(INPUT_TRIGGERS)),
            ("TouchSlot", named(TOUCH_SLOTS)),
            ("ContextActionResult", named(CONTEXT_ACTION_RESULTS)),
            ("PreferredInput", named(PREFERRED_INPUTS)),
            ("ContextActionPriority", CONTEXT_ACTION_PRIORITIES.to_vec()),
            ("RotationOrder", named(&ROTATION_ORDERS)),
            ("NormalId", named(&NORMAL_IDS)),
            ("CameraType", named(metallic_core::classes::CAMERA_TYPES)),
        ];
        let table = lua.create_table()?;
        let mut items = HashMap::new();
        for (enum_name, members) in all {
            let sub = lua.create_table()?;
            for (name, value) in members {
                let ud = lua.create_userdata(LuaEnumItem {
                    enum_name,
                    name,
                    value,
                })?;
                sub.raw_set(name, ud.clone())?;
                items.insert((enum_name, name), ud);
            }
            strict(lua, &sub, format!("Enum.{enum_name}"))?;
            table.raw_set(enum_name, sub)?;
        }
        strict(lua, &table, "Enum".to_string())?;
        Ok(Enums { table, items })
    }

    fn item(&self, enum_name: &str, name: &str) -> Value {
        self.items
            .iter()
            .find(|((e, n), _)| *e == enum_name && *n == name)
            .map_or(Value::Nil, |(_, ud)| Value::UserData(ud.clone()))
    }
}

/// Makes `table` read-only and turns a missing member into an error.
fn strict(lua: &Lua, table: &Table, label: String) -> mlua::Result<()> {
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
    table.set_metatable(Some(meta))?;
    table.set_readonly(true);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Action state and event delivery

fn zero_for(ty: &str) -> InputValue {
    match ty {
        "Direction1D" => InputValue::Axis1(0.0),
        "Direction2D" | "ViewportPosition" => InputValue::Axis2([0.0; 2]),
        "Direction3D" => InputValue::Axis3([0.0; 3]),
        _ => InputValue::Bool(false),
    }
}

fn value_to_lua(lua: &Lua, value: InputValue) -> mlua::Result<Value> {
    Ok(match value {
        InputValue::Bool(b) => Value::Boolean(b),
        InputValue::Axis1(v) => Value::Number(f64::from(v)),
        InputValue::Axis2([x, y]) => {
            Value::UserData(lua.create_userdata(LuaVector2([f64::from(x), f64::from(y)]))?)
        }
        InputValue::Axis3([x, y, z]) => Value::UserData(lua.create_userdata(LuaVector3(
            Vec3::new(f64::from(x), f64::from(y), f64::from(z)),
        ))?),
    })
}

/// True when `id` is an `InputAction` that is enabled inside enabled contexts.
fn action_active(world: &World, id: InstanceId) -> bool {
    if !world.is_alive(id) || world.class_name(id) != Some("InputAction") {
        return false;
    }
    let mut cur = Some(id);
    while let Some(c) = cur {
        if matches!(world.class_name(c), Some("InputAction" | "InputContext"))
            && matches!(world.get(c, "Enabled"), Ok(CoreValue::Bool(false)))
        {
            return false;
        }
        cur = world.parent(c);
    }
    true
}

/// A call to a bound handler: its name, the wrapped function, liveness flag and owner.
type HandlerCall = (String, Function, Rc<Cell<bool>>, u64);

impl ApiState {
    fn action_zero(&self, id: InstanceId) -> InputValue {
        match self.world().ok().and_then(|w| w.get(id, "Type").ok()) {
            Some(CoreValue::String(ty)) => zero_for(&ty),
            _ => InputValue::Bool(false),
        }
    }

    fn require_client(&self) -> mlua::Result<()> {
        if self.context == Context::Client {
            Ok(())
        } else {
            Err(lua_error(LOCAL_ONLY))
        }
    }

    /// `GetState()` and `GetHoldProgress()` of an action.
    pub(crate) fn action_state(&self, id: InstanceId) -> mlua::Result<(InputValue, f32)> {
        self.require_client()?;
        let active = action_active(&*self.world()?, id);
        let stored = self.shared.input.states.borrow().get(&id).copied();
        Ok(match stored {
            Some(state) if active => state,
            _ => (self.action_zero(id), 0.0),
        })
    }

    /// Refuses the members of `InputAction` on the server.
    pub(crate) fn check_input_member(&self, class: &str, member: &str) -> mlua::Result<()> {
        if class == "InputAction"
            && matches!(
                member,
                "GetState"
                    | "GetHoldProgress"
                    | "Pressed"
                    | "Released"
                    | "StateChanged"
                    | "Triggered"
            )
        {
            self.require_client()?;
        }
        Ok(())
    }

    pub(crate) fn enum_value(&self, enum_name: &str, name: &str) -> Value {
        self.enums.item(enum_name, name)
    }

    fn input_object(&self, lua: &Lua, state: &str, value: InputValue) -> mlua::Result<Table> {
        let last = self.shared.input.info.borrow().last;
        let t = lua.create_table()?;
        t.set("KeyCode", self.enum_value("KeyCode", "Unknown"))?;
        t.set("UserInputType", self.enum_value("UserInputType", last))?;
        t.set("UserInputState", self.enum_value("UserInputState", state))?;
        let position = match value {
            InputValue::Bool(_) => Vec3::new(0.0, 0.0, 0.0),
            InputValue::Axis1(x) => Vec3::new(f64::from(x), 0.0, 0.0),
            InputValue::Axis2([x, y]) => Vec3::new(f64::from(x), f64::from(y), 0.0),
            InputValue::Axis3([x, y, z]) => Vec3::new(f64::from(x), f64::from(y), f64::from(z)),
        };
        t.set("Position", LuaVector3(position))?;
        t.set("Delta", LuaVector3(Vec3::new(0.0, 0.0, 0.0)))?;
        t.set_readonly(true);
        Ok(t)
    }

    fn call_bound(&self, call: HandlerCall, state: &'static str, value: InputValue) {
        let (name, handler, alive, owner) = call;
        self.dispatch(handler, alive, owner, &|lua| {
            let obj = self.input_object(lua, state, value)?;
            Ok(vec![
                Value::String(lua.create_string(&name)?),
                self.enum_value("UserInputState", state),
                Value::Table(obj),
            ])
        });
    }

    /// Delivers the waiting `LastInputTypeChanged` events.
    pub(crate) fn deliver_input_type_changes(self: &Rc<Self>, changes: &[&'static str]) {
        let Some(carrier) = self.world().ok().and_then(|w| w.root("RunService")) else {
            return;
        };
        for &name in changes {
            self.fire((carrier, EventKind::LastInputTypeChanged), &|_| {
                Ok(vec![self.enum_value("UserInputType", name)])
            });
        }
    }

    /// Client VM: cancels, disabled actions and up to [`MAX_ACTION_EVENTS_PER_STEP`] events.
    pub(crate) fn deliver_action_events(self: &Rc<Self>) {
        self.flush_hidden();
        // Actions that were unbound while pressed.
        let cancels = std::mem::take(&mut *self.cancels.borrow_mut());
        for c in cancels {
            let flag = c.alive.clone();
            self.call_bound(
                (c.name, c.handler, c.alive, c.owner),
                "Cancel",
                InputValue::Bool(false),
            );
            flag.set(false);
        }
        // Pressed actions whose action or context was disabled meanwhile.
        let disabled: Vec<HandlerCall> = {
            let Ok(world) = self.world() else { return };
            self.bound
                .borrow_mut()
                .iter_mut()
                .filter(|(_, b)| b.pressed && !action_active(&world, b.action))
                .map(|(name, b)| {
                    b.pressed = false;
                    (name.clone(), b.handler.clone(), b.alive.clone(), b.owner)
                })
                .collect()
        };
        for call in disabled {
            self.call_bound(call, "Cancel", InputValue::Bool(false));
        }

        let mut delivered = 0;
        while delivered < MAX_ACTION_EVENTS_PER_STEP {
            let Some(event) = self.next_action_event() else {
                break;
            };
            if !self.world().is_ok_and(|w| action_active(&w, event.action)) {
                continue;
            }
            delivered += 1;
            self.deliver_action_event(&event);
        }
    }

    /// Takes the event with the oldest time (queue order breaks ties).
    fn next_action_event(&self) -> Option<InputActionEvent> {
        let mut events = self.shared.input.events.borrow_mut();
        let mut best: Option<usize> = None;
        for (i, e) in events.iter().enumerate() {
            if best.is_none_or(|b| e.time < events[b].time) {
                best = Some(i);
            }
        }
        best.map(|i| events.remove(i))
    }

    fn deliver_action_event(self: &Rc<Self>, event: &InputActionEvent) {
        let kind = match event.kind {
            InputEventKind::Pressed => EventKind::Pressed,
            InputEventKind::Released => EventKind::Released,
            InputEventKind::Triggered => EventKind::Triggered,
            InputEventKind::StateChanged => EventKind::StateChanged,
        };
        let value = event.value;
        self.fire((event.action, kind), &|lua| match kind {
            EventKind::Triggered | EventKind::StateChanged => Ok(vec![value_to_lua(lua, value)?]),
            _ => Ok(Vec::new()),
        });
        // The bound handler of this action, if there is one.
        let state = match (event.kind, event.value) {
            (InputEventKind::Pressed, _) => "Begin",
            (InputEventKind::Released, _) => "End",
            (InputEventKind::StateChanged, v) if !matches!(v, InputValue::Bool(_)) => "Change",
            _ => return,
        };
        let target = self
            .bound
            .borrow_mut()
            .iter_mut()
            .find(|(_, b)| b.action == event.action)
            .map(|(name, b)| {
                match state {
                    "Begin" => b.pressed = true,
                    "End" => b.pressed = false,
                    _ => {}
                }
                (name.clone(), b.handler.clone(), b.alive.clone(), b.owner)
            });
        if let Some(call) = target {
            self.call_bound(call, state, value);
        }
    }

    /// Removes the bound actions of a stopped script (no `Cancel`: the script is gone).
    pub(crate) fn drop_bound_actions(&self, owner: u64) {
        let gone: Vec<(InstanceId, InstanceId)> = {
            let mut bound = self.bound.borrow_mut();
            let names: Vec<String> = bound
                .iter()
                .filter(|(_, b)| b.owner == owner)
                .map(|(n, _)| n.clone())
                .collect();
            names
                .iter()
                .filter_map(|n| bound.remove(n))
                .map(|b| {
                    b.alive.set(false);
                    (b.action, b.context)
                })
                .collect()
        };
        self.cancels.borrow_mut().retain(|c| c.owner != owner);
        for (action, context) in gone {
            self.destroy_hidden(action, context);
        }
    }

    /// Destroys a hidden action (and its context when empty). When the world is borrowed the
    /// destroy waits in a queue that `flush_hidden` runs at the next safe point.
    fn destroy_hidden(&self, action: InstanceId, context: InstanceId) {
        if !self.try_destroy_hidden(action, context) {
            self.pending_destroy.borrow_mut().push((action, context));
        }
    }

    fn try_destroy_hidden(&self, action: InstanceId, context: InstanceId) -> bool {
        let Ok(mut world) = self.world.try_borrow_mut() else {
            return false;
        };
        if world.is_alive(action) {
            world.destroy(action);
        }
        if world.is_alive(context) && world.children(context).is_empty() {
            world.destroy(context);
        }
        true
    }

    /// Runs the destroys that had to wait for the world.
    pub(crate) fn flush_hidden(&self) {
        if self.pending_destroy.borrow().is_empty() {
            return;
        }
        let waiting = std::mem::take(&mut *self.pending_destroy.borrow_mut());
        for (action, context) in waiting {
            self.destroy_hidden(action, context);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// ContextActionService

/// One action bound with `ContextActionService:BindAction`.
pub(crate) struct Bound {
    action: InstanceId,
    context: InstanceId,
    /// The handler wrapped so that its result reaches the host.
    handler: Function,
    owner: u64,
    pressed: bool,
    alive: Rc<Cell<bool>>,
}

/// A `Cancel` for an action that was unbound while pressed.
pub(crate) struct PendingCancel {
    name: String,
    handler: Function,
    alive: Rc<Cell<bool>>,
    owner: u64,
}

/// Luau source that wraps a handler: after it returns, `record(sink)` tells the host.
const WRAPPER: &str = r#"
local pass = ...
return function(handler, record)
    return function(...)
        local result = handler(...)
        record(result ~= pass)
    end
end
"#;

/// Builds the wrapper factory (see [`WRAPPER`]); `pass` is `Enum.ContextActionResult.Pass`.
pub(crate) fn bind_wrapper(lua: &Lua, enums: &Enums) -> mlua::Result<Function> {
    let pass = enums.item("ContextActionResult", "Pass");
    lua.load(WRAPPER).set_name("=[engine]").call(pass)
}

/// The controls (names of `Control` in the runtime) that a `KeyCode` or `UserInputType` stands for.
fn controls_of(value: &Value) -> mlua::Result<Vec<&'static str>> {
    let Some((enum_name, name, _)) = enum_item(value) else {
        return Err(lua_error(
            "BindAction expects Enum.KeyCode or Enum.UserInputType values",
        ));
    };
    let range = |(first, last): (&str, &str)| -> Vec<&'static str> {
        let a = KEY_CODES.iter().position(|k| *k == first).unwrap_or(0);
        let b = KEY_CODES.iter().position(|k| *k == last).unwrap_or(a);
        KEY_CODES[a..=b].to_vec()
    };
    match (enum_name, name) {
        ("KeyCode", "Unknown") => Ok(Vec::new()),
        ("KeyCode", n) => Ok(KEY_CODES.iter().filter(|k| **k == n).copied().collect()),
        ("UserInputType", "MouseButton1") => Ok(vec!["MouseButton1"]),
        ("UserInputType", "MouseButton2") => Ok(vec!["MouseButton2"]),
        ("UserInputType", "MouseButton3") => Ok(vec!["MouseButton3"]),
        ("UserInputType", "MouseWheel") => Ok(vec!["MouseWheel"]),
        ("UserInputType", "MouseMovement") => Ok(vec!["MouseDelta"]),
        ("UserInputType", "Touch") => Ok(vec!["TouchButton"]),
        ("UserInputType", "Keyboard") => Ok(range(KEYBOARD_RANGE)),
        ("UserInputType", "Gamepad1" | "Gamepad2" | "Gamepad3" | "Gamepad4") => {
            Ok(range(GAMEPAD_RANGE))
        }
        (_, n) => Err(lua_error(format!(
            "BindAction cannot bind Enum.{enum_name}.{n}"
        ))),
    }
}

fn action_type_for(first: Option<&str>) -> &'static str {
    match first {
        Some("Thumbstick1" | "Thumbstick2" | "MouseDelta" | "TouchStick" | "TouchLook") => {
            "Direction2D"
        }
        Some("MouseWheel") => "Direction1D",
        Some("MousePosition") => "ViewportPosition",
        _ => "Bool",
    }
}

fn priority_arg(v: &Value) -> mlua::Result<f64> {
    let n = match v {
        Value::Number(n) => Some(*n),
        Value::Integer(i) => Some(*i as f64),
        other => enum_item(other)
            .filter(|i| i.0 == "ContextActionPriority")
            .map(|i| i.2 as f64),
    };
    n.filter(|n| n.is_finite())
        .ok_or_else(|| lua_error("BindActionAtPriority expects a number priority"))
}

impl ApiState {
    fn bind_action(
        self: &Rc<Self>,
        name: String,
        handler: Function,
        touch: bool,
        priority: f64,
        inputs: &[Value],
    ) -> mlua::Result<()> {
        let mut controls: Vec<&'static str> = Vec::new();
        for input in inputs {
            for c in controls_of(input)? {
                if !controls.contains(&c) {
                    controls.push(c);
                }
            }
        }
        let Some(player) = self.shared.local_player.get() else {
            return Err(lua_error("BindAction needs a local player"));
        };
        let lua = self
            .lua
            .try_upgrade()
            .ok_or_else(|| lua_error("the VM is gone"))?;
        let owner = self.budget.current_owner();
        {
            let bound = self.bound.borrow();
            let replaces = bound.get(&name).is_some_and(|b| b.owner == owner);
            let mine = bound.values().filter(|b| b.owner == owner).count();
            if !replaces && mine >= MAX_BOUND_ACTIONS_PER_SCRIPT {
                return Err(lua_error(format!(
                    "BindAction: too many bound actions (the limit is {MAX_BOUND_ACTIONS_PER_SCRIPT} per script)"
                )));
            }
        }
        // Everything that can fail happens before the old binding is touched; a failure
        // removes what was created.
        let (action, context) = self.create_hidden(player, &name, touch, priority, &controls)?;
        let wrapped = match self.wrap_bound(&lua, handler, action) {
            Ok(f) => f,
            Err(e) => {
                self.destroy_hidden(action, context);
                return Err(e);
            }
        };
        let new = Bound {
            action,
            context,
            handler: wrapped,
            owner,
            pressed: false,
            alive: Rc::new(Cell::new(true)),
        };
        let old = self.bound.borrow_mut().insert(name.clone(), new);
        if let Some(old) = old {
            self.retire(&name, old);
        }
        Ok(())
    }

    fn wrap_bound(
        &self,
        lua: &Lua,
        handler: Function,
        action: InstanceId,
    ) -> mlua::Result<Function> {
        let (shared, now) = (self.shared.clone(), self.now.clone());
        let record = lua.create_function(move |_, sink: bool| {
            let mut results = shared.input.results.borrow_mut();
            if results.len() < MAX_BOUND_RESULTS {
                results.push(BoundResult {
                    action,
                    time: now.get(),
                    sink,
                });
            }
            Ok(())
        })?;
        self.bind_wrapper.call((handler, record))
    }

    /// Creates the hidden context (if needed), action and bindings in one go; on failure
    /// nothing stays behind.
    fn create_hidden(
        &self,
        player: InstanceId,
        name: &str,
        touch: bool,
        priority: f64,
        controls: &[&'static str],
    ) -> mlua::Result<(InstanceId, InstanceId)> {
        let mut world = self.world_mut()?;
        let context_name = format!("ContextActionService@{priority}");
        let existing = world.find_first_child(player, &context_name);
        let mut created: Vec<InstanceId> = Vec::new();
        let result = (|| -> mlua::Result<(InstanceId, InstanceId)> {
            let context = match existing {
                Some(c) => c,
                None => {
                    let c = world
                        .create("InputContext", Some(player))
                        .map_err(lua_error)?;
                    created.push(c);
                    world
                        .set(c, "Name", CoreValue::String(context_name))
                        .map_err(lua_error)?;
                    world
                        .set(c, "Priority", CoreValue::Number(priority))
                        .map_err(lua_error)?;
                    c
                }
            };
            let action = world
                .create("InputAction", Some(context))
                .map_err(lua_error)?;
            created.push(action);
            let ty = action_type_for(controls.first().copied()).to_string();
            for (prop, value) in [
                ("Name", CoreValue::String(name.to_string())),
                ("DisplayName", CoreValue::String(name.to_string())),
                ("ShowOnTouch", CoreValue::Bool(touch)),
                ("Type", CoreValue::String(ty)),
            ] {
                world.set(action, prop, value).map_err(lua_error)?;
            }
            for key in controls {
                let binding = world
                    .create("InputBinding", Some(action))
                    .map_err(lua_error)?;
                world
                    .set(binding, "KeyCode", CoreValue::String((*key).to_string()))
                    .map_err(lua_error)?;
            }
            Ok((action, context))
        })();
        if result.is_err() {
            for id in created.into_iter().rev() {
                if world.is_alive(id) {
                    world.destroy(id);
                }
            }
        }
        result
    }

    /// Removes a bound action; a pressed one gets `Cancel` on the next step.
    fn unbind_action(&self, name: &str) {
        let Some(b) = self.bound.borrow_mut().remove(name) else {
            return;
        };
        self.retire(name, b);
    }

    fn retire(&self, name: &str, b: Bound) {
        if b.pressed {
            self.cancels.borrow_mut().push(PendingCancel {
                name: name.to_string(),
                handler: b.handler,
                alive: b.alive,
                owner: b.owner,
            });
        } else {
            b.alive.set(false);
        }
        self.destroy_hidden(b.action, b.context);
    }

    /// Sets a property of the hidden action bound as `name`; warns when there is none.
    fn set_bound_property(&self, name: &str, props: Vec<(&str, CoreValue)>) -> mlua::Result<()> {
        let action = self.bound.borrow().get(name).map(|b| b.action);
        let Some(action) = action else {
            self.sink.warn(&format!("no action is bound as \"{name}\""));
            return Ok(());
        };
        let mut world = self.world_mut()?;
        for (prop, value) in props {
            world.set(action, prop, value).map_err(lua_error)?;
        }
        Ok(())
    }
}

type Methods = HashMap<&'static str, Function>;

fn member(methods: &Methods, key: &str, service: &str) -> mlua::Result<Value> {
    methods
        .get(key)
        .map(|f| Value::Function(f.clone()))
        .ok_or_else(|| lua_error(format!("{key} is not a valid member of {service}")))
}

/// The Luau `ContextActionService`.
pub(crate) struct LuaCas {
    state: Rc<ApiState>,
    methods: Methods,
}

impl LuaCas {
    fn new(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<Self> {
        let mut methods = Methods::new();
        methods.insert(
            "BindAction",
            errors::guarded_fn(
                lua,
                |_,
                 (this, name, handler, touch, inputs): (
                    UserDataRef<LuaCas>,
                    String,
                    Function,
                    Option<bool>,
                    Variadic<Value>,
                )| {
                    this.state.require_cas()?;
                    this.state
                        .bind_action(name, handler, touch.unwrap_or(false), 2000.0, &inputs)
                },
            )?,
        );
        methods.insert(
            "BindActionAtPriority",
            errors::guarded_fn(
                lua,
                |_,
                 (this, name, handler, touch, priority, inputs): (
                    UserDataRef<LuaCas>,
                    String,
                    Function,
                    Option<bool>,
                    Value,
                    Variadic<Value>,
                )| {
                    this.state.require_cas()?;
                    let priority = priority_arg(&priority)?;
                    this.state
                        .bind_action(name, handler, touch.unwrap_or(false), priority, &inputs)
                },
            )?,
        );
        methods.insert(
            "UnbindAction",
            errors::guarded_fn(lua, |_, (this, name): (UserDataRef<LuaCas>, String)| {
                this.state.require_cas()?;
                this.state.unbind_action(&name);
                Ok(())
            })?,
        );
        methods.insert(
            "SetTitle",
            errors::guarded_fn(
                lua,
                |_, (this, name, title): (UserDataRef<LuaCas>, String, String)| {
                    this.state.require_cas()?;
                    this.state
                        .set_bound_property(&name, vec![("TouchLabel", CoreValue::String(title))])
                },
            )?,
        );
        methods.insert(
            "SetImage",
            errors::guarded_fn(
                lua,
                |_, (this, name, image): (UserDataRef<LuaCas>, String, String)| {
                    this.state.require_cas()?;
                    this.state
                        .set_bound_property(&name, vec![("TouchImage", CoreValue::String(image))])
                },
            )?,
        );
        methods.insert(
            "SetPosition",
            errors::guarded_fn(
                lua,
                |_, (this, name, position): (UserDataRef<LuaCas>, String, Value)| {
                    this.state.require_cas()?;
                    let position =
                        udim2(&position).ok_or_else(|| lua_error("SetPosition expects a UDim2"))?;
                    this.state.set_bound_property(
                        &name,
                        vec![
                            ("TouchLayout", CoreValue::String("Custom".to_string())),
                            ("TouchPosition", CoreValue::UDim2(position)),
                        ],
                    )
                },
            )?,
        );
        // The touch overlay's button of a bound action: nil when the action was bound without
        // `createTouchButton` or the overlay did not place it.
        methods.insert(
            "GetButton",
            errors::guarded_fn(lua, |lua, (this, name): (UserDataRef<LuaCas>, Value)| {
                this.state.require_cas()?;
                let Value::String(name) = name else {
                    return Ok(Value::Nil);
                };
                let name = name.to_str()?.to_string();
                let action = this.state.bound.borrow().get(&name).map(|b| b.action);
                let placed = action.is_some_and(|a| {
                    let buttons = this.state.shared.input.touch_buttons.borrow();
                    buttons.iter().any(|b| b.action == a)
                });
                match action {
                    Some(action) if placed => {
                        Ok(Value::UserData(lua.create_userdata(LuaTouchButton {
                            state: this.state.clone(),
                            action,
                            name,
                        })?))
                    }
                    _ => Ok(Value::Nil),
                }
            })?,
        );
        Ok(LuaCas {
            state: state.clone(),
            methods,
        })
    }
}

impl ApiState {
    fn require_cas(&self) -> mlua::Result<()> {
        if self.context == Context::Client {
            Ok(())
        } else {
            Err(lua_error(CAS_LOCAL_ONLY))
        }
    }
}

impl UserData for LuaCas {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Instance");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(lua, |_, (this, key): (UserDataRef<LuaCas>, String)| {
                member(&this.methods, &key, "ContextActionService")
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("ContextActionService".to_string())
        });
    }
}

/// What `ContextActionService:GetButton` returns: a read-only view of a touch overlay button.
/// The overlay is drawn by the Player, not by GUI instances, so this is not an `ImageButton`;
/// its members follow the GUI names and read the overlay's current placement.
pub(crate) struct LuaTouchButton {
    state: Rc<ApiState>,
    action: InstanceId,
    name: String,
}

impl UserData for LuaTouchButton {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "TouchButton");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(
                lua,
                |lua, (this, key): (UserDataRef<LuaTouchButton>, String)| {
                    let buttons = this.state.shared.input.touch_buttons.borrow();
                    let info = buttons.iter().find(|b| b.action == this.action);
                    let pos = info.map_or([0.0; 2], |b| b.position);
                    let size = info.map_or([0.0; 2], |b| b.size);
                    let v2 = |v: [f32; 2]| -> mlua::Result<Value> {
                        let v = LuaVector2([f64::from(v[0]), f64::from(v[1])]);
                        Ok(Value::UserData(lua.create_userdata(v)?))
                    };
                    let ud = |v: [f32; 2]| -> mlua::Result<Value> {
                        let u =
                            metallic_core::UDim2::new(0.0, f64::from(v[0]), 0.0, f64::from(v[1]));
                        Ok(Value::UserData(lua.create_userdata(LuaUDim2(u))?))
                    };
                    match key.as_str() {
                        "Name" => Ok(Value::String(lua.create_string(&this.name)?)),
                        "ClassName" => Ok(Value::String(lua.create_string("TouchButton")?)),
                        "AbsolutePosition" => v2(pos),
                        "AbsoluteSize" => v2(size),
                        "Position" => ud(pos),
                        "Size" => ud(size),
                        "Text" | "Title" => Ok(Value::String(
                            lua.create_string(info.map_or("", |b| b.title.as_str()))?,
                        )),
                        "Image" => Ok(Value::String(
                            lua.create_string(info.map_or("", |b| b.image.as_str()))?,
                        )),
                        "Visible" => Ok(Value::Boolean(info.is_some_and(|b| b.visible))),
                        other => Err(lua_error(format!(
                            "{other} is not a valid member of TouchButton"
                        ))),
                    }
                },
            )
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, this, ()| Ok(this.name.clone()));
    }
}

// ---------------------------------------------------------------------------------------------
// UserInputService (last input and devices; raw events stay with #80)

/// The Luau `UserInputService`.
pub(crate) struct LuaUis {
    state: Rc<ApiState>,
    methods: Methods,
}

impl LuaUis {
    fn new(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<Self> {
        let mut methods = Methods::new();
        methods.insert(
            "GetLastInputType",
            errors::guarded_fn(lua, |_, this: UserDataRef<LuaUis>| {
                let last = this.state.shared.input.info.borrow().last;
                Ok(this.state.enum_value("UserInputType", last))
            })?,
        );
        Ok(LuaUis {
            state: state.clone(),
            methods,
        })
    }
}

impl UserData for LuaUis {
    fn add_fields<F: UserDataFields<Self>>(fields: &mut F) {
        fields.add_meta_field(MetaMethod::Type, "Instance");
        fields.add_meta_field_with(MetaMethod::Index, |lua| {
            errors::guarded_fn(lua, |lua, (this, key): (UserDataRef<LuaUis>, String)| {
                let info = this.state.shared.input.info.borrow();
                match key.as_str() {
                    "TouchEnabled" => Ok(Value::Boolean(info.touch)),
                    "KeyboardEnabled" => Ok(Value::Boolean(info.keyboard)),
                    "GamepadEnabled" => Ok(Value::Boolean(info.gamepad)),
                    "PreferredInput" => Ok(this
                        .state
                        .enum_value("PreferredInput", info.preferred.enum_name())),
                    "LastInputTypeChanged" => {
                        let carrier = this
                            .state
                            .world()?
                            .root("RunService")
                            .ok_or_else(|| lua_error("the world has no RunService"))?;
                        Ok(Value::UserData(lua.create_userdata(LuaSignal {
                            state: this.state.clone(),
                            key: (carrier, EventKind::LastInputTypeChanged),
                        })?))
                    }
                    other => member(&this.methods, other, "UserInputService"),
                }
            })
        });
    }

    fn add_methods<M: UserDataMethods<Self>>(methods: &mut M) {
        methods.add_meta_method(MetaMethod::ToString, |_, _, ()| {
            Ok("UserInputService".to_string())
        });
    }
}

/// The two service objects of a VM (created once, so `GetService` returns the same object).
pub(crate) type InputServices = OnceCell<(AnyUserData, AnyUserData)>;

/// Creates the `UserInputService` and `ContextActionService` objects of `state`.
pub(crate) fn install_services(lua: &Lua, state: &Rc<ApiState>) -> mlua::Result<()> {
    let uis = lua.create_userdata(LuaUis::new(lua, state)?)?;
    let cas = lua.create_userdata(LuaCas::new(lua, state)?)?;
    let _ = state.input_services.set((uis, cas));
    Ok(())
}

/// The service called `name`, if it is one of the input services.
pub(crate) fn service(state: &ApiState, name: &str) -> Option<Value> {
    let (uis, cas) = state.input_services.get()?;
    match name {
        "UserInputService" => Some(Value::UserData(uis.clone())),
        "ContextActionService" => Some(Value::UserData(cas.clone())),
        _ => None,
    }
}

/// `InputAction:GetState()` and `GetHoldProgress()` as Luau functions.
pub(crate) fn action_methods(lua: &Lua) -> mlua::Result<Vec<(&'static str, Function)>> {
    use super::instance::LuaInstance;
    let get_state = lua.create_function(|lua, this: UserDataRef<LuaInstance>| {
        this.expect_class("InputAction", "GetState")?;
        let (value, _) = this.state.action_state(this.id)?;
        value_to_lua(lua, value)
    })?;
    let hold = lua.create_function(|_, this: UserDataRef<LuaInstance>| {
        this.expect_class("InputAction", "GetHoldProgress")?;
        let (_, progress) = this.state.action_state(this.id)?;
        Ok(f64::from(progress.clamp(0.0, 1.0)))
    })?;
    Ok(vec![("GetState", get_state), ("GetHoldProgress", hold)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vm::{OutputSink, ScriptVm};

    struct Quiet;
    impl OutputSink for Quiet {
        fn print(&self, _: &str) {}
        fn warn(&self, _: &str) {}
        fn error(&self, _: &str) {}
    }

    /// An unbind that finds the world borrowed waits, and the hidden instances go at the next
    /// safe point.
    #[test]
    fn unbind_while_the_world_is_borrowed_is_deferred() {
        let world = Rc::new(RefCell::new(World::new_with_services()));
        let player = {
            let mut w = world.borrow_mut();
            let players = w.root("Players").unwrap();
            w.create("Player", Some(players)).unwrap()
        };
        let shared = Rc::new(super::super::HostShared::default());
        shared.local_player.set(Some(player));
        let sink: Rc<dyn OutputSink> = Rc::new(Quiet);
        let mut vm = ScriptVm::with_output(Context::Client, sink.clone()).unwrap();
        let state = super::super::install(&vm, world.clone(), shared, sink).unwrap();
        vm.run_script(
            "bind",
            r#"game:GetService("ContextActionService"):BindAction("A", function() end, false, Enum.KeyCode.A)"#,
        )
        .unwrap();
        let count = |w: &World| w.children(player).len();
        assert_eq!(count(&world.borrow()), 1, "the hidden context exists");
        {
            let _busy = world.borrow();
            vm.run_script(
                "unbind",
                r#"game:GetService("ContextActionService"):UnbindAction("A")"#,
            )
            .unwrap();
            assert_eq!(state.pending_destroy.borrow().len(), 1, "queued");
        }
        assert_eq!(
            count(&world.borrow()),
            1,
            "still there until the safe point"
        );
        state.flush_hidden();
        assert_eq!(count(&world.borrow()), 0, "context and action are gone");
        assert!(state.pending_destroy.borrow().is_empty());
    }
}

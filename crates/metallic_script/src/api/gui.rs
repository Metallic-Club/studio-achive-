//! GUI element events (#79): `GuiButton.Activated`, `MouseButton1Click`, `MouseButton1Down/Up`
//! and `GuiObject.MouseEnter/Leave`, `InputBegan/Changed/Ended`.
//!
//! The runtime hit-tests the pointer against the laid-out GUI and reports what happened with
//! [`ScriptHost::gui_event`](crate::ScriptHost::gui_event); the next step fires the events in the
//! client VM, in the order they were reported.

use std::rc::Rc;

use metallic_core::{InstanceId, Vec3};
use mlua::{Lua, Table, Value};

use super::ApiState;
use super::datatypes::LuaVector3;

/// Most GUI events kept for one step; later ones are dropped until the next step.
pub const MAX_GUI_EVENTS_PER_STEP: usize = 256;

/// What happened to a GUI element.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GuiEventKind {
    /// The pointer moved onto the element (`MouseEnter(x, y)`).
    MouseEnter,
    /// The pointer left it (`MouseLeave(x, y)`).
    MouseLeave,
    /// Pressed on a button (`MouseButton1Down(x, y)`); also a finger.
    MouseButton1Down,
    /// Released on a button (`MouseButton1Up(x, y)`).
    MouseButton1Up,
    /// Pressed and released on the same button (`MouseButton1Click()`).
    MouseButton1Click,
    /// The button was used (`Activated(inputObject, clickCount)`), right after the click.
    Activated,
    /// A press began over the element (`InputBegan(inputObject)`).
    InputBegan,
    /// The pointer moved over it (`InputChanged(inputObject)`).
    InputChanged,
    /// A press that began over the element ended (`InputEnded(inputObject)`).
    InputEnded,
}

impl GuiEventKind {
    /// The signal this fires. The class files say which classes have it.
    fn signal(self) -> &'static str {
        match self {
            GuiEventKind::MouseEnter => "MouseEnter",
            GuiEventKind::MouseLeave => "MouseLeave",
            GuiEventKind::MouseButton1Down => "MouseButton1Down",
            GuiEventKind::MouseButton1Up => "MouseButton1Up",
            GuiEventKind::MouseButton1Click => "MouseButton1Click",
            GuiEventKind::Activated => "Activated",
            GuiEventKind::InputBegan => "InputBegan",
            GuiEventKind::InputChanged => "InputChanged",
            GuiEventKind::InputEnded => "InputEnded",
        }
    }
}

/// The pointer behind a GUI event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiPointer {
    /// The mouse (left button).
    Mouse,
    /// A finger.
    Touch,
}

/// One GUI event, reported by the runtime.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiEvent {
    /// The element (a Frame, TextLabel or button).
    pub target: InstanceId,
    /// What happened.
    pub kind: GuiEventKind,
    /// Mouse or finger.
    pub pointer: GuiPointer,
    /// The pointer in GUI points from the top-left of the screen.
    pub position: [f32; 2],
}

impl ApiState {
    /// Fires `event` on its target, if the target still exists.
    pub(crate) fn deliver_gui_event(self: &Rc<Self>, event: &GuiEvent) {
        // Nothing for a destroyed target, or one whose class has no such event.
        let Some(key) = self.signal_key(event.target, event.kind.signal()) else {
            return;
        };
        let [x, y] = event
            .position
            .map(|v| if v.is_finite() { v.round() } else { 0.0 });
        let (x, y) = (f64::from(x), f64::from(y));
        self.fire(key, &|lua| {
            Ok(match event.kind {
                GuiEventKind::MouseEnter
                | GuiEventKind::MouseLeave
                | GuiEventKind::MouseButton1Down
                | GuiEventKind::MouseButton1Up => vec![Value::Number(x), Value::Number(y)],
                GuiEventKind::MouseButton1Click => Vec::new(),
                GuiEventKind::Activated => vec![
                    Value::Table(self.gui_input_object(lua, event, x, y)?),
                    Value::Integer(1),
                ],
                GuiEventKind::InputBegan
                | GuiEventKind::InputChanged
                | GuiEventKind::InputEnded => {
                    vec![Value::Table(self.gui_input_object(lua, event, x, y)?)]
                }
            })
        });
    }

    /// The `InputObject` of a GUI event (a read-only table, like the other input objects).
    fn gui_input_object(&self, lua: &Lua, e: &GuiEvent, x: f64, y: f64) -> mlua::Result<Table> {
        let input_type = match (e.pointer, e.kind) {
            (GuiPointer::Touch, _) => "Touch",
            (GuiPointer::Mouse, GuiEventKind::InputChanged) => "MouseMovement",
            (GuiPointer::Mouse, _) => "MouseButton1",
        };
        let state = match e.kind {
            GuiEventKind::InputBegan | GuiEventKind::MouseButton1Down => "Begin",
            GuiEventKind::InputChanged | GuiEventKind::MouseEnter => "Change",
            _ => "End",
        };
        let t = lua.create_table()?;
        t.set("KeyCode", self.enum_value("KeyCode", "Unknown"))?;
        t.set(
            "UserInputType",
            self.enum_value("UserInputType", input_type),
        )?;
        t.set("UserInputState", self.enum_value("UserInputState", state))?;
        t.set("Position", LuaVector3(Vec3::new(x, y, 0.0)))?;
        t.set("Delta", LuaVector3(Vec3::new(0.0, 0.0, 0.0)))?;
        t.set_readonly(true);
        Ok(t)
    }
}

//! Instance: the root of the class tree.
use crate::classes::prelude::*;

static ATTRIBUTE_CHANGED: SignalDesc = SignalDesc::new("AttributeChanged")
    .params(&[Param::new("attribute", Ty::String)])
    .parameterized()
    .describe("An attribute was set, changed or removed: its name.");
static CHANGED: SignalDesc = SignalDesc::new("Changed")
    .params(&[Param::new("property", Ty::String)])
    .parameterized()
    .describe("A property's value changed: its name.");

/// `GetPropertyChangedSignal(name)`: `Changed`, keyed by that property.
fn get_property_changed_signal(call: &mut MethodCall) -> Result<CallOutcome, ApiError> {
    let name = call.string(0)?;
    let Some(arg) = call.property_signal_arg(name) else {
        return Err(ApiError::message(format!(
            "{name} is not a valid property name."
        )));
    };
    Ok(call.signal(CHANGED.name, arg)?.into())
}

pub static CLASS: ClassDesc = ClassDesc::new("Instance", None)
    .flags(ClassFlags::ABSTRACT)
    .category("Base")
    .describe("The base of every class: a named node in the game tree.")
    // The default Name is the class name: resolution writes it into each class's default table.
    .props(&[
        PropDesc::new("Name", ValueType::String, || Value::String(String::new()))
            .describe("What this is called in the Explorer and in scripts."),
    ])
    .methods(&[
        MethodDesc::native("GetPropertyChangedSignal", get_property_changed_signal)
            .params(&[Param::new("property", Ty::String)])
            .returns(Ty::Signal)
            .describe("A signal that fires, with nothing, when this property's value changes."),
    ])
    .signals(&[&ATTRIBUTE_CHANGED, &CHANGED]);

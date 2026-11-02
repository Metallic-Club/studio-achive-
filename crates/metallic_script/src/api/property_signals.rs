//! `Instance.Changed` and `Instance:GetPropertyChangedSignal(name)`, for every property of every
//! class. The signal and the method are declared in `classes/base/instance.rs`.
//!
//! Nothing runs inside `World::set`, which pays nothing for this: the host reads the world's change
//! journal ([`World::property_changes_since`](metallic_core::World::property_changes_since)) once
//! per `step` and fires through the normal dispatch (budget, deferral, per-owner cleanup). A write
//! made by a handler lands in the journal after that delivery's cursor, so it fires on the next
//! `step`, never recursively.
//!
//! A VM reads the journal only while it has a listener on some instance's `Changed`, and only the
//! changes of those instances reach the hub, so everything else costs one early return per step.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use metallic_core::classes::{SignalKey, instance_class, property_signal_arg};
use metallic_core::{ChangeCursor, InstanceId};
use mlua::Value;

use super::ApiState;
use super::signals::plain_key;

/// Which instances one VM watches, and where its last delivery left the journal.
#[derive(Default)]
pub(crate) struct PropWatch {
    /// Watched instances with the journal position when watching began: changes made before it
    /// are not delivered.
    watched: RefCell<HashMap<InstanceId, ChangeCursor>>,
    /// Where the last delivery stopped reading; `None` while nothing is watched.
    cursor: Cell<Option<ChangeCursor>>,
    /// Journal entries read for delivery since the VM started (a test hook for the cost).
    scanned: Cell<u64>,
}

impl ApiState {
    /// `id` has a listener on `Changed`. A no-op when it is watched already, or gone.
    pub(crate) fn watch_properties(&self, id: InstanceId) {
        let Ok(world) = self.world.try_borrow() else {
            return;
        };
        let now = world.change_cursor();
        self.props.watched.borrow_mut().entry(id).or_insert(now);
        if self.props.cursor.get().is_none() {
            self.props.cursor.set(Some(now));
        }
    }

    /// How many journal entries property delivery has read so far.
    pub(crate) fn property_changes_scanned(&self) -> u64 {
        self.props.scanned.get()
    }

    /// Fires `GetPropertyChangedSignal(name)` and then `Changed(name)` for every real change of a
    /// watched property that scripts can see since the last call, oldest first.
    pub(crate) fn deliver_property_changes(self: &Rc<Self>) {
        if self.props.watched.borrow().is_empty() {
            return;
        }
        let changed = self.instance_signals.changed;
        let batch: Vec<(InstanceId, &'static str)> = {
            let Ok(world) = self.world() else { return };
            let listening = self.hub.instances_listening(changed);
            let mut watched = self.props.watched.borrow_mut();
            watched.retain(|id, _| world.is_alive(*id) && listening.contains(id));
            let next = (!watched.is_empty()).then(|| world.change_cursor());
            let Some(start) = self.props.cursor.replace(next) else {
                return;
            };
            if watched.is_empty() {
                return;
            }
            let Some(changes) = world.property_changes_since(start) else {
                self.sink.warn(
                    "too many property changes in one step; some Changed events were dropped",
                );
                return;
            };
            let mut scanned = 0_u64;
            let mut batch = Vec::new();
            for change in changes {
                scanned += 1;
                if watched
                    .get(&change.id)
                    .is_some_and(|since| change.at > *since)
                {
                    batch.push((change.id, change.prop.name));
                }
            }
            self.props.scanned.set(self.props.scanned.get() + scanned);
            batch
        };
        for (id, name) in batch {
            // A handler earlier in this batch may have destroyed it. Engine bookkeeping and
            // members a class hides never reach scripts.
            let member = self.world().ok().and_then(|world| {
                let class = instance_class(self.binding.classes.registry, &world, id)?;
                let member = class.member(name)?;
                let hidden = class.members.get(usize::from(member.0))?.hidden;
                (!hidden).then_some(member)
            });
            let Some(member) = member else { continue };
            let one_property = SignalKey {
                arg: property_signal_arg(member),
                ..plain_key(id, changed)
            };
            self.fire(one_property, &|_| Ok(Vec::new()));
            self.fire(plain_key(id, changed), &|lua| {
                Ok(vec![Value::String(lua.create_string(name)?)])
            });
        }
    }
}

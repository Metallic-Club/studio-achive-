//! Undoable edits: every change Studio makes to the edit world is a [`Command`] applied through
//! [`History`].
//!
//! # The undo API (used by every tool)
//!
//! - [`History::apply`] runs a command on the world and pushes it as one undo step.
//! - [`History::record`] pushes a command whose effect is already in the world. Gestures that
//!   preview live (a gizmo drag, a Properties scrub) write the world while the pointer moves and
//!   call `record` once on release, so one gesture is one undo step.
//! - [`History::undo`] / [`History::redo`] step back and forth; [`History::undo_name`] gives the
//!   menu text ("Undo Move 3 parts").
//! - [`History::is_parked`]: deleted instances (and undone creations) are not destroyed. They are
//!   detached from the tree and *parked* as parentless roots, so undo puts back the very same
//!   [`InstanceId`]s and every id held by the selection or older commands still resolves.
//!   Parked instances are destroyed for real only when the command that holds them leaves the
//!   history (the 200-step limit, or a new edit clearing the redo list). Explorer, save and Play
//!   skip parked roots; the renderer never sees them because they are outside `Workspace`.
//!
//! Commands keep property names as strings and values as [`Value`]s, so any property of any class
//! is undoable without extra code. Undo names follow "Verb object (count)".
//!
//! # Gestures (#213)
//!
//! A building tool never pushes one step per frame. During a gesture it writes through the
//! [`PreviewOverlay`](crate::preview::PreviewOverlay); on release the overlay becomes one
//! [`Command::SetProperties`] (old and new value per change), so moving 500 parts is one entry.
//! Gestures that also create instances (Alt-drag duplicates first) run the creating command with
//! [`History::apply_pending`] and record `Batch[create, SetProperties]` on release, or
//! [`History::abandon`] it on Esc. [`History::take_last`] reverts the newest step and hands it
//! back, so the editable delta after a drag (spec 4.5) can change it without adding a step.
//!
//! # Steps from panels that build their own edits
//!
//! [`Command::Step`] holds an [`EditStep`] (the Input Actions panel's undo unit). A step destroys
//! instances for real and rebuilds them with new ids on undo, so its undo and redo return an
//! [`IdMap`]. History then rewrites the ids in every command it keeps and queues the map;
//! [`History::take_remaps`] hands the maps to whoever holds ids (the Input Actions panel's
//! `follow_remap`).
use std::collections::HashSet;
use std::sync::Arc;

use metallic_core::{CoreError, InstanceId, Value, Vec3, World};

use crate::input_actions::EditStep;
use crate::input_actions::commands::IdMap;

/// A property name inside a command; shared, so a 500-part drag names `Position` once.
pub type PropName = Arc<str>;

/// Bookkeeping outside the world that commands change: parked instances. (Pivots are core
/// properties, `PivotOffset` and `WorldPivot`, changed by `SetProperties` like anything else.)
#[derive(Debug, Default, Clone)]
pub struct Book {
    /// Detached instances kept alive for undo.
    pub parked: HashSet<InstanceId>,
    /// Ids rebuilt by [`Command::Step`]s during the current apply or revert (`old -> new`).
    pub remap: IdMap,
}

/// Most undo steps kept. Older steps are dropped (and their parked instances destroyed).
pub const HISTORY_LIMIT: usize = 200;

/// Where an instance sits in the tree: its parent and its index among the parent's children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// The instance.
    pub id: InstanceId,
    /// Its parent (`None` for a root).
    pub parent: Option<InstanceId>,
    /// Index among the parent's children (or among the roots).
    pub index: usize,
}

/// One undoable edit.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Sets one property.
    SetProperty {
        /// Instance changed.
        id: InstanceId,
        /// Property name.
        prop: String,
        /// Value before.
        old: Value,
        /// Value after.
        new: Value,
    },
    /// Sets, changes or removes one attribute. `Nil` stands for "no attribute" in `old` (the
    /// attribute is added) and in `new` (it is removed).
    SetAttribute {
        /// Instance changed.
        id: InstanceId,
        /// Attribute name.
        name: String,
        /// Value before (`Nil`: there was none).
        old: Value,
        /// Value after (`Nil`: remove it).
        new: Value,
    },
    /// Adds (`on`) or removes one tag.
    SetTag {
        /// Instance changed.
        id: InstanceId,
        /// The tag.
        tag: String,
        /// True to add it, false to remove it.
        on: bool,
        /// Filled by apply: whether the world changed. Undo does nothing when it did not (adding
        /// a tag the instance already had must not remove it on undo).
        changed: bool,
    },
    /// Many property changes as one undo step: (instance, property, old value, new value). A
    /// gesture's preview overlay turns into this on release.
    SetProperties {
        /// Undo name, e.g. "Move 12 parts".
        name: String,
        /// The changes, applied in order and undone in reverse.
        changes: Vec<(InstanceId, PropName, Value, Value)>,
    },
    /// Several commands as one undo step (a gizmo drag over many parts, Properties on a
    /// multi-selection). Applied in order, undone in reverse.
    Batch {
        /// Undo name, e.g. "Move 3 parts".
        name: String,
        /// The parts of the step.
        commands: Vec<Command>,
    },
    /// Creates an instance as the last child of `parent`, with initial property values.
    Create {
        /// Class name.
        class: String,
        /// Parent.
        parent: InstanceId,
        /// Properties set right after creation.
        props: Vec<(String, Value)>,
        /// The created id; filled by the first apply and reused by redo.
        id: Option<InstanceId>,
    },
    /// Removes instances (with their descendants) from the tree.
    Delete {
        /// Where each removed root was; filled by apply.
        items: Vec<Placement>,
    },
    /// Moves instances under a new parent (as last children). Initial placement capture
    /// suppresses selected descendants before excluding built-ins and same-parent roots.
    /// An all-no-op request returns [`CommandError::Nothing`] without a history entry.
    Reparent {
        /// Where each moved instance was before; filled by apply when empty.
        from: Vec<Placement>,
        /// The new parent.
        to: InstanceId,
    },
    /// Copies instances next to their originals, moved by `offset` (BaseParts and the BaseParts
    /// inside copied Models).
    Duplicate {
        /// The originals.
        sources: Vec<InstanceId>,
        /// Position offset given to the copies.
        offset: Vec3,
        /// The copies and where they live; filled by the first apply.
        copies: Vec<Placement>,
    },
    /// Puts instances into a new Model under the first item's parent.
    Group {
        /// Where each grouped instance was before; filled by apply.
        items: Vec<Placement>,
        /// The Model; created by the first apply.
        model: Option<Placement>,
    },
    /// A tree edit built by a panel (Input Actions), already applied when recorded. Undo and
    /// redo may rebuild instances with new ids (see the module docs).
    Step(EditStep),
}

/// Why a command could not run. The world is left as it was before the command.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CommandError {
    /// The world refused a change.
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The command has nothing to act on (empty selection, built-in services only).
    #[error("nothing to {0}")]
    Nothing(&'static str),
}

impl Command {
    /// A property change of one instance.
    pub fn set(id: InstanceId, prop: &str, old: Value, new: Value) -> Command {
        Command::SetProperty {
            id,
            prop: prop.to_string(),
            old,
            new,
        }
    }

    /// Deletes `ids` (ancestors win; built-in services are skipped).
    pub fn delete(ids: &[InstanceId]) -> Command {
        Command::Delete {
            items: ids
                .iter()
                .map(|&id| Placement {
                    id,
                    parent: None,
                    index: 0,
                })
                .collect(),
        }
    }

    /// The undo name shown in Edit > Undo and the status bar.
    pub fn name(&self, world: &World) -> String {
        match self {
            Command::SetProperty { id, prop, .. } => {
                let what = world
                    .get_ref(*id, "Name")
                    .ok()
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .unwrap_or("instance");
                format!("Set {prop} of {what}")
            }
            Command::SetAttribute { id, name, new, .. } => {
                let what = world
                    .get_ref(*id, "Name")
                    .ok()
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .unwrap_or("instance");
                if matches!(new, Value::Nil) {
                    format!("Remove attribute {name} of {what}")
                } else {
                    format!("Set attribute {name} of {what}")
                }
            }
            Command::SetTag { id, tag, on, .. } => {
                let what = world
                    .get_ref(*id, "Name")
                    .ok()
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .unwrap_or("instance");
                if *on {
                    format!("Add tag {tag} to {what}")
                } else {
                    format!("Remove tag {tag} from {what}")
                }
            }
            Command::Batch { name, .. } | Command::SetProperties { name, .. } => name.clone(),
            Command::Create { class, .. } => format!("Insert {class}"),
            Command::Delete { items } => format!("Delete {}", count(items.len(), "object")),
            Command::Reparent { from, .. } => {
                format!("Move {} in Explorer", count(from.len(), "object"))
            }
            Command::Duplicate { sources, .. } => {
                format!("Duplicate {}", count(sources.len(), "object"))
            }
            Command::Group { items, .. } => format!("Group {}", count(items.len(), "object")),
            Command::Step(step) => step.label.clone(),
        }
    }

    /// Rewrites instance ids after an undo or redo rebuilt instances (`old -> new`).
    pub fn remap(&mut self, map: &IdMap) {
        if map.is_empty() {
            return;
        }
        let fix = |id: &mut InstanceId| {
            if let Some(&n) = map.get(id) {
                *id = n;
            }
        };
        let fix_opt = |id: &mut Option<InstanceId>| {
            if let Some(i) = id {
                fix(i);
            }
        };
        let fix_place = |p: &mut Placement| {
            fix(&mut p.id);
            fix_opt(&mut p.parent);
        };
        match self {
            Command::SetProperty { id, old, new, .. } => {
                fix(id);
                for v in [old, new] {
                    if let Value::Ref(r) = v {
                        fix_opt(r);
                    }
                }
            }
            Command::SetAttribute { id, .. } | Command::SetTag { id, .. } => fix(id),
            Command::SetProperties { changes, .. } => {
                for (id, _, old, new) in changes.iter_mut() {
                    fix(id);
                    for v in [old, new] {
                        if let Value::Ref(r) = v {
                            fix_opt(r);
                        }
                    }
                }
            }
            Command::Batch { commands, .. } => commands.iter_mut().for_each(|c| c.remap(map)),
            Command::Create { parent, id, .. } => {
                fix(parent);
                fix_opt(id);
            }
            Command::Delete { items } => items.iter_mut().for_each(fix_place),
            Command::Reparent { from, to } => {
                from.iter_mut().for_each(fix_place);
                fix(to);
            }
            Command::Duplicate {
                sources, copies, ..
            } => {
                sources.iter_mut().for_each(fix);
                copies.iter_mut().for_each(fix_place);
            }
            Command::Group { items, model } => {
                items.iter_mut().for_each(fix_place);
                if let Some(m) = model {
                    fix_place(m);
                }
            }
            Command::Step(step) => step.remap(map),
        }
    }

    /// Runs the command. On error the world is unchanged. Ids rebuilt by a [`Command::Step`] go
    /// into `book.remap`; later commands of a batch are rewritten with it.
    fn apply(&mut self, world: &mut World, book: &mut Book) -> Result<(), CommandError> {
        match self {
            Command::SetProperty { id, prop, new, .. } => Ok(world.set(*id, prop, new.clone())?),
            Command::SetAttribute { id, name, new, .. } => {
                Ok(world.set_attribute(*id, name, new.clone())?)
            }
            Command::SetTag {
                id,
                tag,
                on,
                changed,
            } => {
                *changed = world.has_tag(*id, tag) != *on;
                if *on {
                    world.add_tag(*id, tag)?;
                } else {
                    world.remove_tag(*id, tag)?;
                }
                Ok(())
            }
            Command::SetProperties { changes, .. } => {
                for i in 0..changes.len() {
                    let (id, prop, _, new) = &changes[i];
                    if let Err(e) = world.set(*id, prop, new.clone()) {
                        for (id, prop, old, _) in changes[..i].iter().rev() {
                            let _ = world.set(*id, prop, old.clone());
                        }
                        return Err(e.into());
                    }
                }
                Ok(())
            }
            Command::Batch { commands, .. } => {
                for i in 0..commands.len() {
                    commands[i].remap(&book.remap);
                    if let Err(e) = commands[i].apply(world, book) {
                        for done in commands[..i].iter_mut().rev() {
                            done.remap(&book.remap);
                            done.revert(world, book);
                        }
                        return Err(e);
                    }
                }
                Ok(())
            }
            Command::Step(step) => {
                let fresh = step.redo(world)?;
                book.remap.extend(fresh);
                Ok(())
            }
            Command::Create {
                class,
                parent,
                props,
                id,
            } => {
                if let Some(existing) = *id {
                    unpark(world, &mut book.parked, existing, Some(*parent), usize::MAX)?;
                    return Ok(());
                }
                let new_id = world.create(class, Some(*parent))?;
                for (name, value) in props.iter() {
                    if let Err(e) = world.set(new_id, name, value.clone()) {
                        world.destroy(new_id);
                        return Err(e.into());
                    }
                }
                *id = Some(new_id);
                Ok(())
            }
            Command::Delete { items } => {
                let first_run = items.iter().all(|p| p.parent.is_none() && p.index == 0);
                if first_run {
                    let ids: Vec<InstanceId> = items.iter().map(|p| p.id).collect();
                    *items = top_level(world, &ids)
                        .into_iter()
                        .filter(|&id| !world.is_builtin(id) && world.parent(id).is_some())
                        .filter_map(|id| placement(world, id))
                        .collect();
                    // Restoring in ascending index order puts every item back at its index.
                    items.sort_by_key(|p| p.index);
                }
                if items.is_empty() {
                    return Err(CommandError::Nothing("delete"));
                }
                for p in items.iter() {
                    park(world, &mut book.parked, p.id)?;
                }
                Ok(())
            }
            Command::Reparent { from, to } => {
                if from.iter().all(|p| p.parent.is_none() && p.index == 0) {
                    let ids: Vec<InstanceId> = from.iter().map(|p| p.id).collect();
                    *from = top_level(world, &ids)
                        .into_iter()
                        .filter(|&id| !world.is_builtin(id) && world.parent(id) != Some(*to))
                        .filter_map(|id| placement(world, id))
                        .collect();
                    from.sort_by_key(|p| p.index);
                }
                if from.is_empty() {
                    return Err(CommandError::Nothing("move"));
                }
                for (i, p) in from.iter().enumerate() {
                    if let Err(e) = world.set_parent(p.id, Some(*to)) {
                        for done in from[..i].iter().rev() {
                            let _ = place(world, done.id, done.parent, done.index);
                        }
                        return Err(e.into());
                    }
                }
                Ok(())
            }
            Command::Duplicate {
                sources,
                offset,
                copies,
            } => {
                if !copies.is_empty() {
                    for c in copies.iter() {
                        unpark(world, &mut book.parked, c.id, c.parent, usize::MAX)?;
                    }
                    return Ok(());
                }
                let top = top_level(world, sources);
                for src in top {
                    if world.is_builtin(src) {
                        continue;
                    }
                    let parent = world.parent(src);
                    let copy = world.clone_subtree(src)?;
                    world.set_parent(copy, parent)?;
                    offset_parts(world, copy, *offset);
                    if let Some(p) = placement(world, copy) {
                        copies.push(p);
                    }
                }
                if copies.is_empty() {
                    return Err(CommandError::Nothing("duplicate"));
                }
                Ok(())
            }
            Command::Group { items, model } => {
                if let Some(m) = *model {
                    unpark(world, &mut book.parked, m.id, m.parent, m.index)?;
                    for p in items.iter() {
                        world.set_parent(p.id, Some(m.id))?;
                    }
                    return Ok(());
                }
                let ids: Vec<InstanceId> = items.iter().map(|p| p.id).collect();
                let mut list: Vec<Placement> = top_level(world, &ids)
                    .into_iter()
                    .filter(|&id| !world.is_builtin(id) && world.parent(id).is_some())
                    .filter_map(|id| placement(world, id))
                    .collect();
                let Some(first) = list.first().copied() else {
                    return Err(CommandError::Nothing("group"));
                };
                let parent = first.parent.ok_or(CommandError::Nothing("group"))?;
                let m = world.create("Model", Some(parent))?;
                // The Model takes the first item's place in the tree.
                place(world, m, Some(parent), first.index)?;
                // Placements are taken again after inserting the Model, so undo restores exact indices.
                for p in list.iter_mut() {
                    if let Some(now) = placement(world, p.id) {
                        *p = now;
                    }
                }
                list.sort_by_key(|p| (p.parent.map(|x| x.to_bits()), p.index));
                for p in &list {
                    world.set_parent(p.id, Some(m))?;
                }
                *model = placement(world, m);
                *items = list;
                Ok(())
            }
        }
    }

    /// Undoes the command. Best effort: a failure on one item does not stop the others. Ids
    /// rebuilt by a [`Command::Step`] go into `book.remap`.
    fn revert(&mut self, world: &mut World, book: &mut Book) {
        let parked = &mut book.parked;
        match self {
            Command::SetProperty { id, prop, old, .. } => {
                let _ = world.set(*id, prop, old.clone());
            }
            Command::SetAttribute { id, name, old, .. } => {
                let _ = world.set_attribute(*id, name, old.clone());
            }
            Command::SetTag {
                id,
                tag,
                on,
                changed,
            } => {
                if *changed {
                    let _ = if *on {
                        world.remove_tag(*id, tag)
                    } else {
                        world.add_tag(*id, tag)
                    };
                }
            }
            Command::SetProperties { changes, .. } => {
                for (id, prop, old, _) in changes.iter().rev() {
                    let _ = world.set(*id, prop, old.clone());
                }
            }
            Command::Batch { commands, .. } => {
                for c in commands.iter_mut().rev() {
                    c.remap(&book.remap);
                    c.revert(world, book);
                }
            }
            Command::Step(step) => match step.undo(world) {
                Ok(fresh) => book.remap.extend(fresh),
                Err(e) => log::warn!("undo of `{}` failed: {e}", step.label),
            },
            Command::Create { id, .. } => {
                if let Some(id) = *id {
                    let _ = park(world, parked, id);
                }
            }
            Command::Delete { items } => {
                for p in items.iter() {
                    let _ = unpark(world, parked, p.id, p.parent, p.index);
                }
            }
            Command::Reparent { from, .. } => {
                for p in from.iter() {
                    let _ = place(world, p.id, p.parent, p.index);
                }
            }
            Command::Duplicate { copies, .. } => {
                for c in copies.iter() {
                    let _ = park(world, parked, c.id);
                }
            }
            Command::Group { items, model } => {
                for p in items.iter() {
                    let _ = place(world, p.id, p.parent, p.index);
                }
                if let Some(m) = model {
                    let _ = park(world, parked, m.id);
                }
            }
        }
    }

    /// Destroys the instances this command keeps parked, when it leaves the history.
    /// `applied` says whether the command is currently in effect.
    fn discard(&self, world: &mut World, book: &mut Book, applied: bool) {
        let parked = &mut book.parked;
        let mut drop = |id: InstanceId| {
            if parked.remove(&id) {
                world.destroy(id);
            }
        };
        match self {
            Command::Batch { commands, .. } => {
                for c in commands {
                    c.discard(world, book, applied);
                }
            }
            Command::Delete { items } if applied => items.iter().for_each(|p| drop(p.id)),
            Command::Create { id: Some(id), .. } if !applied => drop(*id),
            Command::Duplicate { copies, .. } if !applied => copies.iter().for_each(|c| drop(c.id)),
            Command::Group { model: Some(m), .. } if !applied => drop(m.id),
            _ => {}
        }
    }

    /// Ids this command created (for selecting the result: Insert, Duplicate, Group).
    pub fn created(&self) -> Vec<InstanceId> {
        match self {
            Command::Create { id: Some(id), .. } => vec![*id],
            Command::Duplicate { copies, .. } => copies.iter().map(|c| c.id).collect(),
            Command::Group { model: Some(m), .. } => vec![m.id],
            Command::Batch { commands, .. } => commands.iter().flat_map(Command::created).collect(),
            _ => Vec::new(),
        }
    }

    /// True when this step does nothing but move instances `within` accepts: every real change
    /// is their `Position`, `Orientation` or `WorldPivot`. (Smart duplicate: a resize, a colour
    /// or an edit of anything else is not a move of the copy.)
    pub fn only_moves(&self, within: &dyn Fn(InstanceId) -> bool) -> bool {
        let moves = |id: InstanceId, prop: &str, old: &Value, new: &Value| {
            old == new || (matches!(prop, "Position" | "Orientation" | "WorldPivot") && within(id))
        };
        match self {
            Command::SetProperty { id, prop, old, new } => moves(*id, prop, old, new),
            Command::SetProperties { changes, .. } => changes
                .iter()
                .all(|(id, prop, old, new)| moves(*id, prop, old, new)),
            Command::Batch { commands, .. } => commands.iter().all(|c| c.only_moves(within)),
            _ => false,
        }
    }
}

/// "1 part", "12 parts".
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

/// The ids in `ids` that have no ancestor in `ids`, in the given order, without duplicates.
pub fn top_level(world: &World, ids: &[InstanceId]) -> Vec<InstanceId> {
    let set: HashSet<InstanceId> = ids.iter().copied().collect();
    let mut seen = HashSet::new();
    ids.iter()
        .copied()
        .filter(|&id| world.is_alive(id) && seen.insert(id))
        .filter(|&id| {
            let mut cur = world.parent(id);
            while let Some(c) = cur {
                if set.contains(&c) {
                    return false;
                }
                cur = world.parent(c);
            }
            true
        })
        .collect()
}

/// Where `id` sits now.
pub fn placement(world: &World, id: InstanceId) -> Option<Placement> {
    if !world.is_alive(id) {
        return None;
    }
    let parent = world.parent(id);
    let siblings = match parent {
        Some(p) => world.children(p),
        None => world.roots(),
    };
    let index = siblings.iter().position(|&c| c == id)?;
    Some(Placement { id, parent, index })
}

/// Moves `id` under `parent` at `index` (clamped to the end).
///
/// Uses `World::set_parent_at` (`set_parent` alone appends, and moving to the same parent is
/// a no-op there).
pub fn place(
    world: &mut World,
    id: InstanceId,
    parent: Option<InstanceId>,
    index: usize,
) -> Result<(), CoreError> {
    if placement(world, id).is_some_and(|p| p.parent == parent && p.index == index) {
        return Ok(());
    }
    world.set_parent_at(id, parent, index)
}

fn park(
    world: &mut World,
    parked: &mut HashSet<InstanceId>,
    id: InstanceId,
) -> Result<(), CoreError> {
    world.set_parent(id, None)?;
    parked.insert(id);
    Ok(())
}

fn unpark(
    world: &mut World,
    parked: &mut HashSet<InstanceId>,
    id: InstanceId,
    parent: Option<InstanceId>,
    index: usize,
) -> Result<(), CoreError> {
    place(world, id, parent, index)?;
    parked.remove(&id);
    Ok(())
}

/// Moves every BasePart in the subtree of `root` by `offset`.
fn offset_parts(world: &mut World, root: InstanceId, offset: Vec3) {
    if offset == Vec3::new(0.0, 0.0, 0.0) {
        return;
    }
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if world.is_a(id, "BasePart")
            && let Ok(Value::Vector3(p)) = world.get(id, "Position")
        {
            let _ = world.set(id, "Position", Value::Vector3(p + offset));
        }
        stack.extend_from_slice(world.children(id));
    }
}

struct Entry {
    command: Command,
    /// Unique per pushed command; the save marker compares against it.
    serial: u64,
}

/// The undo and redo stacks of one edit world.
#[derive(Default)]
pub struct History {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
    book: Book,
    next_serial: u64,
    /// Serial of the newest applied step when the scene was last saved (0: nothing applied).
    saved: u64,
    /// Serial of the newest step dropped by [`HISTORY_LIMIT`] (0: none). With every kept step
    /// undone, the world is in the state after that step, not the opened file's.
    floor: u64,
    /// Bumps on every apply, record, undo and redo.
    revision: u64,
    /// Id maps from undo and redo of [`Command::Step`]s, oldest first, not yet taken.
    remaps: Vec<IdMap>,
}

impl History {
    /// An empty history.
    pub fn new() -> History {
        History {
            next_serial: 1,
            ..History::default()
        }
    }

    /// Runs `cmd` on `world` and pushes it as one undo step. Clears the redo list.
    /// On error nothing is pushed and the world is unchanged.
    pub fn apply(&mut self, mut cmd: Command, world: &mut World) -> Result<&Command, CommandError> {
        let result = cmd.apply(world, &mut self.book);
        self.after_rebuild();
        result?;
        Ok(self.push(cmd, world))
    }

    /// Runs `cmd` without pushing it: the first part of a gesture that is recorded later (an
    /// Alt-drag duplicates, then moves). Finish with [`History::record`] of a step that contains
    /// `cmd`, or [`History::abandon`]. On error the world is unchanged.
    pub fn apply_pending(
        &mut self,
        cmd: &mut Command,
        world: &mut World,
    ) -> Result<(), CommandError> {
        let result = cmd.apply(world, &mut self.book);
        self.after_rebuild();
        result
    }

    /// Undoes a pending command (see [`History::apply_pending`]) and destroys what it created.
    pub fn abandon(&mut self, mut cmd: Command, world: &mut World) {
        cmd.revert(world, &mut self.book);
        self.after_rebuild();
        cmd.discard(world, &mut self.book, false);
        self.revision += 1;
    }

    /// Reverts the newest undo step and hands it back without keeping it, so a tool can change
    /// it and push the changed version (the editable delta after a drag). Re-apply or record the
    /// changed step, or pass it to [`History::abandon`].
    pub fn take_last(&mut self, world: &mut World) -> Option<Command> {
        let mut e = self.undo.pop()?;
        e.command.revert(world, &mut self.book);
        self.after_rebuild();
        self.revision += 1;
        Some(e.command)
    }

    /// Pushes `cmd` as already applied (its `new` values are in the world). Used on release of a
    /// live-preview gesture. Clears the redo list.
    pub fn record(&mut self, cmd: Command, world: &mut World) {
        self.push(cmd, world);
    }

    fn push(&mut self, cmd: Command, world: &mut World) -> &Command {
        for e in self.redo.drain(..) {
            e.command.discard(world, &mut self.book, false);
        }
        let serial = self.next_serial.max(1);
        self.next_serial = serial + 1;
        self.undo.push(Entry {
            command: cmd,
            serial,
        });
        if self.undo.len() > HISTORY_LIMIT {
            let old = self.undo.remove(0);
            self.floor = old.serial;
            old.command.discard(world, &mut self.book, true);
        }
        self.revision += 1;
        &self.undo[self.undo.len() - 1].command
    }

    /// Undoes the newest step; returns its name, or `None` when there is nothing to undo.
    pub fn undo(&mut self, world: &mut World) -> Option<String> {
        let mut e = self.undo.pop()?;
        let name = e.command.name(world);
        e.command.revert(world, &mut self.book);
        self.redo.push(e);
        self.after_rebuild();
        self.revision += 1;
        Some(name)
    }

    /// Redoes the newest undone step; returns its name.
    pub fn redo(&mut self, world: &mut World) -> Option<String> {
        let mut e = self.redo.pop()?;
        let result = e.command.apply(world, &mut self.book);
        self.after_rebuild();
        if result.is_err() {
            // The world changed under the step (should not happen while every edit goes
            // through History); drop it rather than leave a half-applied step.
            e.command.discard(world, &mut self.book, false);
            self.revision += 1;
            return None;
        }
        let name = e.command.name(world);
        self.undo.push(e);
        self.revision += 1;
        Some(name)
    }

    /// Rewrites every kept command with the ids the last apply or revert rebuilt, and queues
    /// the map for [`History::take_remaps`].
    fn after_rebuild(&mut self) {
        let map = std::mem::take(&mut self.book.remap);
        if map.is_empty() {
            return;
        }
        for e in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            e.command.remap(&map);
        }
        self.remaps.push(map);
    }

    /// Id maps (old id to new id) from undo and redo steps that rebuilt instances, oldest first.
    /// Whoever holds instance ids outside the history (a panel's selection) applies them.
    pub fn take_remaps(&mut self) -> Vec<IdMap> {
        std::mem::take(&mut self.remaps)
    }

    /// The newest undo step while nothing is undone, for folding a continuing gesture into it
    /// (a value dragged over many frames). The command stays applied, as with [`History::record`].
    pub fn last_mut(&mut self) -> Option<&mut Command> {
        if !self.redo.is_empty() {
            return None;
        }
        self.undo.last_mut().map(|e| &mut e.command)
    }

    /// The serial of the step Undo would revert. Serials stay with a step through undo and redo.
    pub fn newest_serial(&self) -> Option<u64> {
        self.undo.last().map(|e| e.serial)
    }

    /// The undo steps newer than the step with `serial`, oldest first; `None` when that step is
    /// no longer on the undo list (undone, or dropped by the history limit).
    pub fn steps_after(&self, serial: u64) -> Option<impl Iterator<Item = &Command>> {
        let at = self.undo.iter().rposition(|e| e.serial == serial)?;
        Some(self.undo[at + 1..].iter().map(|e| &e.command))
    }

    /// The step Undo would revert.
    pub fn peek_undo(&self) -> Option<&Command> {
        self.undo.last().map(|e| &e.command)
    }

    /// Name of the step Undo would revert.
    pub fn undo_name(&self, world: &World) -> Option<String> {
        self.undo.last().map(|e| e.command.name(world))
    }

    /// Name of the step Redo would apply.
    pub fn redo_name(&self, world: &World) -> Option<String> {
        self.redo.last().map(|e| e.command.name(world))
    }

    /// Number of undo steps.
    pub fn len(&self) -> usize {
        self.undo.len()
    }

    /// True when there is nothing to undo.
    pub fn is_empty(&self) -> bool {
        self.undo.is_empty()
    }

    /// True when `id` is detached by a delete or an undone creation (see the module docs).
    pub fn is_parked(&self, id: InstanceId) -> bool {
        self.book.parked.contains(&id)
    }

    /// Every parked instance.
    pub fn parked(&self) -> impl Iterator<Item = InstanceId> + '_ {
        self.book.parked.iter().copied()
    }

    /// Changes on every apply, record, undo and redo (for caches and repaint).
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// Remembers the current step as saved.
    pub fn mark_saved(&mut self) {
        self.saved = self.undo.last().map_or(self.floor, |e| e.serial);
    }

    /// True when the world differs from the last save (or was never saved after an edit).
    pub fn is_dirty(&self) -> bool {
        self.undo.last().map_or(self.floor, |e| e.serial) != self.saved
    }
}

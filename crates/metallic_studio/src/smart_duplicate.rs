//! Smart duplicate (spec 8.1): Ctrl+D, move the copy, Ctrl+D again repeats the same offset and
//! rotation.
//!
//! [`EditorState::duplicate_selection`] leaves a [`SmartDuplicate`] behind: which step made the
//! copies, which selection they are, and where each source part stood. Nothing is reported when
//! the user edits; the next Ctrl+D checks instead, so every way of moving the copy counts (gizmo,
//! body drag, typed delta, Properties) and every other edit, wherever it comes from, ends the
//! chain:
//!
//! - the selection must be the one the duplicate made (its revision has not changed);
//! - every undo step newer than the duplicate must only change `Position`, `Orientation` or
//!   `WorldPivot` of the copies (undoing a move simply removes that step from the check; undoing
//!   the duplicate itself takes its step off the stack and ends the chain).
//!
//! The motion is measured per part from source to copy, so it holds for several parts and for
//! Models, and applied to the copy to place the next one. The new copy and its placement are one
//! undo step, named like any duplicate. Without a motion to repeat (a copy that was not moved, or
//! was moved back onto its source), Ctrl+D is the plain duplicate.
use std::collections::{HashMap, HashSet};

use glam::{DMat3, DQuat, DVec3};
use metallic_core::{CoreError, InstanceId, Value, Vec3, World};

use crate::commands::{Command, PropName, top_level};
use crate::editor::EditorState;
use crate::output::{Level, Tag};
use crate::pivot::{Rigid, models_with_set_pivots, to_cframe};
use crate::selection::Selection;
use crate::tools::arrange::math::orientation_of;
use crate::tools::geom::rotation_from_orientation_degrees;
use crate::transform::{from_d, to_d};

/// The last Ctrl+D, for repeating its motion. Whether it still applies is checked when used.
#[derive(Debug, Clone)]
pub struct SmartDuplicate {
    /// The undo step that made the copies.
    step: u64,
    /// The selection's revision once the copies were selected.
    selection: u64,
    /// The copies (roots of the copied subtrees).
    copies: Vec<InstanceId>,
    /// Every copied part with where its source stood.
    parts: Vec<CopiedPart>,
    /// The copies came from a repeat: they already carry a motion, moved or not.
    repeated: bool,
}

#[derive(Debug, Clone, Copy)]
struct CopiedPart {
    id: InstanceId,
    source_position: Vec3,
    source_orientation: Vec3,
}

fn vec3(world: &World, id: InstanceId, prop: &str) -> Option<Vec3> {
    match world.get_ref(id, prop) {
        Ok(Value::Vector3(v)) => Some(*v),
        _ => None,
    }
}

fn rotation(orientation: Vec3) -> DQuat {
    rotation_from_orientation_degrees(to_d(orientation))
}

impl SmartDuplicate {
    /// Tracks the duplicate of `sources` that was just applied and whose copies are selected.
    /// `None` when there are no parts to measure.
    pub(crate) fn arm(
        state: &EditorState,
        sources: &[InstanceId],
        repeated: bool,
    ) -> Option<SmartDuplicate> {
        let world = &state.world;
        let copies = state.selection.ids().to_vec();
        let roots: Vec<InstanceId> = top_level(world, sources)
            .into_iter()
            .filter(|&id| !world.is_builtin(id))
            .collect();
        if roots.len() != copies.len() {
            return None;
        }
        let mut parts = Vec::new();
        for (&source, &copy) in roots.iter().zip(&copies) {
            let from = Selection::parts_of(world, source);
            let to = Selection::parts_of(world, copy);
            if from.len() != to.len() {
                return None;
            }
            for (s, c) in from.into_iter().zip(to) {
                parts.push(CopiedPart {
                    id: c,
                    source_position: vec3(world, s, "Position")?,
                    source_orientation: vec3(world, s, "Orientation")?,
                });
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(SmartDuplicate {
            step: state.history.newest_serial()?,
            selection: state.selection.revision(),
            copies,
            parts,
            repeated,
        })
    }

    /// `None` when the chain has ended (see the module docs); otherwise whether the copies carry
    /// a motion of their own: they were moved since, or came from a repeat.
    fn moved(&self, state: &EditorState) -> Option<bool> {
        if state.selection.revision() != self.selection {
            return None;
        }
        let copies: HashSet<InstanceId> = self.copies.iter().copied().collect();
        let in_copies = |id: InstanceId| {
            let mut cur = Some(id);
            while let Some(c) = cur {
                if copies.contains(&c) {
                    return true;
                }
                cur = state.world.parent(c);
            }
            false
        };
        let mut moved = self.repeated;
        for step in state.history.steps_after(self.step)? {
            if !step.only_moves(&in_copies) {
                return None;
            }
            moved = true;
        }
        Some(moved)
    }

    /// What took `part`'s source to `part`, written so that it applies to `part` as it stands.
    fn motion(part: &CopiedPart, world: &World) -> Option<Rigid> {
        let position = to_d(vec3(world, part.id, "Position")?);
        let orientation = vec3(world, part.id, "Orientation")?;
        let turn = if orientation == part.source_orientation {
            DQuat::IDENTITY
        } else {
            (rotation(orientation) * rotation(part.source_orientation).inverse()).normalize()
        };
        Some(Rigid {
            rotation: turn,
            about: position,
            translation: turn * (position - to_d(part.source_position)),
        })
    }
}

impl EditorState {
    /// The offset Ctrl+D would repeat now (the Array popover's default, spec 8.2).
    pub fn last_duplicate_offset(&self) -> Option<DVec3> {
        let track = self.smart_duplicate.as_ref()?;
        if track.moved(self) != Some(true) {
            return None;
        }
        let part = track.parts.first()?;
        let offset = to_d(vec3(&self.world, part.id, "Position")?) - to_d(part.source_position);
        (offset.is_finite() && offset.length_squared() > 1e-18).then_some(offset)
    }

    /// Ctrl+D with a motion to repeat: makes the next copy, placed by it, as one undo step.
    /// False when there is none or editing is refused: the caller's plain duplicate takes over
    /// (and reports the refusal).
    pub(crate) fn repeat_duplicate(&mut self) -> bool {
        if self.edit_refusal().is_some() {
            return false;
        }
        let Some(track) = self.smart_duplicate.take() else {
            return false;
        };
        if track.moved(self) != Some(true) {
            return false;
        }
        let motions: Option<Vec<Rigid>> = track
            .parts
            .iter()
            .map(|p| SmartDuplicate::motion(p, &self.world))
            .collect();
        let Some(motions) = motions.filter(|m| !m.iter().all(Rigid::is_identity)) else {
            return false;
        };
        let sources = track.copies.clone();
        let mut duplicate = Command::Duplicate {
            sources: sources.clone(),
            offset: Vec3::new(0.0, 0.0, 0.0),
            copies: Vec::new(),
        };
        if let Err(e) = self.history.apply_pending(&mut duplicate, &mut self.world) {
            self.output.push(Tag::Studio, Level::Warning, e.to_string());
            return true;
        }
        let copies = duplicate.created();
        let parts: Vec<InstanceId> = copies
            .iter()
            .flat_map(|&c| Selection::parts_of(&self.world, c))
            .collect();
        if parts.len() != motions.len() {
            // A move cannot change a subtree's shape; fall back rather than misplace anything.
            self.history.abandon(duplicate, &mut self.world);
            return false;
        }
        let changes = match place_copies(&mut self.world, &copies, &parts, &track.parts, &motions) {
            Ok(changes) => changes,
            Err(e) => {
                self.output.push(Tag::Studio, Level::Warning, e.to_string());
                self.history.abandon(duplicate, &mut self.world);
                return true;
            }
        };
        let name = duplicate.name(&self.world);
        self.record(Command::Batch {
            name,
            commands: vec![
                duplicate,
                Command::SetProperties {
                    name: String::new(),
                    changes,
                },
            ],
        });
        self.selection.set(copies);
        self.smart_duplicate = SmartDuplicate::arm(self, &sources, true);
        true
    }
}

/// One property write of the new copies: (instance, property, old value, new value).
type Write = (InstanceId, PropName, Value, Value);

/// Moves the new copies' `parts` by the `motions` measured on `from`. Their Models' set pivots
/// go with their first part.
fn place_copies(
    world: &mut World,
    copies: &[InstanceId],
    parts: &[InstanceId],
    from: &[CopiedPart],
    motions: &[Rigid],
) -> Result<Vec<Write>, CoreError> {
    let mut writes: Vec<(InstanceId, &str, Value)> = Vec::new();
    let mut motion_of = HashMap::new();
    for ((&part, source), motion) in parts.iter().zip(from).zip(motions) {
        motion_of.insert(part, *motion);
        // The new copy starts where the part it was cloned from stands.
        let (Some(position), Some(orientation)) = (
            vec3(world, source.id, "Position"),
            vec3(world, source.id, "Orientation"),
        ) else {
            continue;
        };
        if motion.rotation != DQuat::IDENTITY {
            let turned = DMat3::from_quat((motion.rotation * rotation(orientation)).normalize());
            writes.push((part, "Orientation", Value::Vector3(orientation_of(&turned))));
        }
        let moved = from_d(motion.point(to_d(position)));
        writes.push((part, "Position", Value::Vector3(moved)));
    }
    for (model, pivot) in models_with_set_pivots(world, copies) {
        let first = Selection::parts_of(world, model).first().copied();
        if let Some(motion) = first.and_then(|p| motion_of.get(&p)) {
            let frame = to_cframe(&motion.frame(&pivot));
            writes.push((model, "WorldPivot", Value::CFrame(Box::new(frame))));
        }
    }
    let mut changes = Vec::with_capacity(writes.len());
    for (id, prop, value) in writes {
        let old = world.get(id, prop)?;
        world.set(id, prop, value.clone())?;
        changes.push((id, PropName::from(prop), old, value));
    }
    Ok(changes)
}

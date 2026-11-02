//! What Play changed, and keeping the changes the creator picks (#130).
//!
//! Stop always returns to the edit world exactly as it was: nothing here runs before Stop has
//! finished, and nothing reaches the edit world until the creator presses Keep. This module has
//! no UI ([`crate::play_review`] draws the offer).
//!
//! **Matching Instances.** [`World::deep_clone`] keeps ids and a generational id is never reused,
//! and the edit world is locked while a session runs. So an id that is live in both worlds is the
//! same Instance, and an id live only in the play world was made during Play. No mapping table.
//! Instances parked by undo were destroyed in the play copy, so they count as absent; a `Ref` to
//! one reads as empty, as it does in the copy.
//!
//! **The diff.** [`compare`] walks both trees once, at Stop. It costs nothing while the game runs.
//! Each Instance becomes at most one [`ChangeItem`], so a list row is one Instance.
//!
//! **What is left out**, because the runtime makes it and a creator cannot have meant it:
//! everything under `Players` (the player, its PlayerGui copies, PlayerScripts), the character
//! each `Player.Character` points at, Cameras (the game writes the camera every frame), the
//! `Position` and `Orientation` of any part that is not anchored in both worlds (physics), a
//! Checkpoint's `Color` (the engine paints a reached checkpoint), a `Ref` emptied only because its
//! target is gone, and the order of siblings. A hidden property ([`PropDesc::hidden`]) never makes
//! a row by itself, but it travels with a row that shows, so a kept `WorldPivot` keeps its flag.
//!
//! **Keeping.** [`PlayReview::keep`] runs the picked changes as parts of one
//! [`Command::Batch`], so one Undo reverts them all. The list holds owned values, not the play
//! world: the game's memory goes at Stop whatever the creator does next.
//!
//! **The cap.** A game can change thousands of things. A group lists at most [`MAX_LISTED`] items
//! and holds at most [`MAX_CREATED_NODES`] created Instances; the rest are counted
//! ([`ChangeGroup::unlisted`]), the offer says so, and they cannot be kept.
//!
//! **Groups.** The list is made of [`ChangeGroup`]s. Stop makes one; Build in Play (#229) adds its
//! own ("Built during Play") without changing the list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use metallic_core::classes::{PropDef, all_properties, is_subclass};
use metallic_core::{InstanceId, Value, World};

use crate::commands::{Command, CommandError, History, Placement, PropName};

/// Most items one group lists. The rest are counted, not listed.
pub const MAX_LISTED: usize = 500;
/// Most created Instances one group holds copies of, subtrees included.
pub const MAX_CREATED_NODES: usize = 20_000;

/// The title of the group Stop makes.
pub const CHANGED_DURING_PLAY: &str = "Changed during Play";

/// One property that differs.
#[derive(Debug, Clone, PartialEq)]
pub struct PropChange {
    /// The property.
    pub name: &'static str,
    /// The value in the edit world.
    pub old: Value,
    /// The value when Play stopped. A `Ref` holds a play world id.
    pub new: Value,
    /// False for a hidden property: applied with the row, never named in it.
    pub shown: bool,
}

/// One attribute that differs (`Nil`: none).
#[derive(Debug, Clone, PartialEq)]
pub struct AttributeChange {
    /// The attribute.
    pub name: String,
    /// The value in the edit world.
    pub old: Value,
    /// The value when Play stopped.
    pub new: Value,
}

/// One tag added (`on`) or removed.
#[derive(Debug, Clone, PartialEq)]
pub struct TagChange {
    /// The tag.
    pub tag: String,
    /// True when Play added it.
    pub on: bool,
}

/// An Instance made during Play, inside a [`ChangeKind::Created`].
#[derive(Debug, Clone, PartialEq)]
pub struct CreatedNode {
    /// Its id in the play world, for `Ref`s and moved Instances that point at it.
    pub play_id: InstanceId,
    /// Class name.
    pub class: &'static str,
    /// Index of its parent in the item's nodes; `None` for the top of the subtree.
    pub parent: Option<usize>,
    /// Properties that differ from the class default.
    pub props: Vec<(&'static str, Value)>,
    /// Attributes.
    pub attributes: Vec<(String, Value)>,
    /// Tags.
    pub tags: Vec<String>,
}

/// What happened to the Instance of a list row.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeKind {
    /// It exists in both worlds and differs.
    Changed {
        /// The Instance (the same id in both worlds).
        id: InstanceId,
        /// Its new parent, when Play moved it (a play world id).
        moved_to: Option<InstanceId>,
        /// Properties that differ.
        props: Vec<PropChange>,
        /// Attributes that differ.
        attributes: Vec<AttributeChange>,
        /// Tags that differ.
        tags: Vec<TagChange>,
    },
    /// Play made it: a whole subtree, parents before children, in sibling order.
    Created {
        /// Where it goes (a play world id).
        parent: InstanceId,
        /// The subtree; the first node is its top.
        nodes: Vec<CreatedNode>,
    },
    /// Play destroyed it, or took it out of the tree.
    Removed {
        /// The Instance.
        id: InstanceId,
        /// How many Instances go with it.
        inside: usize,
    },
}

/// One list row: one Instance and what happened to it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeItem {
    /// The Instance's name.
    pub name: String,
    /// Its class.
    pub class: &'static str,
    /// Where it lives, for the row ("Workspace.Course").
    pub place: String,
    /// What happened.
    pub kind: ChangeKind,
    /// The creator ticked Keep. Off until they do.
    pub keep: bool,
}

impl ChangeItem {
    /// What the row says: "Position, Speed", "New, 3 inside", "Removed".
    pub fn summary(&self) -> String {
        match &self.kind {
            ChangeKind::Changed {
                moved_to,
                props,
                attributes,
                tags,
                ..
            } => {
                let mut parts: Vec<String> = Vec::new();
                if moved_to.is_some() {
                    parts.push("Moved".to_string());
                }
                parts.extend(props.iter().filter(|p| p.shown).map(|p| p.name.to_string()));
                parts.extend(attributes.iter().map(|a| format!("Attribute {}", a.name)));
                parts.extend(tags.iter().map(|t| format!("Tag {}", t.tag)));
                const SHOWN: usize = 4;
                if parts.len() > SHOWN {
                    let more = parts.len() - SHOWN;
                    parts.truncate(SHOWN);
                    parts.push(format!("{more} more"));
                }
                parts.join(", ")
            }
            ChangeKind::Created { nodes, .. } => match nodes.len().saturating_sub(1) {
                0 => "New".to_string(),
                n => format!("New, {n} inside"),
            },
            ChangeKind::Removed { inside, .. } => match inside {
                0 => "Removed".to_string(),
                n => format!("Removed, {n} inside"),
            },
        }
    }

    /// Before and after, one line each, for the row's tooltip.
    pub fn details(&self) -> String {
        let ChangeKind::Changed {
            props,
            attributes,
            tags,
            moved_to,
            ..
        } = &self.kind
        else {
            return self.summary();
        };
        let mut lines = Vec::new();
        if moved_to.is_some() {
            lines.push(format!("Moved to {}", self.place));
        }
        for p in props.iter().filter(|p| p.shown) {
            lines.push(format!(
                "{}: {} \u{2192} {}",
                p.name,
                value_text(&p.old),
                value_text(&p.new)
            ));
        }
        for a in attributes {
            lines.push(format!(
                "Attribute {}: {} \u{2192} {}",
                a.name,
                value_text(&a.old),
                value_text(&a.new)
            ));
        }
        for t in tags {
            let what = if t.on { "added" } else { "removed" };
            lines.push(format!("Tag {} {what}", t.tag));
        }
        lines.join("\n")
    }

    /// The edit world Instance a row can select: not one Play made.
    pub fn edit_id(&self) -> Option<InstanceId> {
        match self.kind {
            ChangeKind::Changed { id, .. } | ChangeKind::Removed { id, .. } => Some(id),
            ChangeKind::Created { .. } => None,
        }
    }
}

/// A value as a short line of text.
fn value_text(v: &Value) -> String {
    let n = |x: f64| {
        let r = (x * 1000.0).round() / 1000.0;
        format!("{r}")
    };
    match v {
        Value::Nil => "none".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(x) => n(*x),
        Value::String(s) => {
            const MOST: usize = 40;
            match s.char_indices().nth(MOST) {
                Some((at, _)) => format!("\"{}\u{2026}\"", &s[..at]),
                None => format!("\"{s}\""),
            }
        }
        Value::Vector3(p) => format!("{}, {}, {}", n(p.x), n(p.y), n(p.z)),
        Value::Color3(c) => format!("{}, {}, {}", n(c.r), n(c.g), n(c.b)),
        Value::UDim2(u) => format!(
            "{}, {}, {}, {}",
            n(u.x_scale),
            n(u.x_offset),
            n(u.y_scale),
            n(u.y_offset)
        ),
        Value::Ref(None) => "empty".to_string(),
        Value::Ref(Some(_)) => "an object".to_string(),
        Value::CFrame(cf) => {
            let p = cf.position;
            format!("at {}, {}, {}", n(p.x), n(p.y), n(p.z))
        }
    }
}

/// A titled part of the list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChangeGroup {
    /// "Changed during Play".
    pub title: String,
    /// The rows.
    pub items: Vec<ChangeItem>,
    /// Changes past the cap: counted, not listed, and they cannot be kept.
    pub unlisted: usize,
}

impl ChangeGroup {
    /// Every change, listed or not.
    pub fn total(&self) -> usize {
        self.items.len() + self.unlisted
    }
}

/// Facts about a class that the diff needs for every Instance of it.
struct ClassFacts {
    props: Vec<&'static PropDef>,
    part: bool,
    checkpoint: bool,
}

impl ClassFacts {
    fn of(class: &str) -> ClassFacts {
        ClassFacts {
            props: all_properties(class),
            part: is_subclass(class, "BasePart"),
            checkpoint: is_subclass(class, "Checkpoint"),
        }
    }
}

static EMPTY_REF: Value = Value::Ref(None);

/// True for an Instance the game writes on its own whatever the creator made.
fn is_engine_class(class: Option<&str>) -> bool {
    class == Some("Camera")
}

struct Differ<'a> {
    edit: &'a World,
    play: &'a World,
    /// Every Instance of the edit tree (parked ones are not in it).
    edit_ids: HashSet<InstanceId>,
    players: Option<InstanceId>,
    /// What each `Player.Character` points at in the play world.
    characters: HashSet<InstanceId>,
    facts: HashMap<&'static str, ClassFacts>,
    group: ChangeGroup,
    created_nodes: usize,
}

impl Differ<'_> {
    /// True when the runtime owns `id` and its subtree in the play world.
    fn runtime_owned(&self, id: InstanceId) -> bool {
        Some(id) == self.players
            || self.characters.contains(&id)
            || is_engine_class(self.play.class_name(id))
    }

    /// True when `id` is under something the runtime owns (or is gone from the play world).
    fn under_runtime(&self, id: InstanceId) -> bool {
        let mut cur = Some(id);
        while let Some(c) = cur {
            if self.runtime_owned(c) {
                return true;
            }
            cur = self.play.parent(c);
        }
        false
    }

    fn list(&mut self, item: impl FnOnce() -> ChangeItem) {
        if self.group.items.len() < MAX_LISTED {
            let item = item();
            self.group.items.push(item);
        } else {
            self.group.unlisted += 1;
        }
    }

    /// The name of `id`, or its class.
    fn name(world: &World, id: InstanceId) -> String {
        match world.get_ref(id, "Name") {
            Ok(Value::String(s)) => s.clone(),
            _ => world.class_name(id).unwrap_or("?").to_string(),
        }
    }

    fn place(world: &World, parent: Option<InstanceId>) -> String {
        parent.map_or_else(String::new, |p| world.full_name(p))
    }

    /// The edit world's value as the play copy reads it: a `Ref` to a parked Instance is empty.
    fn edit_value<'v>(&self, value: &'v Value) -> &'v Value {
        match value {
            Value::Ref(Some(r)) if !self.edit_ids.contains(r) => &EMPTY_REF,
            v => v,
        }
    }

    /// The property changes of `id`, which is in both worlds.
    fn prop_changes(&mut self, id: InstanceId, class: &'static str) -> Vec<PropChange> {
        let facts = self
            .facts
            .entry(class)
            .or_insert_with(|| ClassFacts::of(class));
        // Physics writes these on a part that is not anchored in both worlds.
        let held = !facts.part
            || (matches!(self.edit.get_ref(id, "Anchored"), Ok(Value::Bool(true)))
                && matches!(self.play.get_ref(id, "Anchored"), Ok(Value::Bool(true))));
        let (part, checkpoint) = (facts.part, facts.checkpoint);
        let props = std::mem::take(&mut facts.props);
        let mut out = Vec::new();
        for def in &props {
            let name = def.name;
            let (Ok(old), Ok(new)) = (self.edit.get_ref(id, name), self.play.get_ref(id, name))
            else {
                continue;
            };
            let old = self.edit_value(old);
            if old == new
                || (part && !held && matches!(name, "Position" | "Orientation"))
                || (checkpoint && name == "Color")
            {
                continue;
            }
            match (old, new) {
                // Emptied only because the target went; keeping that removal covers it.
                (Value::Ref(Some(target)), Value::Ref(None)) if !self.play.is_in_tree(*target) => {
                    continue;
                }
                // A reference into the runtime's objects means nothing in the edit world.
                (_, Value::Ref(Some(target))) if self.under_runtime(*target) => continue,
                _ => {}
            }
            out.push(PropChange {
                name,
                old: old.clone(),
                new: new.clone(),
                shown: !def.hidden,
            });
        }
        if let Some(facts) = self.facts.get_mut(class) {
            facts.props = props;
        }
        out
    }

    /// Lists `id` when it differs. `id` is in both worlds.
    fn compare_instance(&mut self, id: InstanceId) {
        let Some(class) = self.play.class_name(id) else {
            return;
        };
        let props = self.prop_changes(id, class);
        let (edit, play) = (self.edit, self.play);
        let mut attributes: Vec<AttributeChange> = play
            .attributes(id)
            .filter(|(name, value)| edit.attribute(id, name) != Some(*value))
            .map(|(name, value)| AttributeChange {
                name: name.to_string(),
                old: edit.attribute(id, name).cloned().unwrap_or(Value::Nil),
                new: value.clone(),
            })
            .collect();
        attributes.extend(
            edit.attributes(id)
                .filter(|(name, _)| play.attribute(id, name).is_none())
                .map(|(name, value)| AttributeChange {
                    name: name.to_string(),
                    old: value.clone(),
                    new: Value::Nil,
                }),
        );
        let mut tags: Vec<TagChange> = play
            .tags(id)
            .filter(|t| !edit.has_tag(id, t))
            .map(|t| TagChange {
                tag: t.to_string(),
                on: true,
            })
            .collect();
        tags.extend(
            edit.tags(id)
                .filter(|t| !play.has_tag(id, t))
                .map(|t| TagChange {
                    tag: t.to_string(),
                    on: false,
                }),
        );
        let parent = play.parent(id);
        let moved_to = parent.filter(|_| parent != edit.parent(id));
        if moved_to.is_none()
            && !props.iter().any(|p| p.shown)
            && attributes.is_empty()
            && tags.is_empty()
        {
            return;
        }
        self.list(|| ChangeItem {
            name: Self::name(play, id),
            class,
            place: Self::place(play, parent),
            kind: ChangeKind::Changed {
                id,
                moved_to,
                props,
                attributes,
                tags,
            },
            keep: false,
        });
    }

    /// One Instance made during Play.
    fn created_node(&self, id: InstanceId, parent: Option<usize>) -> Option<CreatedNode> {
        let class = self.play.class_name(id)?;
        let props = all_properties(class)
            .into_iter()
            .filter_map(|def| {
                let value = self.play.get_ref(id, def.name).ok()?;
                let default = metallic_core::classes::default_value_ref(class, def.name);
                (Some(value) != default).then(|| (def.name, value.clone()))
            })
            .collect();
        Some(CreatedNode {
            play_id: id,
            class,
            parent,
            props,
            attributes: self
                .play
                .attributes(id)
                .map(|(n, v)| (n.to_string(), v.clone()))
                .collect(),
            tags: self.play.tags(id).map(str::to_string).collect(),
        })
    }

    /// Lists the subtree made during Play at `top`, whose parent exists in the edit world.
    /// Instances of the edit world moved into it are not part of it: they go on `walk`.
    fn created_subtree(&mut self, top: InstanceId, walk: &mut Vec<InstanceId>) {
        let budget = MAX_CREATED_NODES.saturating_sub(self.created_nodes);
        let mut holding = self.group.items.len() < MAX_LISTED;
        let mut nodes: Vec<CreatedNode> = Vec::new();
        let mut stack = vec![(top, None)];
        while let Some((id, parent)) = stack.pop() {
            if holding && nodes.len() >= budget {
                holding = false;
                nodes = Vec::new();
            }
            let index = if holding {
                self.created_node(id, parent).map(|node| {
                    nodes.push(node);
                    nodes.len() - 1
                })
            } else {
                None
            };
            let kids = self.play.children(id);
            for &child in kids.iter().rev() {
                if self.edit_ids.contains(&child) {
                    walk.push(child);
                } else if !self.runtime_owned(child) {
                    stack.push((child, index));
                }
            }
        }
        let parent = self.play.parent(top);
        match (holding, parent) {
            (true, Some(parent)) if !nodes.is_empty() => {
                self.created_nodes += nodes.len();
                let play = self.play;
                let class = nodes[0].class;
                self.group.items.push(ChangeItem {
                    name: Self::name(play, top),
                    class,
                    place: Self::place(play, Some(parent)),
                    kind: ChangeKind::Created { parent, nodes },
                    keep: false,
                });
            }
            _ => self.group.unlisted += 1,
        }
    }

    /// Marks `top`'s subtree as accounted for: the runtime owns it.
    fn note_seen_subtree(&self, top: InstanceId, seen: &mut HashSet<InstanceId>) {
        let mut stack = vec![top];
        while let Some(id) = stack.pop() {
            if self.edit_ids.contains(&id) {
                seen.insert(id);
            }
            stack.extend_from_slice(self.play.children(id));
        }
    }

    /// Lists what Play changed or made; returns the edit Instances it found in the play tree.
    fn walk_play(&mut self) -> HashSet<InstanceId> {
        let mut seen = HashSet::with_capacity(self.edit_ids.len());
        // A play root that is not an edit root was detached or made during Play: not in the game.
        let edit = self.edit;
        let mut walk: Vec<InstanceId> = self
            .play
            .roots()
            .iter()
            .rev()
            .copied()
            .filter(|&r| self.edit_ids.contains(&r) && edit.parent(r).is_none())
            .collect();
        while let Some(id) = walk.pop() {
            if self.runtime_owned(id) {
                self.note_seen_subtree(id, &mut seen);
            } else if self.edit_ids.contains(&id) {
                seen.insert(id);
                self.compare_instance(id);
                walk.extend(self.play.children(id).iter().rev().copied());
            } else {
                self.created_subtree(id, &mut walk);
            }
        }
        seen
    }

    /// Lists edit Instances the play tree no longer has: the topmost of each missing subtree.
    fn walk_edit(&mut self, seen: &HashSet<InstanceId>) {
        let edit = self.edit;
        let mut walk: Vec<InstanceId> = edit
            .roots()
            .iter()
            .rev()
            .copied()
            .filter(|r| self.edit_ids.contains(r))
            .collect();
        while let Some(id) = walk.pop() {
            let skip = Some(id) == self.players || is_engine_class(self.edit.class_name(id));
            if skip {
                continue;
            }
            if seen.contains(&id) || edit.is_builtin(id) {
                walk.extend(edit.children(id).iter().rev().copied());
                continue;
            }
            self.list(|| ChangeItem {
                name: Self::name(edit, id),
                class: edit.class_name(id).unwrap_or("Instance"),
                place: Self::place(edit, edit.parent(id)),
                kind: ChangeKind::Removed {
                    id,
                    inside: missing_inside(edit, id, seen),
                },
                keep: false,
            });
        }
    }
}

/// How many Instances under `top` the play tree no longer has.
fn missing_inside(edit: &World, top: InstanceId, seen: &HashSet<InstanceId>) -> usize {
    let mut n = 0;
    let mut stack: Vec<InstanceId> = edit.children(top).to_vec();
    while let Some(id) = stack.pop() {
        if !seen.contains(&id) {
            n += 1;
        }
        stack.extend_from_slice(edit.children(id));
    }
    n
}

/// What `play` changed against `edit`. `edit_roots` are the edit world's visible roots (parked
/// Instances left out). See the module docs for what is matched and what is left out.
pub fn compare(edit: &World, edit_roots: &[InstanceId], play: &World) -> ChangeGroup {
    let mut edit_ids = HashSet::new();
    let mut stack: Vec<InstanceId> = edit_roots.to_vec();
    while let Some(id) = stack.pop() {
        edit_ids.insert(id);
        stack.extend_from_slice(edit.children(id));
    }
    let players = play.root("Players");
    let characters = players
        .into_iter()
        .flat_map(|p| play.children(p).iter().copied())
        .filter_map(|p| match play.get_ref(p, "Character") {
            Ok(Value::Ref(Some(c))) => Some(*c),
            _ => None,
        })
        .collect();
    let mut differ = Differ {
        edit,
        play,
        edit_ids,
        players,
        characters,
        facts: HashMap::new(),
        group: ChangeGroup {
            title: CHANGED_DURING_PLAY.to_string(),
            ..ChangeGroup::default()
        },
        created_nodes: 0,
    };
    let seen = differ.walk_play();
    differ.walk_edit(&seen);
    differ.group
}

/// The offer after Stop: what Play changed, what the creator ticked, and whether the list shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayReview {
    /// The list, group by group.
    pub groups: Vec<ChangeGroup>,
    /// The list is open (else only the strip shows).
    pub open: bool,
    /// [`History::revision`] when Play stopped: any later edit ends the offer.
    pub(crate) revision: u64,
    /// Esc counts only once the key that may have stopped Play has been let go.
    pub(crate) esc_ready: bool,
}

impl PlayReview {
    /// The offer for `groups`, or `None` when nothing changed. `revision` is the history's now.
    pub fn new(groups: Vec<ChangeGroup>, revision: u64) -> Option<PlayReview> {
        groups.iter().any(|g| g.total() > 0).then_some(PlayReview {
            groups,
            open: false,
            revision,
            esc_ready: false,
        })
    }

    /// Every change, listed or not (the N of "Review N changes from Play").
    pub fn total(&self) -> usize {
        self.groups.iter().map(ChangeGroup::total).sum()
    }

    /// Changes past the cap.
    pub fn unlisted(&self) -> usize {
        self.groups.iter().map(|g| g.unlisted).sum()
    }

    /// How many rows the creator ticked.
    pub fn picked(&self) -> usize {
        self.items().filter(|i| i.keep).count()
    }

    /// Every row.
    pub fn items(&self) -> impl Iterator<Item = &ChangeItem> {
        self.groups.iter().flat_map(|g| g.items.iter())
    }

    /// Every row, mutably (Keep boxes; tests).
    pub fn items_mut(&mut self) -> impl Iterator<Item = &mut ChangeItem> {
        self.groups.iter_mut().flat_map(|g| g.items.iter_mut())
    }

    /// Runs the ticked changes on `world` as parts of one step, without pushing it. A part the
    /// world refuses is left out and named in the returned list; the rest still apply. Parents
    /// come first, so a kept `Ref` or move can point at a kept new Instance; removals come last.
    pub(crate) fn keep(
        &self,
        history: &mut History,
        world: &mut World,
    ) -> (Vec<Command>, Vec<String>) {
        let mut run = Runner {
            history,
            world,
            done: Vec::new(),
            failed: Vec::new(),
            created: HashMap::new(),
            made: HashSet::new(),
            kept_refs: Vec::new(),
        };
        let items: Vec<&ChangeItem> = self.items().filter(|i| i.keep).collect();
        for item in &items {
            if let ChangeKind::Created { parent, nodes } = &item.kind {
                run.create(item, *parent, nodes);
            }
        }
        for item in &items {
            if let ChangeKind::Changed {
                id,
                moved_to: Some(to),
                ..
            } = item.kind
            {
                run.reparent(item, id, to);
            }
        }
        for item in &items {
            if let ChangeKind::Changed {
                id,
                props,
                attributes,
                tags,
                ..
            } = &item.kind
            {
                run.set(item, *id, props.iter().map(|p| (p.name, &p.new)));
                for a in attributes {
                    run.step(
                        item,
                        Command::SetAttribute {
                            id: *id,
                            name: a.name.clone(),
                            old: a.old.clone(),
                            new: a.new.clone(),
                        },
                    );
                }
                for t in tags {
                    run.step(
                        item,
                        Command::SetTag {
                            id: *id,
                            tag: t.tag.clone(),
                            on: t.on,
                            changed: false,
                        },
                    );
                }
            }
        }
        for (item, id, name, value) in std::mem::take(&mut run.kept_refs) {
            run.set(item, id, std::iter::once((name, value)));
        }
        for item in &items {
            if let ChangeKind::Removed { id, .. } = item.kind {
                run.step(item, Command::delete(&[id]));
            }
        }
        (run.done, run.failed)
    }
}

/// Runs the parts of the kept step.
struct Runner<'a, 'i> {
    history: &'a mut History,
    world: &'a mut World,
    done: Vec<Command>,
    failed: Vec<String>,
    /// Play id to edit id, for what was created so far.
    created: HashMap<InstanceId, InstanceId>,
    /// The edit ids of `created`.
    made: HashSet<InstanceId>,
    /// `Ref` properties of created Instances, set once every target exists.
    kept_refs: Vec<(&'i ChangeItem, InstanceId, &'static str, &'i Value)>,
}

impl<'i> Runner<'_, 'i> {
    fn step(&mut self, item: &ChangeItem, mut cmd: Command) -> Option<&Command> {
        match self.history.apply_pending(&mut cmd, self.world) {
            Ok(()) => {
                self.done.push(cmd);
                self.done.last()
            }
            Err(e) => {
                self.refused(item, &e);
                None
            }
        }
    }

    fn refused(&mut self, item: &ChangeItem, e: &CommandError) {
        self.failed.push(format!("{}: {e}", item.name));
    }

    /// Where a play world id lands in the edit world: a kept new Instance, or the same Instance.
    /// `None` for a new Instance that was not kept. Both worlds hand out ids from the same free
    /// list, so an unkept Instance's id can equal one this step just made: that is not it.
    fn resolve(&self, id: InstanceId) -> Option<InstanceId> {
        self.created
            .get(&id)
            .copied()
            .or_else(|| (self.world.is_alive(id) && !self.made.contains(&id)).then_some(id))
    }

    fn create(&mut self, item: &'i ChangeItem, parent: InstanceId, nodes: &'i [CreatedNode]) {
        let mut ids: Vec<Option<InstanceId>> = Vec::with_capacity(nodes.len());
        for node in nodes {
            let parent = match node.parent {
                Some(i) => ids.get(i).copied().flatten(),
                None => self.resolve(parent),
            };
            let Some(parent) = parent else {
                ids.push(None);
                continue;
            };
            let props = node
                .props
                .iter()
                .filter(|(_, v)| !matches!(v, Value::Ref(_)))
                .map(|(n, v)| ((*n).to_string(), v.clone()))
                .collect();
            let cmd = Command::Create {
                class: node.class.to_string(),
                parent,
                props,
                id: None,
            };
            let made = self
                .step(item, cmd)
                .and_then(|c| c.created().first().copied());
            ids.push(made);
            let Some(id) = made else { continue };
            self.created.insert(node.play_id, id);
            self.made.insert(id);
            for (name, value) in &node.props {
                if matches!(value, Value::Ref(_)) {
                    self.kept_refs.push((item, id, *name, value));
                }
            }
            for (name, value) in &node.attributes {
                self.step(
                    item,
                    Command::SetAttribute {
                        id,
                        name: name.clone(),
                        old: Value::Nil,
                        new: value.clone(),
                    },
                );
            }
            for tag in &node.tags {
                self.step(
                    item,
                    Command::SetTag {
                        id,
                        tag: tag.clone(),
                        on: true,
                        changed: false,
                    },
                );
            }
        }
    }

    fn reparent(&mut self, item: &ChangeItem, id: InstanceId, to: InstanceId) {
        // Moved into a new Instance that was not kept: it stays where it is.
        let Some(to) = self.resolve(to) else { return };
        self.step(
            item,
            Command::Reparent {
                from: vec![Placement {
                    id,
                    parent: None,
                    index: 0,
                }],
                to,
            },
        );
    }

    fn set<'v>(
        &mut self,
        item: &ChangeItem,
        id: InstanceId,
        values: impl Iterator<Item = (&'static str, &'v Value)>,
    ) {
        let mut changes: Vec<(InstanceId, PropName, Value, Value)> = Vec::new();
        for (name, value) in values {
            let new = match value {
                // A reference to a new Instance that was not kept ends up empty.
                Value::Ref(Some(r)) => Value::Ref(self.resolve(*r)),
                v => v.clone(),
            };
            let Ok(old) = self.world.get(id, name) else {
                continue;
            };
            if old != new {
                changes.push((id, Arc::from(name), old, new));
            }
        }
        if !changes.is_empty() {
            self.step(
                item,
                Command::SetProperties {
                    name: format!("Keep {}", item.name),
                    changes,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use metallic_core::Vec3;

    fn level() -> (World, InstanceId, InstanceId) {
        let mut world = World::new_with_services();
        let ws = world.root("Workspace").unwrap();
        let a = world.create("Part", Some(ws)).unwrap();
        world.set(a, "Name", Value::String("A".into())).unwrap();
        world.set(a, "Anchored", Value::Bool(true)).unwrap();
        let b = world.create("Part", Some(ws)).unwrap();
        world.set(b, "Name", Value::String("B".into())).unwrap();
        (world, a, b)
    }

    fn roots(world: &World) -> Vec<InstanceId> {
        world.roots().to_vec()
    }

    fn found(edit: &World, play: &World) -> ChangeGroup {
        compare(edit, &roots(edit), play)
    }

    #[test]
    fn a_copy_nobody_touched_has_no_changes() {
        let (edit, ..) = level();
        assert_eq!(found(&edit, &edit.deep_clone()).total(), 0);
    }

    #[test]
    fn physics_moves_only_count_on_parts_anchored_in_both_worlds() {
        let (edit, anchored, loose) = level();
        let mut play = edit.deep_clone();
        let to = Value::Vector3(Vec3::new(5.0, 5.0, 5.0));
        play.set(loose, "Position", to.clone()).unwrap();
        play.set(loose, "Orientation", to.clone()).unwrap();
        assert_eq!(found(&edit, &play).total(), 0, "physics moved a loose part");
        play.set(anchored, "Position", to.clone()).unwrap();
        let group = found(&edit, &play);
        assert_eq!(group.total(), 1);
        assert_eq!(group.items[0].name, "A");
        assert_eq!(group.items[0].summary(), "Position");
        // Unanchored during Play: it may have fallen, so its place is not a change either.
        play.set(anchored, "Anchored", Value::Bool(false)).unwrap();
        assert_eq!(found(&edit, &play).items[0].summary(), "Anchored");
    }

    #[test]
    fn a_hidden_property_alone_lists_nothing_but_travels_with_a_row() {
        let mut edit = World::new_with_services();
        let ws = edit.root("Workspace").unwrap();
        let model = edit.create("Model", Some(ws)).unwrap();
        let mut play = edit.deep_clone();
        play.set(model, "ScaleFactor", Value::Number(2.0)).unwrap();
        assert_eq!(found(&edit, &play).total(), 0);
        play.set(model, "Name", Value::String("Big".into()))
            .unwrap();
        let group = found(&edit, &play);
        assert_eq!(group.items[0].summary(), "Name");
        let ChangeKind::Changed { props, .. } = &group.items[0].kind else {
            panic!("expected a change");
        };
        assert_eq!(props.len(), 2, "the hidden property stays with the row");
    }

    #[test]
    fn runtime_objects_are_left_out() {
        let (edit, ..) = level();
        let mut play = edit.deep_clone();
        let players = play.root("Players").unwrap();
        let player = play.create("Player", Some(players)).unwrap();
        let gui = play.create("PlayerGui", Some(player)).unwrap();
        play.create("ScreenGui", Some(gui)).unwrap();
        let ws = play.root("Workspace").unwrap();
        let character = play.create("Model", Some(ws)).unwrap();
        play.create("Part", Some(character)).unwrap();
        play.set(player, "Character", Value::Ref(Some(character)))
            .unwrap();
        play.create("Camera", Some(ws)).unwrap();
        assert_eq!(found(&edit, &play).total(), 0);
    }

    #[test]
    fn a_checkpoint_painted_by_the_engine_is_not_a_change() {
        let mut edit = World::new_with_services();
        let ws = edit.root("Workspace").unwrap();
        let cp = edit.create("Checkpoint", Some(ws)).unwrap();
        let mut play = edit.deep_clone();
        play.set(
            cp,
            "Color",
            Value::Color3(metallic_core::Color3::new(1.0, 2.0, 3.0)),
        )
        .unwrap();
        assert_eq!(found(&edit, &play).total(), 0);
    }

    #[test]
    fn instances_parked_by_undo_are_not_counted_as_removed() {
        let (edit, a, b) = level();
        let ws = edit.root("Workspace").unwrap();
        let mut parked = edit.deep_clone();
        parked.set_parent(a, None).unwrap();
        let mut play = parked.deep_clone();
        play.destroy(a);
        let visible: Vec<InstanceId> = parked.roots().iter().copied().filter(|&r| r != a).collect();
        assert_eq!(compare(&parked, &visible, &play).total(), 0);
        assert!(play.is_alive(b));
        assert!(play.is_alive(ws));
    }

    #[test]
    fn a_ref_to_a_parked_instance_reads_empty_as_in_the_play_copy() {
        let mut edit = World::new_with_services();
        let ws = edit.root("Workspace").unwrap();
        let model = edit.create("Model", Some(ws)).unwrap();
        let part = edit.create("Part", Some(model)).unwrap();
        edit.set(model, "PrimaryPart", Value::Ref(Some(part)))
            .unwrap();
        edit.set_parent(part, None).unwrap();
        let mut play = edit.deep_clone();
        play.destroy(part);
        let visible: Vec<InstanceId> = edit
            .roots()
            .iter()
            .copied()
            .filter(|&r| r != part)
            .collect();
        assert_eq!(compare(&edit, &visible, &play).total(), 0);
    }

    #[test]
    fn a_removed_subtree_is_one_row_and_a_ref_it_empties_is_not_another() {
        let mut edit = World::new_with_services();
        let ws = edit.root("Workspace").unwrap();
        let model = edit.create("Model", Some(ws)).unwrap();
        let part = edit.create("Part", Some(model)).unwrap();
        let other = edit.create("Model", Some(ws)).unwrap();
        edit.set(other, "PrimaryPart", Value::Ref(Some(part)))
            .unwrap();
        let mut play = edit.deep_clone();
        play.destroy(model);
        let group = found(&edit, &play);
        assert_eq!(group.total(), 1);
        assert_eq!(group.items[0].summary(), "Removed, 1 inside");
    }

    #[test]
    fn a_detached_instance_counts_as_removed() {
        let (edit, a, _) = level();
        let mut play = edit.deep_clone();
        play.set_parent(a, None).unwrap();
        let group = found(&edit, &play);
        assert_eq!(group.items.len(), 1);
        assert!(matches!(group.items[0].kind, ChangeKind::Removed { id, .. } if id == a));
    }

    #[test]
    fn a_created_top_lists_with_its_subtree_and_a_created_detached_one_not_at_all() {
        let (edit, ..) = level();
        let mut play = edit.deep_clone();
        let ws = play.root("Workspace").unwrap();
        let model = play.create("Model", Some(ws)).unwrap();
        play.set(model, "Name", Value::String("Made".into()))
            .unwrap();
        play.create("Part", Some(model)).unwrap();
        play.create("Part", None).unwrap();
        let group = found(&edit, &play);
        assert_eq!(group.total(), 1);
        assert_eq!(group.items[0].summary(), "New, 1 inside");
        assert_eq!(group.items[0].name, "Made");
    }

    #[test]
    fn past_the_cap_changes_are_counted_not_listed() {
        let (edit, ..) = level();
        let mut play = edit.deep_clone();
        let ws = play.root("Workspace").unwrap();
        for _ in 0..MAX_LISTED + 25 {
            play.create("Part", Some(ws)).unwrap();
        }
        let group = found(&edit, &play);
        assert_eq!(group.items.len(), MAX_LISTED);
        assert_eq!(group.unlisted, 25);
        assert_eq!(group.total(), MAX_LISTED + 25);
    }

    #[test]
    fn a_subtree_past_the_node_budget_is_counted_not_held() {
        let (edit, ..) = level();
        let mut play = edit.deep_clone();
        let ws = play.root("Workspace").unwrap();
        let big = play.create("Folder", Some(ws)).unwrap();
        for _ in 0..MAX_CREATED_NODES + 1 {
            play.create("Folder", Some(big)).unwrap();
        }
        let group = found(&edit, &play);
        assert!(group.items.is_empty());
        assert_eq!(group.unlisted, 1);
    }

    #[test]
    fn the_offer_exists_only_when_something_changed() {
        assert!(PlayReview::new(vec![ChangeGroup::default()], 0).is_none());
        let group = ChangeGroup {
            unlisted: 1,
            ..ChangeGroup::default()
        };
        assert_eq!(PlayReview::new(vec![group], 0).map(|r| r.total()), Some(1));
    }

    #[test]
    fn a_row_says_what_changed_and_shortens_a_long_list() {
        let item = ChangeItem {
            name: "P".into(),
            class: "Part",
            place: String::new(),
            kind: ChangeKind::Changed {
                id: InstanceId::from_bits(1),
                moved_to: None,
                props: ["Position", "Size", "Color", "Name", "Shape", "Material"]
                    .into_iter()
                    .map(|name| PropChange {
                        name,
                        old: Value::Bool(false),
                        new: Value::Bool(true),
                        shown: true,
                    })
                    .collect(),
                attributes: Vec::new(),
                tags: Vec::new(),
            },
            keep: false,
        };
        assert_eq!(item.summary(), "Position, Size, Color, Name, 2 more");
    }
}

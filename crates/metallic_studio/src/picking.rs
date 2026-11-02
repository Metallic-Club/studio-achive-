//! Viewport picking: which part is under the cursor.
//!
//! [`PickCache`] keeps the transform of every BasePart under `Workspace` and refreshes only the
//! instances the world's change journal reports, so hover tests each frame do not re-read
//! thousands of properties. Rays test oriented boxes (spheres for Ball parts) on the CPU.
use std::collections::HashMap;

use metallic_core::{ChangeCursor, Changes, InstanceId, Value, World};

use metallic_render::glam::DVec3;

use crate::transform::{PartTransform, Ray};

#[derive(Debug, Clone, Copy)]
struct Entry {
    id: InstanceId,
    transform: PartTransform,
    ball: bool,
}

/// One ray hit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hit {
    /// The part hit.
    pub id: InstanceId,
    /// Distance along the ray.
    pub distance: f64,
}

/// Cached part boxes for ray tests.
#[derive(Default)]
pub struct PickCache {
    entries: Vec<Entry>,
    index: HashMap<InstanceId, usize>,
    cursor: Option<ChangeCursor>,
}

impl PickCache {
    /// An empty cache; the first [`PickCache::sync`] reads every part.
    pub fn new() -> PickCache {
        PickCache::default()
    }

    /// Brings the cache up to date with `world`.
    pub fn sync(&mut self, world: &World) {
        match self.cursor.map(|c| world.changes_since(c)) {
            Some((Changes::Ids(ids), cursor)) => {
                for id in ids {
                    self.update(world, id);
                }
                self.cursor = Some(cursor);
            }
            _ => self.rebuild(world),
        }
    }

    fn rebuild(&mut self, world: &World) {
        self.entries.clear();
        self.index.clear();
        if let Some(ws) = world.root("Workspace") {
            let mut stack = vec![ws];
            while let Some(id) = stack.pop() {
                if let Some(e) = entry(world, id) {
                    self.index.insert(id, self.entries.len());
                    self.entries.push(e);
                }
                stack.extend_from_slice(world.children(id));
            }
        }
        self.cursor = Some(world.change_cursor());
    }

    fn update(&mut self, world: &World, id: InstanceId) {
        let want = if in_workspace(world, id) {
            entry(world, id)
        } else {
            None
        };
        match (self.index.get(&id).copied(), want) {
            (Some(i), Some(e)) => self.entries[i] = e,
            (Some(i), None) => {
                self.entries.swap_remove(i);
                self.index.remove(&id);
                if let Some(moved) = self.entries.get(i) {
                    self.index.insert(moved.id, i);
                }
            }
            (None, Some(e)) => {
                self.index.insert(id, self.entries.len());
                self.entries.push(e);
            }
            (None, None) => {}
        }
    }

    /// Number of cached parts.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no part is cached.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The nearest part hit by `ray`, skipping `ignore`.
    pub fn first(&self, ray: &Ray, ignore: &[InstanceId]) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for e in &self.entries {
            if ignore.contains(&e.id) {
                continue;
            }
            if let Some(d) = hit(e, ray)
                && best.is_none_or(|b| d < b.distance)
            {
                best = Some(Hit {
                    id: e.id,
                    distance: d,
                });
            }
        }
        best
    }

    /// The nearest part hit by `ray` for which `skip` is false, with the outward normal of the
    /// face hit (sphere normal for Ball parts).
    pub fn surface(&self, ray: &Ray, skip: &dyn Fn(InstanceId) -> bool) -> Option<(Hit, DVec3)> {
        let mut best: Option<(Hit, DVec3)> = None;
        for e in &self.entries {
            if skip(e.id) {
                continue;
            }
            let found = if e.ball {
                e.transform.ray_hit_sphere(ray).map(|d| {
                    let n = (ray.at(d) - e.transform.position).normalize_or(-ray.dir);
                    (d, n)
                })
            } else {
                e.transform.ray_hit_normal(ray)
            };
            if let Some((d, n)) = found
                && best.is_none_or(|(b, _)| d < b.distance)
            {
                best = Some((
                    Hit {
                        id: e.id,
                        distance: d,
                    },
                    n,
                ));
            }
        }
        best
    }

    /// Indices of the parts whose bounding sphere reaches within `radius` of `centre`, written
    /// into `out` (cleared first). A cheap prefilter for many rays in one small region (the
    /// brush): test those rays with [`PickCache::first_in`].
    pub fn near(&self, centre: DVec3, radius: f64, out: &mut Vec<usize>) {
        out.clear();
        for (i, e) in self.entries.iter().enumerate() {
            let reach = radius + e.transform.size.length() * 0.5;
            if e.transform.position.distance_squared(centre) <= reach * reach {
                out.push(i);
            }
        }
    }

    /// The nearest hit by `ray` among the parts `subset` (indices from [`PickCache::near`]).
    pub fn first_in(&self, ray: &Ray, subset: &[usize]) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        for e in subset.iter().filter_map(|&i| self.entries.get(i)) {
            if let Some(d) = hit(e, ray)
                && best.is_none_or(|b| d < b.distance)
            {
                best = Some(Hit {
                    id: e.id,
                    distance: d,
                });
            }
        }
        best
    }

    /// Every cached part: id, transform and whether it is a Ball.
    pub fn iter(&self) -> impl Iterator<Item = (InstanceId, &PartTransform, bool)> {
        self.entries.iter().map(|e| (e.id, &e.transform, e.ball))
    }

    /// Every part hit by `ray`, nearest first, written into `out` (cleared first).
    pub fn all(&self, ray: &Ray, out: &mut Vec<Hit>) {
        out.clear();
        for e in &self.entries {
            if let Some(d) = hit(e, ray) {
                out.push(Hit {
                    id: e.id,
                    distance: d,
                });
            }
        }
        out.sort_by(|a, b| a.distance.total_cmp(&b.distance));
    }

    /// The cached transform of a part.
    pub fn transform(&self, id: InstanceId) -> Option<PartTransform> {
        self.index.get(&id).map(|&i| self.entries[i].transform)
    }
}

fn hit(e: &Entry, ray: &Ray) -> Option<f64> {
    if e.ball {
        e.transform.ray_hit_sphere(ray)
    } else {
        e.transform.ray_hit(ray)
    }
}

fn entry(world: &World, id: InstanceId) -> Option<Entry> {
    if !world.is_a(id, "BasePart") {
        return None;
    }
    let transform = PartTransform::read(world, id)?;
    let ball = matches!(world.get_ref(id, "Shape"), Ok(Value::String(s)) if s == "Ball");
    Some(Entry {
        id,
        transform,
        ball,
    })
}

fn in_workspace(world: &World, id: InstanceId) -> bool {
    let Some(ws) = world.root("Workspace") else {
        return false;
    };
    let mut cur = world.parent(id);
    while let Some(c) = cur {
        if c == ws {
            return true;
        }
        cur = world.parent(c);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use metallic_core::Vec3;

    fn part(world: &mut World, pos: Vec3) -> InstanceId {
        let ws = world.root("Workspace").unwrap();
        let p = world.create("Part", Some(ws)).unwrap();
        world.set(p, "Position", Value::Vector3(pos)).unwrap();
        p
    }

    #[test]
    fn near_prefilters_and_first_in_matches_first_on_the_subset() {
        let mut w = World::new_with_services();
        let close = part(&mut w, Vec3::new(0.0, 0.0, 0.0));
        let _far = part(&mut w, Vec3::new(500.0, 0.0, 0.0));
        let mut c = PickCache::new();
        c.sync(&w);
        let mut near = Vec::new();
        c.near(DVec3::new(0.0, 5.0, 0.0), 8.0, &mut near);
        assert_eq!(near.len(), 1, "only the close part");
        let down = Ray {
            origin: DVec3::new(0.0, 10.0, 0.0),
            dir: DVec3::NEG_Y,
        };
        assert_eq!(c.first_in(&down, &near), c.first(&down, &[]));
        assert_eq!(c.first_in(&down, &near).map(|h| h.id), Some(close));
        assert_eq!(c.first_in(&down, &[]), None);
    }

    #[test]
    fn nearest_hit_and_journal_updates() {
        let mut world = World::new_with_services();
        let low = part(&mut world, Vec3::new(0.0, 0.0, 0.0));
        let high = part(&mut world, Vec3::new(0.0, 5.0, 0.0));
        let mut cache = PickCache::new();
        cache.sync(&world);
        let down = Ray {
            origin: DVec3::new(0.0, 20.0, 0.0),
            dir: DVec3::NEG_Y,
        };
        assert_eq!(cache.first(&down, &[]).unwrap().id, high);
        assert_eq!(cache.first(&down, &[high]).unwrap().id, low);
        world
            .set(high, "Position", Value::Vector3(Vec3::new(50.0, 5.0, 0.0)))
            .unwrap();
        cache.sync(&world);
        assert_eq!(cache.first(&down, &[]).unwrap().id, low);
        world.set_parent(low, None).unwrap();
        cache.sync(&world);
        assert!(cache.first(&down, &[]).is_none());
        assert_eq!(cache.len(), 1);
    }
}

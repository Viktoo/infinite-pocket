//! The live layer: things that move, burn, grow or act. Most of the world is
//! static and versioned (story instances, procedural scatter); a thing joins
//! the live layer the moment it is touched, thrown, set alight, or has
//! behaviour code. Live things are saved continuously, not versioned.

use super::ActorId;
use super::props::*;
use crate::world::TypeEntry;
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub type ThingId = i64;

/// Where a live thing came from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Origin {
    /// Story instance it was promoted from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<i64>,
    /// Scatter cell it was picked from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cell: Option<(i32, i32)>,
    /// Who made it ("the traveler", a character's name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub made_by: Option<String>,
    /// What it was made from (names).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub made_from: Vec<String>,
    /// The being whose body this is (it died; see `remains`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remains: Option<i64>,
}

/// One change to what a thing is (cut, reshaped), and by whom.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct EditRec {
    pub t: f64,
    pub by: String,
    pub what: String,
}

/// How a thing differs from a fresh one of its type: cuts and history.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Shape {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cuts: Vec<[f32; 4]>,
    /// Straight holes along a line, in its local frame: from a.xyz to b.xyz,
    /// radius a.w (a shot that went through a plank).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bores: Vec<[f32; 8]>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub edits: Vec<EditRec>,
    /// Lying tilted (x, y, z, w of a rotation after the yaw), when not upright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tilt: Option<[f32; 4]>,
    /// How far open its joint is (0 shut … 1 open), and where it is going.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub open: f32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub open_to: f32,
    /// Driven in where it struck (an arrow in a tree, a spear in the
    /// ground): it holds fast there until someone pulls it out.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stuck: bool,
    /// Lit: the game time it goes off (`meta.burst`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fuse_at: Option<f64>,
    /// Shots a launcher has sent off since it was last full.
    #[serde(default, skip_serializing_if = "is_zero_u")]
    pub spent: u32,
    /// The types of shots loaded into a launcher by hand, the last loaded
    /// on top: they go first (fire arrows put in a bow fly as fire arrows).
    /// Shots beyond these are its own ammunition.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loaded: Vec<u32>,
}

fn is_zero_u(v: &u32) -> bool {
    *v == 0
}

/// Driven into a body: where, in the body's own frame (x right, y up,
/// z forward), and how it lies against it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StuckIn {
    pub who: ActorId,
    pub off: Vec3,
    /// Its turn relative to the body's (after the body's yaw).
    pub turn: glam::Quat,
    /// When it works loose.
    pub until: f64,
}

fn is_zero(v: &f32) -> bool {
    *v == 0.0
}

impl Shape {
    pub fn is_empty(&self) -> bool {
        self.cuts.is_empty() && self.bores.is_empty() && self.edits.is_empty() && self.tilt.is_none() && self.open == 0.0 && self.open_to == 0.0 && !self.stuck && self.fuse_at.is_none() && self.spent == 0 && self.loaded.is_empty()
    }
}

pub const MAX_EDITS: usize = 16;

#[derive(Clone, Debug)]
pub struct Thing {
    pub id: ThingId,
    pub type_id: u32,
    /// Origin position (as for a placed instance: the type's local origin).
    pub pos: Vec3,
    pub vel: Vec3,
    pub yaw: f32,
    pub spin: f32,
    pub scale: f32,
    pub params: [f32; 8],
    /// Behaviour state, visible to the shape as k.s0 … k.s7.
    pub state: [f32; 8],
    /// Look sliders past the first five (k.g … k.z): a body's remains keep
    /// its face, hair and clothes. Empty for most things.
    pub look: Vec<f32>,
    pub props: Props,
    pub holder: Option<ActorId>,
    /// A second holder carrying it together with the first.
    pub co_holder: Option<ActorId>,
    /// Worn as a layer (clothing, armour) by someone.
    pub worn: Option<ActorId>,
    pub asleep: bool,
    /// Never moves (buildings, very heavy things).
    pub anchored: bool,
    pub origin: Origin,
    /// Game time it joined the live layer.
    pub born: f64,
    /// Who last threw it, and when.
    pub thrown_by: Option<(ActorId, f64)>,
    pub rest_t: f32,
    pub in_contact: bool,
    pub removed: bool,
    pub dirty: bool,
    /// Last behaviour tick (game time).
    pub last_tick: f64,
    /// Recently fired rules (name, game time), newest last.
    pub fired: Vec<(String, f64)>,
    /// Earliest game time behaviour code may spawn again.
    pub cooldown: f64,
    /// The type it last fell through (one "through" per throw).
    pub through: Option<u32>,
    /// Cuts and the history of changes.
    pub shape: Shape,
    /// How the hand holds it this moment (a tool mid-swing); not saved.
    pub hold_tilt: Option<glam::Quat>,
    /// In flight point first (a thrown spear, a loosed arrow): it turns to
    /// face the way it goes, and strikes with its tip. Not saved.
    pub point_first: bool,
    /// Sent off by a launcher (spent shots too small to see are cleared
    /// away once they come to rest). Not saved.
    pub shot: bool,
    /// Driven into someone's body. Not saved (it works loose).
    pub stuck_in: Option<StuckIn>,
}

impl Thing {
    pub fn new(id: ThingId, ty: &TypeEntry, pos: Vec3, yaw: f32, scale: f32, params: [f32; 8], props: Props, born: f64) -> Thing {
        // A vehicle, however heavy, rolls.
        let anchored = ty.ct.meta.drive.is_none() && (props[P_MASS] >= ANCHOR_MASS || ty.has_tag("building") || ty.has_tag("landmark") || ty.ct.meta.joint.is_some());
        Thing {
            id,
            type_id: ty.id,
            pos,
            vel: Vec3::ZERO,
            yaw,
            spin: 0.0,
            scale,
            params,
            look: Vec::new(),
            state: [0.0; 8],
            props,
            holder: None,
            co_holder: None,
            worn: None,
            asleep: true,
            anchored,
            origin: Origin::default(),
            born,
            thrown_by: None,
            rest_t: 0.0,
            in_contact: true,
            removed: false,
            dirty: true,
            last_tick: born,
            fired: Vec::new(),
            cooldown: 0.0,
            through: None,
            shape: Shape::default(),
            hold_tilt: None,
            point_first: false,
            shot: false,
            stuck_in: None,
        }
    }

    /// Remember a change (newest last, the oldest forgotten).
    pub fn note_edit(&mut self, t: f64, by: &str, what: &str) {
        self.shape.edits.push(EditRec { t, by: by.to_string(), what: what.to_string() });
        if self.shape.edits.len() > MAX_EDITS {
            self.shape.edits.remove(0);
        }
        self.dirty = true;
    }

    /// The first cuts, as each instance holds them.
    pub fn gpu_cuts(&self) -> [[f32; 4]; crate::render::MAX_CUTS] {
        let mut out = [[0.0; 4]; crate::render::MAX_CUTS];
        for (i, c) in self.shape.cuts.iter().take(crate::render::MAX_CUTS).enumerate() {
            out[i] = *c;
        }
        out
    }

    /// The cuts past those (a wall shot full of holes).
    pub fn more_cuts(&self) -> &[[f32; 4]] {
        self.shape.cuts.get(crate::render::MAX_CUTS..).unwrap_or(&[])
    }

    /// Those and its bores, for its CPU shape; None (no allocation) when it has neither.
    pub fn holes(&self) -> Option<std::sync::Arc<crate::world::Holes>> {
        let cuts = self.more_cuts();
        (!cuts.is_empty() || !self.shape.bores.is_empty()).then(|| std::sync::Arc::new(crate::world::Holes { cuts: cuts.to_vec(), bores: self.shape.bores.clone() }))
    }

    /// Held or worn: not lying about.
    pub fn held(&self) -> bool {
        self.holder.is_some() || self.worn.is_some()
    }

    /// Held fast where it struck (in a tree, the ground, a wall, a body):
    /// it doesn't fall, but a hand pulls it free.
    pub fn stuck(&self) -> bool {
        self.shape.stuck || self.stuck_in.is_some()
    }

    pub fn mass(&self) -> f32 {
        self.props[P_MASS]
    }

    /// Holdable by one person / by two together.
    pub fn liftable(&self, holders: usize) -> bool {
        (!self.anchored || self.shape.stuck) && self.mass() <= STRENGTH * holders as f32
    }

    /// Its tilt after the yaw (upright: identity).
    pub fn tilt(&self) -> glam::Quat {
        self.shape.tilt.map(glam::Quat::from_array).filter(|q| q.is_finite() && q.length_squared() > 0.5).map(|q| q.normalize()).unwrap_or(glam::Quat::IDENTITY)
    }

    pub fn set_tilt(&mut self, q: glam::Quat) {
        let q = q.normalize();
        self.shape.tilt = if q.is_finite() && q.angle_between(glam::Quat::IDENTITY) > 1e-3 { Some(q.to_array()) } else { None };
    }

    /// Physics proxy: a sphere around the centre of the shape.
    pub fn proxy(&self, ty: &TypeEntry) -> (Vec3, f32) {
        let b = ty.ct.meta.bounds;
        let r = (b[0].min(b[1]).min(b[2]) * self.scale).clamp(0.03, 1.2);
        let cy = (ty.bottom + r / self.scale.max(0.01)) * self.scale;
        (self.pos + Vec3::Y * cy, r)
    }

    /// The origin height that puts the shape's lowest point on `ground`.
    pub fn rest_y(&self, ty: &TypeEntry, ground: f32) -> f32 {
        ground - ty.bottom * self.scale
    }

    pub fn note_rule(&mut self, name: &str, t: f64) {
        if self.fired.last().is_some_and(|(n, _)| n == name) {
            if let Some(last) = self.fired.last_mut() {
                last.1 = t;
            }
            return;
        }
        self.fired.push((name.to_string(), t));
        if self.fired.len() > 8 {
            self.fired.remove(0);
        }
    }
}

/// All live things, in id order (iteration order is part of determinism).
#[derive(Default)]
pub struct Things {
    pub map: BTreeMap<ThingId, Thing>,
    pub next_id: ThingId,
    grid: HashMap<(i32, i32), Vec<ThingId>>,
    grid_dirty: bool,
    /// Story instances that live here now (hidden from the static layer).
    pub by_instance: HashMap<i64, ThingId>,
    /// Scatter cells whose item was taken into the live layer.
    pub taken: HashMap<(i32, i32), ThingId>,
    /// Removed ids not yet deleted from the database.
    pub removed: Vec<ThingId>,
    /// Saved things kept back on a quick start until the whole world (their
    /// kind, the placed thing they came from) has come.
    pub waiting: Vec<ThingId>,
    /// Scatter cells whose item was used up (eaten, burnt), and when.
    pub spent_cells: std::collections::BTreeMap<(i32, i32), f64>,
}

const GRID: f32 = 8.0;

fn gcell(p: Vec3) -> (i32, i32) {
    ((p.x / GRID).floor() as i32, (p.z / GRID).floor() as i32)
}

impl Things {
    pub fn alloc(&mut self) -> ThingId {
        self.next_id = self.next_id.max(1);
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn insert(&mut self, t: Thing) {
        if let Some(i) = t.origin.instance {
            self.by_instance.insert(i, t.id);
        }
        if let Some(c) = t.origin.cell {
            self.taken.insert(c, t.id);
        }
        self.next_id = self.next_id.max(t.id + 1);
        self.map.insert(t.id, t);
        self.grid_dirty = true;
    }

    pub fn get(&self, id: ThingId) -> Option<&Thing> {
        self.map.get(&id).filter(|t| !t.removed)
    }

    pub fn get_mut(&mut self, id: ThingId) -> Option<&mut Thing> {
        self.map.get_mut(&id).filter(|t| !t.removed)
    }

    /// Remove a thing. The story instance or scatter item it came from stays
    /// hidden (it was moved, burnt or eaten, not undone).
    pub fn remove(&mut self, id: ThingId) {
        if let Some(t) = self.map.get_mut(&id) {
            if let Some(c) = t.origin.cell {
                self.spent_cells.insert(c, t.born.max(t.last_tick));
                self.taken.insert(c, -1);
            }
            t.removed = true;
            t.holder = None;
            t.co_holder = None;
            self.removed.push(id);
            self.grid_dirty = true;
        }
    }

    /// Forget a thing completely (its source was undone): the source shows again.
    pub fn forget(&mut self, id: ThingId) {
        if let Some(t) = self.map.remove(&id) {
            if let Some(i) = t.origin.instance {
                self.by_instance.remove(&i);
            }
            if let Some(c) = t.origin.cell {
                self.taken.remove(&c);
            }
            self.removed.push(id);
            self.grid_dirty = true;
        }
    }

    pub fn moved(&mut self) {
        self.grid_dirty = true;
    }

    fn rebuild(&mut self) {
        self.grid.clear();
        for (id, t) in &self.map {
            if !t.removed {
                self.grid.entry(gcell(t.pos)).or_default().push(*id);
            }
        }
        self.grid_dirty = false;
    }

    /// Ids of things whose origin is within `r` of `p` (sorted).
    pub fn near(&mut self, p: Vec3, r: f32) -> Vec<ThingId> {
        if self.grid_dirty {
            self.rebuild();
        }
        let mut out = Vec::new();
        let n = (r / GRID).ceil() as i32;
        let c = gcell(p);
        for dz in -n..=n {
            for dx in -n..=n {
                if let Some(v) = self.grid.get(&(c.0 + dx, c.1 + dz)) {
                    for id in v {
                        if let Some(t) = self.map.get(id) {
                            let d = t.pos - p;
                            if d.x * d.x + d.z * d.z <= r * r && !t.removed {
                                out.push(*id);
                            }
                        }
                    }
                }
            }
        }
        out.sort_unstable();
        out
    }

    /// Take a thing out of the world as it is (it goes with a traveler who
    /// leaves): not drawn, moved or saved until put back. Its row stays.
    pub fn stow(&mut self, id: ThingId) -> Option<Thing> {
        let t = self.map.remove(&id)?;
        self.grid_dirty = true;
        Some(t)
    }

    /// Put a stowed thing back.
    pub fn unstow(&mut self, t: Thing) {
        self.next_id = self.next_id.max(t.id + 1);
        self.map.insert(t.id, t);
        self.grid_dirty = true;
    }

    pub fn live(&self) -> impl Iterator<Item = &Thing> {
        self.map.values().filter(|t| !t.removed)
    }

    pub fn len(&self) -> usize {
        self.map.values().filter(|t| !t.removed).count()
    }
}

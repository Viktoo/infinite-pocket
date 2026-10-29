//! Pointing at things: which thing, person, placed object or scatter item a
//! ray from the eye meets first. Used for the mouse, the centre reticle and
//! `pocket inspect --at`.

use super::{ActorId, Sim, Target};
use crate::render::GpuInst;
use crate::world::TypeEntry;
use glam::Vec3;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Picked {
    pub target: Target,
    pub pos: Vec3,
    pub dist: f32,
    pub name: String,
}

/// Things smaller than this (bounding radius, m) get a halo around them.
const SMALL: f32 = 0.35;
const HALO: f32 = 0.22;

/// How far past its bounding radius a small thing can be pointed at, `t`
/// metres away: whichever is more of 1.25 times radius + HALO, or most of the
/// reticle (`cone`: half its size, as a slope) at that distance.
fn halo(r: f32, cone: f32, t: f32) -> f32 {
    ((r + HALO) * 1.25 - r).max(cone * 0.6 * t)
}

/// Loose things are padded along their own shape, so a thin one (a rake's
/// stem, a stick) is as easy to point at as a thick one, without the whole
/// thing becoming a ball: at least `THIN`, more far off (as `halo`).
const THIN: f32 = 0.06;

fn pad(cone: f32, t: f32) -> f32 {
    THIN.max(cone * 0.6 * t)
}

enum Cand {
    /// The last field: loose (could be picked up), so padded.
    Shape(Target, GpuInst, Arc<TypeEntry>, bool),
    Body(ActorId, Vec3, f32),
}

fn sdf(c: &Cand, p: Vec3) -> f32 {
    match c {
        Cand::Shape(_, g, ty, _) => {
            let ds = (p - g.center()).length() - g.radius();
            if ds > 0.4 {
                return ds;
            }
            g.sdf(&ty.ct, p)
        }
        Cand::Body(_, feet, h) => {
            // A person's capsule, in proportion for smaller and bigger bodies.
            let s = h / 1.75;
            let y = p.y.clamp(feet.y + 0.3 * s, feet.y + h - 0.2 * s);
            (p - Vec3::new(feet.x, y, feet.z)).length() - 0.3 * s
        }
    }
}

/// Ray vs bounding sphere: does the ray come within the sphere before `max`?
fn ray_hits(ro: Vec3, rd: Vec3, c: Vec3, r: f32, max: f32) -> bool {
    let oc = ro - c;
    let b = oc.dot(rd);
    let cc = oc.dot(oc) - r * r;
    let disc = b * b - cc;
    disc >= 0.0 && -b + disc.sqrt() >= 0.0 && -b - disc.sqrt() <= max
}

impl Sim {
    /// The first thing a ray from `ro` along `rd` meets within `max` metres.
    /// `except` is never picked (the one looking). `cone` widens what counts
    /// as pointing at a small thing to the reticle's size (see `halo`).
    pub fn pick(&mut self, ro: Vec3, rd: Vec3, max: f32, cone: f32, except: Option<ActorId>) -> Option<Picked> {
        let rd = rd.normalize_or_zero();
        let snap = self.snap.clone();
        let mut cands: Vec<Cand> = Vec::new();
        let steps = (max / 6.0).ceil() as i32;
        let mut seen_things = std::collections::HashSet::new();
        let mut seen_inst = std::collections::HashSet::new();
        let mut seen_cells = std::collections::HashSet::new();
        for s in 0..=steps {
            let p = ro + rd * (s as f32 * 6.0);
            for id in self.things.near(p, 8.0) {
                if !seen_things.insert(id) {
                    continue;
                }
                let Some(t) = self.things.get(id) else { continue };
                if except.is_some_and(|e| t.holder == Some(e)) {
                    continue;
                }
                let Some(ty) = snap.type_of(t.type_id) else { continue };
                let g = super::render::thing_inst(t, ty, [0.0; 4]);
                let loose = !t.held() && t.liftable(1);
                if ray_hits(ro, rd, g.center(), g.radius() + halo(g.radius(), cone, max) + THIN, max) {
                    cands.push(Cand::Shape(Target::Thing(id), g, ty.clone(), loose));
                }
            }
            for pl in snap.near_chunk(crate::world::chunk_of(p.x, p.z)) {
                if self.things.by_instance.contains_key(&pl.id) || !seen_inst.insert(pl.id) {
                    continue;
                }
                let Some(ty) = snap.type_of(pl.type_id) else { continue };
                let g = pl.gpu(ty, 1.0);
                if ray_hits(ro, rd, g.center(), g.radius(), max) {
                    cands.push(Cand::Shape(Target::Instance(pl.id), g, ty.clone(), false));
                }
            }
            for it in self.cache.items_near(&snap, p, 8.0) {
                if !seen_cells.insert(it.cell) {
                    continue;
                }
                let Some(ty) = snap.type_of(it.inst.info[0]) else { continue };
                if ty.has_tag("grass") {
                    continue;
                }
                let g = it.inst;
                if ray_hits(ro, rd, g.center(), g.radius() + halo(g.radius(), cone, max) + THIN, max) {
                    cands.push(Cand::Shape(Target::Cell([it.cell.0, it.cell.1]), g, ty.clone(), ty.has_tag("small")));
                }
            }
        }
        for a in self.actor_ids() {
            if Some(a) == except || a == ActorId::Player {
                continue;
            }
            let Some(x) = self.actor(a) else { continue };
            let h = self.actor_height(a);
            if ray_hits(ro, rd, x.pos + Vec3::Y * h * 0.5, h * 0.6, max) {
                cands.push(Cand::Body(a, x.pos, h));
            }
        }
        let terrain = &snap.terrain;
        let mut t = 0.15;
        for _ in 0..220 {
            let p = ro + rd * t;
            let mut d = (p.y - terrain.height(p.x, p.z)) * 0.6;
            // Under the surface in a hollow (a cave, a home set into a hill,
            // the way dug down to it) the air goes on to its walls.
            if d < 0.5 && terrain.caves_near(p.x, p.z) {
                d = d.max(-terrain.void(p) * 0.9);
            }
            let mut hit: Option<usize> = None;
            for (i, c) in cands.iter().enumerate() {
                let mut dc = sdf(c, p);
                if let Cand::Shape(_, g, _, loose) = c {
                    if *loose {
                        dc -= pad(cone, t);
                    }
                    if g.radius() < SMALL {
                        // Small things are hard to point at: a halo around them counts.
                        dc = dc.min((p - g.center()).length() - g.radius() - halo(g.radius(), cone, t));
                    }
                }
                if dc < d {
                    d = dc;
                    hit = Some(i);
                }
            }
            if d < 0.01 + 0.0015 * t {
                return Some(match hit {
                    Some(i) => {
                        let (target, name) = match &cands[i] {
                            Cand::Shape(tg, _, ty, _) => (tg.clone(), ty.name().to_string()),
                            Cand::Body(a, _, _) => (Target::Actor(*a), self.actor_name(*a)),
                        };
                        Picked { target, pos: p, dist: t, name }
                    }
                    None => Picked { target: Target::Point(p.to_array()), pos: p, dist: t, name: "the ground".into() },
                });
            }
            t += d.max(0.02);
            if t > max {
                return None;
            }
        }
        None
    }
}

impl Sim {
    /// A loose thing with no use of its own (no code, no hinge, nothing to
    /// ride in): the use key picks it up, as in most games.
    pub fn just_to_pick_up(&mut self, t: &Target) -> bool {
        let snap = self.snap.clone();
        let ty = match t {
            Target::Thing(id) => {
                let Some(x) = self.things.get(*id) else { return false };
                if x.held() || !x.liftable(1) {
                    return false;
                }
                snap.type_of(x.type_id)
            }
            Target::Cell(c) => match self.things.taken.get(&(c[0], c[1])) {
                Some(id) => return self.just_to_pick_up(&Target::Thing(*id)),
                None => {
                    let it = self.cache.item_at(&snap, (c[0], c[1]));
                    it.and_then(|i| snap.type_of(i.inst.info[0])).filter(|ty| ty.has_tag("small"))
                }
            },
            Target::Instance(i) => match self.things.by_instance.get(i) {
                Some(id) => return self.just_to_pick_up(&Target::Thing(*id)),
                None => None,
            },
            _ => None,
        };
        let plain = ty.is_some_and(|ty| ty.ct.behavior(crate::lang::ir::Behavior::Use).is_none());
        plain && !self.target_hinged(t) && self.target_drive(t).is_none() && !self.target_seats(t)
    }

    /// Whether the grab key can take hold of a target: Some(true) to lift
    /// it alone, Some(false) to grip one end of something that needs two.
    pub fn grabbable(&mut self, t: &Target) -> Option<bool> {
        match t {
            Target::Thing(id) => self.things.get(*id).filter(|x| !x.held() && x.liftable(2)).map(|x| x.liftable(1)),
            Target::Cell(c) => match self.things.taken.get(&(c[0], c[1])) {
                Some(id) => self.grabbable(&Target::Thing(*id)),
                None => {
                    let snap = self.snap.clone();
                    let it = self.cache.item_at(&snap, (c[0], c[1]));
                    it.and_then(|i| snap.type_of(i.inst.info[0])).filter(|ty| ty.has_tag("small")).map(|_| true)
                }
            },
            Target::Instance(i) => self.things.by_instance.get(i).copied().and_then(|id| self.grabbable(&Target::Thing(id))),
            _ => None,
        }
    }

    /// The nearest small loose thing within reach in front of someone (for
    /// picking things up without aiming).
    pub fn nearest_in_front(&mut self, who: ActorId) -> Option<Target> {
        let a = self.actor(who)?.clone();
        let reach = a.dims.reach;
        let ok = |p: Vec3| {
            let d = Vec3::new(p.x - a.pos.x, 0.0, p.z - a.pos.z);
            d.length() <= reach && d.normalize_or_zero().dot(a.forward()) > 0.3
        };
        let mut best: Option<(f32, Target)> = None;
        for id in self.things.near(a.pos, reach + 0.5) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() || !t.liftable(1) || !ok(t.pos) {
                continue;
            }
            let d = (t.pos - a.pos).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Thing(id)));
            }
        }
        let snap = self.snap.clone();
        for it in self.cache.items_near(&snap, a.pos, reach + 0.5) {
            let Some(ty) = snap.type_of(it.inst.info[0]) else { continue };
            if !ty.has_tag("small") || !ok(it.inst.pos()) {
                continue;
            }
            let d = (it.inst.pos() - a.pos).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Cell([it.cell.0, it.cell.1])));
            }
        }
        best.map(|b| b.1)
    }
}

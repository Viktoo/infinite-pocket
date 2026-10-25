//! Things that go off (`meta.burst`): a grenade, a stick of dynamite, a
//! powder keg, a shell, a firework. One blast does it all, by its numbers:
//! it hurts bodies near it (less the further, softened by what they wear),
//! throws them and everything loose outward, blows the ground out into a
//! crater and tears holes in what stands close, heats what is near (what
//! burns may catch), and sets off other things that go off. Everyone
//! within earshot startles, and the hurt fear whoever set it off.
//!
//! What sets it off: being lit (used by hand: a fuse, a pin pulled; or
//! caught by fire or great heat), then its fuse running down; or, for one
//! that goes off on impact, a hard hit.

use super::actions::{ActErr, Outcome, fail};
use super::props::*;
use super::things::ThingId;
use super::tool::Blow;
use super::{ActorId, Note, Sim, Target};
use crate::lang::ir::Burst;
use glam::Vec3;
use serde_json::json;

/// How often things that could go off are checked for fire (game s).
const CHECK: f64 = 0.1;
/// Heat that lights a fuse (°C).
const LIGHTS_AT: f32 = 250.0;
/// How hard a hit sets off one that goes off on impact (m/s).
const IMPACT_AT: f32 = 4.0;
/// How long a blast lights things up (s).
pub const BLAST_LIGHT: f64 = 0.7;
/// The most standing things one blast tears holes in.
const MOST_HOLES: usize = 8;

impl Sim {
    /// What happens when a thing goes off, if it does.
    pub fn burst_of(&self, id: ThingId) -> Option<Burst> {
        let t = self.things.get(id)?;
        self.snap.type_of(t.type_id)?.ct.meta.burst.clone()
    }

    /// Light a thing that goes off: its fuse starts. False if it doesn't
    /// go off, or is already lit.
    pub fn light(&mut self, by: Option<ActorId>, id: ThingId) -> bool {
        let Some(b) = self.burst_of(id) else { return false };
        let now = self.t;
        let Some(t) = self.things.get_mut(id) else { return false };
        if t.shape.fuse_at.is_some() {
            return false;
        }
        t.shape.fuse_at = Some(now + b.fuse.max(0.0) as f64);
        t.dirty = true;
        let pos = t.pos;
        if let Some(w) = by {
            self.lit_by.insert(id, w);
        }
        let name = self.thing_name(id);
        let who = by.map(|w| self.actor_name(w));
        let msg = match &who {
            Some(w) => format!("{w} lit the {name}"),
            None => format!("the {name} was lit"),
        };
        self.event("lit", by, Some(format!("thing:{id}")), msg, Some(pos), json!({ "fuse": b.fuse }));
        if b.fuse >= 1.0 {
            self.cue(pos, by, crate::audio::Heard::Hit { mat: crate::audio::call::Material::FIRE, mass: 0.2, speed: 1.5, by: None });
        }
        true
    }

    /// Use a held thing that goes off: light it.
    pub(super) fn light_held(&mut self, who: ActorId, id: ThingId) -> Result<Outcome, ActErr> {
        let name = self.actor_name(who);
        let tname = self.thing_name(id);
        let b = self.burst_of(id).ok_or(ActErr::Fail(format!("the {tname} doesn't go off")))?;
        if !self.light(Some(who), id) {
            return fail(format!("the {tname} is already lit"));
        }
        let msg = if b.fuse >= 1.0 { format!("{name} lights the {tname}: {:.0} s to throw it", b.fuse) } else { format!("{name} sets off the {tname}") };
        if who != ActorId::Player {
            if let Some(p) = self.actor(who).map(|a| a.pos) {
                self.note_near(p, 25.0, Note::Ambient(format!("{}.", super::physics::cap(&msg))));
            }
        }
        Ok(Outcome::ok(msg).thing(id))
    }

    /// Fuses burn down; fire and great heat light what can go off.
    pub fn step_bursts(&mut self) {
        let now = self.t;
        if now - self.burst_check < CHECK {
            return;
        }
        self.burst_check = now;
        let mut due = Vec::new();
        let mut catch = Vec::new();
        for t in self.things.live() {
            let Some(ty) = self.snap.type_of(t.type_id) else { continue };
            if ty.ct.meta.burst.is_none() {
                continue;
            }
            match t.shape.fuse_at {
                Some(at) if at <= now => due.push(t.id),
                Some(_) => {}
                None if t.props[P_FIRE] > 0.05 || t.props[P_TEMP] > LIGHTS_AT => catch.push(t.id),
                None => {}
            }
        }
        // Or something burning touches it (a fire arrow in it, a torch to it).
        let unlit: Vec<(ThingId, Vec3)> = self
            .things
            .live()
            .filter(|t| t.shape.fuse_at.is_none() && !catch.contains(&t.id) && self.snap.type_of(t.type_id).is_some_and(|ty| ty.ct.meta.burst.is_some()))
            .map(|t| (t.id, t.pos))
            .collect();
        for (id, p) in unlit {
            let r = self.things.get(id).and_then(|t| self.snap.type_of(t.type_id).map(|ty| ty.radius() * t.scale)).unwrap_or(0.3);
            if self.things.near(p, r + 1.5).into_iter().any(|o| o != id && self.things.get(o).is_some_and(|x| x.props[P_FIRE] > 0.1 && (x.pos - p).length() < r + 0.8)) {
                catch.push(id);
            }
        }
        for id in catch {
            // Lit by fire: whoever set the fire, or threw it in, did it.
            let by = self.things.get(id).and_then(|t| t.thrown_by.map(|x| x.0).or(t.holder));
            self.light(by, id);
        }
        for id in due {
            self.go_off(id);
        }
    }

    /// A thing that goes off on impact hit something hard.
    pub(super) fn burst_on_impact(&mut self, id: ThingId, speed: f32) -> bool {
        if speed < IMPACT_AT || !self.burst_of(id).is_some_and(|b| b.impact) {
            return false;
        }
        self.go_off(id);
        true
    }

    /// It goes off.
    pub fn go_off(&mut self, id: ThingId) {
        let Some(t) = self.things.get(id).cloned() else { return };
        let Some(ty) = self.snap.type_of(t.type_id).cloned() else { return };
        let Some(b) = ty.ct.meta.burst.clone() else { return };
        let by = self.lit_by.remove(&id).or(t.thrown_by.map(|x| x.0)).or(t.holder);
        let (at, _) = t.proxy(&ty);
        let name = ty.name().to_string();
        let r = b.radius.max(0.1);
        self.release(id);
        self.things.remove(id);
        let ground = self.snap.terrain.height(at.x, at.z);

        // The ground blown out.
        if b.crater > 0.05 && at.y - ground < b.crater + 0.6 && ground > self.snap.terrain.water(at.x, at.z) + 0.05 {
            let depth = b.crater;
            let edge = (depth * 1.4).max(crate::terrain::HOLLOW_EDGE);
            let half = (depth * 1.5 - edge).max(depth * 0.75);
            let hollow = crate::terrain::Hollow { c: [at.x, at.z], rot: [1.0, 0.0], half: [half, half], floor: ground - depth, round: true, edge };
            self.open_hollow(at, hollow);
            self.seep(glam::Vec2::new(at.x, at.z), half + edge * 2.0 + 4.0);
        }

        // Holes torn in what stands close (a wall, a cart, a tree).
        if b.crater > 0.05 {
            let reach = (b.crater * 2.0 + 0.5).min(r);
            for (target, surface) in self.standing_near(at, reach).into_iter().take(MOST_HOLES) {
                let d = (surface - at).length();
                let size = b.crater * 0.8 * (1.0 - d / reach).max(0.0);
                if size < 0.08 {
                    continue;
                }
                let Some(sid) = self.liven(&target) else { continue };
                let _ = self.cut(by.unwrap_or(ActorId::Player), sid, Some(surface), size, false);
                self.impact(sid, b.push * (1.0 - d / reach), Some(name.clone()));
            }
        }

        // Bodies: hurt and thrown.
        let told = by.map(|w| format!("{}'s {name}", self.actor_name(w))).unwrap_or_else(|| format!("the {name}"));
        for a in self.actor_ids() {
            let Some((feet, height)) = self.actor(a).map(|x| (x.pos, x.dims.height)) else { continue };
            let mid = feet + Vec3::Y * height * 0.5;
            let d = ((mid - at).length() - height * 0.3).max(0.0);
            if d >= r {
                continue;
            }
            let f = (1.0 - d / r).powi(2);
            let away = Vec3::new(mid.x - at.x, 0.0, mid.z - at.z).normalize_or(Vec3::X);
            let hurt = b.hurt * f;
            let push = away * (b.push * f * 0.08).min(2.5);
            if hurt < 0.01 && push.length() < 0.05 {
                continue;
            }
            let oname = self.actor_name(a);
            self.take_blow(Blow {
                by,
                to: a,
                hurt,
                push,
                how: format!("caught in the blast of {told}"),
                told: format!("{} was caught in the blast of {told}", super::physics::cap(&oname)),
                felt: format!("You were caught in the blast of {told}. It hurt."),
                weapon: Some(name.clone()),
                mass: 20.0,
                speed: b.push * f,
                motion: "burst",
            });
        }

        // Things: thrown outward, heated, and others that go off set off.
        let now = self.t;
        let mut chain = Vec::new();
        for o in self.things.near(at, r + 1.0) {
            let Some(x) = self.things.get(o) else { continue };
            if x.held() {
                continue;
            }
            let Some(oty) = self.snap.type_of(x.type_id).cloned() else { continue };
            let (c, _) = x.proxy(&oty);
            let d = (c - at).length();
            if d >= r {
                continue;
            }
            let f = 1.0 - d / r;
            if oty.ct.meta.burst.is_some() {
                chain.push((o, d));
            }
            let x = self.things.get_mut(o).expect("live");
            if b.fire > 0.0 {
                x.props[P_TEMP] = x.props[P_TEMP].max(AMBIENT_TEMP + 700.0 * b.fire * f);
            }
            if !x.anchored && !x.stuck() {
                let away = (c - at + Vec3::Y * 0.3 * r).normalize_or(Vec3::Y);
                let kick = b.push * f * f / x.mass().max(1.0).sqrt();
                if kick > 0.3 {
                    x.vel += away * kick.min(40.0);
                    x.asleep = false;
                    x.rest_t = 0.0;
                    x.thrown_by = by.map(|w| (w, now));
                }
            }
            x.dirty = true;
        }
        for (o, d) in chain {
            // A moment later, by how far the blast had to go.
            if let Some(x) = self.things.get_mut(o) {
                let soon = now + 0.12 + d as f64 * 0.03;
                x.shape.fuse_at = Some(x.shape.fuse_at.map_or(soon, |f| f.min(soon)));
                x.dirty = true;
            }
            if let Some(w) = by {
                self.lit_by.entry(o).or_insert(w);
            }
        }

        // Seen and heard.
        let msg = format!("{} went off", super::physics::cap(&told));
        self.event("burst", by, None, msg.clone(), Some(at), json!({ "what": name, "radius": r, "crater": b.crater }));
        let now = self.t;
        self.blasts.retain(|b| now - b.1 < BLAST_LIGHT);
        self.blasts.push((at, now, r));
        let mat = crate::audio::call::Material { hard: 1.0, dry: 1.0, ring: 0.05, leafy: 0.0 };
        self.cue(at, by, crate::audio::Heard::Hit { mat, mass: (r * r * 60.0).min(4000.0), speed: 30.0, by: None });
        let heard = (r * 12.0).max(40.0);
        self.note_near(at, heard, Note::Notable(format!("{}!", super::physics::cap(&msg))));
        let memory = format!("I saw {told} go off.");
        let tell = format!("{} just went off near you, with a great bang.", super::physics::cap(&told));
        let sight = super::surprise::Sight { how: super::surprise::Arrival::Changed, extent: r * 2.0, strange: self.type_props.get(&self.vocab, &ty)[P_STRANGE] };
        let what = told.clone();
        self.startle(heard, &super::surprise::News { at, sight, memory: &memory, importance: 0.7, event: "burst", tell: &tell, what: &what }, &[]);
    }

    /// Standing shapes near `p` that a blast reaches (placed things, live
    /// things, scattered trees and rocks), with the nearest point of each.
    pub(super) fn standing_near(&mut self, p: Vec3, reach: f32) -> Vec<(Target, Vec3)> {
        let mut out: Vec<(f32, Target, Vec3)> = Vec::new();
        let near = |s: &crate::world::Solid| -> Option<(f32, Vec3)> {
            let d = s.sdf(p);
            if d > reach {
                return None;
            }
            let e = 0.02;
            let n = Vec3::new(s.sdf(p + Vec3::X * e) - s.sdf(p - Vec3::X * e), s.sdf(p + Vec3::Y * e) - s.sdf(p - Vec3::Y * e), s.sdf(p + Vec3::Z * e) - s.sdf(p - Vec3::Z * e)).normalize_or(Vec3::Y);
            Some((d.max(0.0), p - n * d.max(0.0)))
        };
        let snap = self.snap.clone();
        for q in &snap.instances {
            if self.things.by_instance.contains_key(&q.id) {
                continue;
            }
            let Some(ty) = snap.type_of(q.type_id) else { continue };
            if !ty.solid || ty.ct.meta.body.is_some() {
                continue;
            }
            let s = crate::world::Solid::new(q.gpu(ty, 1.0), ty.clone());
            if (s.inst.center() - p).length() - s.inst.radius() > reach {
                continue;
            }
            if let Some((d, at)) = near(&s) {
                out.push((d, Target::Instance(q.id), at));
            }
        }
        for id in self.things.near(p, reach + 12.0) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() || t.origin.remains.is_some() {
                continue;
            }
            let Some(ty) = snap.type_of(t.type_id) else { continue };
            if !ty.solid || ty.radius() * t.scale < 0.3 {
                continue;
            }
            let s = super::render::thing_solid(t, ty.clone());
            if (s.inst.center() - p).length() - s.inst.radius() > reach {
                continue;
            }
            if let Some((d, at)) = near(&s) {
                out.push((d, Target::Thing(id), at));
            }
        }
        for it in self.cache.items_near(&snap, p, reach + 4.0) {
            let Some(ty) = snap.type_of(it.inst.info[0]) else { continue };
            if !ty.solid || ty.radius() * it.inst.pos_scale[3] < 0.3 {
                continue;
            }
            let s = crate::world::Solid::new(it.inst, ty.clone());
            if let Some((d, at)) = near(&s) {
                out.push((d, Target::Cell([it.cell.0, it.cell.1]), at));
            }
        }
        out.sort_by(|a, b| a.0.total_cmp(&b.0));
        out.into_iter().map(|(_, t, at)| (t, at)).collect()
    }
}

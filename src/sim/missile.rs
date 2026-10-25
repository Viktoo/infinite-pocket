//! Things that fly and strike. Anything thrown flies (see `physics`); this
//! is what a thing's shape adds to that:
//!
//! - A tool with a keen tip (`meta.tool.sharp`: a spear, a knife, an arrow)
//!   flies point first, turning to face the way it goes, and strikes with
//!   its tip: going fast enough along its point into the ground, a tree, a
//!   wall or a body, it sticks there until someone pulls it out.
//! - Whatever flies hurts a body it hits by its weight, its speed and how
//!   keen it is (a ball tossed in play, nothing; a stone, a bruise; a
//!   spear or an arrow, a wound).
//! - A launcher (`meta.tool.launch`: a bow, a sling, a musket, a cannon)
//!   holds shots of its ammunition, a type by name, and sends them off from
//!   its tip at its own speed, aimed to drop onto what it was pointed at.
//!   Ammunition lying about (an arrow fallen short, one pulled from a tree)
//!   loads it again; ammunition no one has written yet is asked for, and
//!   until it comes the built-in `shot` flies in its place.

use super::actions::{ActErr, Outcome, fail};
#[cfg(test)]
use super::physics::GRAVITY;
use super::things::{Origin, StuckIn, ThingId};
use super::tool::{Blow, Knock, MotionRun, bite_of};
use super::{ActorId, Note, Sim, Target};
use crate::lang::ir::{Launch, Tool};
use crate::world::TypeEntry;
use glam::{Quat, Vec3};
use serde_json::json;
use std::sync::Arc;

/// Spent shots smaller than this (bounding radius, m) are not worth seeing
/// lying about: they are cleared away once they come to rest.
pub const UNSEEN: f32 = 0.06;
/// A thing at least this keen, flying point first at least this fast along
/// its point (m/s), sticks where it strikes.
pub const STICK_KEEN: f32 = 0.45;
pub const STICK_SPEED: f32 = 7.0;
/// How long something stays in a living body before it works loose (s).
const IN_BODY: f64 = 45.0;
/// A launcher this fast (m/s) is a gun: its stand-in shot is a slug.
const FAST: f32 = 150.0;
/// The collision radius of a flying tip (m).
pub const TIP_R: f32 = 0.03;

/// The way to send something off from `from` at `speed` so that it falls
/// onto `to` (the flatter of the two arcs that do; as far as it goes when
/// none does), where things fall at `g`.
pub fn aim(from: Vec3, to: Vec3, speed: f32, g: f32) -> Vec3 {
    let d = to - from;
    let flat = glam::Vec2::new(d.x, d.z);
    let x = flat.length();
    if x < 0.05 {
        return d.normalize_or(Vec3::Y);
    }
    let v2 = speed * speed;
    let disc = v2 * v2 - g * (g * x * x + 2.0 * d.y * v2);
    let up = if disc >= 0.0 { ((v2 - disc.sqrt()) / (g * x)).atan() } else { std::f32::consts::FRAC_PI_4 };
    let h = flat / x;
    Vec3::new(h.x * up.cos(), up.sin(), h.y * up.cos())
}

/// "arrows", "musket balls", "shot".
pub fn plural(name: &str) -> String {
    let n = name.trim();
    if n.ends_with('s') || n.ends_with("shot") || n.ends_with("ammunition") {
        n.to_string()
    } else {
        format!("{n}s")
    }
}

/// A name in the singular ("arrows" → "arrow"; "glass" stays).
fn singular(name: &str) -> String {
    let n = name.trim().to_lowercase();
    match n.strip_suffix('s') {
        Some(s) if !n.ends_with("ss") && s.len() >= 2 => s.to_string(),
        _ => n,
    }
}

/// How a thing's name stands to a launcher's ammunition.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum AmmoName {
    /// One of it: the ammunition itself, or a kind of it ("fire arrow",
    /// "barbed arrow", "arrow of fire" for "arrow").
    One,
    /// A bundle of it ("quiver of arrows", "box of bullets", "arrows").
    Bundle,
    No,
}

/// Read by the head of the name: what it is, not what it is of (a quiver
/// of arrows is a quiver).
pub fn ammo_name(name: &str, ammo: &str) -> AmmoName {
    let n = name.trim().to_lowercase();
    let want = singular(ammo);
    if want.len() < 2 {
        return AmmoName::No;
    }
    let is = |s: &str| s == want || s.ends_with(&format!(" {want}"));
    let (head, rest) = match n.split_once(" of ") {
        Some((h, r)) => (h.trim().to_string(), Some(r.trim().to_string())),
        None => (n.clone(), None),
    };
    if is(&head) {
        return AmmoName::One;
    }
    if is(&singular(&head)) || rest.is_some_and(|r| is(&singular(&r))) {
        return AmmoName::Bundle;
    }
    AmmoName::No
}

/// How much of a body's health something flying takes: its energy, made
/// worse by a keen point, against the body's size. A ball tossed in play
/// takes nothing; a stone thrown hard, a little; a spear or an arrow, a lot.
pub fn flight_hurt(mass: f32, speed: f32, keen: f32, body_mass: f32) -> f32 {
    let e = 0.5 * mass.max(0.0) * speed * speed;
    ((e * (0.5 + 2.0 * keen.clamp(0.0, 1.0)) - 20.0) / 400.0 / (body_mass.max(1.0) / 70.0).sqrt()).clamp(0.0, 0.95)
}

/// A direction strayed by up to `deg` degrees.
fn stray(d: Vec3, deg: f32, a: f32, b: f32) -> Vec3 {
    if deg <= 0.0 {
        return d;
    }
    let side = d.cross(Vec3::Y).normalize_or(Vec3::X);
    let up = side.cross(d).normalize_or(Vec3::Y);
    let k = deg.to_radians().tan();
    (d + side * (a * 2.0 - 1.0) * k + up * (b * 2.0 - 1.0) * k).normalize_or(d)
}

impl Sim {
    /// The tool a thing's type says it is.
    pub fn tool_of(&self, id: ThingId) -> Option<Tool> {
        let t = self.things.get(id)?;
        self.snap.type_of(t.type_id)?.ct.meta.tool.clone()
    }

    /// How keen a thing is (0 for anything that is no tool).
    pub fn keen_of(&self, id: ThingId) -> f32 {
        self.tool_of(id).map(|t| t.keen()).unwrap_or(0.0)
    }

    /// Something that hurts when thrown at someone (not a toy to catch):
    /// keen, or going off.
    pub fn is_missile(&self, id: ThingId) -> bool {
        self.keen_of(id) >= 0.3 || self.burst_of(id).is_some() || self.things.get(id).is_some_and(|t| t.shot)
    }

    /// Where a thing's tip is, as it flies point first, and how big a tip.
    pub fn flight_tip(&self, id: ThingId) -> Option<(Vec3, f32)> {
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?;
        let tool = ty.ct.meta.tool.as_ref()?;
        Some((super::render::thing_inst(t, ty, [0.0; 4]).from_local(Vec3::from_array(tool.tip)), TIP_R))
    }

    /// Which way a thing's point faces (world): grip to tip.
    pub fn point_dir(&self, id: ThingId) -> Option<Vec3> {
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?;
        let tool = ty.ct.meta.tool.as_ref()?;
        let g = super::render::thing_inst(t, ty, [0.0; 4]);
        (g.from_local(Vec3::from_array(tool.tip)) - g.from_local(Vec3::from_array(tool.grip))).try_normalize()
    }

    /// Turn a thing to point along `v` (its grip→tip line); with `keep`, so
    /// that its tip stays there.
    pub fn point_along(&mut self, id: ThingId, v: Vec3, keep: Option<Vec3>) {
        let Some(d) = v.try_normalize() else { return };
        let Some(tool) = self.tool_of(id) else { return };
        let Some(a) = (Vec3::from_array(tool.tip) - Vec3::from_array(tool.grip)).try_normalize() else { return };
        let Some(t) = self.things.get_mut(id) else { return };
        if d.x.abs() + d.z.abs() > 1e-3 {
            t.yaw = d.x.atan2(d.z);
        }
        let local = Quat::from_rotation_y(-t.yaw) * d;
        t.set_tilt(Quat::from_rotation_arc(a, local));
        t.spin = 0.0;
        t.dirty = true;
        if let Some(p) = keep {
            if let Some((tip, _)) = self.flight_tip(id) {
                if let Some(t) = self.things.get_mut(id) {
                    t.pos += p - tip;
                }
            }
        }
    }

    /// Lay a thing that stopped flying point first down along the way it went.
    pub(super) fn lay_flat(&mut self, id: ThingId, v: Vec3) {
        let Some(tool) = self.tool_of(id) else { return };
        let Some(a) = (Vec3::from_array(tool.tip) - Vec3::from_array(tool.grip)).try_normalize() else { return };
        let Some(t) = self.things.get_mut(id) else { return };
        t.point_first = false;
        if v.x.abs() + v.z.abs() > 1e-3 {
            t.yaw = v.x.atan2(v.z);
        }
        // Its point along +z after the yaw: level, the way it was going.
        t.set_tilt(Quat::from_rotation_arc(a, Vec3::Z));
        t.dirty = true;
    }

    /// Drive a thing in where it struck, `depth` m along its point.
    pub(super) fn stick_fast(&mut self, id: ThingId, depth: f32, into: &str) {
        let along = self.point_dir(id).unwrap_or(Vec3::NEG_Y);
        let Some(t) = self.things.get_mut(id) else { return };
        t.pos += along * depth;
        t.vel = Vec3::ZERO;
        t.spin = 0.0;
        t.asleep = true;
        t.anchored = true;
        t.point_first = false;
        t.shape.stuck = true;
        t.thrown_by = None;
        t.dirty = true;
        let pos = t.pos;
        let name = self.thing_name(id);
        self.event("stuck", None, Some(format!("thing:{id}")), format!("the {name} stuck fast in {}", super::actions::the(into)), Some(pos), json!({ "in": into }));
        // Driven into something, it touches it, by the world's rules (a
        // burning arrow in a thatched roof).
        if let Some((tip, _)) = self.flight_tip(id).or_else(|| self.things.get(id).map(|t| (t.pos, 0.0))) {
            if let Some((target, _)) = self.standing_near(tip, 0.25).into_iter().next() {
                if let Some(o) = self.liven(&target).filter(|o| *o != id) {
                    self.contact(id, o);
                }
            }
        }
    }

    /// Drive a thing into a body: it goes where the body goes for a while.
    pub(super) fn stick_in(&mut self, id: ThingId, who: ActorId, depth: f32) {
        let along = self.point_dir(id).unwrap_or(Vec3::NEG_Y);
        let Some((bp, byaw)) = self.actor(who).map(|a| (a.pos, a.yaw)) else { return };
        let until = self.t + IN_BODY;
        let Some(t) = self.things.get_mut(id) else { return };
        t.pos += along * depth;
        let back = Quat::from_rotation_y(-byaw);
        let off = back * (t.pos - bp);
        let turn = back * Quat::from_rotation_y(t.yaw) * t.tilt();
        t.stuck_in = Some(StuckIn { who, off, turn, until });
        t.vel = Vec3::ZERO;
        t.spin = 0.0;
        t.asleep = true;
        t.point_first = false;
        t.thrown_by = None;
        t.dirty = true;
    }

    /// What is stuck in bodies goes where they go, until it works loose
    /// (or the body is gone).
    pub(super) fn update_stuck(&mut self) {
        let list: Vec<(ThingId, StuckIn)> = self.things.live().filter_map(|t| t.stuck_in.map(|s| (t.id, s))).collect();
        let now = self.t;
        for (id, s) in list {
            let body = self.actor(s.who).filter(|_| now < s.until).map(|a| (a.pos, a.yaw));
            let Some(t) = self.things.get_mut(id) else { continue };
            match body {
                Some((p, yaw)) => {
                    t.pos = p + Quat::from_rotation_y(yaw) * s.off;
                    t.yaw = yaw;
                    t.set_tilt(s.turn);
                    t.vel = Vec3::ZERO;
                    t.asleep = true;
                }
                None => {
                    t.stuck_in = None;
                    t.asleep = false;
                    t.rest_t = 0.0;
                    t.vel = Vec3::new(0.0, 0.5, 0.0);
                }
            }
            t.dirty = true;
        }
    }

    /// Pull a stuck thing free (into a hand, or to fall).
    pub(super) fn unstick(&mut self, id: ThingId) {
        if let Some(t) = self.things.get_mut(id) {
            if t.shape.stuck {
                t.shape.stuck = false;
                t.anchored = false;
            }
            t.stuck_in = None;
            t.dirty = true;
        }
    }

    /// Something flying hit a body at `speed` (m/s, into it): it hurts by
    /// its weight, its speed and how keen it is, if anyone sent it. True
    /// when the body died of it.
    pub(super) fn flying_hit(&mut self, id: ThingId, o: ActorId, speed: f32, dir: Vec3) -> bool {
        let Some(t) = self.things.get(id) else { return false };
        let Some((by, _)) = t.thrown_by else { return false };
        if by == o {
            return false;
        }
        let (mass, shot, pointed) = (t.mass(), t.shot, t.point_first);
        let keen = self.keen_of(id) * if pointed { 1.0 } else { 0.3 };
        let omass = self.actor(o).map(|a| a.dims.mass).unwrap_or(70.0);
        let hurt = flight_hurt(mass, speed, keen, omass);
        if hurt < 0.01 {
            return false;
        }
        let name = self.actor_name(by);
        let oname = self.actor_name(o);
        let tname = self.thing_name(id);
        let (how, told, felt) = if shot {
            let from = self.actor(by).and_then(|a| a.held).map(|h| self.thing_name(h));
            let with = from.map(|f| format!(" from the {f}")).unwrap_or_default();
            (format!("hit by {} {tname} {name} shot{with}", super::article(&tname)), format!("{name} shot {oname} with {} {tname}", super::article(&tname)), format!("{name} just shot you with {} {tname}. It hurt.", super::article(&tname)))
        } else {
            (format!("hit by the {tname} {name} threw"), format!("{name} hit {oname} with the thrown {tname}"), format!("{name} just threw a {tname} at you and hit you. It hurt."))
        };
        let push = Vec3::new(dir.x, 0.0, dir.z).normalize_or_zero() * (mass * speed / 70.0).min(0.4);
        self.take_blow(Blow { by: Some(by), to: o, hurt, push, how, told, felt, weapon: Some(tname), mass, speed, motion: if shot { "shot" } else { "thrown" } })
    }

    /// Something sent flying strikes a standing thing (a wall, a tree, a
    /// cart): it is knocked as a blow would knock it, by the flier's weight
    /// and speed; going fast, it leaves a hole its size; and the two meet
    /// by the world's rules (a hot shot, dry thatch).
    pub(super) fn flying_knock(&mut self, id: ThingId, at: Vec3, v: Vec3, speed: f32) {
        let Some(t) = self.things.get(id) else { return };
        let Some((by, _)) = t.thrown_by else { return };
        let Some(ty) = self.snap.type_of(t.type_id).cloned() else { return };
        let (mass, size, shot, props) = (t.mass(), ty.radius() * t.scale, t.shot, t.props.clone());
        let hard = crate::audio::call::Material::of(&props, &ty.ct.meta.tags, crate::audio::call::Material::from_meta(&ty.ct.meta.sound).as_ref()).hard;
        let keen = self.keen_of(id);
        let Some((on, spot)) = self.standing_near(at, size + 0.3).into_iter().find(|(o, _)| *o != Target::Thing(id)) else { return };
        let Some(on) = self.liven(&on) else { return };
        if on == id {
            return;
        }
        self.contact(id, on);
        let name = self.thing_name(id);
        let from = self.actor(by).map(|a| a.pos).unwrap_or(at - v.normalize_or_zero());
        let deed = if shot { "shoot at".to_string() } else { format!("throw the {name} at") };
        let notch = if speed > 60.0 { (size * 3.0).clamp(0.03, 0.6) } else { 0.14 * keen };
        let knock = Knock { by, on, at: spot, from, deed, with: name, bite: bite_of("flight", keen), momentum: mass * speed, hard, notch };
        self.take_knock(knock);
    }

    /// What a held thing can do as a weapon, in words for a planner ("it
    /// shoots arrows, 7 left"; "it goes off 4 s after it is lit").
    pub fn arms_note(&self, id: ThingId) -> Option<String> {
        let mut out = Vec::new();
        if let Some(l) = self.launch_of(id) {
            let left = match self.shots_left(id) {
                Some(0) => format!(", empty: load it with {} lying about", plural(&l.ammo)),
                Some(n) => format!(", {n} left"),
                None => String::new(),
            };
            out.push(format!("it shoots {}{left}, out to about {:.0} m", plural(&l.ammo), (l.speed * l.speed / self.gravity()).min(600.0) * 0.5));
        }
        if let Some(b) = self.burst_of(id) {
            let lit = self.things.get(id).and_then(|t| t.shape.fuse_at).map(|f| format!(", LIT: it goes off in {:.0} s, throw it now", (f - self.t).max(0.0)));
            out.push(match lit {
                Some(l) => format!("it goes off{l}"),
                None if b.impact && b.fuse <= 0.0 => "it goes off when it hits something hard".into(),
                None => format!("it goes off {:.0} s after it is lit; it hurts everyone within {:.0} m", b.fuse, b.radius),
            });
        }
        if out.is_empty() && self.keen_of(id) >= 0.3 && self.launch_of(id).is_none() {
            out.push("keen: it can be thrown point first".into());
        }
        (!out.is_empty()).then(|| out.join("; "))
    }

    // ------------------------------------------------------------ launchers

    /// What a thing launches, if it is a launcher.
    pub fn launch_of(&self, id: ThingId) -> Option<Launch> {
        self.tool_of(id)?.launch
    }

    /// Shots a launcher has left (None: it never runs out).
    pub fn shots_left(&self, id: ThingId) -> Option<u32> {
        let l = self.launch_of(id)?;
        if l.holds == 0 {
            return None;
        }
        Some(l.holds.saturating_sub(self.things.get(id).map(|t| t.shape.spent).unwrap_or(0)))
    }

    /// Whether a thing is what a launcher shoots: its ammunition by name,
    /// or the built-in shot standing in for ammunition not yet written.
    pub fn is_ammo_for(&self, l: &Launch, id: ThingId) -> bool {
        self.ammo_kind(l, id) != AmmoName::No
    }

    /// Whether a thing is one shot of a launcher's ammunition, a bundle of
    /// it, or neither.
    pub fn ammo_kind(&self, l: &Launch, id: ThingId) -> AmmoName {
        let Some(ty) = self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)) else { return AmmoName::No };
        if ty.ct.meta.body.is_some() || ty.ct.meta.tool.as_ref().is_some_and(|t| t.launch.is_some()) {
            return AmmoName::No;
        }
        let stand_in = if l.speed > FAST { "slug" } else { "shot" };
        if ty.builtin && ty.name() == stand_in && self.type_by_name(&l.ammo).is_none() {
            return AmmoName::One;
        }
        ammo_name(ty.name(), &l.ammo)
    }

    /// Load a launcher with a piece of its ammunition lying within reach.
    pub fn load(&mut self, who: ActorId, launcher: ThingId, ammo: ThingId) -> Result<Outcome, ActErr> {
        let name = self.actor_name(who);
        let lname = self.thing_name(launcher);
        let aname = self.thing_name(ammo);
        let l = self.launch_of(launcher).ok_or(ActErr::Fail(format!("the {lname} shoots nothing")))?;
        let t = self.things.get(ammo).cloned().ok_or(ActErr::Fail("it's gone".into()))?;
        if l.holds == 0 {
            return fail(format!("the {lname} never needs loading"));
        }
        if self.shots_left(launcher) == Some(l.holds) {
            return fail(format!("the {lname} is full"));
        }
        if t.holder.is_some_and(|h| h != who) || t.worn.is_some() {
            return fail(format!("someone has the {aname}"));
        }
        if t.anchored && !t.shape.stuck {
            return fail(format!("the {aname} won't budge"));
        }
        if !self.in_reach(who, t.pos) {
            let d = self.actor(who).map(|a| (a.pos - t.pos).length()).unwrap_or(0.0);
            return Err(ActErr::TooFar { at: t.pos, dist: d });
        }
        let kind = self.ammo_kind(&l, ammo);
        // One shot goes in as what it is; a bundle fills it with what it holds.
        let (each, n) = match kind {
            AmmoName::Bundle => {
                let inner = aname.split_once(" of ").map(|(_, r)| singular(r)).unwrap_or_else(|| singular(&aname));
                (self.type_by_name(&inner).filter(|t| t.ct.meta.body.is_none()).map(|t| t.id), self.things.get(launcher).map(|x| x.shape.spent).unwrap_or(0))
            }
            _ => (Some(t.type_id), 1),
        };
        self.unstick(ammo);
        self.release(ammo);
        self.things.remove(ammo);
        if let Some(x) = self.things.get_mut(launcher) {
            let n = n.min(x.shape.spent);
            x.shape.spent -= n;
            for _ in 0..n {
                if let Some(e) = each {
                    x.shape.loaded.push(e);
                }
            }
            let room = l.holds.saturating_sub(x.shape.spent) as usize;
            let over = x.shape.loaded.len().saturating_sub(room);
            x.shape.loaded.drain(..over);
            x.dirty = true;
        }
        let left = self.shots_left(launcher).unwrap_or(0);
        let msg = match kind {
            AmmoName::Bundle => format!("{name} fills the {lname} from the {aname} ({left} of {})", l.holds),
            _ => format!("{name} loads the {aname} into the {lname} ({left} of {})", l.holds),
        };
        let at = self.actor(who).map(|a| a.pos);
        self.event("loaded", Some(who), Some(format!("thing:{launcher}")), msg.clone(), at, json!({ "left": left }));
        Ok(Outcome::ok(msg).thing(launcher))
    }

    /// The type a launcher's shots are, by name; asked for when nobody has
    /// written it yet (the built-in shot flies meanwhile).
    fn ammo_type(&mut self, l: &Launch, launcher: &str) -> Option<Arc<TypeEntry>> {
        let want = l.ammo.trim().to_lowercase();
        let found = self.type_by_name(&l.ammo).or_else(|| self.type_by_name(&plural(&l.ammo))).filter(|t| t.ct.meta.body.is_none()).or_else(|| {
            self.snap
                .scene
                .types
                .values()
                .filter(|t| t.ct.meta.body.is_none() && !t.has_tag("building") && t.ct.meta.tool.as_ref().is_none_or(|x| x.launch.is_none()) && ammo_name(t.name(), &want) == AmmoName::One)
                .max_by_key(|t| t.id)
                .cloned()
        });
        if found.is_some() {
            return found;
        }
        let desc = format!(
            "one {} as shot from a {launcher}, at {:.0} m/s. Make it point along +z with meta.tool {{ grip, tip, motions: [\"thrust\"], sharp }} (tip at its point) if it has a point, and meta.burst if it goes off when it lands. Its real size and weight.",
            l.ammo, l.speed
        );
        self.build_type_for(&l.ammo, &desc, None);
        // Meanwhile: a slug from a gun, a dart from anything slower.
        self.type_by_name(if l.speed > FAST { "slug" } else { "shot" })
    }

    /// A launcher's motion comes to its end: what it holds leaves its tip.
    pub(super) fn loose(&mut self, who: ActorId, run: &MotionRun) {
        let Some((tool_id, tool)) = self.held_tool(who) else { return };
        if tool_id != run.tool {
            return;
        }
        let Some(l) = tool.launch.clone() else { return };
        let lname = self.thing_name(tool_id);
        let Some(me) = self.actor(who).cloned() else { return };
        if self.shots_left(tool_id) == Some(0) {
            self.note_near(me.pos, 10.0, Note::seen(format!("The {lname} is empty."), who == ActorId::Player));
            return;
        }
        let tip = self.tool_tip(tool_id).unwrap_or(me.hand(false));
        // Where to: at whom it was aimed, where they are now; else at the
        // spot pointed at; else straight ahead the way they look.
        let target_at = match &run.target {
            Some(Target::Actor(o)) => self.actor(*o).map(|a| a.pos + Vec3::Y * a.dims.height * 0.62).or(run.at),
            _ => run.at,
        };
        let base = match target_at {
            Some(p) => aim(tip, p, l.speed, self.gravity()),
            None => {
                let pitch = if who == ActorId::Player { self.look_pitch } else { 0.05 };
                let look = Vec3::new(me.yaw.sin() * pitch.cos(), pitch.sin(), me.yaw.cos() * pitch.cos());
                (me.eye() + look * 60.0 - tip).normalize_or(look)
            }
        };
        // What was loaded by hand goes first; then its own ammunition.
        let top = self.things.get(tool_id).and_then(|t| t.shape.loaded.last().copied());
        let ty = match top.and_then(|id| self.snap.type_of(id).cloned()) {
            Some(ty) => Some(ty),
            None => self.ammo_type(&l, &lname),
        };
        let Some(ty) = ty else { return };
        let points = ty.ct.meta.tool.is_some();
        let burst = ty.ct.meta.burst.clone();
        let now = self.t;
        let mut sent = 0;
        for i in 0..l.per_shot.max(1) {
            let (a, b, c) = (self.rand(), self.rand(), self.rand());
            let d = stray(base, l.spread, a, b);
            let v = d * l.speed * (0.97 + 0.06 * c);
            let origin = Origin { made_from: vec![lname.clone()], ..Default::default() };
            let Some(nid) = self.spawn_thing(ty.id, tip, me.yaw, 1.0, origin, false) else { break };
            sent += 1;
            if let Some(t) = self.things.get_mut(nid) {
                t.vel = v;
                t.asleep = false;
                t.anchored = false;
                t.rest_t = 0.0;
                t.thrown_by = Some((who, now));
                t.shot = true;
                t.point_first = points;
                t.params[0] += i as f32;
            }
            // Out of the tip, a hand's width ahead of it.
            let start = tip + d * 0.08;
            if points {
                self.point_along(nid, v, Some(start));
            } else if let Some(t) = self.things.get_mut(nid) {
                let (cen, _) = t.proxy(&ty);
                t.pos += start - cen;
            }
            if let Some(b) = &burst {
                // A shell is lit as it leaves; one that bursts on landing waits for it.
                if b.fuse > 0.0 || !b.impact {
                    self.light(Some(who), nid);
                }
            }
        }
        if sent == 0 {
            return;
        }
        if let Some(t) = self.things.get_mut(tool_id) {
            if l.holds > 0 {
                t.shape.spent = (t.shape.spent + 1).min(l.holds);
            }
            if top.is_some() {
                t.shape.loaded.pop();
            }
            t.dirty = true;
        }
        // A character turns to what it shoots at. The traveler's view is
        // their own: the shot leaves the muzzle, beside the eye, angled in
        // to where they look, and turning them along it would pull the
        // view aside a little with every shot.
        if who != ActorId::Player {
            let mark = target_at.unwrap_or(me.pos + base);
            if let Some(a) = self.actor_mut(who) {
                let d = mark - a.pos;
                a.face(d, 3.0);
            }
        }
        let name = self.actor_name(who);
        let at_whom = match &run.target {
            Some(Target::Actor(o)) => format!(" at {}", self.actor_name(*o)),
            Some(t) if !matches!(t, Target::Point(_)) => self.resolve(t, who).map(|r| format!(" at {}", super::actions::the(&r.name))).unwrap_or_default(),
            _ => String::new(),
        };
        let msg = format!("{name} shoots the {lname}{at_whom}");
        self.event("shot", Some(who), Some(format!("thing:{tool_id}")), msg.clone(), Some(me.pos), json!({ "speed": l.speed, "ammo": ty.name(), "to": run.target }));
        // A gun cracks; a bow or a sling only snaps.
        let (mat, mass, speed) = if l.speed > 120.0 {
            (crate::audio::call::Material { hard: 1.0, dry: 1.0, ring: 0.15, leafy: 0.0 }, 60.0, 30.0)
        } else {
            let tm = self.things.get(tool_id).map(|t| t.mass()).unwrap_or(1.0);
            (crate::audio::call::Material { hard: 0.6, dry: 0.4, ring: 0.5, leafy: 0.0 }, tm.min(4.0), 5.0)
        };
        self.cue(tip, Some(who), crate::audio::Heard::Hit { mat, mass, speed, by: None });
        if who != ActorId::Player {
            let at_player = matches!(run.target, Some(Target::Actor(ActorId::Player)));
            self.note_near(me.pos, if l.speed > 120.0 { 80.0 } else { 30.0 }, Note::seen(format!("{}.", super::physics::cap(&msg)), at_player));
        }
        if l.speed > 120.0 {
            self.witness(me.pos, 60.0, &format!("I heard a {lname} fired."), 0.5, &[who]);
        }
        self.animals_notice_throw(me.pos);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aim_drops_onto_the_mark() {
        // Fly the aimed shot and see where it comes down at the mark's distance.
        for (to, speed) in [(Vec3::new(30.0, 0.0, 0.0), 40.0), (Vec3::new(0.0, 5.0, 60.0), 55.0), (Vec3::new(-80.0, -3.0, 20.0), 400.0)] {
            let d = aim(Vec3::ZERO, to, speed, GRAVITY);
            let v = d * speed;
            let flat = glam::Vec2::new(to.x, to.z).length();
            let t = flat / glam::Vec2::new(v.x, v.z).length();
            let y = v.y * t - 0.5 * GRAVITY * t * t;
            assert!((y - to.y).abs() < 0.05, "lands at {y} not {} ({to:?} at {speed})", to.y);
        }
        // Out of range: as far as it goes.
        let d = aim(Vec3::ZERO, Vec3::new(500.0, 0.0, 0.0), 30.0, GRAVITY);
        assert!((d.y - std::f32::consts::FRAC_PI_4.sin()).abs() < 1e-3);
    }

    #[test]
    fn play_doesnt_hurt_and_points_do() {
        assert_eq!(flight_hurt(0.6, 9.0, 0.0, 70.0), 0.0, "a ball tossed");
        let stone = flight_hurt(1.0, 12.0, 0.0, 70.0);
        let arrow = flight_hurt(0.05, 55.0, 0.9, 70.0);
        let spear = flight_hurt(2.0, 11.0, 0.8, 70.0);
        let ball = flight_hurt(0.03, 400.0, 0.3, 70.0);
        assert!(stone > 0.0 && stone < 0.1, "a stone bruises: {stone}");
        assert!(arrow > 0.25 && arrow < 0.6, "an arrow wounds: {arrow}");
        assert!(spear > 0.4, "a spear wounds badly: {spear}");
        assert!(ball > 0.9, "a musket ball nearly kills: {ball}");
        assert!(flight_hurt(0.05, 55.0, 0.9, 600.0) < arrow, "a horse takes an arrow better");
    }

    #[test]
    fn ammunition_by_what_it_is() {
        assert_eq!(ammo_name("arrow", "arrow"), AmmoName::One);
        assert_eq!(ammo_name("fire arrow", "arrow"), AmmoName::One);
        assert_eq!(ammo_name("arrow of fire", "arrows"), AmmoName::One);
        assert_eq!(ammo_name("quiver of arrows", "arrow"), AmmoName::Bundle);
        assert_eq!(ammo_name("quiver of fire arrows", "arrow"), AmmoName::Bundle);
        assert_eq!(ammo_name("fire arrows", "arrow"), AmmoName::Bundle);
        assert_eq!(ammo_name("box of .308 rounds", "round"), AmmoName::Bundle);
        assert_eq!(ammo_name("quiver", "arrow"), AmmoName::No);
        assert_eq!(ammo_name("arrowhead mould", "arrow"), AmmoName::No);
        assert_eq!(ammo_name("ball", "musket ball"), AmmoName::No);
        assert_eq!(ammo_name("glass", "glass"), AmmoName::One);
    }

    #[test]
    fn plurals() {
        assert_eq!(plural("arrow"), "arrows");
        assert_eq!(plural("musket balls"), "musket balls");
        assert_eq!(plural("buckshot"), "buckshot");
    }
}

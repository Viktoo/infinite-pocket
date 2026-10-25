//! Seats and vehicles. Anything with seat anchors can be sat on (a bench,
//! a throne); a thing that also has `meta.drive` carries whoever sits in it
//! and is steered from its first seat (a cart, a car, a boat, a flying
//! carpet), its other seats for passengers. The walk keys are throttle and
//! wheel; characters steer toward where they are going. What drives it
//! (`drive.by`) matters: muscle tires the driver and goes at their
//! strength, a sail goes with the wind, a beast's cart needs a beast
//! harnessed. The engine knows no cars: a top speed, what it goes on, seats.

use super::actions::{ActErr, Outcome, Resolved};
use super::render::thing_inst;
use super::things::ThingId;
use super::{ActorId, Note, Sim, Target};
use crate::lang::ir::Drive;
use crate::world::collide::{Capsule, MAX_WADE, Obstacles, free_spot, support_under};
use crate::world::{Solid, TypeEntry};
use glam::{Quat, Vec2, Vec3};
use serde_json::json;
use std::sync::Arc;

/// How fast a vehicle turns at its best (rad/s).
const TURN: f32 = 1.3;
/// How fast a flyer climbs or dives (m/s).
const CLIMB: f32 = 4.0;
/// A boat out of the water (beached, launching) only crawls (m/s).
const AGROUND: f32 = 1.0;
/// Characters give up driving after this long getting nowhere (s).
const STUCK_FOR: f64 = 4.0;

fn fail<T>(s: impl Into<String>) -> Result<T, ActErr> {
    Err(ActErr::Fail(s.into()))
}

/// Where its riders sit, in the shape's own frame, and which way each
/// faces: its seat anchors, else the middle of its top.
fn seats_local(ty: &TypeEntry) -> Vec<(Vec3, f32)> {
    let v: Vec<(Vec3, f32)> = ty.ct.meta.anchors.iter().filter(|a| a.kind == "seat").map(|a| (Vec3::from_array(a.at), a.face.to_radians())).collect();
    if v.is_empty() { vec![(Vec3::new(0.0, ty.top, 0.0), 0.0)] } else { v }
}

fn seat_local(ty: &TypeEntry, n: u8) -> (Vec3, f32) {
    let v = seats_local(ty);
    v[(n as usize).min(v.len() - 1)]
}

/// What drives a vehicle, as the engine sorts it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Power {
    /// Someone's own strength: pedals, oars, a pole, pushing.
    Muscle,
    /// A sail.
    Wind,
    /// A beast in harness.
    Beast,
    /// Anything else: an engine, steam, magic.
    Engine,
}

pub fn power_of(d: &Drive) -> Power {
    let w = d.by.as_str();
    let has = |words: &[&str]| words.iter().any(|k| w.contains(k));
    if has(&["leg", "pedal", "foot", "feet", "oar", "paddle", "pole", "row", "push", "pull by hand", "muscle", "scoot", "skate", "ski"]) {
        Power::Muscle
    } else if has(&["wind", "sail", "kite"]) {
        Power::Wind
    } else if has(&["beast", "horse", "ox", "oxen", "mule", "donkey", "dog", "pony", "camel", "animal", "team", "harness", "reindeer", "yak", "llama"]) {
        Power::Beast
    } else {
        Power::Engine
    }
}

impl Sim {
    /// How a target moves when steered, if it is a vehicle.
    pub fn target_drive(&self, t: &Target) -> Option<Drive> {
        let (_, ty, _) = self.target_shape(t)?;
        ty.ct.meta.drive.clone()
    }

    /// Whether a target has seats to sit on (a bench, a chair, a vehicle).
    pub fn target_seats(&self, t: &Target) -> bool {
        self.target_shape(t).is_some_and(|(_, ty, _)| ty.ct.meta.anchors.iter().any(|a| a.kind == "seat"))
    }

    /// Who drives a vehicle (sits in its first seat).
    pub fn aboard_of(&self, id: ThingId) -> Option<ActorId> {
        self.actor_ids().into_iter().find(|a| self.actor(*a).is_some_and(|x| x.aboard == Some(id) && x.seat_no == 0))
    }

    /// Everyone in or on a thing, with their seats.
    pub fn riders_in(&self, id: ThingId) -> Vec<(ActorId, u8)> {
        self.actor_ids().into_iter().filter_map(|a| self.actor(a).filter(|x| x.aboard == Some(id)).map(|x| (a, x.seat_no))).collect()
    }

    /// How fast the vehicle `who` is aboard is going (m/s), if any.
    pub fn vehicle_speed(&self, who: ActorId) -> Option<f32> {
        let id = self.actor(who)?.aboard?;
        Some(self.drives.get(&id).copied().unwrap_or(0.0))
    }

    /// What the vehicle `who` is in goes on ("land", "water", "under", "air").
    pub fn vehicle_on(&self, who: ActorId) -> Option<String> {
        let id = self.actor(who)?.aboard?;
        Some(self.snap.type_of(self.things.get(id)?.type_id)?.ct.meta.drive.as_ref()?.on.clone())
    }

    /// Whether `who` steers what they are in (its driver's seat, and it moves).
    pub fn drives_it(&self, who: ActorId) -> bool {
        self.actor(who).is_some_and(|a| a.seat_no == 0 && a.aboard.and_then(|v| self.things.get(v)).and_then(|t| self.snap.type_of(t.type_id)).is_some_and(|ty| ty.ct.meta.drive.is_some()))
    }

    /// Get into a vehicle (the driver's seat if it is free, else a
    /// passenger's) or sit on something with seats, within reach.
    pub fn board(&mut self, who: ActorId, r: &Resolved) -> Result<Outcome, ActErr> {
        let name = self.actor_name(who);
        let drive = self.target_drive(&r.target);
        let seated = self.target_shape(&r.target).is_some_and(|(_, ty, _)| ty.ct.meta.anchors.iter().any(|a| a.kind == "seat"));
        if drive.is_none() && !seated {
            return fail(format!("{name} can't ride or sit on the {}", r.name));
        }
        let me = self.actor(who).cloned().ok_or(ActErr::Fail("no such actor".into()))?;
        if me.seated() {
            return fail(format!("{name} is already sitting"));
        }
        if me.dims.mass > 400.0 && self.mind_of(who) != crate::world::species::Mind::Sapient {
            return fail(format!("{name} is too big to sit in the {}", r.name));
        }
        // One steps down into a boat from the quay, up into a cart.
        if self.reach(who, r).is_err() {
            let p = r.nearest(me.pos);
            let d = p - me.pos;
            if Vec3::new(d.x, 0.0, d.z).length() > me.dims.reach + 1.6 || d.y < -2.5 || d.y > me.dims.height * 1.5 {
                self.reach(who, r)?;
            }
        }
        let id = self.liven(&r.target).ok_or(ActErr::Fail(format!("{name} can't get onto the {}", r.name)))?;
        let Some((pos, yaw, held, ty)) = self.things.get(id).and_then(|t| Some((t.pos, t.yaw, t.held(), self.snap.type_of(t.type_id)?.clone()))) else { return fail("it's gone") };
        if held {
            return fail(format!("the {} is being carried", r.name));
        }
        // The driver's seat first, if it is free; else the nearest free one.
        let taken: Vec<u8> = self.riders_in(id).into_iter().map(|x| x.1).collect();
        let seats = seats_local(&ty);
        let gi = self.things.get(id).map(|t| thing_inst(t, &ty, [0.0; 4]));
        let free = (0..seats.len() as u8).filter(|n| !taken.contains(n));
        let seat = if drive.is_some() && !taken.contains(&0) {
            Some(0)
        } else {
            free.min_by(|a, b| {
                let d = |n: &u8| gi.map(|g| (g.from_local(seats[*n as usize].0) - me.pos).length()).unwrap_or(0.0);
                d(a).total_cmp(&d(b))
            })
        };
        let Some(seat) = seat else { return fail(format!("there's no room on the {}", r.name)) };
        if drive.is_some() {
            if let Some(t) = self.things.get_mut(id) {
                t.anchored = false;
                t.asleep = true;
                t.vel = Vec3::ZERO;
                t.dirty = true;
            }
            self.drives.entry(id).or_insert(0.0);
        }
        let face = seats[seat as usize].1;
        if let Some(a) = self.actor_mut(who) {
            a.aboard = Some(id);
            a.seat_no = seat;
            a.task = None;
            a.crouching = false;
            a.crouch = 0.0;
            a.vy = 0.0;
            a.yaw = yaw + face;
        }
        self.update_aboard();
        let tname = self.thing_name(id);
        let Some(drive) = drive else {
            let msg = format!("{name} sits down on the {tname}");
            self.event("sat", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(pos), json!({ "seat": seat }));
            self.note_near(pos, 15.0, Note::seen(format!("{}.", super::physics::cap(&msg)), who == ActorId::Player));
            return Ok(Outcome::ok(format!("{msg} (e to get up)")));
        };
        if seat == 0 && power_of(&drive) == Power::Beast {
            self.hitch(id);
        }
        let how = match (seat, drive.on.as_str()) {
            (1.., _) => "a passenger: the driver steers",
            (_, "water") => "W/S row or sail, A/D steer",
            (_, "under") => "W/S speed, A/D steer, Space up, c down (or look up or down while going)",
            (_, "air") => "W/S speed, A/D steer, Space up, c down (or look up or down while flying)",
            _ => "W/S speed and brake, A/D steer",
        };
        let msg = if seat == 0 { format!("{name} climbs into the {tname}") } else { format!("{name} climbs into the {tname} as a passenger") };
        self.event("boarded", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(pos), json!({ "on": drive.on, "seat": seat }));
        self.note_near(pos, 25.0, Note::seen(format!("{}.", super::physics::cap(&msg)), who == ActorId::Player));
        Ok(Outcome::ok(format!("{msg} ({how}, e to get out)")))
    }

    /// Harness the nearest beast willing and big enough to what `id` is.
    fn hitch(&mut self, id: ThingId) {
        if self.hitched.contains_key(&id) {
            return;
        }
        let Some(driver) = self.aboard_of(id) else { return };
        let Some(at) = self.things.get(id).map(|t| t.pos) else { return };
        let busy: Vec<ActorId> = self.hitched.values().copied().collect();
        let beast = self
            .actor_ids()
            .into_iter()
            .filter(|a| *a != driver && !busy.contains(a) && self.mind_of(*a) != crate::world::species::Mind::Sapient)
            .filter(|a| self.actor(*a).is_some_and(|x| !x.carried() && !x.asleep && x.dims.mass >= 60.0 && (x.pos - at).length() < 8.0) && self.rider_of(*a).is_none())
            .filter(|a| self.temper_of(*a).map(|t| t.tame).unwrap_or(0.5) >= 0.2 && self.social.affection(*a, driver) >= 0.0)
            .min_by(|a, b| {
                let d = |x: &ActorId| self.actor(*x).map(|y| (y.pos - at).length()).unwrap_or(f32::MAX);
                d(a).total_cmp(&d(b))
            });
        if let Some(b) = beast {
            if let ActorId::Npc(c) = b {
                if let Some(n) = self.cast.get_mut(c) {
                    n.plan.clear();
                    n.a.task = None;
                }
            }
            self.hitched.insert(id, b);
            let msg = format!("{} is harnessed to the {}", self.actor_name(b), self.thing_name(id));
            self.event("hitched", Some(b), Some(format!("thing:{id}")), msg.clone(), Some(at), json!({}));
            self.note_near(at, 20.0, Note::seen(format!("{}.", super::physics::cap(&msg)), driver == ActorId::Player));
        }
    }

    /// Get out of (or up from) what you are in, beside your seat.
    pub fn leave_vehicle(&mut self, who: ActorId) -> Result<Outcome, ActErr> {
        let Some(id) = self.actor(who).and_then(|a| a.aboard) else { return fail("not in anything") };
        let seat_no = self.actor(who).map(|a| a.seat_no).unwrap_or(0);
        let name = self.actor_name(who);
        let tname = self.thing_name(id);
        let moves = self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)).is_some_and(|ty| ty.ct.meta.drive.is_some());
        let spot = match self.things.get(id).cloned().zip(self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)).cloned()) {
            Some((t, ty)) => {
                let b = ty.ct.meta.bounds;
                let (seat, face) = seat_local(&ty, seat_no);
                let gi = thing_inst(&t, &ty, [0.0; 4]);
                // Out to the side for a vehicle, forward off a bench.
                let p = if moves {
                    let side = if seat.x >= 0.0 { 1.0 } else { -1.0 };
                    gi.from_local(Vec3::new(side * (b[0] + 0.6 / t.scale.max(0.1)), 0.0, seat.z))
                } else {
                    gi.from_local(seat + Vec3::new(face.sin(), 0.0, face.cos()) * (0.7 / t.scale.max(0.1)))
                };
                let solids = self.solids_near(p, 6.0);
                let obs = Obstacles { solids: &solids, bodies: &[] };
                let q = free_spot(&self.snap.terrain, &obs, Vec3::new(p.x, self.snap.terrain.height(p.x, p.z), p.z), crate::world::collide::PLAYER_RADIUS, 4.0, false);
                Vec3::new(q.x, self.snap.terrain.height(q.x, q.z), q.z)
            }
            None => self.actor(who).map(|a| a.pos).unwrap_or_default(),
        };
        // Into deep water only the traveler (who swims off), or anyone from a
        // submarine; out of one under the water, beside it at its depth.
        let under = self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)).and_then(|ty| ty.ct.meta.drive.as_ref()).is_some_and(|d| d.on == "under");
        let deep = self.snap.terrain.height(spot.x, spot.z) < self.snap.terrain.water(spot.x, spot.z) - MAX_WADE;
        let spot = if (deep && who.is_traveler()) || under {
            let at = self.actor(who).map(|a| a.pos.y).unwrap_or(spot.y);
            Vec3::new(spot.x, at.max(spot.y), spot.z)
        } else {
            spot
        };
        if moves && deep && !under && !who.is_traveler() {
            return fail(format!("the water is too deep to get out of the {tname} here"));
        }
        if let Some(a) = self.actor_mut(who) {
            a.aboard = None;
            a.seat_no = 0;
            a.pos = spot;
            a.vy = 0.0;
            a.grounded = !deep;
            a.alt = 0.0;
            a.support = None;
        }
        self.driving.remove(&who);
        self.routes.remove(&who);
        if seat_no == 0 && moves {
            self.drives.insert(id, 0.0);
            if let Some(t) = self.things.get_mut(id) {
                t.vel = Vec3::ZERO;
                t.dirty = true;
            }
        }
        if self.riders_in(id).is_empty() {
            self.drives.remove(&id);
            self.hitched.remove(&id);
        }
        let msg = if moves { format!("{name} gets out of the {tname}") } else { format!("{name} gets up from the {tname}") };
        self.event("left_vehicle", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(spot), json!({}));
        Ok(Outcome::ok(msg))
    }

    /// Riders sit in their seats; harnessed beasts walk before what they pull.
    pub(super) fn update_aboard(&mut self) {
        let riders: Vec<(ActorId, ThingId, u8)> = self.actor_ids().into_iter().filter_map(|a| self.actor(a).and_then(|x| x.aboard.map(|v| (a, v, x.seat_no)))).collect();
        for (r, v, n) in riders {
            let seat = self.things.get(v).and_then(|t| {
                let ty = self.snap.type_of(t.type_id)?;
                let (at, face) = seat_local(ty, n);
                Some((thing_inst(t, ty, [0.0; 4]).from_local(at), t.yaw + face))
            });
            let Some((seat, yaw)) = seat else {
                if let Some(a) = self.actor_mut(r) {
                    a.aboard = None;
                    a.seat_no = 0;
                }
                self.drives.remove(&v);
                continue;
            };
            let drives = self.drives_it(r);
            if let Some(a) = self.actor_mut(r) {
                let hip = 0.67 * a.dims.height / 1.75;
                a.pos = seat - Vec3::Y * hip;
                a.task = match a.task.take() {
                    // A driver (or a passenger) keeps where they are going.
                    Some(t @ super::actor::Task::Goto { .. }) if !r.is_traveler() => Some(t),
                    _ => None,
                };
                if !r.is_traveler() || !drives {
                    a.yaw = yaw;
                }
            }
        }
        let hitched: Vec<(ThingId, ActorId)> = self.hitched.iter().map(|(t, b)| (*t, *b)).collect();
        for (v, b) in hitched {
            let front = self.things.get(v).and_then(|t| {
                let ty = self.snap.type_of(t.type_id)?;
                let bz = ty.ct.meta.bounds[2];
                Some((thing_inst(t, ty, [0.0; 4]).from_local(Vec3::new(0.0, 0.0, bz + 0.4 / t.scale.max(0.1))), t.yaw))
            });
            let ok = front.zip(self.actor(b).filter(|x| !x.asleep && !x.carried()).map(|x| (x.pos, x.dims.radius)));
            let Some(((f, yaw), (was, r))) = ok else {
                self.hitched.remove(&v);
                continue;
            };
            let p = f + Vec3::new(yaw.sin(), 0.0, yaw.cos()) * r;
            let p = Vec3::new(p.x, self.snap.terrain.height(p.x, p.z), p.z);
            if let Some(a) = self.actor_mut(b) {
                let step = Vec3::new(p.x - was.x, 0.0, p.z - was.z).length();
                a.pos = p;
                a.yaw = yaw;
                a.task = None;
                a.moved = step;
                a.phase += step * 2.0;
            }
        }
    }

    /// How hard what drives a vehicle pushes it, as a share of its top
    /// speed: the driver's strength at the oars or pedals, the wind in a
    /// sail, a beast in harness (pushed by hand without one).
    pub fn drive_power(&self, id: ThingId, who: ActorId) -> f32 {
        let Some(d) = self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)).and_then(|ty| ty.ct.meta.drive.clone()) else { return 1.0 };
        match power_of(&d) {
            Power::Muscle => {
                let tired = match who {
                    ActorId::Npc(c) => self.cast.get(c).map(|n| n.needs.fatigue).unwrap_or(0.0),
                    _ => 0.0,
                };
                ((self.strength(who) / super::props::STRENGTH).sqrt() * (1.0 - 0.5 * tired)).clamp(0.35, 1.3)
            }
            Power::Wind => (0.15 + 0.95 * self.wx.wind).clamp(0.15, 1.1),
            Power::Beast => match self.hitched.get(&id) {
                Some(b) => ((self.strength(*b) / (super::props::STRENGTH * 4.0)).sqrt()).clamp(0.5, 1.2),
                None => 0.15,
            },
            Power::Engine => 1.0,
        }
    }

    /// A character drives toward `dest`: along a way through the water for
    /// a boat (round islands, along canals, under bridges), straight for
    /// the rest, slowing to stop within `stop` of it. A boat stops where
    /// the water ends nearest it. Some(true): there (or as near as it goes);
    /// Some(false): stuck, gave up; None: still on the way.
    pub fn drive_toward(&mut self, who: ActorId, dest: Vec3, stop: f32, dt: f32) -> Option<bool> {
        let id = self.actor(who)?.aboard?;
        let (pos, yaw, ty) = self.things.get(id).and_then(|t| Some((t.pos, t.yaw, self.snap.type_of(t.type_id)?.clone())))?;
        let drive = ty.ct.meta.drive.clone()?;
        // The next waypoint: a boat's route, worked out once.
        let mut aim = Vec2::new(dest.x, dest.z);
        let mut last_leg = true;
        if drive.on == "water" || drive.on == "under" {
            if !self.routes.contains_key(&who) {
                let draft = ((ty.top - ty.bottom) * 0.25).clamp(0.1, 1.0);
                let r = water_route(&self.snap.terrain, Vec2::new(pos.x, pos.z), aim, draft).unwrap_or_default();
                self.routes.insert(who, (r, 0));
            }
            if let Some((r, i)) = self.routes.get_mut(&who) {
                while *i < r.len() && (r[*i] - Vec2::new(pos.x, pos.z)).length() < 3.5 {
                    *i += 1;
                }
                if *i < r.len() {
                    aim = r[*i];
                    last_leg = *i + 1 == r.len();
                } else if let Some(end) = r.last() {
                    // At the route's end: as near as the water goes.
                    if (*end - aim).length() > stop.max(1.5) {
                        self.steer(who, 0.0, 0.0, 0.0, dt);
                        self.routes.remove(&who);
                        return Some(true);
                    }
                }
            }
        }
        let to = aim - Vec2::new(pos.x, pos.z);
        let dist = to.length();
        let goal = Vec2::new(dest.x - pos.x, dest.z - pos.z).length();
        if goal <= stop.max(1.5) {
            self.steer(who, 0.0, 0.0, 0.0, dt);
            self.routes.remove(&who);
            return Some(true);
        }
        let want = to.x.atan2(to.y);
        let mut diff = want - yaw;
        while diff > std::f32::consts::PI {
            diff -= std::f32::consts::TAU;
        }
        while diff < -std::f32::consts::PI {
            diff += std::f32::consts::TAU;
        }
        let v = self.drives.get(&id).copied().unwrap_or(0.0);
        let mut throttle = if diff.abs() > 1.4 { 0.35 } else { 1.0 };
        if last_leg {
            // Slow enough near the end to stop there.
            throttle *= (dist / 6.0).clamp(0.3, 1.0);
            if v > dist * 0.8 {
                throttle = -0.5;
            }
        }
        let climb = match drive.on.as_str() {
            "air" => ((dest.y + 3.0 - pos.y) * 0.3).clamp(-1.0, 1.0),
            "under" => ((dest.y - pos.y) * 0.3).clamp(-1.0, 1.0),
            _ => 0.0,
        };
        self.steer(who, throttle, (diff * 1.6).clamp(-1.0, 1.0), climb, dt);
        // Getting nowhere: run aground, blocked, or the water ends.
        let now = self.t;
        let p = self.things.get(id).map(|t| t.pos).unwrap_or(pos);
        let (since, at) = *self.driving.entry(who).or_insert((now, p));
        if (p - at).length() > 1.0 {
            self.driving.insert(who, (now, p));
        } else if now - since > STUCK_FOR {
            self.driving.remove(&who);
            self.routes.remove(&who);
            return Some(goal < 25.0);
        }
        None
    }

    /// A free vehicle near `who` worth taking to `dest`: on land a cart for
    /// a long way; a boat when the way there crosses water.
    pub fn vehicle_for(&mut self, who: ActorId, dest: Vec3) -> Option<Target> {
        let me = self.actor(who)?.pos;
        let flat = Vec2::new(dest.x - me.x, dest.z - me.z);
        if flat.length() < 40.0 {
            return None;
        }
        let t = self.snap.terrain.clone();
        let wet = (1..20).any(|i| {
            let p = Vec2::new(me.x, me.z) + flat * (i as f32 / 20.0);
            t.height(p.x, p.y) < t.water(p.x, p.y) - MAX_WADE
        });
        let fits = |d: &Drive| match d.on.as_str() {
            "water" | "under" => wet,
            "air" => true,
            _ => !wet && flat.length() > 60.0,
        };
        let mut best: Option<(f32, Target)> = None;
        for id in self.things.near(me, 14.0) {
            let Some(th) = self.things.get(id) else { continue };
            if th.held() || self.aboard_of(id).is_some() {
                continue;
            }
            let Some(d) = self.snap.type_of(th.type_id).and_then(|ty| ty.ct.meta.drive.clone()) else { continue };
            let gap = (th.pos - me).length();
            if fits(&d) && best.as_ref().is_none_or(|b| gap < b.0) {
                best = Some((gap, Target::Thing(id)));
            }
        }
        // Placed ones nobody has touched yet.
        for p in self.snap.instances.iter().filter(|p| (p.pos - me).length() < 14.0 && !self.things.by_instance.contains_key(&p.id)) {
            let Some(d) = self.snap.type_of(p.type_id).and_then(|ty| ty.ct.meta.drive.clone()) else { continue };
            let gap = (p.pos - me).length();
            if fits(&d) && best.as_ref().is_none_or(|b| gap < b.0) {
                best = Some((gap, Target::Instance(p.id)));
            }
        }
        best.map(|b| b.1)
    }

    /// Going somewhere while in or on something: the driver steers there;
    /// a passenger rides along and gets out near it; someone on a bench
    /// gets up and walks. A boat that reaches the shore (or anything that
    /// gets stuck) is left, and the rest is walked. True while still going.
    pub(super) fn goto_aboard(&mut self, who: ActorId, target: &Target, stop: f32, deadline: f64, dt: f32) -> bool {
        let Some(dest) = self.resolve(target, who).map(|r| r.pos) else { return false };
        let Some(me) = self.actor(who).map(|a| a.pos) else { return false };
        let near = Vec2::new(dest.x - me.x, dest.z - me.z).length() <= stop + 3.0;
        let id = self.actor(who).and_then(|a| a.aboard);
        let driver = id.and_then(|v| self.aboard_of(v));
        let walk_on = |s: &mut Sim| {
            if s.leave_vehicle(who).is_err() {
                return false;
            }
            // Walk the rest (a fresh deadline: the ride took its time).
            let deadline = s.t.max(deadline) + 60.0;
            if let Some(a) = s.actor_mut(who) {
                a.task = Some(super::actor::Task::Goto { target: target.clone(), stop, run: false, deadline });
            }
            true
        };
        if self.drives_it(who) {
            match self.drive_toward(who, dest, stop, dt) {
                None if self.t <= deadline + 120.0 => true,
                None | Some(false) => walk_on(self),
                Some(true) if near => {
                    let _ = self.leave_vehicle(who);
                    false
                }
                Some(true) => walk_on(self),
            }
        } else if driver.is_some_and(|d| d != who) {
            // Riding along.
            if near || self.t > deadline + 120.0 {
                let _ = self.leave_vehicle(who);
                return !near && walk_on(self);
            }
            true
        } else {
            walk_on(self)
        }
    }

    /// The vehicle's own solid, as others meet it.
    fn vehicle_solid(&self, id: ThingId) -> Option<Solid> {
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?.clone();
        Some(super::render::thing_solid(t, ty))
    }

    /// How far to lift the traveler's eye so it isn't inside the vehicle
    /// they sit in (a closed cab): just over the part above the seat.
    pub fn eye_lift(&self, eye: Vec3) -> f32 {
        let Some(s) = self.player.aboard.and_then(|id| self.vehicle_solid(id)) else { return 0.0 };
        let top = s.inst.center().y + s.inst.radius();
        let mut lift = 0.0;
        while s.sdf(eye + Vec3::Y * lift) < 0.12 && eye.y + lift < top + 0.3 {
            lift += 0.12;
        }
        lift
    }

    /// Steer the vehicle `who` is in: `throttle` forward (−1 back … 1),
    /// `steer` right (−1 left … 1), and for a flyer `climb` (−1 down … 1
    /// up; it hovers at 0) or a submarine. Land vehicles keep to the ground
    /// and can't turn standing still; boats keep to the water; submarines
    /// keep in it, from its bed to its surface; flyers climb over what is ahead.
    pub fn steer(&mut self, who: ActorId, throttle: f32, steer: f32, climb: f32, dt: f32) {
        let Some(id) = self.actor(who).filter(|a| a.seat_no == 0).and_then(|a| a.aboard) else { return };
        let Some(t) = self.things.get(id).cloned() else { return };
        let Some(ty) = self.snap.type_of(t.type_id).cloned() else { return };
        let Some(drive) = ty.ct.meta.drive.clone() else { return };
        let (water, under, air) = (drive.on == "water", drive.on == "under", drive.on == "air");
        let power = self.drive_power(id, who);
        let max = (drive.speed * power).max(0.5);
        let b = ty.ct.meta.bounds;
        let sc = t.scale.max(0.05);
        let radius = (b[0].min(b[2]) * sc).clamp(0.2, 3.0);
        let height = ((ty.top - ty.bottom) * sc).max(0.3);
        let cap = Capsule { radius, height, step: (height * 0.25).clamp(0.15, 0.6), swims: false, needs_water: 0.0 };
        let feet = t.pos.y + ty.bottom * sc;
        let ground_here = self.snap.terrain.height(t.pos.x, t.pos.z);
        let water_here = self.snap.terrain.water(t.pos.x, t.pos.z);
        let alt = (feet - ground_here.max(if water { water_here } else { f32::MIN })).max(0.0);
        let airborne = air && alt > 0.5;

        // Speed: throttle pushes, the brake is the throttle against it, and
        // it coasts down when let be.
        let mut v = self.drives.get(&id).copied().unwrap_or(0.0);
        let accel = (max * 0.5).clamp(1.5, 8.0);
        if throttle.abs() > 0.01 {
            let k = if throttle * v >= 0.0 { 1.0 } else { 2.5 };
            v += throttle * accel * k * dt;
        } else {
            v -= v.signum() * v.abs().min((1.0 + 0.25 * v.abs()) * dt);
        }
        v = v.clamp(-max * 0.35, max);
        if (water || under) && ground_here > water_here - 0.3 {
            v = v.clamp(-AGROUND, AGROUND);
        }
        // Muscle tires whoever works it.
        if v.abs() > 0.3 && power_of(&drive) == Power::Muscle {
            if let ActorId::Npc(c) = who {
                if let Some(n) = self.cast.get_mut(c) {
                    n.needs.fatigue = (n.needs.fatigue + dt * 0.0015 * (v.abs() / max).min(1.0)).min(1.0);
                }
            }
        }

        // Turning: wheels and keels turn only when moving; flyers in place.
        let grip = if air { 1.0 } else { (v.abs() / 2.0).min(1.0) * v.signum() };
        let turn = steer * TURN * grip * (1.0 - 0.4 * (v.abs() / max).min(1.0));
        let yaw = t.yaw + turn * dt;
        let fwd = Vec3::new(yaw.sin(), 0.0, yaw.cos());
        let delta = fwd * v * dt;

        // Moving: blocked by what stands in the way, at its height.
        let from = Vec3::new(t.pos.x, feet, t.pos.z);
        let own = thing_inst(&t, &ty, [0.0; 4]).center();
        let solids: Vec<Solid> = self.solids_near(from, radius + delta.length() + 4.0).into_iter().filter(|s| !(Arc::ptr_eq(&s.ty, &ty) && (s.inst.center() - own).length() < 0.05)).collect();
        let bodies: Vec<(Vec3, f32)> = self
            .actor_ids()
            .into_iter()
            .filter(|a| *a != who)
            .filter_map(|a| self.actor(a).filter(|x| !x.carried() && x.alt < 1.0 && (x.pos - from).length() < radius + delta.length() + 4.0).map(|x| (x.pos, super::footing::walk_radius(a, x))))
            .collect();
        let obs = Obstacles { solids: &solids, bodies: &bodies };
        let terrain = &self.snap.terrain;
        let goes = |x: f32, z: f32| {
            let g = terrain.height(x, z);
            if airborne {
                true
            } else if water || under {
                g < terrain.water(x, z) + 0.2
            } else {
                g > terrain.water(x, z) - MAX_WADE
            }
        };
        let free = |x: f32, z: f32| goes(x, z) && (obs.dist_body(x, z, feet, cap) >= radius || obs.clear(x, z, feet, cap));
        let mut to = from;
        if delta.length() > 1e-5 {
            let stuck = obs.dist_body(from.x, from.z, feet, cap) < radius * 0.5 && !obs.clear(from.x, from.z, feet, cap);
            let tries = [Vec2::new(delta.x, delta.z), Vec2::new(delta.x, 0.0), Vec2::new(0.0, delta.z)];
            for (i, d) in tries.iter().enumerate() {
                let (x, z) = (from.x + d.x, from.z + d.y);
                if (stuck && goes(x, z)) || free(x, z) {
                    to = Vec3::new(x, feet, z);
                    if i > 0 {
                        v *= 0.9;
                    }
                    break;
                }
            }
            if to == from {
                // Ran into something: it stops dead.
                if v.abs() > 3.0 {
                    self.event("crashed", Some(who), Some(format!("thing:{id}")), format!("the {} runs into something", ty.name()), Some(from), json!({ "speed": v.abs() }));
                }
                v = 0.0;
            }
        }

        // Height: on the ground, afloat, or flying.
        let g = self.snap.terrain.height(to.x, to.z);
        let wl = self.snap.terrain.water(to.x, to.z);
        let support = support_under(&self.snap.terrain, &solids, to, radius * 0.7, feet + cap.step);
        let draft = (height * 0.25).clamp(0.1, 1.0);
        let bob = (self.t as f32 * 1.3 + id as f32).sin() * 0.04;
        let new_feet = if water {
            if g < wl - draft { wl - draft + bob } else { support.max(wl - draft) }
        } else if under {
            // Up or down as asked, between the bed (or what lies on it) and
            // the surface, where it floats; let be, it holds its depth.
            (feet + climb.clamp(-1.0, 1.0) * CLIMB * dt).min(wl - draft + bob).max(support)
        } else if air {
            // Up or down as asked, in place or moving; let be, it hovers.
            (feet + climb.clamp(-1.0, 1.0) * CLIMB * dt).max(support).min(g.max(wl) + 150.0)
        } else {
            // Down a slope at once; up only what it can roll over.
            support
        };
        let afloat = (water || under) && g < wl - draft && new_feet > wl - draft - 0.2;
        let dived = under && !afloat && new_feet > g + 0.3;

        // Lean with the ground under it (or into the turn in the air).
        let tilt = if (air && new_feet - g.max(wl) > 0.5) || dived {
            let rise = (new_feet - feet) / (v.abs() * dt).max(0.05);
            Quat::from_rotation_x(-rise.atan().clamp(-0.4, 0.4)) * Quat::from_rotation_z(-steer * 0.25)
        } else if afloat {
            Quat::from_rotation_z((self.t as f32 * 1.1 + id as f32).sin() * 0.03 - steer * 0.05 * (v.abs() / max))
        } else {
            let (hz, hx) = (b[2] * sc * 0.8, b[0] * sc * 0.8);
            let right = Vec3::new(fwd.z, 0.0, -fwd.x);
            let h = |p: Vec3| self.snap.terrain.height(p.x, p.z).max(if water { self.snap.terrain.water(p.x, p.z) - draft } else { f32::MIN });
            let pitch = ((h(to + fwd * hz) - h(to - fwd * hz)) / (2.0 * hz).max(0.1)).atan().clamp(-0.5, 0.5);
            let roll = ((h(to + right * hx) - h(to - right * hx)) / (2.0 * hx).max(0.1)).atan().clamp(-0.5, 0.5);
            Quat::from_rotation_x(-pitch) * Quat::from_rotation_z(roll)
        };
        let tilt = t.tilt().slerp(tilt, (dt * 8.0).min(1.0));

        let dyaw = yaw - t.yaw;
        if let Some(th) = self.things.get_mut(id) {
            th.pos = Vec3::new(to.x, new_feet - ty.bottom * sc, to.z);
            th.yaw = yaw;
            th.set_tilt(tilt);
            th.vel = Vec3::ZERO;
            th.asleep = true;
            th.dirty = true;
        }
        self.things.moved();
        self.drives.insert(id, v);
        if let Some(a) = self.actor_mut(who) {
            a.yaw += dyaw;
            a.moved = (to - from).length();
            a.alt = (new_feet - if under { g } else { g.max(wl) }).max(0.0);
        }
        self.update_aboard();
    }
}

/// A way by water from `from` toward `to` for something drawing `draft`:
/// waypoints about 6 m apart through water deep enough, ending at `to` or,
/// when it is on land, at the water nearest it. A search on a 2 m grid in a
/// box round both ends; None when there is no water to start from.
pub fn water_route(t: &crate::terrain::Terrain, from: Vec2, to: Vec2, draft: f32) -> Option<Vec<Vec2>> {
    use std::cmp::Reverse;
    use std::collections::{BinaryHeap, HashMap};
    const CELL: f32 = 2.0;
    let lo = from.min(to) - Vec2::splat(80.0);
    let hi = from.max(to) + Vec2::splat(80.0);
    let n = ((hi - lo) / CELL).ceil().min(Vec2::splat(320.0));
    let (nx, nz) = (n.x as i32, n.y as i32);
    let cell = |p: Vec2| (((p.x - lo.x) / CELL) as i32, ((p.y - lo.y) / CELL) as i32);
    let mid = |c: (i32, i32)| lo + (Vec2::new(c.0 as f32, c.1 as f32) + 0.5) * CELL;
    let mut wet_cache: HashMap<(i32, i32), bool> = HashMap::new();
    let mut wet = |c: (i32, i32)| -> bool {
        if c.0 < 0 || c.1 < 0 || c.0 >= nx || c.1 >= nz {
            return false;
        }
        *wet_cache.entry(c).or_insert_with(|| {
            let p = mid(c);
            t.water(p.x, p.y) - t.height(p.x, p.y) > draft + 0.2
        })
    };
    let start = cell(from);
    // Afloat but close to a quay the cell's middle may be dry: the nearest wet one.
    let start = (0..3).flat_map(|r| (-r..=r).flat_map(move |dz| (-r..=r).map(move |dx| (start.0 + dx, start.1 + dz)))).find(|c| wet(*c))?;
    let goal = cell(to);
    let h = |c: (i32, i32)| ((c.0 - goal.0) as f32).hypot((c.1 - goal.1) as f32);
    let mut open = BinaryHeap::new();
    let mut came: HashMap<(i32, i32), (i32, i32)> = HashMap::new();
    let mut cost: HashMap<(i32, i32), f32> = HashMap::new();
    cost.insert(start, 0.0);
    open.push(Reverse(((h(start) * 1000.0) as u32, start)));
    let mut best = (h(start), start);
    let mut seen = 0;
    while let Some(Reverse((_, c))) = open.pop() {
        seen += 1;
        if c == goal || seen > 60_000 {
            break;
        }
        let g = cost[&c];
        for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
            let nb = (c.0 + dx, c.1 + dz);
            // Not cutting a corner past the shore.
            if !wet(nb) || (dx != 0 && dz != 0 && (!wet((c.0 + dx, c.1)) || !wet((c.0, c.1 + dz)))) {
                continue;
            }
            // Keep off the shore: the middle of a canal, under a bridge's arch.
            let shore = [(2, 0), (-2, 0), (0, 2), (0, -2), (1, 1), (1, -1), (-1, 1), (-1, -1)].iter().filter(|o| !wet((nb.0 + o.0, nb.1 + o.1))).count() as f32;
            let ng = g + if dx != 0 && dz != 0 { 1.414 } else { 1.0 } + shore * 0.6;
            if cost.get(&nb).is_none_or(|old| ng < *old) {
                cost.insert(nb, ng);
                came.insert(nb, c);
                let hn = h(nb);
                if hn < best.0 {
                    best = (hn, nb);
                }
                open.push(Reverse((((ng + hn) * 1000.0) as u32, nb)));
            }
        }
    }
    let end = if cost.contains_key(&goal) { goal } else { best.1 };
    let mut path = vec![end];
    let mut c = end;
    while let Some(p) = came.get(&c) {
        path.push(*p);
        c = *p;
    }
    path.reverse();
    let mut out: Vec<Vec2> = path.iter().skip(1).step_by(3).map(|c| mid(*c)).collect();
    let last = if end == goal && wet(goal) { to } else { mid(end) };
    if out.last().is_none_or(|l| (*l - last).length() > 0.5) {
        out.push(last);
    }
    Some(out)
}

//! Toy physics: gravity, velocity, bounce and friction for loose things.
//! Things are spheres against the terrain and against the signed distance
//! fields of nearby solids, so a ball bounces off any shape the LLM wrote
//! with no extra work. Things at rest sleep and cost nothing.

use super::props::*;
use super::things::ThingId;
use super::{ActorId, Note, Sim, Target};
use crate::world::Solid;
use glam::{Quat, Vec3};
use serde_json::json;

/// Earth's pull (m/s²): a world's own is `Sim::gravity`. Muscles are
/// built for this one: a jump leaves the ground as fast anywhere.
pub const GRAVITY: f32 = 9.81;
/// Below this impact speed (m/s) nothing gets hurt.
pub const IMPACT_SAFE: f32 = 2.5;

/// The outward normal of a solid's surface at `p` (finite differences).
fn solid_normal(s: &Solid, p: Vec3) -> Vec3 {
    let e = 0.02;
    let n = Vec3::new(
        s.sdf(p + Vec3::X * e) - s.sdf(p - Vec3::X * e),
        s.sdf(p + Vec3::Y * e) - s.sdf(p - Vec3::Y * e),
        s.sdf(p + Vec3::Z * e) - s.sdf(p - Vec3::Z * e),
    );
    n.try_normalize().unwrap_or(Vec3::Y)
}

/// Is `p` ringed by the shape: walking out horizontally in most directions,
/// do you run into it within `reach`?
fn enclosed(s: &Solid, p: Vec3, reach: f32) -> bool {
    let mut hits = 0;
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::FRAC_PI_4;
        let dir = Vec3::new(a.cos(), 0.0, a.sin());
        let mut t = 0.02;
        for _ in 0..12 {
            let d = s.sdf(p + dir * t);
            if d < 0.01 {
                hits += 1;
                break;
            }
            t += d.max(0.01);
            if t > reach {
                break;
            }
        }
    }
    hits >= 6
}

/// Distance from a point to an actor's body (a vertical capsule).
fn body_dist(p: Vec3, feet: Vec3, height: f32) -> (f32, Vec3) {
    // A person's capsule (0.3 m round, from 0.3 m to 0.25 m below the top),
    // in proportion for smaller and bigger bodies.
    let s = height / 1.75;
    let y = p.y.clamp(feet.y + 0.3 * s, feet.y + height - 0.25 * s);
    let c = Vec3::new(feet.x, y, feet.z);
    let d = p - c;
    let l = d.length();
    (l - 0.3 * s, if l > 1e-5 { d / l } else { Vec3::Y })
}

impl Sim {
    /// How hard things fall in this world (m/s²).
    pub fn gravity(&self) -> f32 {
        self.snap.look.gravity
    }

    pub fn step_physics(&mut self, dt: f32) {
        self.update_held();
        let mut ids: Vec<(f32, ThingId)> = self
            .things
            .live()
            .filter(|t| !t.asleep && !t.anchored && t.holder.is_none() && t.worn.is_none() && t.stuck_in.is_none())
            .map(|t| (self.dist_to_travelers(t.pos), t.id))
            .collect();
        if ids.is_empty() {
            return;
        }
        ids.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
        let max = self.cfg.max_awake;
        let medium = self.cfg.medium;
        let sub = ((dt * 90.0).ceil() as usize).clamp(1, 8);
        let h = dt / sub as f32;
        for (i, (d, id)) in ids.into_iter().enumerate() {
            // Something sent flying (a spear, an arrow, a shell) is followed
            // further out than things that only roll about.
            let flying = self.things.get(id).is_some_and(|t| t.thrown_by.is_some() && t.vel.length() > 6.0);
            if i >= max || d > if flying { medium * 3.0 } else { medium } {
                // Too many in motion, or too far: freeze where it is; a shot
                // gone that far is gone.
                if self.things.get(id).is_some_and(|t| t.shot) {
                    self.things.remove(id);
                    continue;
                }
                if let Some(t) = self.things.get_mut(id) {
                    t.asleep = true;
                    t.vel = Vec3::ZERO;
                }
                continue;
            }
            for _ in 0..sub {
                if !self.integrate(id, h) {
                    break;
                }
            }
        }
        self.things.moved();
    }

    /// Worn layers go where their wearer goes (drawn in the wearer's pose).
    fn update_worn(&mut self) {
        let worn: Vec<(ThingId, ActorId)> = self.things.live().filter_map(|t| t.worn.map(|w| (t.id, w))).collect();
        for (id, w) in worn {
            match self.actor(w).map(|a| (a.pos, a.yaw)) {
                Some((p, yaw)) => {
                    if let Some(t) = self.things.get_mut(id) {
                        t.pos = p;
                        t.yaw = yaw;
                        t.vel = Vec3::ZERO;
                        t.asleep = true;
                    }
                }
                None => {
                    if let Some(t) = self.things.get_mut(id) {
                        t.worn = None;
                        t.asleep = false;
                    }
                }
            }
        }
    }

    /// Held things follow their holders' hands.
    fn update_held(&mut self) {
        self.update_worn();
        self.update_stuck();
        let held: Vec<(ThingId, ActorId, Option<ActorId>)> = self.things.live().filter_map(|t| t.holder.map(|h| (t.id, h, t.co_holder))).collect();
        for (id, h, co) in held {
            let Some(t) = self.things.get(id) else { continue };
            let Some(ty) = self.snap.type_of(t.type_id).cloned() else { continue };
            // Too heavy for one: gripped, not lifted, until a second pair of hands comes.
            if co.is_none() && t.mass() > self.strength(h) {
                let tp = t.pos;
                if self.actor(h).is_none_or(|a| (a.pos - tp).length() > 3.0) {
                    self.release(id);
                }
                continue;
            }
            let big = ty.radius() * t.scale > 0.35 || co.is_some();
            let hand = |s: &Sim, a: ActorId| s.actor(a).map(|x| x.hand(big));
            let Some(mut p) = hand(self, h) else {
                self.release(id);
                continue;
            };
            let yaw = self.actor(h).map(|a| a.yaw).unwrap_or(0.0);
            if let Some(c) = co {
                match hand(self, c) {
                    Some(q) => p = (p + q) * 0.5,
                    None => {
                        if let Some(t) = self.things.get_mut(id) {
                            t.co_holder = None;
                        }
                    }
                }
            }
            // A tool is held by its grip, turned as the hand turns it (at
            // rest, or through a motion); anything else by its middle.
            let hold = match (&ty.ct.meta.tool, co) {
                (Some(tool), None) => {
                    let fr = match self.actor(h).and_then(|a| a.motion.as_ref()) {
                        Some(m) if m.tool == id => m.frame(self.t),
                        _ => super::tool::REST,
                    };
                    Some((super::tool::tool_turn(tool, &fr), Vec3::from_array(tool.grip)))
                }
                _ => None,
            };
            let dt = self.frame_dt.max(1e-3);
            if let Some(t) = self.things.get_mut(id) {
                let target = match hold {
                    Some((q, grip)) => {
                        let yq = glam::Quat::from_rotation_y(yaw);
                        p - yq * (q * (grip * t.scale))
                    }
                    None => {
                        let (c0, _) = t.proxy(&ty);
                        p - (c0 - t.pos)
                    }
                };
                t.vel = (target - t.pos) / dt;
                t.vel = t.vel.clamp_length_max(30.0);
                t.pos = target;
                t.yaw = yaw;
                t.hold_tilt = hold.map(|h| h.0);
                t.asleep = false;
                t.rest_t = 0.0;
            }
        }
        // Carriers who drift too far apart let go (a long thing can be held
        // by its two ends).
        let pairs: Vec<(ThingId, ActorId, ActorId, f32)> = self
            .things
            .live()
            .filter_map(|t| {
                let b = self.snap.type_of(t.type_id).map(|ty| ty.ct.meta.bounds).unwrap_or_default();
                Some((t.id, t.holder?, t.co_holder?, 3.0 + 2.0 * b[0].max(b[2]) * t.scale))
            })
            .collect();
        for (id, a, b, apart) in pairs {
            let (Some(pa), Some(pb)) = (self.actor(a).map(|x| x.pos), self.actor(b).map(|x| x.pos)) else { continue };
            if (pa - pb).length() > apart {
                let name = self.thing_name(id);
                self.drop_thing(id, Vec3::ZERO);
                self.event("dropped", Some(a), Some(format!("thing:{id}")), format!("{} and {} lost their grip on the {name}", self.actor_name(a), self.actor_name(b)), Some(pa), json!({}));
            }
        }
    }

    /// One physics substep for one thing. Returns false once it sleeps.
    fn integrate(&mut self, id: ThingId, h: f32) -> bool {
        let Some(t) = self.things.get(id) else { return false };
        let Some(ty) = self.snap.type_of(t.type_id).cloned() else { return false };
        // Flying point first, it is its tip that meets things.
        let pointed = t.point_first && ty.ct.meta.tool.is_some();
        let (c, r) = if pointed { self.flight_tip(id).unwrap_or_else(|| t.proxy(&ty)) } else { t.proxy(&ty) };
        let Some(t) = self.things.get(id) else { return false };
        let off = c - t.pos;
        let mut v = t.vel;
        let mass = t.mass();
        let bounce = t.props[P_BOUNCE];
        let friction = t.props[P_FRICTION];
        let thrown = t.thrown_by;
        let old_pos = t.pos;
        // Not something anyone catches: a point, a shot, a thing that goes off.
        let missile = pointed || t.shot || ty.ct.meta.burst.is_some();
        let g = self.gravity();
        v.y -= g * h;
        // Water: float if lighter than water, drag either way.
        // Inside a cave the sea doesn't reach.
        let wl = self.snap.terrain.water(c.x, c.z);
        let wl = if c.y < wl && !(self.snap.terrain.caves_near(c.x, c.z) && self.snap.terrain.void(c) < 0.0) { wl } else { f32::MIN };
        if c.y < wl {
            let vol = volume(&ty) * t.scale.powi(3);
            let density = mass / vol.max(1e-4);
            let depth = ((wl - c.y) / r.max(0.05)).min(1.0);
            v.y += g * depth * (1000.0 / density.max(50.0)).min(4.0) * h;
            v *= 1.0 - (2.5 * h).min(0.5);
        }
        let mut p = c + v * h;
        // Fast and small, it could pass through a wall or a body between
        // one step and the next: it stops where it first meets one.
        if (p - c).length() > r {
            p = self.sweep(id, c, p, r, ty.id, old_pos, thrown);
        }
        let v_in = v;
        let mut contact = false;
        let mut impact = 0.0f32;
        let mut hit_name: Option<String> = None;
        // What it struck hardest: the surface's outward normal, and the body if one.
        let mut struck: Option<(Vec3, Option<ActorId>)> = None;
        // Terrain.
        let ground = self.snap.terrain.floor(p.x, p.y + r * 0.5, p.z);
        let mut grip = false;
        // In a cave: its walls and roof hold it in.
        let caved = ground < self.snap.terrain.height(p.x, p.z) - 0.3;
        if caved {
            let t = &self.snap.terrain;
            let dv = t.void(p);
            if dv > -r {
                let e = 0.05;
                let g = Vec3::new(t.void(p + Vec3::X * e) - t.void(p - Vec3::X * e), t.void(p + Vec3::Y * e) - t.void(p - Vec3::Y * e), t.void(p + Vec3::Z * e) - t.void(p - Vec3::Z * e));
                let n = -g.normalize_or_zero();
                if n != Vec3::ZERO {
                    p += n * (dv + r + 1e-3);
                    let vn = v.dot(n);
                    if vn < 0.0 {
                        if -vn > impact {
                            struck = Some((n, None));
                        }
                        impact = impact.max(-vn);
                        v -= (1.0 + if -vn < 1.0 { 0.0 } else { bounce }) * vn * n;
                    }
                    contact = true;
                    grip = n.y > 0.7 && friction > 0.2;
                }
            }
        }
        if !caved && p.y - r < ground {
            let n = self.snap.terrain.normal(p.x, p.z);
            p.y = ground + r;
            let vn = v.dot(n);
            if vn < 0.0 {
                if -vn > impact {
                    struck = Some((n, None));
                }
                impact = impact.max(-vn);
                // Slow landings don't bounce (no endless jitter).
                let e = if -vn < 1.0 { 0.0 } else { bounce };
                v -= (1.0 + e) * vn * n;
            }
            contact = true;
            // Things stay put on slopes gentler than their grip (round,
            // bouncy things roll on).
            let tan = (1.0 - n.y * n.y).max(0.0).sqrt() / n.y.max(0.05);
            grip = tan < friction * (1.25 - bounce).max(0.1);
        }
        // Solids.
        let solids = self.solids_near(p, r + 0.6);
        for s in &solids {
            // Its own shape is not an obstacle.
            if (s.inst.pos() - old_pos).length() < 1e-3 && s.ty.id == ty.id {
                continue;
            }
            let d = s.sdf(p);
            if d < r {
                let n = solid_normal(s, p);
                p += n * (r - d + 1e-3);
                let vn = v.dot(n);
                if vn < 0.0 {
                    if -vn > impact {
                        impact = -vn;
                        hit_name = Some(s.ty.name().to_string());
                        struck = Some((n, None));
                    }
                    // Slow landings don't bounce: a ball comes to rest on a
                    // shelf, a floor, a bush, as it does on the ground.
                    let e = if -vn < 1.0 { 0.0 } else { bounce };
                    v -= (1.0 + e) * vn * n;
                }
                contact = true;
                // Resting on top of it grips like the ground does.
                if n.y > 0.7 {
                    grip = grip || friction * (1.25 - bounce).max(0.1) > (1.0 - n.y * n.y).max(0.0).sqrt() / n.y.max(0.05);
                }
            } else if v.y < -0.5 && !s.ty.builtin && d > r && thrown.is_some() {
                // Falling through an opening of something (a hoop, a well, a basket…):
                // the thing is surrounded by the shape on most sides.
                let lp = s.inst.to_local(p);
                let b = s.ty.ct.meta.bounds;
                if lp[0].abs() < b[0] && lp[2].abs() < b[2] && lp[1].abs() < b[1] && self.things.get(id).is_some_and(|t| t.through != Some(s.ty.id)) {
                    let reach = (b[0].max(b[2]) * s.inst.pos_scale[3]).min(2.0);
                    if enclosed(s, p, reach) {
                        let name = s.ty.name().to_string();
                        if let Some(t) = self.things.get_mut(id) {
                            t.through = Some(s.ty.id);
                        }
                        self.event("through", thrown.map(|x| x.0), Some(format!("thing:{id}")), format!("the {} went through the {name}", ty.name()), Some(p), json!({ "target": name }));
                        self.note_near(p, 25.0, Note::Ambient(format!("The {} drops through the {name}!", ty.name())));
                        self.witness(p, 25.0, &format!("I saw the {} go through the {name}.", ty.name()), 0.35, &[]);
                        if let Some((who, _)) = thrown {
                            self.on_scored(who, &name);
                        }
                    }
                }
            }
        }
        // People.
        let actors = self.actor_ids();
        for a in actors {
            let Some(body) = self.actor(a).map(|x| (x.pos, x.catching)) else { continue };
            if self.things.get(id).is_some_and(|t| t.holder == Some(a)) {
                continue;
            }
            // Just let go of: it leaves the hand that threw it without
            // striking the body behind (a throw over the shoulder).
            if thrown.is_some_and(|(by, at)| by == a && self.t - at < 0.4) {
                continue;
            }
            let height = self.actor_height(a);
            let (d, n) = body_dist(p, body.0, height);
            if d < r + 0.25 && body.1 > self.t && !missile && v.length() > 0.5 && self.actor(a).is_some_and(|x| x.held.is_none()) && mass <= self.strength(a) {
                // Caught.
                let thrower = thrown.map(|x| x.0);
                if let Some(t) = self.things.get_mut(id) {
                    t.holder = Some(a);
                    t.asleep = false;
                    t.thrown_by = None;
                    t.dirty = true;
                }
                if let Some(x) = self.actor_mut(a) {
                    x.held = Some(id);
                    x.catching = 0.0;
                }
                let name = ty.name().to_string();
                let who = self.actor_name(a);
                self.event("caught", Some(a), Some(format!("thing:{id}")), format!("{who} caught the {name}"), Some(p), json!({ "from": thrower }));
                self.note_near(p, 30.0, Note::Ambient(format!("{} caught the {name}.", cap(&who))));
                self.on_caught(a, thrower, id);
                return false;
            }
            if d < r {
                p += n * (r - d + 1e-3);
                let vn = v.dot(n);
                if vn < 0.0 {
                    if -vn > impact {
                        impact = -vn;
                        hit_name = Some(self.actor_name(a));
                        struck = Some((n, Some(a)));
                    }
                    // Something heavy and fast rocks the body it hits, and
                    // what someone sent flying hurts it.
                    let body = self.actor(a).map(|x| x.dims.mass.max(1.0)).unwrap_or(70.0);
                    let give = mass * -vn / body * 0.25;
                    if -vn > 2.0 && give > 0.03 {
                        self.jolt(a, -n, give.min(0.35));
                    }
                    // A point goes in with all its speed; anything else
                    // strikes with what of it meets the body.
                    let into = if pointed { v.length() } else { -vn };
                    if into > IMPACT_SAFE {
                        self.flying_hit(id, a, into, -n);
                    }
                    v -= (1.0 + bounce * 0.5) * vn * n;
                }
            }
        }
        // Rolling and sliding.
        if contact {
            let k = (friction * 6.0 * h).min(1.0);
            let vy = v.y;
            v *= 1.0 - k;
            v.y = vy;
            if grip && v.length() < 1.2 {
                v *= 1.0 - (14.0 * h).min(1.0);
            }
        }
        let speed = v.length();
        let mut sleep = false;
        if let Some(t) = self.things.get_mut(id) {
            t.pos = p - off;
            t.vel = v;
            t.in_contact = contact;
            // Rolling turns it (not one flying point first: that keeps its line).
            if contact && !pointed {
                t.spin = Vec3::new(v.x, 0.0, v.z).length() / r.max(0.05) * 0.3;
            }
            t.yaw += t.spin * h;
            t.dirty = true;
            if contact && speed < 0.15 {
                t.rest_t += h;
                if t.rest_t > 0.4 {
                    t.asleep = true;
                    t.vel = Vec3::ZERO;
                    t.spin = 0.0;
                    t.thrown_by = None;
                    let g = self.snap.terrain.height(t.pos.x, t.pos.z);
                    let ry = t.rest_y(&ty, g);
                    if (t.pos.y - ry).abs() < r + 0.1 && t.pos.y - ry < 0.3 {
                        t.pos.y = t.pos.y.max(ry);
                    }
                    sleep = true;
                }
            } else {
                t.rest_t = 0.0;
            }
        }
        // Flying point first: it sticks where it strikes keen and fast
        // enough along its point; else it glances off and falls flat.
        if pointed {
            match struck {
                Some((n, who)) => {
                    let along = self.point_dir(id).map(|d| v_in.dot(d)).unwrap_or(0.0);
                    let keen = self.keen_of(id);
                    let alive = who.is_none_or(|a| self.actor(a).is_some());
                    if keen >= super::missile::STICK_KEEN && along > super::missile::STICK_SPEED && v_in.dot(n) < 0.0 && alive {
                        let len = ty.ct.meta.tool.as_ref().map(|t| (Vec3::from_array(t.tip) - Vec3::from_array(t.grip)).length()).unwrap_or(0.3) * self.things.get(id).map(|t| t.scale).unwrap_or(1.0);
                        let depth = ((0.02 + 0.006 * along) * keen).min(len * 0.35);
                        if impact > IMPACT_SAFE {
                            self.impact(id, impact, hit_name.clone());
                        }
                        if self.things.get(id).is_none() {
                            return false;
                        }
                        match who {
                            Some(a) => self.stick_in(id, a, depth),
                            None => self.stick_fast(id, depth, hit_name.as_deref().unwrap_or("the ground")),
                        }
                        if self.things.get(id).is_some_and(|t| t.shot) && ty.radius() * self.things.get(id).map(|t| t.scale).unwrap_or(1.0) < super::missile::UNSEEN {
                            self.things.remove(id);
                        }
                        return false;
                    }
                    self.lay_flat(id, v);
                }
                None if contact => self.lay_flat(id, v),
                None if v.length() > 1.0 => self.point_along(id, v, Some(p)),
                None => {}
            }
        }
        if sleep {
            self.rest_tilted(id);
            // A spent shot too small to see is cleared away.
            if self.things.get(id).is_some_and(|t| t.shot) && ty.radius() * self.things.get(id).map(|t| t.scale).unwrap_or(1.0) < super::missile::UNSEEN {
                self.things.remove(id);
                return false;
            }
        }
        if impact > IMPACT_SAFE {
            self.impact(id, impact, hit_name.clone());
            if self.things.get(id).is_none() {
                return false;
            }
        }
        self.push_neighbours(id, p, r, v);
        !sleep
    }

    /// The first point on the way from `from` to `to` where a sphere of
    /// radius `r` meets the ground, a solid or a body (or `to`).
    #[allow(clippy::too_many_arguments)]
    fn sweep(&mut self, id: ThingId, from: Vec3, to: Vec3, r: f32, type_id: u32, old_pos: Vec3, thrown: Option<(ActorId, f64)>) -> Vec3 {
        let d = to - from;
        let len = d.length();
        let Some(dir) = d.try_normalize() else { return to };
        let solids: Vec<Solid> = self.solids_near((from + to) * 0.5, len * 0.5 + r + 0.6).into_iter().filter(|s| !((s.inst.pos() - old_pos).length() < 1e-3 && s.ty.id == type_id)).collect();
        let holder = self.things.get(id).and_then(|t| t.holder);
        let bodies: Vec<(Vec3, f32)> = self
            .actor_ids()
            .into_iter()
            .filter(|a| Some(*a) != holder && !thrown.is_some_and(|(by, at)| by == *a && self.t - at < 0.4))
            .filter_map(|a| self.actor(a).map(|x| (x.pos, self.actor_height(a))))
            .filter(|(p, hgt)| {
                // Only those near the way.
                let q = *p + Vec3::Y * hgt * 0.5 - from;
                let s = q.dot(dir).clamp(0.0, len);
                (q - dir * s).length() < hgt + r + 1.0
            })
            .collect();
        let terrain = &self.snap.terrain;
        let above = |q: Vec3| (q.y - r - terrain.floor(q.x, q.y + r * 0.5, q.z)) * 0.6;
        // Already on the ground (sliding along it): the ground is met after
        // the step, not swept for, or it never leaves where it lies.
        let on_ground = above(from) <= 0.0;
        let mut s = 0.0f32;
        for _ in 0..160 {
            let q = from + dir * s;
            // The ground: how far above it, taken short (it slopes).
            let mut gap = if on_ground { f32::MAX } else { above(q) };
            for so in &solids {
                gap = gap.min(so.sdf(q) - r);
            }
            for (feet, hgt) in &bodies {
                gap = gap.min(body_dist(q, *feet, *hgt).0 - r);
            }
            if gap <= 0.0 {
                return q;
            }
            s += gap.max(0.01);
            if s >= len {
                return to;
            }
        }
        from + dir * s.min(len)
    }

    /// Small things in the way get knocked.
    fn push_neighbours(&mut self, id: ThingId, p: Vec3, r: f32, v: Vec3) {
        let near = self.things.near(p, r + 1.5);
        for o in near {
            if o == id {
                continue;
            }
            let Some(ot) = self.things.get(o) else { continue };
            if ot.anchored || ot.held() {
                continue;
            }
            let Some(oty) = self.snap.type_of(ot.type_id).cloned() else { continue };
            let (oc, or) = ot.proxy(&oty);
            let d = (oc - p).length();
            if d >= r + or || d < 1e-5 {
                continue;
            }
            let n = (oc - p) / d;
            // Fire meeting something that goes off lights it, at once.
            let lights = |a: &super::things::Thing| a.props[P_FIRE] > 0.1;
            let (mine, theirs) = (self.things.get(id).is_some_and(lights), lights(ot));
            if mine && oty.ct.meta.burst.is_some() {
                let by = self.things.get(id).and_then(|t| t.thrown_by.map(|x| x.0));
                self.light(by, o);
            } else if theirs && self.burst_of(id).is_some() {
                self.light(None, id);
            }
            let Some(ot) = self.things.get(o) else { continue };
            let my_mass = self.things.get(id).map(|t| t.mass()).unwrap_or(1.0);
            let om = ot.mass();
            let rel = v.dot(n) - ot.vel.dot(n);
            let push = r + or - d;
            let share = my_mass / (my_mass + om);
            if let Some(t) = self.things.get_mut(o) {
                t.pos += n * push * share;
                if rel > 0.0 {
                    t.vel += n * rel * share * 1.6;
                }
                if t.vel.length() > 0.2 {
                    t.asleep = false;
                    t.rest_t = 0.0;
                }
                t.dirty = true;
            }
            if let Some(t) = self.things.get_mut(id) {
                t.pos -= n * push * (1.0 - share);
                if rel > 0.0 {
                    t.vel -= n * rel * (1.0 - share) * 1.6;
                }
            }
        }
    }

    /// Something hit something at `speed` m/s.
    pub fn impact(&mut self, id: ThingId, speed: f32, with: Option<String>) {
        if self.burst_on_impact(id, speed) {
            return;
        }
        let Some(t) = self.things.get(id) else { return };
        // Something that goes off is set off, not shattered into more of itself.
        let fragile = if self.burst_of(id).is_some() { 0.0 } else { t.props[P_FRAGILE] };
        let pos = t.pos;
        let (type_id, props) = (t.type_id, t.props.clone());
        let name = self.thing_name(id);
        if fragile > 0.0 {
            if let Some(t) = self.things.get_mut(id) {
                t.props[P_HEALTH] -= fragile * (speed - IMPACT_SAFE) * 0.4;
            }
        }
        if speed > 1.2 {
            if let Some(ty) = self.snap.type_of(type_id).cloned() {
                let mass = props[P_MASS];
                let mat = crate::audio::call::Material::of(&props, &ty.ct.meta.tags, crate::audio::call::Material::from_meta(&ty.ct.meta.sound).as_ref());
                self.cue(pos, None, crate::audio::Heard::Hit { mat, mass, speed, by: None });
            }
        }
        self.run_touch(id, speed);
        if speed > 4.0 {
            let what = with.clone().unwrap_or_else(|| "the ground".into());
            let by = self.things.get(id).and_then(|t| t.thrown_by.map(|x| x.0));
            self.event("hit", by, Some(format!("thing:{id}")), format!("the {name} hit {what} at {speed:.0} m/s"), Some(pos), json!({ "with": with, "speed": (speed as f64 * 10.0).round() / 10.0 }));
        }
        if fragile > 0.0 && self.things.get(id).is_some_and(|t| t.props[P_HEALTH] <= 0.0 && t.props[P_FRAGILE] > 0.0) {
            self.break_thing(id);
        }
    }

    /// A fragile thing breaks into pieces that keep its properties (a broken
    /// lamp is still hot and still full of oil). Each piece is a different
    /// part of it, cut out of the whole: slices across its longer side, with
    /// a bite out of the broken face, falling apart where they stood.
    pub fn break_thing(&mut self, id: ThingId) {
        let Some(t) = self.things.get(id).cloned() else { return };
        let Some(ty) = self.type_entry(t.type_id) else { return };
        self.release(id);
        self.things.remove(id);
        let n = 2 + (self.rand() * 2.0) as usize;
        let b = ty.ct.meta.bounds;
        // Across the longer flat side, so every piece keeps its footing.
        let axis = if b[0] >= b[2] { 0 } else { 2 };
        let half = b[axis].max(0.01);
        let big = 4.0 * b[0].max(b[1]).max(b[2]).max(ty.top - ty.bottom) + 1.0;
        let mid_y = (ty.top + ty.bottom) * 0.5;
        let at = |along: f32, y: f32| {
            let mut c = [0.0, y, 0.0];
            c[axis] = along;
            c
        };
        let rot = Quat::from_rotation_y(t.yaw) * t.tilt();
        for i in 0..n {
            if self.things.len() >= self.cfg.max_things {
                break;
            }
            let lo = -half + 2.0 * half * i as f32 / n as f32;
            let hi = -half + 2.0 * half * (i + 1) as f32 / n as f32;
            let mut cuts: Vec<[f32; 4]> = Vec::new();
            if i > 0 {
                let c = at(lo - big, mid_y);
                cuts.push([c[0], c[1], c[2], -big]);
            }
            if i + 1 < n {
                let c = at(hi + big, mid_y);
                cuts.push([c[0], c[1], c[2], -big]);
            }
            // A bite out of a broken face, so no two pieces are alike.
            let face = if i + 1 < n { hi } else { lo };
            let r = (half / n as f32).max(0.03) * (0.5 + self.rand() * 0.5);
            let y = ty.bottom + (ty.top - ty.bottom) * (0.3 + self.rand() * 0.6);
            let mut c = at(face, y);
            c[if axis == 0 { 2 } else { 0 }] = (self.rand() * 2.0 - 1.0) * b[2 - axis] * 0.6;
            cuts.push([c[0], c[1], c[2], r]);
            for o in &t.shape.cuts {
                if cuts.len() < crate::render::MAX_CUTS {
                    cuts.push(*o);
                }
            }
            // Out from the middle, the way its part lay.
            let out_local = at((lo + hi) * 0.5, 0.0);
            let out = (rot * Vec3::from_array(out_local) * t.scale).normalize_or(Vec3::new(self.rand() - 0.5, 0.0, self.rand() - 0.5).normalize_or(Vec3::X));
            let nid = self.things.alloc();
            let mut p = t.props.clone();
            p[P_MASS] /= n as f32;
            p[P_FRAGILE] = 0.0;
            p[P_HEALTH] = 0.0;
            let mut piece = super::things::Thing::new(nid, &ty, t.pos + out * 0.04, t.yaw, t.scale, t.params, p, self.t);
            piece.set_tilt(t.tilt());
            piece.shape.cuts = cuts;
            // What smashed it took its speed: the pieces only scatter.
            piece.vel = (t.vel * 0.4).clamp_length_max(4.0) + out * (0.8 + self.rand() * 0.8) + Vec3::Y * (0.6 + self.rand() * 0.6);
            piece.asleep = false;
            piece.anchored = false;
            piece.origin.made_from = vec![ty.name().to_string()];
            // Whoever threw it threw its pieces (a smashed lamp's fire is theirs).
            piece.thrown_by = t.thrown_by.or(t.holder.map(|h| (h, self.t)));
            self.things.insert(piece);
        }
        let name = ty.name().to_string();
        self.event("broke", None, Some(format!("thing:{id}")), format!("the {name} broke into {n} pieces"), Some(t.pos), json!({ "pieces": n }));
        self.note_near(t.pos, 30.0, Note::Info(format!("The {name} breaks!")));
        self.witness(t.pos, 25.0, &format!("I saw the {name} break."), 0.4, &[]);
    }

    /// Let go of a thing with a velocity (0 = just drop it).
    pub fn drop_thing(&mut self, id: ThingId, vel: Vec3) {
        self.release(id);
        if let Some(t) = self.things.get_mut(id) {
            t.vel = vel;
            t.asleep = false;
            t.rest_t = 0.0;
            t.dirty = true;
        }
        self.things.moved();
    }

    /// Walking into small loose things nudges them along.
    pub fn kick_things(&mut self, who: ActorId, step: Vec3) {
        let Some(p) = self.actor(who).map(|a| a.pos) else { return };
        if step.length() < 1e-4 {
            return;
        }
        // Not what they are walking up to (to pick it up).
        let aim = match self.actor(who).and_then(|a| a.task.clone()) {
            Some(super::actor::Task::Goto { target: Target::Point(q), .. }) => Some(Vec3::from_array(q)),
            _ => None,
        };
        let strength = self.strength(who);
        for id in self.things.near(p, 0.9) {
            let Some(t) = self.things.get(id) else { continue };
            if t.anchored || t.held() || t.mass() > strength * 0.4 {
                continue;
            }
            let Some(ty) = self.snap.type_of(t.type_id).cloned() else { continue };
            let (c, r) = t.proxy(&ty);
            let d = Vec3::new(c.x - p.x, 0.0, c.z - p.z);
            if d.length() > r + 0.35 || c.y > p.y + 1.0 {
                continue;
            }
            if aim.is_some_and(|q| Vec3::new(q.x - t.pos.x, 0.0, q.z - t.pos.z).length() < 1.2) {
                continue;
            }
            let dir = (d.normalize_or_zero() + step.normalize_or_zero()).normalize_or_zero();
            if let Some(t) = self.things.get_mut(id) {
                t.vel = dir * (2.5 + step.length() * 30.0).min(6.0) + Vec3::Y * 1.2;
                t.asleep = false;
                t.rest_t = 0.0;
                t.thrown_by = Some((who, self.t));
                t.dirty = true;
            }
        }
    }

    /// Whether `a` can reach point `p`.
    pub fn in_reach(&self, a: ActorId, p: Vec3) -> bool {
        self.actor(a).is_some_and(|x| {
            let d = p - x.pos;
            Vec3::new(d.x, 0.0, d.z).length() <= x.dims.reach && d.y > -1.5 && d.y < x.dims.height * 1.5
        })
    }
}

pub fn cap(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

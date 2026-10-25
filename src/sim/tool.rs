//! Tools in the hand: a held thing whose type says how it is held
//! (`meta.tool`: grip, tip, motions) is worked with a motion, not just
//! touched to things. A sword swings, an axe chops, a spear thrusts, a
//! shovel digs, a bucket pours. The arm and the tool move through the
//! motion's key poses; at its moment of impact the tip meets what was aimed
//! at, at the tip's own speed, and the world's physics and rules decide what
//! that does: a fragile thing breaks, a being is hurt and reacts, a tree
//! takes a notch and in time falls, the ground gives a pit. A launcher
//! (`meta.tool.launch`: a bow, a sling, a gun) shoots: it is raised and
//! made ready, and at the end of it what it holds leaves its tip (see
//! `missile`).

use super::actions::{ActErr, Outcome, fail};
use super::actor::{LEAN, R_FWD, R_RAISE};
use super::props::*;
use super::things::ThingId;
use super::{ActorId, Note, Sim, Target};
use crate::lang::ir::Tool;
use glam::{Quat, Vec3};
use serde_json::json;

/// One key pose of a motion: arm (raise, reach), lean, and the tool's
/// direction (pitch up from level, sweep to the right, roll about itself).
#[derive(Clone, Copy, Debug)]
pub struct Frame {
    pub raise: f32,
    pub reach: f32,
    pub lean: f32,
    pub pitch: f32,
    pub sweep: f32,
    pub roll: f32,
}

const fn f(raise: f32, reach: f32, lean: f32, pitch: f32, sweep: f32, roll: f32) -> Frame {
    Frame { raise, reach, lean, pitch, sweep, roll }
}

/// How a tool is held at rest: arm forward, tip up and a little out.
pub const REST: Frame = f(0.1, 0.55, 0.0, 0.9, 0.12, 0.0);

/// A motion: how long it takes, the moment it strikes (0..1), its key poses.
pub struct MotionDef {
    pub name: &'static str,
    pub dur: f32,
    pub impact: f32,
    pub keys: &'static [(f32, Frame)],
}

pub const MOTION_DEFS: [MotionDef; 6] = [
    MotionDef {
        name: "swing",
        dur: 0.7,
        impact: 0.45,
        keys: &[(0.0, REST), (0.28, f(0.55, 0.35, -0.05, 0.25, 1.35, 0.0)), (0.45, f(0.35, 0.95, 0.12, 0.05, 0.0, 0.0)), (0.62, f(0.25, 0.7, 0.08, 0.0, -1.25, 0.0)), (1.0, REST)],
    },
    MotionDef {
        name: "chop",
        dur: 0.8,
        impact: 0.5,
        keys: &[(0.0, REST), (0.32, f(1.0, 0.35, -0.1, 2.0, 0.1, 0.0)), (0.5, f(0.35, 0.95, 0.3, -0.35, 0.0, 0.0)), (0.65, f(0.2, 0.85, 0.3, -0.7, 0.0, 0.0)), (1.0, REST)],
    },
    MotionDef {
        name: "thrust",
        dur: 0.55,
        impact: 0.42,
        keys: &[(0.0, REST), (0.3, f(0.2, 0.15, -0.05, 0.05, 0.05, 0.0)), (0.42, f(0.25, 1.0, 0.2, 0.0, 0.0, 0.0)), (0.6, f(0.25, 0.95, 0.15, 0.0, 0.0, 0.0)), (1.0, REST)],
    },
    MotionDef {
        name: "dig",
        dur: 1.1,
        impact: 0.45,
        keys: &[(0.0, REST), (0.25, f(0.45, 0.6, 0.15, -0.5, 0.0, 0.0)), (0.45, f(0.2, 0.85, 0.45, -1.3, 0.0, 0.0)), (0.7, f(0.55, 0.55, 0.25, -0.35, 0.3, 0.0)), (1.0, REST)],
    },
    MotionDef {
        name: "pour",
        dur: 1.4,
        impact: 0.55,
        keys: &[(0.0, REST), (0.3, f(0.3, 0.85, 0.05, 0.2, 0.0, 0.0)), (0.6, f(0.3, 0.9, 0.1, 0.1, 0.0, 1.9)), (0.8, f(0.3, 0.85, 0.05, 0.2, 0.0, 0.3)), (1.0, REST)],
    },
    // Raised level before the eye, held while it is made ready, loosed at
    // the end (its length is the launcher's own `ready`).
    MotionDef {
        name: "shoot",
        dur: 0.8,
        impact: 0.86,
        keys: &[(0.0, REST), (0.3, f(0.28, 0.98, 0.02, 0.0, 0.0, 0.0)), (0.86, f(0.3, 1.0, 0.0, 0.0, 0.0, 0.0)), (0.92, f(0.34, 0.92, -0.04, 0.08, 0.0, 0.0)), (1.0, f(0.3, 0.9, 0.0, 0.02, 0.0, 0.0))],
    },
];

pub fn motion_def(name: &str) -> Option<&'static MotionDef> {
    MOTION_DEFS.iter().find(|m| m.name == name)
}

/// A motion under way.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionRun {
    pub name: &'static str,
    pub t0: f64,
    pub tool: ThingId,
    pub target: Option<Target>,
    pub at: Option<Vec3>,
    pub struck: bool,
    /// The tool's tip at the last step (world), for its speed.
    pub tip: Option<Vec3>,
    pub speed: f32,
    /// How long it takes (s): the motion's own, or a launcher's `ready`.
    pub dur: f32,
    /// How far up it is aimed (radians; a shot at something high or far).
    pub aim_pitch: f32,
}

impl MotionRun {
    pub fn progress(&self, now: f64) -> f32 {
        ((now - self.t0) as f32 / self.dur.max(0.05)).clamp(0.0, 1.0)
    }

    /// The pose now: the motion's, tipped up to its aim while aiming.
    pub fn frame(&self, now: f64) -> Frame {
        let mut fr = frame(self.name, self.progress(now));
        if self.name == "shoot" {
            let up = ((fr.reach - REST.reach) / (0.98 - REST.reach)).clamp(0.0, 1.0);
            fr.pitch += self.aim_pitch * up;
            fr.raise += self.aim_pitch.max(0.0) * 0.3 * up;
        }
        fr
    }
}

/// The pose of a motion at progress u (eased between its keys).
pub fn frame(name: &str, u: f32) -> Frame {
    let Some(m) = motion_def(name) else { return REST };
    let keys = m.keys;
    let j = keys.iter().position(|k| k.0 >= u).unwrap_or(keys.len() - 1).max(1);
    let (ua, a) = keys[j - 1];
    let (ub, b) = keys[j];
    let w = if ub > ua { ((u - ua) / (ub - ua)).clamp(0.0, 1.0) } else { 1.0 };
    let w = w * w * (3.0 - 2.0 * w);
    let l = |x: f32, y: f32| x + (y - x) * w;
    Frame { raise: l(a.raise, b.raise), reach: l(a.reach, b.reach), lean: l(a.lean, b.lean), pitch: l(a.pitch, b.pitch), sweep: l(a.sweep, b.sweep), roll: l(a.roll, b.roll) }
}

/// The turn that points a tool's grip→tip along a frame's direction (in its
/// holder's frame: +z ahead, +x right, +y up).
pub fn tool_turn(tool: &Tool, fr: &Frame) -> Quat {
    let a = (Vec3::from_array(tool.tip) - Vec3::from_array(tool.grip)).try_normalize().unwrap_or(Vec3::Y);
    let d = Vec3::new(fr.sweep.sin() * fr.pitch.cos(), fr.pitch.sin(), fr.sweep.cos() * fr.pitch.cos()).normalize();
    Quat::from_axis_angle(d, fr.roll) * Quat::from_rotation_arc(a, d)
}

/// The motion a tool makes against a target: digging into ground, chopping
/// what grows, swinging at beings; a launcher shoots at what is beyond its
/// reach as a blade (or a bayonet) would strike, or has nothing else.
fn choose(tool: &Tool, target: Option<&Target>, beyond_reach: bool) -> &'static str {
    let has = |m: &str| tool.motions.iter().any(|x| x == m);
    if has("shoot") && (beyond_reach || target.is_none() || tool.motions.len() == 1 || matches!(target, Some(Target::Point(_)) if !has("dig"))) {
        return "shoot";
    }
    let order: &[&str] = match target {
        Some(Target::Point(_)) => &["dig", "chop", "pour", "swing", "thrust"],
        Some(Target::Actor(_)) => &["swing", "thrust", "chop", "dig", "pour"],
        Some(_) => &["chop", "swing", "thrust", "pour", "dig"],
        None => &["swing", "chop", "thrust", "pour", "dig"],
    };
    let name = order.iter().find(|m| has(m)).copied().unwrap_or(if has("shoot") { "shoot" } else { "swing" });
    // Digging at something that is not the ground is a swing of the spade.
    if name == "dig" && !matches!(target, Some(Target::Point(_)) | None) { "swing" } else { name }
}

impl Sim {
    /// The tool an actor holds, if its type says how to work it.
    pub fn held_tool(&self, who: ActorId) -> Option<(ThingId, Tool)> {
        let id = self.actor(who)?.held?;
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?;
        Some((id, ty.ct.meta.tool.clone()?))
    }

    /// Work the held tool at a target (None: at the air in front). Returns
    /// None when the held thing is no tool.
    pub fn strike(&mut self, who: ActorId, on: Option<Target>, at: Option<Vec3>) -> Result<Option<Outcome>, ActErr> {
        let Some((tool_id, tool)) = self.held_tool(who) else { return Ok(None) };
        if self.actor(who).is_some_and(|a| a.motion.as_ref().is_some_and(|m| !m.struck)) {
            return Err(ActErr::Fail("already mid-swing".into()));
        }
        let me = self.actor(who).cloned().ok_or(ActErr::Fail("nobody".into()))?;
        let name = self.actor_name(who);
        let tname = self.thing_name(tool_id);
        let scale = self.things.get(tool_id).map(|t| t.scale).unwrap_or(1.0);
        let len = (Vec3::from_array(tool.tip) - Vec3::from_array(tool.grip)).length() * scale;
        let reach = self.reach_of(who) + len * 0.8;
        // Where it lands, and whether that is within reach.
        let mut beyond = false;
        let (target, spot, tgt_name) = match &on {
            Some(t) => {
                let r = self.resolve(t, who).ok_or_else(|| ActErr::Fail("there is nothing like that here".into()))?;
                // What a launcher shoots, offered to it: it is loaded.
                if let (Some(l), false) = (&tool.launch, matches!(r.target, Target::Actor(_) | Target::Point(_))) {
                    if let Some(id) = self.liven_peek(&r.target).filter(|id| self.is_ammo_for(l, *id)) {
                        return self.load(who, tool_id, id).map(Some);
                    }
                }
                let p = match (&r.target, at) {
                    (Target::Point(_), _) => r.pos,
                    (_, Some(a)) => a,
                    _ => r.nearest(me.pos + Vec3::Y * me.dims.height * 0.6),
                };
                let flat = Vec3::new(p.x - me.pos.x, 0.0, p.z - me.pos.z).length();
                beyond = flat > reach || p.y > me.pos.y + me.dims.height + len || p.y < me.pos.y - len - 0.5;
                if beyond && tool.launch.is_none() {
                    return Err(ActErr::TooFar { at: r.pos, dist: flat });
                }
                (Some(r.target.clone()), Some(p), Some(r.name.clone()))
            }
            None => (None, None, None),
        };
        let motion = choose(&tool, target.as_ref(), beyond);
        let mut dur = motion_def(motion).map(|m| m.dur).unwrap_or(1.0);
        let mut aim_pitch = 0.0;
        if motion == "shoot" {
            let l = tool.launch.clone().ok_or(ActErr::Fail("it shoots nothing".into()))?;
            if let Some(p) = spot {
                let range = (l.speed * l.speed / self.gravity()).min(600.0);
                let flat = Vec3::new(p.x - me.pos.x, 0.0, p.z - me.pos.z).length();
                if flat > range {
                    return fail(format!("{} is out of range of the {tname}", tgt_name.as_deref().map(super::actions::the).unwrap_or_else(|| "that".into())));
                }
                let from = me.pos + Vec3::Y * me.eye_height();
                let d = super::missile::aim(from, p, l.speed, self.gravity());
                aim_pitch = d.y.clamp(-1.0, 1.0).asin();
            } else if who == ActorId::Player {
                aim_pitch = self.look_pitch;
            }
            if self.shots_left(tool_id) == Some(0) {
                return fail(format!("the {tname} has nothing left to shoot: load it with {} (point at them with it in hand)", super::missile::plural(&l.ammo)));
            }
            dur = l.ready.max(0.3);
        }
        if let (Some(p), Some(a)) = (spot, self.actor_mut(who)) {
            a.face(p - a.pos, 1.0);
        }
        let now = self.t;
        if let Some(a) = self.actor_mut(who) {
            a.motion = Some(MotionRun { name: motion, t0: now, tool: tool_id, target: target.clone(), at: spot, struck: false, tip: None, speed: 0.0, dur, aim_pitch });
            a.gesture = None;
        }
        let verb = match motion {
            "shoot" => "aims",
            "chop" => "chops",
            "thrust" => "thrusts",
            "dig" => "digs",
            "pour" => "pours",
            _ => "swings",
        };
        let msg = match (&tgt_name, motion) {
            (Some(n), "dig") if matches!(target, Some(Target::Point(_))) => {
                let _ = n;
                format!("{name} digs with the {tname}")
            }
            (Some(n), "chop") => format!("{name} chops at {} with the {tname}", super::actions::the(n)),
            (Some(n), "pour") => format!("{name} pours the {tname} over {}", super::actions::the(n)),
            (Some(n), "shoot") if !matches!(target, Some(Target::Point(_))) => format!("{name} aims the {tname} at {}", super::actions::the(n)),
            (_, "shoot") => format!("{name} raises the {tname}"),
            (Some(n), _) => format!("{name} {verb} the {tname} at {}", super::actions::the(n)),
            (None, _) => format!("{name} {verb} the {tname}"),
        };
        self.event(motion, Some(who), Some(format!("thing:{tool_id}")), msg.clone(), Some(me.pos), json!({ "on": target }));
        Ok(Some(Outcome::ok(msg).thing(tool_id)))
    }

    /// Motions under way go on; at their moment of impact they strike.
    pub fn step_motions(&mut self, dt: f32) {
        let now = self.t;
        for who in self.actor_ids() {
            let Some(run) = self.actor(who).and_then(|a| a.motion.clone()) else { continue };
            // The tool left the hand: the motion stops.
            if self.actor(who).and_then(|a| a.held) != Some(run.tool) {
                if let Some(a) = self.actor_mut(who) {
                    a.motion = None;
                }
                continue;
            }
            let u = run.progress(now);
            let tip = self.tool_tip(run.tool);
            let speed = match (run.tip, tip) {
                (Some(a), Some(b)) if dt > 1e-4 => (b - a).length() / dt,
                _ => run.speed,
            };
            let impact = motion_def(run.name).map(|m| m.impact).unwrap_or(0.5);
            let strike_now = !run.struck && u >= impact;
            // A launcher looses at the end of making ready.
            if strike_now && run.name == "shoot" {
                if let Some(m) = self.actor_mut(who).and_then(|a| a.motion.as_mut()) {
                    m.struck = true;
                }
                self.loose(who, &run);
                continue;
            }
            if let Some(a) = self.actor_mut(who) {
                if let Some(m) = a.motion.as_mut() {
                    m.tip = tip;
                    m.speed = m.speed.max(speed);
                    m.struck |= strike_now;
                }
                if u >= 1.0 {
                    a.motion = None;
                }
            }
            if strike_now {
                // A blow is at least as fast as a brisk arm, at most a whip.
                let v = run.speed.max(speed).clamp(3.0, 22.0);
                self.land_strike(who, &run, v, tip);
            }
        }
    }

    /// Where a tool's tip is (world).
    pub(super) fn tool_tip(&self, id: ThingId) -> Option<Vec3> {
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?;
        let tool = ty.ct.meta.tool.as_ref()?;
        Some(super::render::thing_inst(t, ty, [0.0; 4]).from_local(Vec3::from_array(tool.tip)))
    }

    /// The blow lands.
    fn land_strike(&mut self, who: ActorId, run: &MotionRun, speed: f32, tip: Option<Vec3>) {
        let name = self.actor_name(who);
        let tname = self.thing_name(run.tool);
        let (tool_props, tool_ty) = match self.things.get(run.tool) {
            Some(t) => (t.props.clone(), self.snap.type_of(t.type_id).cloned()),
            None => return,
        };
        let tool_mass = tool_props[P_MASS].max(0.2);
        let me = self.actor(who).map(|a| a.pos).unwrap_or_default();
        let at = run.at.or(tip).unwrap_or(me);
        let mat = tool_ty.as_ref().map(|ty| crate::audio::call::Material::of(&tool_props, &ty.ct.meta.tags, crate::audio::call::Material::from_meta(&ty.ct.meta.sound).as_ref())).unwrap_or_default();
        // Still in reach when it lands? (The target may have moved.)
        let target = run.target.clone().filter(|t| match t {
            Target::Actor(a) => self.actor(*a).is_some_and(|x| (x.pos - me).length() < self.reach_of(who) + 2.0),
            _ => true,
        });
        match target {
            Some(Target::Point(_)) if run.name == "dig" => self.dig(who, at, run.tool),
            Some(Target::Point(_)) | None => {
                self.cue(at, Some(who), crate::audio::Heard::Hit { mat: crate::audio::call::Material::SOIL, mass: tool_mass, speed: speed * 0.4, by: Some((mat, tool_mass)) });
            }
            Some(Target::Actor(o)) => self.strike_being(who, o, run, speed, tool_mass),
            Some(t) => {
                let Some(id) = self.liven(&t) else { return };
                if id == run.tool {
                    return;
                }
                let oname = self.thing_name(id);
                if run.name == "pour" {
                    // What it holds goes over the other: the world's rules say what that does.
                    self.work_on(run.tool, id);
                    self.event("poured", Some(who), Some(format!("thing:{id}")), format!("{name} poured the {tname} over the {oname}"), Some(at), json!({}));
                    return;
                }
                self.impact(id, speed * (tool_mass / (tool_mass + 1.0)).sqrt().max(0.4), Some(tname.clone()));
                self.work_on(run.tool, id);
                // A loose thing is knocked away.
                let dir = (at - me).normalize_or_zero();
                if let Some(t) = self.things.get_mut(id) {
                    if !t.anchored && !t.held() {
                        let m = t.mass().max(0.1);
                        t.vel += (dir + Vec3::Y * 0.3) * speed * (tool_mass / (tool_mass + m)) * 1.4;
                        t.asleep = false;
                        t.rest_t = 0.0;
                        t.thrown_by = Some((who, self.t));
                        t.dirty = true;
                    }
                }
                let keen = tool_ty.as_ref().and_then(|ty| ty.ct.meta.tool.as_ref()).map(|t| t.keen()).unwrap_or(0.0);
                let notch = if run.name == "chop" { 0.14 } else { 0.14 * keen };
                self.take_knock(Knock { by: who, on: id, at, from: me, deed: format!("{} the {tname} at", run.name), with: tname.clone(), bite: bite_of(run.name, keen), momentum: tool_mass * speed, hard: mat.hard, notch });
                self.event("struck", Some(who), Some(format!("thing:{id}")), format!("{name} struck the {oname} with the {tname}"), Some(at), json!({ "speed": (speed * 10.0).round() / 10.0, "motion": run.name }));
            }
        }
    }

    /// The tool and what it works on touch hard, by the world's rules
    /// (the tool bears down: `force`).
    fn work_on(&mut self, tool: ThingId, id: ThingId) {
        let before = self.things.get(tool).map(|t| t.props[P_FORCE]).unwrap_or(0.0);
        if let Some(t) = self.things.get_mut(tool) {
            t.props[P_FORCE] = 1.0;
        }
        self.contact(tool, id);
        if let Some(t) = self.things.get_mut(tool) {
            t.props[P_FORCE] = before;
        }
    }

    /// A knock wears down what it lands on: by the blow's weight and speed
    /// and how it cuts, against the thing's size and hardness. An edge or a
    /// shot leaves a notch or a hole. Each third worn through, the world
    /// says what it does (a branch comes off, a stone chips, a wall gives
    /// way); something growing worn right through falls first.
    pub(super) fn take_knock(&mut self, k: Knock) {
        let Knock { by: who, on: id, at, from, deed, with: tname, bite, momentum, hard: tool_hard, notch } = k;
        let Some(t) = self.things.get(id) else { return };
        if t.held() || t.props[P_HEALTH] <= 0.0 {
            return;
        }
        let Some(ty) = self.snap.type_of(t.type_id).cloned() else { return };
        let hard = crate::audio::call::Material::of(&t.props, &ty.ct.meta.tags, crate::audio::call::Material::from_meta(&ty.ct.meta.sound).as_ref()).hard;
        let wear = blow_wear(bite, momentum, tool_hard, hard, t.mass());
        if wear <= 0.0 {
            return;
        }
        // It bites into what it can, less into harder stuff.
        let notch = notch * (1.1 - hard).clamp(0.0, 1.0);
        if notch > 0.03 && self.cut_fits(id, at, notch) {
            let _ = self.cut(who, id, Some(at), notch, false);
        }
        let (pos, alive, before) = match self.things.get(id) {
            Some(t) => (t.pos, t.props[P_ALIVE] > 0.0, t.props[P_HEALTH]),
            None => return,
        };
        let after = (before - wear).max(0.0);
        if let Some(t) = self.things.get_mut(id) {
            t.props[P_HEALTH] = after;
            t.dirty = true;
        }
        let Some(stage) = stage_crossed(before, after) else { return };
        let name = self.actor_name(who);
        let n = ty.name().to_string();
        let what = if stage == 3 && alive {
            // It goes over away from the one striking.
            if let Some(t) = self.things.get_mut(id) {
                let away = Vec3::new(pos.x - from.x, 0.0, pos.z - from.z).normalize_or(Vec3::Z);
                let axis = Vec3::Y.cross(away).normalize_or(Vec3::X);
                let local_axis = Quat::from_rotation_y(t.yaw).inverse() * axis;
                t.set_tilt(Quat::from_axis_angle(local_axis, 1.45) * t.tilt());
                t.props[P_ALIVE] = 0.0;
            }
            let msg = format!("{name} felled the {n}");
            self.event("felled", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(pos), json!({}));
            self.note_near(pos, 40.0, Note::Notable(format!("{}.", super::physics::cap(&msg))));
            self.witness(pos, 30.0, &msg, 0.4, &[who]);
            (format!("fell the {n}"), format!("The {n} was felled and lies where it fell: let it come apart into what a felled {n} gives here, replacing it."))
        } else {
            let how = ["", "a third", "two thirds", "right"][stage];
            let msg = format!("{name} wore the {n} {how} through with the {tname}");
            self.event("worn", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(pos), json!({ "stage": stage }));
            self.witness(pos, 30.0, &msg, 0.1 * stage as f32, &[who]);
            (format!("{deed} the {n}, wearing it {how} through"), format!("Blows have worn the {n} {how} through. Say what that does to it here: a piece comes off, it cracks, splits or gives way, or it holds."))
        };
        // What it comes to is the world's to say, as for any deed.
        let r = super::actions::Resolved::new(Target::Thing(id), pos, n.clone());
        if let Err(e) = self.interpret_noted(who, &what.0, Some(r), None, Some(&what.1)) {
            crate::log::info(format!("the {n} stays as it is: {e:?}"));
        }
    }

    /// A blow lands on a being: it hurts by the tool's weight and speed and
    /// the body's size; the struck one and those who saw it react.
    fn strike_being(&mut self, who: ActorId, o: ActorId, run: &MotionRun, speed: f32, tool_mass: f32) {
        if o == who {
            return;
        }
        let name = self.actor_name(who);
        let oname = self.actor_name(o);
        let tname = self.thing_name(run.tool);
        let Some((opos, omass)) = self.actor(o).map(|a| (a.pos, a.dims.mass.max(1.0))) else { return };
        let hurt = (0.012 * tool_mass.min(20.0).sqrt() * speed * speed / 10.0 / (omass / 70.0).sqrt()).clamp(0.02, 0.9);
        let me = self.actor(who).map(|a| a.pos).unwrap_or(opos);
        let push = Vec3::new(opos.x - me.x, 0.0, opos.z - me.z).normalize_or_zero() * (0.15 + 0.03 * speed) * (70.0 / omass).sqrt().min(2.0);
        self.take_blow(Blow {
            by: Some(who),
            to: o,
            hurt,
            push,
            how: format!("struck by {name} with the {tname}"),
            told: format!("{name} struck {oname} with the {tname}"),
            felt: format!("{name} just struck you with a {tname}. It hurt."),
            weapon: Some(tname),
            mass: tool_mass,
            speed,
            motion: run.name,
        });
    }

    /// A blow lands on a being, from a hand, something flying or a blast:
    /// what they wear softens it; it hurts, rocks them, may kill them; the
    /// struck one and those who saw it remember, fear and react. True when
    /// it killed.
    pub(super) fn take_blow(&mut self, b: Blow) -> bool {
        let o = b.to;
        if b.by == Some(o) && b.motion != "burst" {
            return false;
        }
        let Some((opos, omass)) = self.actor(o).map(|a| (a.pos, a.dims.mass.max(1.0))) else { return false };
        let hurt = b.hurt * (1.0 - self.protection(o));
        // Whoever dealt it (a blast nobody set off is the world's doing).
        let by = b.by.unwrap_or(o);
        let died = self.wound(o, hurt, by, &b.how);
        if let (ActorId::Npc(c), Some(w)) = (o, b.by.filter(|w| *w != o)) {
            self.phantom_hurt(c, w, b.weapon.clone(), died);
        }
        if let (Some(ActorId::Npc(c)), true) = (b.by, b.by != Some(o)) {
            self.phantom_struck(c, o, died);
        }
        // Knocked back a little.
        if b.push.length() > 1e-3 {
            self.walk(o, b.push);
        }
        // The body gives with the blow (a bigger blow, a bigger give), and
        // whoever took or saw it fears the one who dealt it.
        if !died {
            let dir = if b.push.length() > 1e-3 { b.push } else { Vec3::Y };
            self.jolt(o, dir, (0.1 + hurt * 0.9).min(0.4));
            if let Some(w) = b.by.filter(|w| *w != o) {
                self.hurt_by(o, w, hurt, false);
            }
        }
        self.event("struck", b.by, Some(o.key()), b.told.clone(), Some(opos), json!({ "hurt": (hurt * 100.0).round() / 100.0, "motion": b.motion }));
        let with = Some((crate::audio::call::Material::FLESH, b.mass));
        self.cue(opos + Vec3::Y, Some(o), crate::audio::Heard::Hit { mat: crate::audio::call::Material::FLESH, mass: omass.min(400.0), speed: b.speed.min(30.0), by: with });
        let mine = o == ActorId::Player || b.by == Some(ActorId::Player);
        self.note_near(opos, 30.0, Note::seen(format!("{}!", super::physics::cap(&b.told)), mine));
        self.witness(opos, 25.0, &b.told, 0.7, &b.by.into_iter().collect::<Vec<_>>());
        if let Some(w) = b.by.filter(|w| *w != o) {
            let r = self.social.rel_mut(o, w);
            r.trust = (r.trust - 0.4).max(-1.0);
            r.affection = (r.affection - 0.3).max(-1.0);
        }
        if died {
            if let ActorId::Npc(c) = o {
                self.kill(c, &format!("{}, and died", b.how), b.by.filter(|w| *w != o));
            }
            return true;
        }
        if let ActorId::Npc(c) = o {
            let speaks = self.speaks(o);
            // The dark's own don't run.
            if let Some(w) = b.by.filter(|w| *w != o) {
                let from = self.actor(w).map(|a| a.pos).unwrap_or(opos);
                if !self.of_the_dark(o) && (!speaks || self.cast.get(c).is_some_and(|n| n.traits.brave < 0.5)) {
                    self.flee(c, w, from);
                }
            }
            if self.has_llm && speaks {
                let context = self.decide_context(c, &b.felt);
                self.request_weighted(super::Request::Decide { cid: c, event: "struck".into(), context }, opos, 1.0);
            }
        }
        false
    }

    /// Dig at the ground: a pit opens there, or the pit there deepens.
    fn dig(&mut self, who: ActorId, at: Vec3, tool: ThingId) {
        let terrain = self.snap.terrain.clone();
        let natural = terrain.natural_height(at.x, at.z);
        if natural < terrain.water(at.x, at.z) + 0.1 {
            return;
        }
        let tool_mass = self.things.get(tool).map(|t| t.mass()).unwrap_or(1.0);
        let bite = (0.18 + 0.06 * tool_mass.min(4.0)).min(0.4);
        let depth;
        {
            let mut guard = terrain.carve.write();
            let c = &mut *guard;
            // A spade's own pit (not a crater someone conjured around it).
            match c.dug.iter_mut().find(|h| h.round && h.half[0] <= 1.1 && (h.c[0] - at.x).hypot(h.c[1] - at.z) < h.half[0] * 0.8) {
                Some(h) => {
                    let ground = terrain.raw_height(h.c[0], h.c[1]) + c.ground.delta(h.c[0], h.c[1]);
                    h.floor = (h.floor - bite).max(ground - 2.2);
                    h.half[0] = (h.half[0] + 0.04).min(1.1);
                    depth = ground - h.floor;
                }
                None => {
                    c.dug.push(crate::terrain::Hollow { c: [at.x, at.z], rot: [1.0, 0.0], half: [0.55, 0.55], floor: natural - bite, round: true, edge: crate::terrain::HOLLOW_EDGE });
                    crate::terrain::Carve::trim_dug(&mut c.dug);
                    depth = bite;
                }
            }
            c.reindex();
        }
        self.dug_dirty = true;
        // What lay there drops in.
        for id in self.things.near(at, 1.2) {
            if let Some(t) = self.things.get_mut(id) {
                if !t.anchored && !t.held() {
                    t.asleep = false;
                    t.rest_t = 0.0;
                }
            }
        }
        let name = self.actor_name(who);
        let msg = format!("{name} dug a hole {:.0} cm deep", depth * 100.0);
        self.event("dug", Some(who), None, msg, Some(at), json!({ "depth": (depth * 100.0).round() / 100.0 }));
        let by = Some((crate::audio::call::Material::default(), tool_mass));
        self.cue(at, Some(who), crate::audio::Heard::Hit { mat: crate::audio::call::Material::SOIL, mass: 20.0, speed: 2.5, by });
    }
}

/// A blow to a thing (see `Sim::take_knock`): a tool worked at it, or
/// something flying into it.
pub struct Knock {
    pub by: ActorId,
    pub on: ThingId,
    /// Where it lands, and where the blow came from.
    pub at: Vec3,
    pub from: Vec3,
    /// The deed as it goes on "… the cottage" ("swing the sword at").
    pub deed: String,
    /// What struck it, by name.
    pub with: String,
    /// How it cuts (see `bite_of`).
    pub bite: f32,
    /// Its weight times its speed (kg m/s).
    pub momentum: f32,
    /// How hard what struck is (0..1).
    pub hard: f32,
    /// How big a notch or hole it leaves (m), before the thing's hardness.
    pub notch: f32,
}

/// A blow to a being (see `Sim::take_blow`).
pub struct Blow {
    /// Who dealt it (None: nobody, a blast that went off by itself).
    pub by: Option<ActorId>,
    pub to: ActorId,
    /// How much of their health it takes before what they wear softens it.
    pub hurt: f32,
    /// How far it shoves them (m, world).
    pub push: Vec3,
    /// What happened to them, as it ends "they were …".
    pub how: String,
    /// The deed, told ("Oda struck Mara with the axe").
    pub told: String,
    /// What the struck one's planner hears.
    pub felt: String,
    pub weapon: Option<String>,
    /// What struck (kg) and how fast (m/s), for the sound.
    pub mass: f32,
    pub speed: f32,
    /// "swing", "shot", "thrown", "burst"…
    pub motion: &'static str,
}

/// The arm's pose during a motion, laid over a body's pose.
pub fn arm_pose(run: &MotionRun, now: f64, pose: &mut super::actor::Pose) {
    let fr = run.frame(now);
    pose[super::actor::GRIP] = 0.9;
    pose[R_RAISE] = fr.raise;
    pose[R_FWD] = fr.reach;
    pose[LEAN] = fr.lean;
}

/// How a blow cuts: a chop bites deepest, a keen swing more than a blunt
/// one, a point least; what flies strikes as a swing would.
pub fn bite_of(motion: &str, keen: f32) -> f32 {
    match motion {
        "chop" => 1.0,
        "swing" | "flight" => 0.5 + 0.4 * keen,
        "thrust" => 0.3 + 0.3 * keen,
        "dig" => 0.5,
        _ => 0.0,
    }
}

/// How much one blow wears a thing down (of its health, 0..1): its weight
/// times its speed and how it cuts, against the thing's size and how much
/// harder it is than what struck.
pub fn blow_wear(bite: f32, momentum: f32, tool_hard: f32, hard: f32, mass: f32) -> f32 {
    let give = (1.15 - hard).clamp(0.1, 1.0) * (tool_hard / hard.max(0.05)).clamp(0.2, 1.0);
    0.005 * momentum * bite * give / (mass.max(1.0) / 200.0).max(1.0).sqrt()
}

/// The deepest third of health a blow wore past (1, 2, 3: worn through).
fn stage_crossed(before: f32, after: f32) -> Option<usize> {
    let stage = |h: f32| ((1.0 - h) * 3.0 + 1e-4).floor().clamp(0.0, 3.0) as usize;
    (stage(after) > stage(before)).then(|| stage(after))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A heavy blade cuts a live tree in a few blows and stone only in
    /// many; an axe bites deeper than a club, and a club on stone barely.
    #[test]
    fn blows_wear_by_weight_edge_and_hardness() {
        let (steel, wood, live, stone) = (0.85, 0.65, 0.3, 0.85);
        let sword = bite_of("swing", 0.8);
        let birch = blow_wear(sword, 6.0 * 20.0, steel, live, 300.0);
        let boulder = blow_wear(sword, 6.0 * 20.0, steel, stone, 3000.0);
        assert!(birch > 0.25 && boulder < 0.05, "birch {birch:.3}, boulder {boulder:.3}");
        assert!(blow_wear(bite_of("chop", 0.6), 2.5 * 15.0, steel, live, 300.0) > blow_wear(bite_of("swing", 0.0), 2.5 * 15.0, wood, live, 300.0));
        assert!(blow_wear(bite_of("swing", 0.0), 2.5 * 15.0, wood, stone, 3000.0) < 0.01);
        assert_eq!(bite_of("pour", 0.0), 0.0);
        // A bullet does a little of what a sword does; a cannonball, far more.
        let (bullet, ball) = (blow_wear(bite_of("flight", 0.0), 0.02 * 700.0, steel, wood, 20000.0), blow_wear(bite_of("flight", 0.0), 5.0 * 300.0, steel, wood, 20000.0));
        let cut = blow_wear(sword, 6.0 * 20.0, steel, wood, 20000.0);
        assert!(bullet < cut * 0.2 && ball > cut * 5.0, "bullet {bullet:.4}, sword {cut:.4}, ball {ball:.4}");
        assert_eq!(stage_crossed(1.0, 0.9), None);
        assert_eq!(stage_crossed(0.9, 0.3), Some(2));
        assert_eq!(stage_crossed(0.2, 0.0), Some(3));
    }
}

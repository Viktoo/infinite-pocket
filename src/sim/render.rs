//! What the live world adds to a frame: characters (posed), live things
//! (with their state and generic looks: charred, wet, glowing, highlighted),
//! flames over everything burning, and point lights from fires and lamps.
//! Work in progress shows too: what is being changed pulses, where something
//! is being conjured glows violet until it appears in a flash.

use super::props::*;
use super::things::Thing;
use super::things::ThingId;
use super::{ActorId, Sim, Target};
use crate::render::{FX_CHAR, FX_GLOW, FX_HIGHLIGHT, FX_WET, GpuInst, PointLight};
use crate::world::TypeEntry;
use glam::Vec3;

/// Instance for a live thing. Living things that are still growing are
/// drawn smaller, with their base kept on the ground.
pub fn thing_inst(t: &Thing, ty: &TypeEntry, fx: [f32; 4]) -> GpuInst {
    let grow = if t.props[P_ALIVE] > 0.0 || t.props[P_GROWTH] < 1.0 { 0.25 + 0.75 * t.props[P_GROWTH].clamp(0.0, 1.0) } else { 1.0 };
    let scale = (t.scale * grow).max(0.01);
    let y = t.pos.y + ty.bottom * (t.scale - scale);
    let mut k = t.params;
    k[1] = scale;
    let mut g = GpuInst {
        pos_scale: [t.pos.x, y, t.pos.z, scale],
        rot: [t.yaw.cos(), t.yaw.sin(), ty.sphere_r * scale, 1.0],
        k0: [k[0], k[1], k[2], k[3]],
        k1: [k[4], k[5], k[6], k[7]],
        fx,
        info: ty.gpu_info(),
        ..Default::default()
    };
    g.set_state(&t.state);
    for (i, v) in t.look.iter().take(20).enumerate() {
        g.look[i / 4][i % 4] = *v;
    }
    // The dead's eyes are shut.
    if t.origin.remains.is_some() {
        g.set_face(0.0, 1.0, 0.0);
    }
    g.cuts = t.gpu_cuts();
    let q = t.hold_tilt.unwrap_or_else(|| t.tilt());
    if q != glam::Quat::IDENTITY {
        g.set_tilt(q);
    }
    super::joint::pose(&mut g, t, ty);
    g
}

/// A live thing's shape as the CPU sees it, every cut included.
pub fn thing_solid(t: &Thing, ty: std::sync::Arc<TypeEntry>) -> crate::world::Solid {
    let mut s = crate::world::Solid::new(thing_inst(t, &ty, [0.0; 4]), ty);
    s.more = t.more_cuts().into();
    s.bores = t.shape.bores.as_slice().into();
    s
}

/// How shut someone's eyes are at `t` (0 open … 1 shut): a blink every few
/// seconds, each their own rhythm, now and then two together.
pub fn blink(t: f64, id: i64) -> f32 {
    let h = |i: u32| crate::noise::u2f(crate::noise::pcg(id as u32 ^ i.wrapping_mul(0x9E37_79B9)));
    let period = 2.5 + 3.5 * h(1) as f64;
    let n = ((t + h(2) as f64 * 10.0) / period).floor();
    let into = (t + h(2) as f64 * 10.0) - n * period;
    // Each blink comes a little early or late, so they don't tick.
    let jit = (crate::noise::u2f(crate::noise::pcg(id as u32 ^ n as i64 as u32)) as f64 - 0.5) * period * 0.5;
    let twice = crate::noise::u2f(crate::noise::pcg(n as i64 as u32 ^ 0xB11 ^ id as u32)) < 0.15;
    let at = into - (period * 0.5 + jit);
    let one = |s: f64| if (0.0..0.17).contains(&s) { (1.0 - ((s - 0.06) / if s < 0.06 { 0.06 } else { 0.11 }).abs()) as f32 } else { 0.0 };
    one(at).max(if twice { one(at - 0.3) } else { 0.0 })
}

/// A worn layer: the wearer's instance (place, size, look sliders, pose)
/// drawing the layer's shape instead.
pub fn layer_inst(wearer: &GpuInst, body: &TypeEntry, layer: &TypeEntry, fx: [f32; 4]) -> GpuInst {
    let factor = wearer.rot[2] / body.sphere_r.max(1e-3);
    let mut g = *wearer;
    g.info = layer.gpu_info();
    // It lies on the body: centred where the body is, at least as big.
    g.info[1] = wearer.info[1];
    g.rot[2] = (layer.sphere_r * factor).max(wearer.rot[2] * 1.1);
    g.fx = fx;
    g.fx[FX_HIGHLIGHT] = wearer.fx[FX_HIGHLIGHT];
    g.cuts = Default::default();
    g.more = Default::default();
    g
}

impl Sim {
    /// A layer as worn on `g` (a body of type `body`): drawn in its frame
    /// and pose, and, when it was made for a body this one was written from
    /// (a person's helmet on a phantom), moved with the part it sits on to
    /// where this body has that part (see `layer_shift`).
    pub fn worn_inst(&self, g: &GpuInst, body: &TypeEntry, layer: &TypeEntry, fx: [f32; 4]) -> GpuInst {
        let mut l = layer_inst(g, body, layer, fx);
        if let Some(d) = self.layer_shift(g, body, layer) {
            l.pos_scale[0] += d.x;
            l.pos_scale[1] += d.y;
            l.pos_scale[2] += d.z;
        }
        l
    }

    /// How far a layer made for another body moves on this one: the part
    /// it sits on (the nearest to its middle: a helmet the head, a
    /// breastplate the chest) is where each body's `parts()` says, in the
    /// same pose; the layer goes from that part on the body it was made
    /// for to that part on this one. Nothing when either body doesn't say.
    pub fn layer_shift(&self, g: &GpuInst, body: &TypeEntry, layer: &TypeEntry) -> Option<Vec3> {
        let fits = layer.ct.meta.fits.as_deref()?;
        if fits == body.name() || body.ct.parts.is_none() {
            return None;
        }
        let base = self.snap.body_named(fits)?;
        base.ct.parts.as_ref()?;
        let k = g.k();
        let key = (layer.id, base.id);
        let cached = self.layer_parts.lock().ok().and_then(|m| m.get(&key).copied());
        let part = match cached {
            Some(p) => p,
            None => {
                let p = layer_part(layer, base, &k);
                if let Ok(mut m) = self.layer_parts.lock() {
                    m.insert(key, p);
                }
                p
            }
        }?;
        let (a, b) = (body.ct.part(part, &k)?, base.ct.part(part, &k)?);
        Some(g.from_local(a) - g.from_local(b))
    }
}

/// Which part of `base` a layer made for it sits on: the one nearest the
/// middle of what the layer covers (probed through its bounds).
fn layer_part(layer: &TypeEntry, base: &TypeEntry, k: &[f32; crate::lang::vm::K_LEN]) -> Option<usize> {
    const N: usize = 20;
    let b = layer.ct.meta.bounds;
    // All of its bounds: where it is depends on the wearer's size and pose.
    let (y0, y1) = (-b[1] * 0.1, b[1]);
    // Near its surface: a layer is often a thin shell.
    let near = 2.0 * b[0].max(b[2]).max((y1 - y0) * 0.5) / N as f32 * 0.6;
    let mut sum = Vec3::ZERO;
    let mut n = 0;
    for i in 0..N {
        for j in 0..N {
            for l in 0..N {
                let f = |q: usize| (q as f32 + 0.5) / N as f32;
                let p = Vec3::new(-b[0] + 2.0 * b[0] * f(i), y0 + (y1 - y0) * f(j), -b[2] + 2.0 * b[2] * f(l));
                if layer.ct.sdf(p.to_array(), k) < near {
                    sum += p;
                    n += 1;
                }
            }
        }
    }
    if n == 0 {
        return None;
    }
    let mid = sum / n as f32;
    (0..crate::lang::ir::PARTS.len()).filter_map(|i| base.ct.part(i, k).map(|p| (i, (p - mid).length()))).min_by(|a, b| a.1.total_cmp(&b.1)).map(|(i, _)| i)
}

/// `FX_CHAR` for the dark's own: blackened, and what glows on it glows red.
pub const DARK_OWN: f32 = 2.0;

/// Generic looks from properties.
pub fn look(p: &Props) -> [f32; 4] {
    let mut fx = [0.0; 4];
    fx[FX_CHAR] = p[P_CHAR].clamp(0.0, 1.0);
    fx[FX_WET] = (p[P_WET] * 0.6).clamp(0.0, 1.0);
    fx[FX_GLOW] = (p[P_LIGHT] * 0.9 + p[P_FIRE] * 0.4 + ((p[P_TEMP] - 400.0) / 800.0).max(0.0)).min(2.0);
    fx
}

/// Largest thing (bounding radius, m) that glows all over when its type
/// doesn't mark lit parts with glow(); a building would glare as one block.
const WHOLE_GLOW_MAX_R: f32 = 2.0;

/// `look` for a thing of this type: unmarked big things keep only their light.
fn thing_look(p: &Props, ty: &TypeEntry, scale: f32) -> [f32; 4] {
    let mut fx = look(p);
    if !ty.ct.marks_glow() && ty.radius() * scale > WHOLE_GLOW_MAX_R {
        fx[FX_GLOW] = 0.0;
    }
    fx
}

/// How long the flash lasts where something new appears (seconds).
pub const FLASH_FOR: f64 = 1.4;
const CONJURE_LIGHT: Vec3 = Vec3::new(0.95, 0.65, 1.0);

/// How bright something being changed is lit now: a slow, gentle breath.
fn pulse(t: f64) -> f32 {
    0.15 + 0.2 * (0.5 + 0.5 * (t * 3.0).sin() as f32)
}

/// What work in progress marks this frame.
#[derive(Default)]
struct Marks {
    things: Vec<ThingId>,
    actors: Vec<ActorId>,
    /// Where something is being conjured: (spot, the traveler's own).
    conjures: Vec<(Vec3, bool)>,
}

pub struct Drawn {
    pub insts: Vec<GpuInst>,
    /// Cuts past each instance's own few (see `GpuInst::more`).
    pub holes: Vec<[f32; 4]>,
    pub lights: Vec<PointLight>,
}

impl Sim {
    /// Everything live to draw this frame, near `cam`.
    pub fn draw(&self, cam: Vec3, view: f32) -> Drawn {
        self.draw_from(cam, view, None)
    }

    /// As `draw`, seen through the player's eyes: their own arms at work
    /// come into view (see `hands`), and what they hold is in their hand.
    pub fn draw_first_person(&self, cam: &crate::render::Camera, view: f32) -> Drawn {
        self.draw_from(cam.pos, view, Some(cam))
    }

    /// What work in progress marks (the placed objects' pulse is kept in
    /// the overlay, for the static world's own drawing).
    fn work_marks(&self) -> Marks {
        let mut m = Marks::default();
        // A guest's copy: the host says what is being conjured.
        if self.mirror.on {
            m.conjures = self.mirror.conjuring.clone();
            return m;
        }
        for w in self.work() {
            if w.appears {
                if let Some(at) = w.at {
                    m.conjures.push((at, w.who == Some(ActorId::Player)));
                }
            }
            match w.on {
                Some(Target::Thing(id)) => m.things.push(id),
                Some(Target::Instance(i)) => m.things.extend(self.things.by_instance.get(&i).copied()),
                Some(Target::Actor(a)) => m.actors.push(a),
                _ => {}
            }
        }
        m
    }

    /// Placed objects being worked on pulse (once a frame).
    pub(super) fn mark_work(&mut self) {
        let k = pulse(self.t);
        let ids: Vec<i64> = self.work().into_iter().filter_map(|w| match w.on {
            Some(Target::Instance(i)) => Some(i),
            _ => None,
        }).collect();
        let pulse = &mut self.cache.overlay.pulse;
        pulse.clear();
        pulse.extend(ids.into_iter().map(|i| (i, k)));
    }

    fn draw_from(&self, cam: Vec3, view: f32, eye: Option<&crate::render::Camera>) -> Drawn {
        let mut insts = Vec::new();
        let mut holes: Vec<[f32; 4]> = Vec::new();
        let mut lights: Vec<(f32, PointLight)> = Vec::new();
        let marks = self.work_marks();
        let breath = pulse(self.t);
        let near = |p: Vec3| (p - cam).length() < view;
        let figure = self.snap.figure_type.and_then(|f| self.snap.type_of(f));
        let mut worn: std::collections::HashMap<ActorId, Vec<&Thing>> = std::collections::HashMap::new();
        for t in self.things.live() {
            if let Some(w) = t.worn {
                worn.entry(w).or_default().push(t);
            }
        }
        for n in &self.cast.npcs {
            let Some(body) = self.snap.type_of(n.body_ty).or(figure) else { continue };
            if !n.here() {
                continue;
            }
            {
                if (n.a.pos - cam).length() < 200.0 {
                    let mut g = n.gpu(body);
                    let id = n.def.id;
                    let shut = if n.a.asleep { 1.0 } else { blink(self.t, id) };
                    let (mouth, shut) = self.face_hold.unwrap_or((self.mouths.get(&id).copied().unwrap_or(0.0), shut));
                    g.set_face(mouth, shut, (self.t % 3600.0) as f32 + (id % 97) as f32);
                    if let Some(q) = n.a.sway(self.t, n.hurt()) {
                        g.set_tilt(q);
                    }
                    // A touch's glow shows as light. The dark's own beings
                    // (whose touch harms, but who don't think) are black whatever colours their body has:
                    // only their eyes show (the parts marked glow()), always, and red
                    // (charred past 1 is the renderer's sign for that).
                    let of_the_dark = n.species.touch.harms() && !n.species.hostile;
                    g.fx[FX_GLOW] = n.glow();
                    g.fx[FX_WET] = n.props.get(super::props::P_WET).copied().unwrap_or(0.0).min(1.0) * 0.6;
                    if of_the_dark {
                        g.fx[FX_CHAR] = DARK_OWN;
                        g.fx[FX_GLOW] = if body.ct.marks_glow() { 1.6 } else { 0.0 };
                    } else if body.ct.marks_glow() {
                        // A body that marks lit parts (a phantom's eyes) always shows them.
                        g.fx[FX_GLOW] = g.fx[FX_GLOW].max(1.6);
                    }
                    if marks.actors.contains(&ActorId::Npc(n.def.id)) {
                        g.fx[FX_HIGHLIGHT] = g.fx[FX_HIGHLIGHT].max(breath);
                    }
                    insts.push(g);
                    if of_the_dark {
                        let p = n.a.pos + Vec3::Y * n.a.dims.eye;
                        lights.push(((p - cam).length(), PointLight { pos: p, color: Vec3::new(1.0, 0.08, 0.04), intensity: 0.12, reach: 1.6 }));
                    } else if body.ct.marks_glow() && n.glow() <= 0.05 {
                        let p = n.a.pos + Vec3::Y * n.a.dims.eye;
                        lights.push(((p - cam).length(), PointLight { pos: p, color: Vec3::new(0.85, 0.92, 1.0), intensity: 0.1, reach: 1.4 }));
                    } else if n.glow() > 0.05 {
                        let p = n.a.pos + Vec3::Y * n.a.dims.height * 0.6;
                        let gl = n.glow().min(1.0);
                        lights.push(((p - cam).length(), PointLight { pos: p, color: Vec3::new(0.95, 0.9, 0.5), intensity: 0.5 * gl, reach: 3.0 + 4.0 * gl }));
                    }
                    // What they wear, drawn in their frame and pose.
                    for t in worn.get(&ActorId::Npc(n.def.id)).map(|v| v.as_slice()).unwrap_or(&[]) {
                        let Some(ty) = self.snap.type_of(t.type_id) else { continue };
                        insts.push(self.worn_inst(&g, body, ty, look(&t.props)));
                    }
                }
            }
        }
        // The other travelers, in what they wear.
        for (id, g) in self.draw_travelers(cam, view) {
            for t in worn.get(&id).map(|v| v.as_slice()).unwrap_or(&[]) {
                let (Some(body), Some(ty)) = (self.own_body_type(), self.snap.type_of(t.type_id)) else { continue };
                insts.push(self.worn_inst(&g, body, ty, look(&t.props)));
            }
            insts.push(g);
        }
        // The traveler's own body, seen from inside, in what they wear.
        let own = eye.and_then(|c| self.own_view(c));
        if let Some(v) = &own {
            insts.push(v.inst);
            for t in worn.get(&ActorId::Player).map(|v| v.as_slice()).unwrap_or(&[]) {
                let Some(ty) = self.snap.type_of(t.type_id) else { continue };
                let mut l = self.worn_inst(&v.inst, &v.body, ty, look(&t.props));
                l.cuts = v.inst.cuts;
                insts.push(l);
            }
        }
        // A glow on the traveler lights the ground about them.
        let pg = self.night.glow;
        if pg > 0.05 {
            let p = self.player.pos + Vec3::Y * 1.2;
            lights.push(((p - cam).length(), PointLight { pos: p, color: Vec3::new(0.95, 0.9, 0.5), intensity: 0.4 * pg, reach: 3.0 + 4.0 * pg }));
        }
        for t in self.things.live() {
            if !near(t.pos) || t.worn.is_some() {
                continue;
            }
            // Held by someone away (a phantom by day): gone with them.
            if let Some(ActorId::Npc(c)) = t.holder {
                if self.cast.get(c).is_some_and(|n| n.away) {
                    continue;
                }
            }
            let Some(ty) = self.snap.type_of(t.type_id) else { continue };
            let mut fx = thing_look(&t.props, ty, t.scale);
            if marks.things.contains(&t.id) {
                fx[FX_HIGHLIGHT] = fx[FX_HIGHLIGHT].max(breath);
            }
            let tool = ty.ct.meta.tool.as_ref();
            match eye.filter(|_| t.holder == Some(ActorId::Player) && t.co_holder.is_none()) {
                Some(c) if tool.is_some() => {
                    // A tool in the view: held by its grip low and right,
                    // turned through its motion as the arm would.
                    let tool = tool.expect("tool");
                    let (f, r, u) = c.basis();
                    let len = ((Vec3::from_array(tool.tip) - Vec3::from_array(tool.grip)).length() * t.scale).max(0.05);
                    let k = (0.55 / len).min(1.0);
                    let fr = match self.player.motion.as_ref() {
                        Some(m) if m.tool == t.id => m.frame(self.t),
                        _ => super::tool::REST,
                    };
                    let mut shown = t.clone();
                    shown.scale = t.scale * k;
                    shown.yaw = c.yaw;
                    // Pitch with the view, so it stays in sight looking up or down.
                    let q = glam::Quat::from_rotation_x(-c.pitch) * super::tool::tool_turn(tool, &fr);
                    // In the fist, else where a hand would bring it.
                    let hand = match &own {
                        Some(v) => v.hand(false) + v.forward() * 0.05,
                        None => c.pos + f * (0.32 + 0.25 * fr.reach) + r * (0.22 - 0.1 * fr.reach) - u * (0.26 - 0.18 * fr.raise),
                    };
                    shown.pos = hand - glam::Quat::from_rotation_y(c.yaw) * (q * (Vec3::from_array(tool.grip) * shown.scale));
                    shown.hold_tilt = Some(q);
                    insts.push(thing_inst(&shown, ty, fx));
                }
                Some(c) => {
                    // The view-model: in front, low right, scaled to fit.
                    let (f, r, u) = c.basis();
                    let size = (ty.radius() * t.scale).max(0.05);
                    let big = own.is_some() && self.held_size(ActorId::Player).unwrap_or(false);
                    // Something big lies across both hands, as wide as they
                    // are apart; something small sits in one.
                    let k = match &own {
                        Some(v) if big => (v.hand_gap() * 0.6 / size).min(1.0),
                        _ => (0.2 / size).min(1.0),
                    };
                    let mut shown = t.clone();
                    shown.scale = t.scale * k;
                    let centre = match &own {
                        Some(v) if big => v.hand(true) + v.up() * 0.03,
                        Some(v) => v.hand(false) + v.forward() * (0.07 + size * k * 0.5) + v.up() * 0.03,
                        None => c.pos + f * 0.6 + r * 0.27 - u * 0.21,
                    };
                    let (cc, _) = shown.proxy(ty);
                    shown.pos += centre - cc;
                    shown.yaw = if big { c.yaw } else { c.yaw + 0.6 };
                    insts.push(thing_inst(&shown, ty, fx));
                }
                None => {
                    let fell = self.falling(t);
                    let mut g = thing_inst(fell.as_ref().unwrap_or(t), ty, fx);
                    let (more, bores) = (t.more_cuts(), &t.shape.bores);
                    if !more.is_empty() || !bores.is_empty() {
                        g.more = [holes.len() as u32, more.len() as u32, (holes.len() + more.len()) as u32, bores.len() as u32];
                        holes.extend_from_slice(more);
                        holes.extend(bores.iter().flat_map(|b| [[b[0], b[1], b[2], b[3]], [b[4], b[5], b[6], b[7]]]));
                    }
                    insts.push(g);
                    // Remains wear what the living body wore.
                    if let Some(c) = t.origin.remains {
                        for w in worn.get(&ActorId::Npc(c)).map(|v| v.as_slice()).unwrap_or(&[]) {
                            let Some(lty) = self.snap.type_of(w.type_id) else { continue };
                            insts.push(self.worn_inst(&g, ty, lty, look(&w.props)));
                        }
                    }
                }
            }
            if t.props[P_LIGHT] > 0.05 {
                let p = t.pos + Vec3::Y * (ty.sphere_cy * t.scale).max(0.2);
                let l = t.props[P_LIGHT].min(2.0);
                lights.push(((p - cam).length(), PointLight { pos: p, color: Vec3::new(1.0, 0.82, 0.55), intensity: 0.9 * l, reach: 6.0 + 6.0 * l }));
            }
        }
        // Flames over everything burning.
        if let Some(flame) = self.flame_type() {
            let mut fires: Vec<(f32, super::env::Burning)> = self.burning().into_iter().map(|b| ((b.pos - cam).length(), b)).filter(|(d, _)| *d < view).collect();
            fires.sort_by(|a, b| a.0.total_cmp(&b.0));
            for (d, b) in fires.iter().take(self.cfg.max_flames) {
                let s = (0.35 + 0.75 * b.fire) * (b.size / 0.5).clamp(0.6, 6.0);
                let p = b.pos + Vec3::Y * 0.7 * s;
                let mut g = GpuInst {
                    pos_scale: [p.x, p.y, p.z, s],
                    rot: [1.0, 0.0, flame.sphere_r * s, 1.0],
                    k0: [b.seed % 97.0, s, 0.5, 0.5],
                    k1: [0.5; 4],
                    fx: { let mut f = [0.0; 4]; f[FX_GLOW] = 1.6; f },
                    info: flame.gpu_info(),
                    ..Default::default()
                };
                g.s0[0] = self.t as f32 + b.seed * 0.37;
                insts.push(g);
                let flicker = 0.85 + 0.15 * ((self.t as f32 * 9.0 + b.seed).sin() * (self.t as f32 * 5.3 + b.seed * 0.3).cos());
                lights.push((*d, PointLight { pos: p + Vec3::Y * 0.5 * s, color: Vec3::new(1.0, 0.55, 0.22), intensity: (0.5 + 0.7 * b.fire * (b.size / 1.5).min(2.0)) * flicker, reach: 6.0 + 5.0 * s.min(4.0) }));
            }
        }
        // Where something is being conjured: a violet sigil on the ground
        // with motes rising from it, and its breathing light, until it
        // appears; then the sigil opens out and fades in a flash.
        let sigil = self.snap.scene.types.values().find(|t| t.builtin && t.name() == "conjuring").cloned();
        let ground = |p: Vec3| Vec3::new(p.x, self.snap.terrain.height(p.x, p.z).max(p.y - 1.0).min(p.y), p.z);
        let mut sigil_at = |insts: &mut Vec<GpuInst>, at: Vec3, s: f32, arrival: f32, seed: f32| {
            if let Some(ty) = &sigil {
                let p = ground(at);
                let mut g = GpuInst {
                    pos_scale: [p.x, p.y, p.z, s],
                    rot: [1.0, 0.0, ty.sphere_r * s, 1.0],
                    k0: [seed % 97.0, s, 0.5, 0.5],
                    k1: [0.5; 4],
                    fx: { let mut f = [0.0; 4]; f[FX_GLOW] = 0.95 * (1.0 - arrival * 0.4); f },
                    info: ty.gpu_info(),
                    ..Default::default()
                };
                g.s0[0] = self.t as f32 + seed * 0.37;
                g.s0[1] = arrival;
                insts.push(g);
            }
        };
        for (i, (at, mine)) in marks.conjures.iter().enumerate() {
            let p = *at + Vec3::Y * 0.8;
            let d = (p - cam).length();
            if d < view {
                sigil_at(&mut insts, *at, if *mine { 1.0 } else { 0.8 }, 0.0, i as f32 * 13.0 + at.x);
                // The traveler's own keeps its light among the many.
                let d = if *mine { d * 0.2 } else { d };
                let k = 0.75 + 0.25 * (self.t * 2.2 + i as f64).sin() as f32;
                let (intensity, reach) = if *mine { (0.9, 7.0) } else { (0.45, 4.5) };
                lights.push((d, PointLight { pos: p, color: CONJURE_LIGHT, intensity: intensity * k, reach }));
            }
        }
        for (at, when) in &self.flashes {
            let age = (self.t - when) as f32 / FLASH_FOR as f32;
            let p = *at + Vec3::Y * 1.0;
            let d = (p - cam).length();
            if (0.0..1.0).contains(&age) && d < view {
                sigil_at(&mut insts, *at, 1.0, age, at.x + at.z);
                let k = (1.0 - age).powi(2);
                lights.push((d, PointLight { pos: p, color: Vec3::new(0.95, 0.88, 1.0), intensity: 2.5 * k, reach: 6.0 + 8.0 * k }));
            }
        }
        // A blast: a hot white-orange flash, as big as the blast.
        for (at, when, r) in &self.blasts {
            let age = (self.t - when) as f32 / super::burst::BLAST_LIGHT as f32;
            let p = *at + Vec3::Y * 0.8;
            let d = (p - cam).length();
            if (0.0..1.0).contains(&age) && d < view {
                let k = (1.0 - age).powi(3);
                lights.push((d * 0.5, PointLight { pos: p, color: Vec3::new(1.0, 0.72, 0.4), intensity: (2.0 + r * 0.4).min(6.0) * k, reach: (r * 3.0 + 6.0) * (0.6 + 0.4 * k) }));
            }
        }
        // Placed lamps and lit windows at night, and their light anchors.
        let lit = self.cache.overlay.lamps;
        if lit > 0.05 {
            let c = crate::world::chunk_of(cam.x, cam.z);
            for p in self.snap.near_chunk(c) {
                if self.cache.overlay.hidden.contains(&p.id) {
                    continue;
                }
                let Some(ty) = self.snap.type_of(p.type_id) else { continue };
                let l = ty.light();
                let anchors: Vec<Vec3> = ty.ct.meta.anchors.iter().filter(|a| a.kind == "light").map(|a| Vec3::from_array(a.at)).collect();
                if l <= 0.0 && anchors.is_empty() {
                    continue;
                }
                let g = p.gpu(ty, 1.0);
                let spots: Vec<Vec3> = if anchors.is_empty() { vec![g.from_local(Vec3::new(0.0, ty.top * 0.9, 0.0))] } else { anchors.iter().map(|a| g.from_local(*a)).collect() };
                let l = if l > 0.0 { l.min(2.0) } else { 0.8 };
                for s in spots {
                    let d = (s - cam).length();
                    if d < 60.0 {
                        lights.push((d, PointLight { pos: s, color: Vec3::new(1.0, 0.8, 0.52), intensity: 0.8 * l * lit, reach: 7.0 + 5.0 * l }));
                    }
                }
            }
        }
        // Lights dim near the dark's own.
        let dark: Vec<(Vec3, f32)> = self.cast.npcs.iter().filter(|n| n.here() && n.species.touch.harms() && !n.species.hostile).map(|n| (n.a.pos, 1.0)).collect();
        if !dark.is_empty() {
            for (_, l) in lights.iter_mut() {
                if l.color.z > l.color.x {
                    continue;
                }
                let k = dark.iter().map(|(p, c)| c * (1.0 - (*p - l.pos).length() / 12.0).clamp(0.0, 1.0)).fold(0.0f32, f32::max);
                l.intensity *= 1.0 - 0.7 * k;
            }
        }
        lights.sort_by(|a, b| a.0.total_cmp(&b.0));
        Drawn { insts, holes, lights: lights.into_iter().map(|x| x.1).take(crate::render::MAX_LIGHTS).collect() }
    }

    fn flame_type(&self) -> Option<&std::sync::Arc<TypeEntry>> {
        self.snap.scene.types.values().find(|t| t.builtin && t.has_tag("flame"))
    }
}

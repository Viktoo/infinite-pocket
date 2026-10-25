//! Changing what a thing is, at the spot where it was touched: cutting
//! pieces out of it (holes, dents, bites), and rewriting its shape code
//! (reshape), optionally with something else worked into it (a stick added
//! to a wall, a wheel to a boat). Cuts are plain data, drawn and collided
//! without new shader code, and show at once; a reshape writes a new type
//! that only this thing uses. Every change goes into the thing's history.

use super::props::*;
use super::render::thing_inst;
use super::things::ThingId;
use super::{ActorId, Note, Request, Sim};
use crate::render::MAX_CUTS;
use crate::world::Solid;
use glam::Vec3;
use serde_json::json;

/// A thing's solid as the CPU sees it (cuts included).
fn solid_of(s: &Sim, id: ThingId) -> Option<Solid> {
    let t = s.things.get(id)?;
    let ty = s.snap.type_of(t.type_id)?.clone();
    Some(Solid { inst: thing_inst(t, &ty, [0.0; 4]), ty })
}

fn normal(s: &Solid, p: Vec3) -> Vec3 {
    let e = 0.02;
    let n = Vec3::new(
        s.sdf(p + Vec3::X * e) - s.sdf(p - Vec3::X * e),
        s.sdf(p + Vec3::Y * e) - s.sdf(p - Vec3::Y * e),
        s.sdf(p + Vec3::Z * e) - s.sdf(p - Vec3::Z * e),
    );
    n.try_normalize().unwrap_or(Vec3::Y)
}

/// World point → the type's own coordinates (what its sdf code sees).
fn to_local(s: &Solid, p: Vec3) -> Vec3 {
    Vec3::from(s.inst.to_local(p))
}

impl Sim {
    /// The point on a thing's surface nearest the way in from `from` (used
    /// when nobody pointed at a spot, e.g. a character's plan).
    pub fn touch_point(&self, id: ThingId, from: Vec3) -> Option<Vec3> {
        let s = solid_of(self, id)?;
        let c = s.inst.center();
        let dir = (c - from).try_normalize().unwrap_or(Vec3::Y * -1.0);
        let mut p = from;
        for _ in 0..96 {
            let d = s.sdf(p);
            if d < 0.01 {
                return Some(p);
            }
            p += dir * d.max(0.01);
            if (p - from).length() > (c - from).length() + s.inst.radius() {
                break;
            }
        }
        Some(c)
    }

    /// Where on `id` a touch lands: the given point, else the way in from the actor.
    pub fn touch_spot(&self, id: ThingId, at: Option<Vec3>, who: ActorId) -> Option<Vec3> {
        match at {
            Some(p) => Some(p),
            None => {
                let from = self.actor(who).map(|a| a.eye()).unwrap_or_default();
                self.touch_point(id, from)
            }
        }
    }

    /// A short description of a spot on a thing, in its own coordinates (for the LLM).
    pub fn describe_spot(&self, id: ThingId, at: Vec3) -> Option<serde_json::Value> {
        let s = solid_of(self, id)?;
        let lp = to_local(&s, at);
        let ln = {
            let n = normal(&s, at);
            (Vec3::from(s.inst.to_local(s.inst.pos() + n)) - Vec3::from(s.inst.to_local(s.inst.pos()))).normalize_or_zero()
        };
        let ty = &s.ty;
        let h = (ty.top - ty.bottom).max(1e-3);
        let up = ((lp.y - ty.bottom) / h).clamp(0.0, 1.0);
        let level = if up < 0.33 {
            "low"
        } else if up < 0.66 {
            "halfway up"
        } else {
            "high up"
        };
        let r = |v: f32| (v * 100.0).round() / 100.0;
        Some(json!({
            "local_point": [r(lp.x), r(lp.y), r(lp.z)],
            "surface_normal": [r(ln.x), r(ln.y), r(ln.z)],
            "where": format!("{level}, {:.1} m above its base", (lp.y - ty.bottom) * s.inst.pos_scale[3]),
        }))
    }

    /// Whether a small cut at `at` (world) shows: there is room for one
    /// more, or one already near enough to grow into. Blows spread over a
    /// whole wall leave as many holes as it can show, not one great gap.
    pub fn cut_fits(&self, id: ThingId, at: Vec3, size: f32) -> bool {
        let (Some(t), Some(solid)) = (self.things.get(id), solid_of(self, id)) else { return false };
        if t.shape.cuts.len() < crate::render::MAX_CUTS {
            return true;
        }
        let c = to_local(&solid, at);
        let r = size / solid.inst.pos_scale[3].max(0.01);
        t.shape.cuts.iter().any(|o| Vec3::new(o[0], o[1], o[2]).distance(c) < (o[3].abs() + r) * 1.5)
    }

    /// Cut a piece out of a thing at `at` (world): a round hole or dent of
    /// `size` metres radius, or a square one. When a thing already has as many
    /// cuts as it can show, the new one merges with the nearest.
    pub fn cut(&mut self, who: ActorId, id: ThingId, at: Option<Vec3>, size: f32, square: bool) -> Result<String, String> {
        let name = self.thing_name(id);
        let spot = self.touch_spot(id, at, who).ok_or("nowhere to cut")?;
        let solid = solid_of(self, id).ok_or("nowhere to cut")?;
        let scale = solid.inst.pos_scale[3].max(0.01);
        let b = solid.ty.ct.meta.bounds;
        let size = size.clamp(0.02, b[0].max(b[1]).max(b[2]) * scale);
        let n = normal(&solid, spot);
        // Centred a little inside the surface, so a small cut is a dent and
        // a big one goes through.
        let c = to_local(&solid, spot - n * size * 0.35);
        let r = size / scale;
        let new = [c.x, c.y, c.z, if square { -r } else { r }];
        let by = self.actor_name(who);
        let t = self.t;
        let Some(x) = self.things.get_mut(id) else { return Err("it's gone".into()) };
        if x.shape.cuts.len() >= MAX_CUTS {
            // Merge with the nearest cut into one sphere that covers both.
            let (i, _) = x
                .shape
                .cuts
                .iter()
                .enumerate()
                .map(|(i, o)| (i, Vec3::new(o[0], o[1], o[2]).distance(c)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .unwrap_or((0, 0.0));
            let o = x.shape.cuts[i];
            let oc = Vec3::new(o[0], o[1], o[2]);
            let (ra, rb) = (o[3].abs(), r);
            let d = oc.distance(c);
            let mid = (oc + c) * 0.5;
            let rr = d * 0.5 + ra.max(rb);
            x.shape.cuts[i] = [mid.x, mid.y, mid.z, rr];
        } else {
            x.shape.cuts.push(new);
        }
        x.note_edit(t, &by, &format!("cut ({:.2} m) at {:.1} m up", size, (spot.y - x.pos.y).max(0.0)));
        x.asleep = false;
        let msg = format!("{} cuts into the {name}", super::physics::cap(&by));
        self.event("cut", Some(who), Some(format!("thing:{id}")), msg.clone(), Some(spot), json!({ "size": size }));
        Ok(msg)
    }

    /// Rewrite a thing's shape code ("make the roof a dome"), optionally
    /// working another thing into it (`with`, e.g. the stick in your hand:
    /// it is used up when the new shape arrives). A type that already has
    /// the new name is reused at once; otherwise the builder edits this
    /// thing's code, and until it is done the thing keeps its old shape (and cuts).
    /// Returns the build it waits on, if it isn't done at once.
    pub fn reshape(&mut self, who: ActorId, id: ThingId, name: &str, change: &str, with: Option<ThingId>, at: Option<Vec3>) -> Result<Option<u64>, String> {
        let old = self.thing_name(id);
        let with = with.filter(|w| *w != id && self.things.get(*w).is_some());
        let name = name.trim();
        // The changed thing needs a name of its own: its old name would find
        // its old shape again, and nothing would change.
        let name = if name.is_empty() || name.eq_ignore_ascii_case(&old) { format!("{old} (changed {})", self.next_id()) } else { name.to_string() };
        if let Some(ty) = self.type_by_name(&name).filter(|ty| self.things.get(id).is_some_and(|t| t.type_id != ty.id)) {
            // The deed's own story says what happened.
            self.set_shape_type(id, ty.id, who, change, false, false);
            if let Some(w) = with {
                self.use_up(w);
            }
            return Ok(None);
        }
        if !self.has_llm {
            return Err("reshaping needs an LLM".into());
        }
        let t = self.things.get(id).cloned().ok_or("it's gone")?;
        let ty = self.snap.type_of(t.type_id).cloned().ok_or("unknown kind of thing")?;
        let spot = self.touch_spot(id, at, who).and_then(|p| self.describe_spot(id, p));
        let key = name.to_lowercase();
        if let Some(rid) = self.interp.building_names.get(&key).copied() {
            if let Some(b) = self.interp.building.get_mut(&rid) {
                b.reshape.push((id, who, with));
            }
            return Ok(Some(rid));
        }
        let ingredient = with.and_then(|w| {
            let wt = self.things.get(w)?;
            let wty = self.snap.type_of(wt.type_id)?;
            Some((wty.name().to_string(), wty.ct.source.clone(), wt.scale / t.scale.max(0.01)))
        });
        let rid = self.next_id();
        self.interp.building_names.insert(key, rid);
        self.interp.building.insert(rid, super::interp::PendingBuild { name: name.clone(), by: Some(who), reshape: vec![(id, who, with)], change: change.to_string(), ..Default::default() });
        self.request_now(Request::EditType {
            id: rid,
            name: name.clone(),
            source: ty.ct.source.clone(),
            change: change.to_string(),
            spot: spot.map(|v| v.to_string()).unwrap_or_default(),
            cuts: t.shape.cuts.clone(),
            with: ingredient,
        });
        Ok(Some(rid))
    }

    /// A thing worked into another one is gone (from the hands, too).
    pub fn use_up(&mut self, id: ThingId) {
        self.release(id);
        self.things.remove(id);
    }

    /// Give a thing a new shape type, keeping where it is, what it is made
    /// of and its history. `baked`: the new code already includes its cuts.
    /// `tell`: say so to those near (unless a story already did).
    pub fn set_shape_type(&mut self, id: ThingId, type_id: u32, who: ActorId, change: &str, baked: bool, tell: bool) {
        let by = self.actor_name(who);
        let Some(nty) = self.snap.type_of(type_id).cloned() else { return };
        let old = self.thing_name(id);
        let nname = nty.name().to_string();
        let t = self.t;
        let Some(x) = self.things.get_mut(id) else { return };
        x.type_id = type_id;
        if baked {
            x.shape.cuts.clear();
        }
        x.anchored = x.anchored || x.props[P_MASS] >= ANCHOR_MASS || nty.has_tag("building");
        x.asleep = false;
        x.note_edit(t, &by, &format!("reshaped: {change}"));
        let pos = x.pos;
        self.event("reshaped", Some(who), Some(format!("thing:{id}")), format!("the {old} became {} {nname}", super::article(&nname)), Some(pos), json!({ "into": nname, "change": change }));
        if tell {
            self.note_near(pos, 30.0, Note::Made(format!("The {old} becomes {} {nname}.", super::article(&nname))));
        }
        self.witness(pos, 20.0, &format!("I saw the {old} change: {change}."), 0.35, &[]);
    }
}

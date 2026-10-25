//! Finding the way between levels: up a stair or a ladder, along a
//! walkway, across a platform, and down again. The ends of every stair and
//! ladder, every walkway and every doorway are the places a way can turn;
//! two of them at one level are joined where something holds a body up
//! all the way between them (a platform, a deck, a roof, the ground). A
//! body on the ground going somewhere on the ground needs none of this.
//! Nothing here knows what a tree-house is.

use super::{ActorId, Sim};
use crate::world::collide::support;
use crate::world::{Solid, WorldSnapshot};
use glam::{Vec2, Vec3};
use std::collections::HashMap;

/// The places a way can turn, and how they are joined.
#[derive(Default)]
pub struct Levels {
    pub nodes: Vec<Vec3>,
    /// Neighbours and the cost to each (metres, climbing dearer).
    edges: Vec<Vec<(usize, f32)>>,
    /// On the ground (or a cave's floor): joined to the ground everywhere.
    ground: Vec<bool>,
    /// Solid placed shapes, for what holds a body up.
    statics: Vec<Solid>,
}

/// How far apart two places at one level may be joined (m).
const JOIN: f32 = 30.0;
/// Within this of each other in height, two places are at one level.
const LEVEL: f32 = 0.6;
/// A way's next place is passed when this near.
const NEAR: f32 = 0.6;

fn flat(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

impl Levels {
    /// Whether a body stands at `p` (on something within a step under it).
    fn stands(&self, t: &crate::terrain::Terrain, solids: &[Solid], p: Vec3) -> bool {
        (support(t, solids, p.x, p.z, p.y + 0.4) - p.y).abs() < 0.5
    }

    fn near_line(&self, a: Vec3, b: Vec3) -> Vec<Solid> {
        let mid = (a + b) * 0.5;
        let r = (b - a).length() * 0.5 + 1.0;
        self.statics.iter().filter(|s| (s.inst.center() - mid).length() < s.inst.radius() + r).cloned().collect()
    }

    /// Whether a body could walk straight from `a` to `b` at their level:
    /// held up all the way.
    fn joined(&self, t: &crate::terrain::Terrain, a: Vec3, b: Vec3) -> bool {
        if (a.y - b.y).abs() > LEVEL {
            return false;
        }
        let solids = self.near_line(a, b);
        let n = (flat(a, b) / 0.5).ceil().max(1.0) as usize;
        (0..=n).all(|i| self.stands(t, &solids, a.lerp(b, i as f32 / n as f32)))
    }

    fn on_ground(t: &crate::terrain::Terrain, p: Vec3) -> bool {
        p.y - t.floor(p.x, p.y + 0.5, p.z) < LEVEL
    }
}

/// The levels of a world version: from its stairs and ladders, its
/// walkways and its doorways.
pub fn levels_of(snap: &WorldSnapshot, climbs: &[super::climb::Climb], rooms: &[super::rooms::Room]) -> Levels {
    let t = &snap.terrain;
    let mut lv = Levels::default();
    lv.statics = snap
        .instances
        .iter()
        .filter_map(|p| {
            let ty = snap.type_of(p.type_id)?;
            (ty.solid && ty.ct.meta.joint.is_none()).then(|| Solid::new(p.gpu(ty, 1.0), ty.clone()))
        })
        .collect();
    let mut pairs: Vec<(Vec3, Vec3, f32)> = Vec::new();
    for c in climbs {
        pairs.push((c.foot, c.top, (c.top - c.foot).length() * if c.steep { 2.0 } else { 1.0 }));
    }
    for p in &snap.instances {
        let Some(ty) = snap.type_of(p.type_id) else { continue };
        if !(ty.builtin && ty.name() == "walkway") {
            continue;
        }
        let k = p.params;
        let l = 0.5 + k[2].clamp(0.0, 1.0) * 7.5 - 0.2;
        let rise = (k[4].clamp(0.0, 1.0) - 0.5) * 16.0;
        let g = p.gpu(ty, 1.0);
        let (a, b) = (g.from_local(Vec3::new(0.0, -rise * 0.5 * l / (l + 0.2), -l)), g.from_local(Vec3::new(0.0, rise * 0.5 * l / (l + 0.2), l)));
        pairs.push((a, b, (b - a).length()));
    }
    // Every place: ends of the pairs, and each doorway's step outside.
    let add = |lv: &mut Levels, p: Vec3| -> usize {
        if let Some(i) = lv.nodes.iter().position(|q| (*q - p).length() < 0.3) {
            return i;
        }
        lv.nodes.push(p);
        lv.edges.push(Vec::new());
        lv.ground.push(Levels::on_ground(t, p));
        lv.nodes.len() - 1
    };
    for (a, b, cost) in pairs {
        let (i, j) = (add(&mut lv, a), add(&mut lv, b));
        lv.edges[i].push((j, cost));
        lv.edges[j].push((i, cost));
    }
    if lv.nodes.is_empty() {
        return lv;
    }
    for r in rooms {
        for (out, _) in &r.doors {
            add(&mut lv, *out);
        }
    }
    // Places at one level, held up between.
    let n = lv.nodes.len();
    for i in 0..n {
        for j in i + 1..n {
            let (a, b) = (lv.nodes[i], lv.nodes[j]);
            let d = flat(a, b);
            if d > JOIN || lv.edges[i].iter().any(|e| e.0 == j) {
                continue;
            }
            if (lv.ground[i] && lv.ground[j]) || lv.joined(t, a, b) {
                lv.edges[i].push((j, d));
                lv.edges[j].push((i, d));
            }
        }
    }
    lv
}

impl Sim {
    /// The next place on the way to `dest` by stairs, ladders and
    /// walkways, when `me` or `dest` is off the ground and no straight walk
    /// joins them. None: go straight (or by doorways, see `route`).
    pub fn level_route(&self, who: ActorId, me: Vec3, dest: Vec3) -> Option<Vec3> {
        let lv = &self.levels;
        if lv.nodes.is_empty() {
            return None;
        }
        let t = &self.snap.terrain;
        // In a room whose doorway opens onto the ground (any floor of it, a
        // basement too): its own stairs and doorways lead there.
        let grounded = |p: Vec3| Levels::on_ground(t, p) || self.room_at(p).is_some_and(|r| self.rooms[r].doors.iter().any(|(out, _)| Levels::on_ground(t, *out)));
        let (mg, dg) = (grounded(me), grounded(dest));
        if mg && dg {
            return None;
        }
        // Remembered for a moment (finding it costs a look along each way).
        if let Some((d, at, next)) = self.level_ways.lock().get(&who).copied() {
            if (d - dest).length() < 0.5 && self.t - at < 1.5 && (flat(next, me) > NEAR || (next.y - me.y).abs() > LEVEL) {
                return (next != dest).then_some(next);
            }
        }
        let next = self.find_level_way(me, dest, mg, dg).unwrap_or(dest);
        self.level_ways.lock().insert(who, (dest, self.t, next));
        (next != dest).then_some(next)
    }

    fn find_level_way(&self, me: Vec3, dest: Vec3, mg: bool, dg: bool) -> Option<Vec3> {
        let lv = &self.levels;
        let t = &self.snap.terrain;
        if flat(me, dest) < JOIN && lv.joined(t, me, dest) {
            return None;
        }
        let n = lv.nodes.len();
        let link = |p: Vec3, ground: bool| -> Vec<(usize, f32)> {
            (0..n)
                .filter_map(|i| {
                    let q = lv.nodes[i];
                    let d = flat(p, q);
                    let ok = if ground { lv.ground[i] && d < 120.0 } else { d < JOIN && lv.joined(t, p, q) };
                    ok.then_some((i, d))
                })
                .collect()
        };
        let starts = link(me, mg);
        let goals: HashMap<usize, f32> = link(dest, dg).into_iter().collect();
        if starts.is_empty() || goals.is_empty() {
            return None;
        }
        // Dijkstra from `me` to `dest` (the node n).
        let mut cost = vec![f32::MAX; n + 1];
        let mut prev = vec![usize::MAX; n + 1];
        let mut done = vec![false; n + 1];
        for (i, d) in &starts {
            cost[*i] = *d;
        }
        loop {
            let Some(u) = (0..=n).filter(|i| !done[*i] && cost[*i] < f32::MAX).min_by(|a, b| cost[*a].total_cmp(&cost[*b])) else { return None };
            if u == n {
                break;
            }
            done[u] = true;
            let mut out: Vec<(usize, f32)> = lv.edges[u].clone();
            if let Some(g) = goals.get(&u) {
                out.push((n, *g));
            }
            for (v, w) in out {
                if cost[u] + w < cost[v] {
                    cost[v] = cost[u] + w;
                    prev[v] = u;
                }
            }
        }
        // Back from the goal to the first place on the way not yet reached.
        let mut path = Vec::new();
        let mut u = prev[n];
        while u != usize::MAX {
            path.push(u);
            u = prev[u];
        }
        path.reverse();
        path.into_iter().map(|i| lv.nodes[i]).find(|p| flat(*p, me) > NEAR || (p.y - me.y).abs() > LEVEL)
    }
}

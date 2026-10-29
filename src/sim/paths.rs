//! Paths: how a body finds its way round what stands in its way. The ground
//! is looked at in cells half a metre across, each as the body's own walk
//! would meet it there (its step, its clearance, water it may be in, rock
//! faces), only where a search needs it, and kept a while for the next
//! search by a body of that size. A search (A*) finds the cells round, and
//! the way is pulled straight between its corners. Rooms and levels keep
//! their own routing (doorways, stairs, walkways): a path only gets a body
//! to the next place on that way.
//!
//! Other bodies aren't in the cells (they move): a walker leans away from
//! anyone too near as it goes, and one standing about steps apart (`crowd`).

use super::{ActorId, Sim};
use crate::world::Solid;
use crate::world::collide::{Capsule, MAX_CLIMB, MAX_WADE, Obstacles, at_the_top, cave_wall, support};
use glam::{Vec2, Vec3};
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::hash::BuildHasherDefault;
use std::sync::Arc;

/// A cell's width (m).
pub const CELL: f32 = 0.5;
/// The solids for this many cells along each side are gathered at once.
const TILE: i32 = 16;
/// Cells and solids are kept this long (s): long enough to share, short
/// enough to see what changed (a felled tree, a parked cart).
const KEEP: f64 = 20.0;
/// Most places one search goes through, and all searches in one frame;
/// most new cells looked at in one frame.
const SEARCH_NODES: usize = 1500;
const FRAME_NODES: usize = 3000;
const FRAME_CELLS: usize = 2000;
/// A way is kept while its goal moves less than this (m, or a quarter of
/// the way there), for this long (s).
const GOAL_MOVES: f32 = 0.8;
const WAY_LIFE: f64 = 8.0;
/// A corner is passed when this near (m).
const PASSED: f32 = 0.45;
/// Nearer than this (m) a body just walks there.
const STRAIGHT: f32 = 1.2;
/// The room a body keeps from others past touching (m), and how hard it
/// leans away.
const ROOM: f32 = 0.3;

/// A quick hash for the small keys of cells, looked up many times a search.
#[derive(Default, Clone, Copy)]
struct Quick(u64);

impl Quick {
    fn add(&mut self, v: u64) {
        self.0 = (self.0.rotate_left(5) ^ v).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

impl std::hash::Hasher for Quick {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.add(*b as u64);
        }
    }
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    fn write_u16(&mut self, i: u16) {
        self.add(i as u64);
    }
    fn write_i16(&mut self, i: i16) {
        self.add(i as u16 as u64);
    }
    fn write_i32(&mut self, i: i32) {
        self.add(i as u32 as u64);
    }
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
}

type QuickMap<K, V> = HashMap<K, V, BuildHasherDefault<Quick>>;

/// Bodies that meet the world alike share cells: by size, step, whether
/// they swim and whether doors are in their way.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct Kind {
    radius: u16,
    height: u16,
    step: u16,
    swims: bool,
    hinges: bool,
}

impl Kind {
    fn of(cap: Capsule, hinges: bool) -> Kind {
        // In steps of 5 cm.
        let q = |v: f32| (v * 20.0).round() as u16;
        Kind { radius: q(cap.radius), height: q(cap.height), step: q(cap.step), swims: cap.swims, hinges }
    }
}

/// A body in a cell, come there at some height.
#[derive(Clone, Copy, Debug)]
struct Cell {
    /// Where its feet are: on the ground, a floor, a deck; afloat in deep water.
    feet: f32,
    ok: bool,
    /// Close by a wall (a way keeps off these where it can).
    snug: bool,
    deep: bool,
    /// On something built, not the land.
    built: bool,
    /// The land's own height there (for its slope).
    land: f32,
    made: f64,
}

/// One body's way: the corners left to pass.
#[derive(Clone, Debug)]
pub struct Way {
    goal: Vec3,
    pub pts: Vec<Vec3>,
    at: usize,
    made: f64,
}

#[derive(Default)]
pub struct Paths {
    cells: QuickMap<(Kind, i32, i32, i16), Cell>,
    tiles: HashMap<(i32, i32), (Arc<Vec<Solid>>, f64)>,
    pub ways: HashMap<ActorId, Way>,
    /// Cells and places left to look at in this frame, and which frame.
    budget: usize,
    nodes: usize,
    frame: f64,
    swept: f64,
}

impl Paths {
    /// The world changed: nothing seen is true any more.
    pub fn forget(&mut self) {
        self.cells.clear();
        self.tiles.clear();
        self.ways.clear();
    }
}

fn cell_of(p: Vec3) -> (i32, i32) {
    ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32)
}

fn centre(c: (i32, i32)) -> Vec2 {
    Vec2::new((c.0 as f32 + 0.5) * CELL, (c.1 as f32 + 0.5) * CELL)
}

fn flat(a: Vec3, b: Vec3) -> f32 {
    Vec2::new(a.x - b.x, a.z - b.z).length()
}

/// Heights a body comes into a cell at, a quarter metre apart.
fn level(y: f32) -> i16 {
    (y * 4.0).round() as i16
}

impl Sim {
    /// Where to walk next on the way to `dest`: through doorways and up
    /// stairs (see `route`), and round what stands in the way.
    pub fn way_to(&mut self, who: ActorId, dest: Vec3) -> Vec3 {
        let next = self.route(who, dest);
        let Some(me) = self.actor(who).map(|a| a.pos) else { return next };
        // Inside, and on the last steps through a doorway or onto a stair,
        // rooms and levels know the way (`dest` itself may carry no height).
        let near_turn = next != dest && ((next.y - me.y).abs() > 1.2 || flat(me, next) < 2.5);
        if near_turn || flat(me, next) < STRAIGHT || self.room_at(me).is_some() || !self.finds_paths(who) {
            self.paths.ways.remove(&who);
            return next;
        }
        self.sweep_paths();
        if let Some(w) = self.paths.ways.get_mut(&who) {
            if flat(w.goal, next) < GOAL_MOVES.max(flat(me, next) * 0.25) && self.t - w.made < WAY_LIFE {
                while w.at < w.pts.len() && flat(w.pts[w.at], me) < PASSED {
                    w.at += 1;
                }
                return w.pts.get(w.at).copied().unwrap_or(next);
            }
        }
        match self.find_way(who, me, next) {
            Some(pts) => {
                let first = pts.first().copied().unwrap_or(next);
                self.paths.ways.insert(who, Way { goal: next, pts, at: 0, made: self.t });
                first
            }
            // No looking left this frame: straight on, and look again next.
            None => next,
        }
    }

    /// Whether a body finds its way by the cells: on its feet or afloat
    /// near a traveler (far off, nobody sees), not diving, flying or climbing.
    fn finds_paths(&self, who: ActorId) -> bool {
        let Some(a) = self.actor(who) else { return false };
        if a.dive_to.is_some() || a.alt > 0.3 || a.ladder.is_some() || a.carried() {
            return false;
        }
        self.capsule(who).needs_water <= 0.0 && self.dist_to_travelers(a.pos) <= self.cfg.near
    }

    /// Whether a body could stand at `p` on the ground there (or afloat,
    /// for one that swims): not in a wall, on water it may be in.
    pub fn open_at(&mut self, who: ActorId, p: Vec3) -> bool {
        let cap = self.capsule(who);
        let kind = Kind::of(cap, self.can_work_hinges(who));
        let ground = self.snap.terrain.height(p.x, p.z);
        self.cell(kind, cap, cell_of(p), ground, true).is_some_and(|c| c.ok)
    }

    /// Something stood in the way of a way just taken (the cells didn't see
    /// it: a cart, a fallen tree): the cell ahead is shut a while, and the
    /// way looked for again.
    pub fn way_blocked(&mut self, who: ActorId, ahead: Vec3) {
        if self.paths.ways.remove(&who).is_none() {
            return;
        }
        let cap = self.capsule(who);
        let kind = Kind::of(cap, self.can_work_hinges(who));
        let c = cell_of(ahead);
        for (k, cell) in self.paths.cells.iter_mut() {
            if k.0 == kind && k.1 == c.0 && k.2 == c.1 {
                cell.ok = false;
                cell.made = self.t;
            }
        }
    }

    fn sweep_paths(&mut self) {
        let t = self.t;
        if t != self.paths.frame {
            self.paths.frame = t;
            self.paths.budget = FRAME_CELLS;
            self.paths.nodes = FRAME_NODES;
        }
        if t - self.paths.swept < 5.0 {
            return;
        }
        self.paths.swept = t;
        self.paths.cells.retain(|_, c| t - c.made < KEEP);
        self.paths.tiles.retain(|_, s| t - s.1 < KEEP);
        self.paths.ways.retain(|_, w| t - w.made < WAY_LIFE);
    }

    /// The solids that matter to cells in one tile.
    fn tile_solids(&mut self, c: (i32, i32)) -> Arc<Vec<Solid>> {
        let tc = (c.0.div_euclid(TILE), c.1.div_euclid(TILE));
        if let Some((s, made)) = self.paths.tiles.get(&tc) {
            if self.t - made < KEEP {
                return s.clone();
            }
        }
        let side = TILE as f32 * CELL;
        let (x, z) = ((tc.0 as f32 + 0.5) * side, (tc.1 as f32 + 0.5) * side);
        let p = Vec3::new(x, self.snap.terrain.height(x, z), z);
        let s = Arc::new(self.solids_near(p, side * 0.75 + 6.0));
        self.paths.tiles.insert(tc, (s.clone(), self.t));
        s
    }

    /// A body of `kind` come into cell `c` at the height `from`: where it
    /// stands there and whether it fits. None once this frame has looked
    /// at all it may (unless `always`).
    fn cell(&mut self, kind: Kind, cap: Capsule, c: (i32, i32), from: f32, always: bool) -> Option<Cell> {
        let key = (kind, c.0, c.1, level(from));
        if let Some(cell) = self.paths.cells.get(&key) {
            return Some(*cell);
        }
        if !always {
            if self.paths.budget == 0 {
                return None;
            }
            self.paths.budget -= 1;
        }
        let from = key.3 as f32 / 4.0;
        let all = self.tile_solids(c);
        let q = centre(c);
        let solids: Vec<Solid> = all
            .iter()
            .filter(|s| {
                let sc = s.inst.center();
                !(kind.hinges && s.ty.ct.meta.joint.is_some()) && Vec2::new(sc.x - q.x, sc.z - q.y).length() < s.inst.radius() + cap.radius + 1.0
            })
            .cloned()
            .collect();
        let t = &self.snap.terrain;
        let top = from + cap.step;
        let ground = support(t, &solids, q.x, q.y, top);
        let land = t.floor(q.x, top, q.y);
        let built = ground > land + 0.02;
        let wl = t.water(q.x, q.y);
        // As deep as a swimmer of its size floats (see `footing`), eyes just over.
        let deep = wl - ground > if cap.swims { MAX_WADE * cap.height / 1.75 } else { MAX_WADE };
        let feet = if deep { (wl + 0.12 - cap.height * 0.9).max(ground) } else { ground };
        // Down further than its own height (off a roof, a cliff): a fall
        // that hurts, never a way.
        let falls = !deep && from - feet > cap.height.max(1.0);
        let (ok, snug) = if (deep && !cap.swims) || falls {
            (false, false)
        } else {
            let obs = Obstacles { solids: &solids, bodies: &[] };
            let b = if deep { at_the_top(cap, feet, wl) } else { cap };
            let d = obs.dist_body(q.x, q.y, feet, b);
            ((d >= b.radius || obs.clear(q.x, q.y, feet, b)) && !cave_wall(t, q, feet, b), d < b.radius + ROOM)
        };
        let cell = Cell { feet, ok, snug, deep, built, land: t.natural_height(q.x, q.y), made: self.t };
        self.paths.cells.insert(key, cell);
        Some(cell)
    }

    /// The step from one cell to the next (an edge, not a diagonal): None
    /// with no looking left; Some(None) where it can't be taken.
    fn edge(&mut self, kind: Kind, cap: Capsule, a: (i32, i32), at: &Cell, b: (i32, i32)) -> Option<Option<Cell>> {
        let to = self.cell(kind, cap, b, at.feet, false)?;
        if !to.ok {
            return Some(None);
        }
        let ok = if at.deep && !to.deep {
            // Out of the water onto a bank no higher than a pull up.
            let pb = centre(b);
            to.feet <= self.snap.terrain.water(pb.x, pb.y) + 0.9
        } else if !to.built && !to.deep {
            // Not up a rock face (as `too_steep`, from the cells' land).
            let run = if a.0 != b.0 && a.1 != b.1 { CELL * std::f32::consts::SQRT_2 } else { CELL };
            let cave = at.feet < at.land - 0.5;
            cave || !(to.land - at.land > MAX_CLIMB * run && to.land > at.feet + 0.05 && at.feet < at.land + 0.5)
        } else {
            true
        };
        Some(ok.then_some(to))
    }

    /// The corners of a way from `me` to `goal` round what stands between
    /// (its last corner the goal, or as near as it goes). None when this
    /// frame has no looking left.
    fn find_way(&mut self, who: ActorId, me: Vec3, goal: Vec3) -> Option<Vec<Vec3>> {
        let cap = self.capsule(who);
        let kind = Kind::of(cap, self.can_work_hinges(who));
        let start = cell_of(me);
        let end = cell_of(goal);
        let h = |c: (i32, i32)| {
            let (dx, dz) = ((c.0 - end.0).abs() as f32, (c.1 - end.1).abs() as f32);
            (dx.max(dz) + (std::f32::consts::SQRT_2 - 1.0) * dx.min(dz)) * 1.2
        };
        // A place: a cell at a height (a bridge's deck over the bank under it).
        type Node = (i32, i32, i16);
        let afloat = self.actor(who).is_some_and(|a| a.swimming);
        let first = Cell { feet: me.y, ok: true, snug: false, deep: afloat, built: false, land: self.snap.terrain.natural_height(me.x, me.z), made: self.t };
        // Straight there when nothing is in the way.
        if self.straight(kind, cap, (start, first), (end, first)) {
            return Some(vec![goal]);
        }
        if self.paths.nodes < SEARCH_NODES {
            return None;
        }
        let mut seen: QuickMap<Node, (f32, Option<Node>, Cell)> = QuickMap::default();
        let mut done: HashSet<Node, BuildHasherDefault<Quick>> = HashSet::default();
        let mut open = BinaryHeap::new();
        let n0: Node = (start.0, start.1, level(me.y));
        seen.insert(n0, (0.0, None, first));
        open.push((Reverse(h(start).to_bits()), 0usize, n0));
        let mut best = (h(start), n0);
        let mut found = None;
        let mut count = 0usize;
        let steps = [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)];
        while let Some((_, _, n)) = open.pop() {
            if !done.insert(n) {
                continue;
            }
            let (g, _, here) = seen[&n];
            let c = (n.0, n.1);
            if (c.0 - end.0).abs() <= 1 && (c.1 - end.1).abs() <= 1 {
                found = Some(n);
                break;
            }
            if count >= SEARCH_NODES {
                break;
            }
            for (dx, dz) in steps {
                let b = (c.0 + dx, c.1 + dz);
                let to = if dx != 0 && dz != 0 {
                    // Round a corner, not across it.
                    let side1 = self.edge(kind, cap, c, &here, (c.0 + dx, c.1))?;
                    let side2 = self.edge(kind, cap, c, &here, (c.0, c.1 + dz))?;
                    if side1.is_none() || side2.is_none() {
                        continue;
                    }
                    self.edge(kind, cap, c, &here, b)?
                } else {
                    self.edge(kind, cap, c, &here, b)?
                };
                let Some(to) = to else { continue };
                let m: Node = (b.0, b.1, level(to.feet));
                // Down off something built: only when there is no other way.
                let drop = if here.built || to.built { (here.feet - to.feet - cap.step).max(0.0) } else { 0.0 };
                let cost = g + if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 } + if to.snug { 0.6 } else { 0.0 } + if drop > 0.05 { 3.0 + drop * 4.0 } else { 0.0 };
                if seen.get(&m).is_some_and(|s| s.0 <= cost) {
                    continue;
                }
                seen.insert(m, (cost, Some(n), to));
                count += 1;
                let hm = h(b);
                if hm < best.0 {
                    best = (hm, m);
                }
                open.push((Reverse((cost + hm).to_bits()), count, m));
            }
        }
        self.paths.nodes -= count.min(self.paths.nodes);
        // Back from the goal (or the nearest place reached) to the start.
        let mut cells = Vec::new();
        let mut n = found.unwrap_or(best.1);
        loop {
            let (_, prev, cell) = seen[&n];
            cells.push(((n.0, n.1), cell));
            match prev {
                Some(p) => n = p,
                None => break,
            }
        }
        cells.reverse();
        let mut pts = self.pull_straight(kind, cap, &cells);
        if found.is_some() {
            if let Some(last) = pts.last_mut() {
                *last = Vec3::new(goal.x, last.y, goal.z);
            }
        }
        Some(pts)
    }

    /// The corners of a way through `cells`: from each corner, on to the
    /// furthest cell walked to straight through open cells.
    fn pull_straight(&mut self, kind: Kind, cap: Capsule, cells: &[((i32, i32), Cell)]) -> Vec<Vec3> {
        let at = |i: usize| {
            let q = centre(cells[i].0);
            Vec3::new(q.x, cells[i].1.feet, q.y)
        };
        let mut pts = Vec::new();
        let mut i = 0;
        while i + 1 < cells.len() {
            let mut j = i + 1;
            for k in (i + 2..cells.len().min(i + 40)).rev() {
                if self.straight(kind, cap, cells[i], cells[k]) {
                    j = k;
                    break;
                }
            }
            pts.push(at(j));
            i = j;
        }
        pts
    }

    /// Whether a body walks straight from one cell to another through open
    /// cells (none snug against a wall, but at the ends).
    fn straight(&mut self, kind: Kind, cap: Capsule, a: ((i32, i32), Cell), b: ((i32, i32), Cell)) -> bool {
        let (pa, pb) = (centre(a.0), centre(b.0));
        let n = ((pb - pa).length() / (CELL * 0.5)).ceil() as usize;
        let (mut c, mut here) = a;
        for s in 1..=n {
            let q = pa.lerp(pb, s as f32 / n as f32);
            let next = cell_of(Vec3::new(q.x, 0.0, q.y));
            if next == c {
                continue;
            }
            // A diagonal crossing goes by a side cell first.
            if next.0 != c.0 && next.1 != c.1 {
                let side = (next.0, c.1);
                match self.edge(kind, cap, c, &here, side) {
                    Some(Some(cell)) if !cell.snug => {}
                    _ => return false,
                }
            }
            match self.edge(kind, cap, c, &here, next) {
                // Never off a ledge on the way.
                Some(Some(cell)) if (!cell.snug || next == b.0) && !((here.built || cell.built) && here.feet - cell.feet > cap.step + 0.05) => here = cell,
                _ => return false,
            }
            c = next;
        }
        true
    }

    /// Which way, and how hard, a body at `me` leans away from those too
    /// near it (flat; zero with room enough).
    pub fn crowd(&self, who: ActorId, me: Vec3) -> Vec3 {
        let Some(a) = self.actor(who) else { return Vec3::ZERO };
        let r = super::footing::walk_radius(who, a);
        let mut push = Vec3::ZERO;
        let others = std::iter::once((ActorId::Player, &self.player))
            .chain(self.travelers.iter().map(|(n, t)| (ActorId::Traveler(*n), &t.a)))
            .chain(self.cast.npcs.iter().filter(|n| n.here()).map(|n| (ActorId::Npc(n.def.id), &n.a)));
        for (o, b) in others {
            if o == who || b.carried() || a.riding == Some(o) || b.riding == Some(who) || b.alt > 1.0 || (b.pos.y - me.y).abs() > 1.5 {
                continue;
            }
            let ro = super::footing::walk_radius(o, b);
            let want = r + ro + ROOM;
            let off = Vec3::new(me.x - b.pos.x, 0.0, me.z - b.pos.z);
            let d = off.length();
            if d >= want {
                continue;
            }
            // On one spot: apart by who they are.
            let dir = if d > 1e-3 {
                off / d
            } else if who < o {
                Vec3::X
            } else {
                -Vec3::X
            };
            push += dir * (want - d) / want;
        }
        push
    }

    /// Whether someone else already stands at `p` (a body arriving there
    /// stops beside them).
    pub fn spot_taken(&self, who: ActorId, p: Vec3) -> bool {
        let Some(a) = self.actor(who) else { return false };
        let r = super::footing::walk_radius(who, a);
        self.actor_ids().into_iter().filter(|o| *o != who).filter_map(|o| self.actor(o).map(|b| (o, b))).any(|(o, b)| !b.carried() && flat(b.pos, p) < r + super::footing::walk_radius(o, b) + 0.2)
    }

    /// A body standing about too near others steps apart.
    pub fn make_room(&mut self, who: ActorId, dt: f32) {
        let Some(a) = self.actor(who) else { return };
        if a.task.is_some() || a.asleep || a.carried() || a.alt > 0.3 || a.ladder.is_some() || self.social.busy(who) {
            return;
        }
        let push = self.crowd(who, a.pos);
        if push.length() < 0.1 {
            return;
        }
        self.walk(who, push.normalize() * (0.6 * dt).min(0.1));
    }
}

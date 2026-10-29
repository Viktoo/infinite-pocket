//! Collision against the CPU SDF of nearby solids, with sliding.

use super::Solid;
use crate::terrain::Terrain;
use glam::{Vec2, Vec3};

pub const PLAYER_RADIUS: f32 = 0.35;
pub const NPC_RADIUS: f32 = 0.3;
/// Growth (bushes, flowers) gives way below this height above the feet.
pub const GROWTH_GIVES: f32 = 0.5;
/// Water deeper than this is impassable.
pub const MAX_WADE: f32 = 1.1;
/// The steepest ground anyone walks up (rise over run, about 54°): steeper
/// is a rock face. Down it they go (sliding, or falling off its edge).
pub const MAX_CLIMB: f32 = 1.4;
/// How far ahead the slope is judged (a single small step says little).
const CLIMB_PROBE: f32 = 0.3;

pub struct Obstacles<'a> {
    pub solids: &'a [Solid],
    /// Other bodies (feet position, radius) treated as vertical capsules.
    pub bodies: &'a [(Vec3, f32)],
}

/// A walking body: an upright capsule.
#[derive(Clone, Copy, Debug)]
pub struct Capsule {
    pub radius: f32,
    /// How tall it stands now (less when crouching).
    pub height: f32,
    /// The highest step it walks up without jumping.
    pub step: f32,
    /// Goes into water too deep to wade, and swims there.
    pub swims: bool,
    /// Lives only in water: it keeps where the water is at least this deep
    /// (0: anywhere it can go). Stranded, it can still go deeper.
    pub needs_water: f32,
}

impl Capsule {
    /// A body of this height and radius, standing.
    pub fn of(height: f32, radius: f32) -> Capsule {
        Capsule { radius, height, step: step_for(height), swims: false, needs_water: 0.0 }
    }
}

/// The step a body of `height` takes in its stride (stairs, kerbs, a root).
pub fn step_for(height: f32) -> f32 {
    (height * 0.25).clamp(0.08, 1.2)
}

/// Just above its step, a body only meets what stands up past the step:
/// a sample there reaches out no further than its height above the step, so
/// the next tread of a stair (its top below the step) is no wall, and a
/// riser or wall standing higher still is.
fn under_step(y: f32, feet: f32, b: Capsule) -> f32 {
    (b.radius - (y - feet - b.step).max(0.0)).max(0.0)
}

impl Obstacles<'_> {
    /// Distance from a person-sized column standing on the ground at (x, z)
    /// to the nearest obstacle.
    pub fn dist(&self, terrain: &Terrain, x: f32, z: f32) -> f32 {
        let g = terrain.height(x, z);
        self.dist_body(x, z, g, Capsule::of(1.75, NPC_RADIUS))
    }

    /// Distance from a body's column at (x, z), feet at `feet`, to the nearest
    /// obstacle: solids from just above its step to the top of its head (the
    /// top sample a radius below it: a capsule's cap), and other bodies at
    /// about its height.
    pub fn dist_body(&self, x: f32, z: f32, feet: f32, b: Capsule) -> f32 {
        let lo0 = feet + b.step + 0.05;
        let hi = (feet + b.height - b.radius).max(lo0);
        let mut d = f32::MAX;
        for s in self.solids {
            let lo = if s.ty.is_growth() { lo0.max(feet + GROWTH_GIVES) } else { lo0 };
            if lo > hi {
                continue;
            }
            let n = (((hi - lo) / 0.7).ceil() as usize).clamp(1, 8);
            for i in 0..=n {
                let y = lo + (hi - lo) * i as f32 / n as f32;
                d = d.min(s.sdf(Vec3::new(x, y, z)) + under_step(y, feet, b));
            }
        }
        for (o, r) in self.bodies {
            if o.y > feet + b.height || o.y + 2.2 < feet {
                continue;
            }
            d = d.min(Vec2::new(o.x - x, o.z - z).length() - r);
        }
        d
    }

    /// Whether a body really stands clear at (x, z): its rim, at each height,
    /// is outside every solid. Shapes carved by subtraction (a doorway cut
    /// out of a wall) report distances that are too small near the cut; the
    /// sign is still right, so the rim tells the truth `dist_body` can't.
    pub fn clear(&self, x: f32, z: f32, feet: f32, b: Capsule) -> bool {
        for (o, r) in self.bodies {
            if o.y > feet + b.height || o.y + 2.2 < feet {
                continue;
            }
            if Vec2::new(o.x - x, o.z - z).length() - r < b.radius {
                return false;
            }
        }
        let lo0 = feet + b.step + 0.05;
        let hi = (feet + b.height - b.radius).max(lo0);
        for s in self.solids {
            let c = s.inst.center();
            if Vec2::new(c.x - x, c.z - z).length() > s.inst.radius() + b.radius + 0.1 {
                continue;
            }
            let lo = if s.ty.is_growth() { lo0.max(feet + GROWTH_GIVES) } else { lo0 };
            if lo > hi {
                continue;
            }
            let n = (((hi - lo) / 0.7).ceil() as usize).clamp(1, 8);
            for i in 0..=n {
                let y = lo + (hi - lo) * i as f32 / n as f32;
                let mut d0 = s.sdf(Vec3::new(x, y, z));
                if d0 >= 0.0 && d0 < b.radius {
                    d0 = s.exact_sdf(Vec3::new(x, y, z));
                }
                if d0 < 0.0 {
                    return false;
                }
                if d0 + under_step(y, feet, b) >= b.radius {
                    continue;
                }
                // An uncarved shape's distance is true enough: it touches.
                if !s.ty.ct.carved {
                    return false;
                }
                // March out along each of eight directions to the rim: the
                // distance is a safe step, so even a thin door is met.
                for k in 0..8 {
                    let a = k as f32 * std::f32::consts::FRAC_PI_4;
                    let dir = Vec3::new(a.cos(), 0.0, a.sin());
                    let mut t = d0;
                    while t < b.radius {
                        let d = s.sdf(Vec3::new(x, y, z) + dir * t);
                        if d < 0.0 {
                            return false;
                        }
                        t += d.max(0.03);
                    }
                    if s.sdf(Vec3::new(x, y, z) + dir * b.radius) < 0.0 {
                        return false;
                    }
                }
            }
            // And up each line of its height, centre and rim: a beam lower
            // than its head is met however thin.
            if !s.ty.ct.carved {
                continue;
            }
            for k in 0..9 {
                let (ox, oz) = if k == 8 {
                    (0.0, 0.0)
                } else {
                    let a = k as f32 * std::f32::consts::FRAC_PI_4;
                    (a.cos() * b.radius, a.sin() * b.radius)
                };
                let mut y = lo;
                while y <= hi {
                    let d = s.sdf(Vec3::new(x + ox, y, z + oz));
                    if d < 0.0 {
                        return false;
                    }
                    y += d.max(0.03);
                }
            }
        }
        true
    }
}

fn wadeable(terrain: &Terrain, x: f32, z: f32) -> bool {
    terrain.height(x, z) > terrain.water(x, z) - MAX_WADE
}

/// Whether a body at `q` with its feet at `feet` would be in a cave's rock:
/// above its step (a wall) or at its head (a low roof). Only near caves.
pub fn cave_wall(terrain: &Terrain, q: Vec2, feet: f32, b: Capsule) -> bool {
    if !terrain.caves_near(q.x, q.y) {
        return false;
    }
    let h = terrain.height(q.x, q.y);
    if feet > h - 0.05 {
        // Above the surface: a cave's mouth is a way down, not a wall.
        return false;
    }
    [feet + b.step + 0.05, feet + b.height * 0.85].iter().any(|y| terrain.solid(glam::Vec3::new(q.x, *y, q.y)))
}

/// A body afloat at the top, or bobbing just under it, meets what reaches
/// down into the water as a wall (a wall standing in a pond): it doesn't
/// slip under and come up inside. A diver deep under is left as it is.
pub fn at_the_top(b: Capsule, feet: f32, wl: f32) -> Capsule {
    if feet + b.height < wl - 0.6 {
        return b;
    }
    Capsule { height: b.height.max(wl + 0.05 + b.radius - feet), ..b }
}

/// Whether going from `from` towards `to` climbs a rock face: the natural
/// ground ahead rises steeper than `MAX_CLIMB` and above the feet. Dug
/// ground is left out (anyone climbs out of a pit or a cellar), and so is a
/// body standing above the ground on something (a roof, a bridge).
pub fn too_steep(terrain: &Terrain, from: Vec2, to: Vec2, feet: f32) -> bool {
    let d = to - from;
    let run = d.length();
    if run < 1e-5 {
        return false;
    }
    let reach = run.max(CLIMB_PROBE);
    let ahead = from + d / run * reach;
    let here = terrain.natural_height(from.x, from.y);
    // Under the ground (in a cave) its walls stop a body, not the slope above.
    if feet < here - 0.5 {
        return false;
    }
    let there = terrain.natural_height(ahead.x, ahead.y);
    there - here > MAX_CLIMB * reach && there > feet + 0.05 && feet < here + 0.5
}

/// Move a body by `delta` (XZ), sliding along surfaces. Returns the new feet
/// position (its height unchanged: footing settles it, see `sim::footing`).
pub fn move_body(terrain: &Terrain, obs: &Obstacles, from: Vec3, delta: Vec3, b: Capsule) -> Vec3 {
    // Another body only stands in the way of a step toward it: two who
    // touch can always step apart, or round each other.
    let toward: Vec<(Vec3, f32)> = obs.bodies.iter().copied().filter(|(o, _)| (o.x - from.x) * delta.x + (o.z - from.z) * delta.z > 0.0).collect();
    let obs = &Obstacles { solids: obs.solids, bodies: &toward };
    let radius = b.radius;
    let start = Vec2::new(from.x, from.z);
    // A swimmer goes into deep water and, in it or at its edge, pulls
    // itself out onto a bank (however sheer) no higher than a little over
    // the water.
    let wl = terrain.water(start.x, start.y);
    let out = wl + 0.9;
    let afloat = b.swims && from.y < out && [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0), (0.0, 0.0)].iter().any(|(dx, dz)| terrain.height(start.x + dx, start.y + dz) < wl - 0.05);
    // Afloat, what stands no higher than that (a step, a slipway, a
    // pontoon) is somewhere to climb onto, not a wall in the way.
    let feet = if afloat && b.needs_water <= 0.0 && !obs.solids.is_empty() {
        let q = Vec3::new(from.x + delta.x, from.y, from.z + delta.z);
        let up = support_under(terrain, obs.solids, q, radius, out);
        // Only near the top: a roof down on the bed is no way out.
        if up > wl - 1.0 { from.y.max(up) } else { from.y }
    } else {
        from.y
    };
    let b = if afloat { at_the_top(b, feet, wl) } else { b };
    let dist = |x: f32, z: f32| obs.dist_body(x, z, feet, b);
    // Clear by the fast distance, or (near a carved opening) by the rim.
    let ok = |x: f32, z: f32, d: f32| d >= radius || obs.clear(x, z, feet, b);
    let here = wl - terrain.height(start.x, start.y);
    let water_ok = |q: Vec2| {
        if b.needs_water > 0.0 {
            // A fish keeps to its water; stranded, it never goes shallower.
            // Over a step or a slipway the water is only as deep as over it.
            let floor = if obs.solids.is_empty() { terrain.height(q.x, q.y) } else { support_under(terrain, obs.solids, Vec3::new(q.x, feet, q.y), radius, feet + b.step) };
            let there = terrain.water(q.x, q.y) - floor;
            there >= b.needs_water || there >= here
        } else if b.swims {
            !afloat || terrain.height(q.x, q.y) < out.max(feet + b.step)
        } else {
            wadeable(terrain, q.x, q.y)
        }
    };
    let passable = |q: Vec2| water_ok(q) && (afloat || !too_steep(terrain, start, q, feet)) && !cave_wall(terrain, q, feet, b);
    let mut p = Vec2::new(from.x + delta.x, from.z + delta.z);
    if !passable(p) {
        // Slide along the shoreline or the foot of a cliff: try each axis on its own.
        let px = Vec2::new(from.x + delta.x, from.z);
        let pz = Vec2::new(from.x, from.z + delta.z);
        p = if delta.x.abs() > 1e-5 && passable(px) {
            px
        } else if delta.z.abs() > 1e-5 && passable(pz) {
            pz
        } else {
            start
        };
    }
    if !obs.solids.is_empty() || !obs.bodies.is_empty() {
        // Overlapping a solid already (an old save, a building that appeared
        // on top of us): free to walk out. Overlapping only other bodies
        // (two stood on one spot) is not leave to walk through walls.
        let walls = Obstacles { solids: obs.solids, bodies: &[] };
        let d_start = walls.dist_body(from.x, from.z, feet, b);
        if d_start < radius * 0.5 && !walls.clear(from.x, from.z, feet, b) {
            // Already overlapping something (an old save, or a building that
            // appeared on top of us): move freely until clear, so we can walk out.
            return Vec3::new(p.x, feet, p.y);
        }
        for _ in 0..5 {
            let d = dist(p.x, p.y);
            if ok(p.x, p.y, d) {
                break;
            }
            let e = 0.04;
            let gx = dist(p.x + e, p.y) - dist(p.x - e, p.y);
            let gz = dist(p.x, p.y + e) - dist(p.x, p.y - e);
            let mut n = Vec2::new(gx, gz);
            if n.length_squared() < 1e-10 {
                n = -Vec2::new(delta.x, delta.z);
            }
            let n = n.normalize_or_zero();
            if n == Vec2::ZERO {
                break;
            }
            p += n * (radius - d + 0.005);
        }
        // Never end up deeper inside than we started (e.g. squeezed between two solids).
        // Never end up deeper inside than we started (e.g. squeezed between
        // two solids, or against a bush and a friend at once): no jostling.
        let d_end = dist(p.x, p.y);
        if d_end < radius && d_end < d_start - 1e-4 && !ok(p.x, p.y, d_end) {
            p = Vec2::new(from.x, from.z);
        }
        // Met someone head on and got nowhere: step to the right of them.
        let step = Vec2::new(delta.x, delta.z);
        if !obs.bodies.is_empty() && step.length() > 1e-4 && (p - start).length() < step.length() * 0.2 {
            let right = Vec2::new(-step.y, step.x);
            let q = start + step * 0.5 + right * 0.85;
            if passable(q) && ok(q.x, q.y, dist(q.x, q.y)) {
                p = q;
            }
        }
    }
    Vec3::new(p.x, feet, p.y)
}

/// The top of the support under a column at (x, z): the ground, or the
/// highest surface of a solid between the ground and `top` (a floor, a stair,
/// a roof, a rock). A solid that fills the column at `top` is a wall there,
/// not a floor.
pub fn support(terrain: &Terrain, solids: &[Solid], x: f32, z: f32, top: f32) -> f32 {
    let g = terrain.floor(x, top, z);
    let mut best = g;
    for s in solids {
        let c = s.inst.center();
        let r = s.inst.radius();
        if (c.x - x) * (c.x - x) + (c.z - z) * (c.z - z) > r * r || c.y - r > top || c.y + r < best {
            continue;
        }
        if let Some(y) = column_top(s, x, z, top, best) {
            best = best.max(y);
        }
    }
    best
}

/// Marching down a solid's column from `top`, its first surface above `floor`.
fn column_top(s: &Solid, x: f32, z: f32, top: f32, floor: f32) -> Option<f32> {
    let mut y = top;
    if s.sdf(Vec3::new(x, y, z)) <= 0.0 {
        return None;
    }
    for _ in 0..40 {
        let d = s.sdf(Vec3::new(x, y, z));
        if d < 0.01 {
            return Some(y - d.max(0.0));
        }
        y -= d.max(0.01);
        if y < floor {
            return None;
        }
    }
    None
}

/// The support under a body's footprint: the ground under its centre, and
/// solids under its centre or four points round it (it can stand on the edge
/// of a stair).
pub fn support_under(terrain: &Terrain, solids: &[Solid], p: Vec3, radius: f32, top: f32) -> f32 {
    let mut best = support(terrain, solids, p.x, p.z, top);
    if solids.is_empty() {
        return best;
    }
    let r = radius * 0.6;
    for (dx, dz) in [(r, 0.0), (-r, 0.0), (0.0, r), (0.0, -r)] {
        let (x, z) = (p.x + dx, p.z + dz);
        for s in solids {
            let c = s.inst.center();
            let rr = s.inst.radius();
            if (c.x - x) * (c.x - x) + (c.z - z) * (c.z - z) > rr * rr || c.y - rr > top || c.y + rr < best {
                continue;
            }
            if let Some(y) = column_top(s, x, z, top, best) {
                best = best.max(y);
            }
        }
    }
    best
}

/// The nearest spot to `p` (spiralling out up to `max` metres) where a body of
/// `radius` fits on dry land, or anywhere clear in the water for one that
/// `swims` (kept at its own height there). Returns `p` if it already fits.
pub fn free_spot(terrain: &Terrain, obs: &Obstacles, p: Vec3, radius: f32, max: f32, swims: bool) -> Vec3 {
    let fits = |x: f32, z: f32| (swims || wadeable(terrain, x, z)) && obs.dist(terrain, x, z) >= radius + 0.15;
    if fits(p.x, p.z) {
        return p;
    }
    let mut k = 1;
    loop {
        let r = (k as f32).sqrt() * 0.6;
        if r > max {
            return p;
        }
        let a = k as f32 * 2.399_963;
        let (x, z) = (p.x + a.cos() * r, p.z + a.sin() * r);
        if fits(x, z) {
            let h = terrain.height(x, z);
            return Vec3::new(x, if swims && !wadeable(terrain, x, z) { p.y.max(h) } else { h }, z);
        }
        k += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lang::compile;
    use crate::render::GpuInst;
    use crate::world::TypeEntry;
    use std::sync::Arc;

    #[test]
    fn can_walk_out_of_a_solid_and_find_free_spot() {
        let ct = compile(
            "export const meta = { name: \"hut\", bounds: [3, 2, 3], tags: [] };\n\
             export function sdf(x, y, z, k) { return box(x, y, z, 3, 2, 3); }\n\
             export function color(x, y, z, k) { return rgb(200, 200, 200); }",
        )
        .unwrap();
        let ty = Arc::new(TypeEntry { id: 1, ct: Arc::new(ct), builtin: false, solid: true, bottom: -2.0, top: 2.0, sphere_cy: 0.0, sphere_r: 4.7 });
        let terrain = Terrain::new(1, crate::terrain::default_biomes());
        let c = Vec3::new(500.0, 0.0, 500.0);
        let inst = GpuInst { pos_scale: [c.x, terrain.height(c.x, c.z) + 1.0, c.z, 1.0], rot: [1.0, 0.0, ty.radius(), 1.0], ..Default::default() };
        let solids = vec![Solid::new(inst, ty)];
        let obs = Obstacles { solids: &solids, bodies: &[] };
        // Standing inside: walking forward must work until we are out.
        let mut p = Vec3::new(c.x + 0.5, terrain.height(c.x, c.z), c.z);
        for _ in 0..80 {
            p = move_body(&terrain, &obs, p, Vec3::new(0.0, 0.0, 0.1), Capsule::of(1.75, PLAYER_RADIUS));
            p.y = terrain.height(p.x, p.z);
        }
        assert!(p.z - c.z > 3.0 + PLAYER_RADIUS, "walked out of the hut: dz = {}", p.z - c.z);
        // And back towards it is blocked again.
        let before = p;
        for _ in 0..40 {
            p = move_body(&terrain, &obs, p, Vec3::new(0.0, 0.0, -0.1), Capsule::of(1.75, PLAYER_RADIUS));
            p.y = terrain.height(p.x, p.z);
        }
        assert!(p.z - c.z > 3.0, "blocked at the wall: {}", p.z - c.z);
        assert!(before.z > p.z, "walked back until the wall");
        let f = free_spot(&terrain, &obs, Vec3::new(c.x, 0.0, c.z), PLAYER_RADIUS, 20.0, false);
        assert!(obs.dist(&terrain, f.x, f.z) >= PLAYER_RADIUS);
    }

    #[test]
    fn cannot_walk_through_a_box_and_slides() {
        let ct = compile(
            "export const meta = { name: \"wall\", bounds: [3, 2, 0.5], tags: [] };\n\
             export function sdf(x, y, z, k) { return box(x, y, z, 3, 2, 0.5); }\n\
             export function color(x, y, z, k) { return rgb(200, 200, 200); }",
        )
        .unwrap();
        let ty = Arc::new(TypeEntry { id: 1, ct: Arc::new(ct), builtin: false, solid: true, bottom: -2.0, top: 2.0, sphere_cy: 0.0, sphere_r: 3.7 });
        let terrain = Terrain::new(1, crate::terrain::default_biomes());
        let base = Vec3::new(500.0, 0.0, 500.0);
        let g = terrain.height(base.x, base.z + 5.0);
        let inst = GpuInst {
            pos_scale: [base.x, g + 1.0, base.z + 5.0, 1.0],
            rot: [1.0, 0.0, ty.radius(), 1.0],
            ..Default::default()
        };
        let solids = vec![Solid::new(inst, ty)];
        let obs = Obstacles { solids: &solids, bodies: &[] };
        let mut p = Vec3::new(base.x, terrain.height(base.x, base.z), base.z);
        // Walk straight at the wall (+z), slightly diagonally.
        for _ in 0..120 {
            p = move_body(&terrain, &obs, p, Vec3::new(0.02, 0.0, 0.1), Capsule::of(1.75, PLAYER_RADIUS));
            p.y = terrain.height(p.x, p.z);
        }
        assert!(p.z - base.z < 4.5 - 0.3, "walked into the wall: z = {}", p.z - base.z);
        assert!(p.x - base.x > 2.0, "should have slid sideways along the wall: x = {}", p.x - base.x);
    }

    /// Walk `steps` steps of `dir`, standing on the ground after each.
    fn walk(terrain: &Terrain, from: Vec3, dir: Vec2, steps: usize) -> Vec3 {
        let obs = Obstacles { solids: &[], bodies: &[] };
        let mut p = from;
        for _ in 0..steps {
            p = move_body(terrain, &obs, p, Vec3::new(dir.x, 0.0, dir.y) * 0.05, Capsule::of(1.75, PLAYER_RADIUS));
            p.y = terrain.height(p.x, p.z);
        }
        p
    }

    #[test]
    fn rock_faces_stop_a_climber_but_not_a_descent() {
        let mut b = crate::terrain::default_biomes()[2].clone();
        (b.base, b.amp, b.rough, b.cliffs) = (10.0, 30.0, 0.5, 1.0);
        let terrain = Terrain::new(9, vec![b]);
        // Sheer faces: steeper than 70°.
        let mut faces = Vec::new();
        'scan: for j in 0..300 {
            for i in 0..300 {
                let (x, z) = (i as f32 * 3.0 - 450.0, j as f32 * 3.0 - 450.0);
                let n = terrain.normal(x, z);
                if n.y < 0.33 && terrain.height(x, z) > terrain.water(x, z) + 4.0 {
                    faces.push((Vec2::new(x, z), Vec2::new(n.x, n.z).normalize()));
                    if faces.len() == 20 {
                        break 'scan;
                    }
                }
            }
        }
        assert!(faces.len() >= 10, "found faces: {}", faces.len());
        let (mut held, mut went_down) = (0, 0);
        for (at, away) in &faces {
            // From 4 m out, straight at it.
            let s = *at + *away * 4.0;
            let start = Vec3::new(s.x, terrain.height(s.x, s.y), s.y);
            let end = walk(&terrain, start, -*away, 200);
            let top = terrain.height(at.x - away.x * 3.0, at.y - away.y * 3.0);
            held += (end.y < start.y + (top - start.y) * 0.5) as usize;
            // From above it, off the edge and down.
            let a = *at - *away * 2.5;
            let high = Vec3::new(a.x, terrain.height(a.x, a.y), a.y);
            let low = walk(&terrain, high, *away, 200);
            went_down += (low.y < high.y - 2.0) as usize;
        }
        assert!(held as f32 >= faces.len() as f32 * 0.9, "faces hold climbers back: {held} of {}", faces.len());
        assert_eq!(went_down, faces.len(), "and anyone goes down them");
    }

    #[test]
    fn a_swimmer_does_not_slip_under_a_wall_standing_in_the_water() {
        let ct = compile(
            "export const meta = { name: \"wall\", bounds: [3, 1.2, 0.3], tags: [] };\n\
             export function sdf(x, y, z, k) { return box(x, y, z, 3, 1.2, 0.3); }\n\
             export function color(x, y, z, k) { return rgb(200, 200, 200); }",
        )
        .unwrap();
        let ty = Arc::new(TypeEntry { id: 1, ct: Arc::new(ct), builtin: false, solid: true, bottom: -1.2, top: 1.2, sphere_cy: 0.0, sphere_r: 3.3 });
        let terrain = Terrain::new(1, crate::terrain::default_biomes());
        let sea = (0..400).map(|i| Vec2::new(i as f32 * 5.0 - 1000.0, -600.0)).find(|q| terrain.height(q.x, q.y) < terrain.water(q.x, q.y) - 4.0).expect("deep sea");
        let wl = terrain.water(sea.x, sea.y);
        // Its foot 15 cm under the water: under the reach of a swimmer's body, not its head.
        let inst = GpuInst { pos_scale: [sea.x, wl + 1.05, sea.y + 2.0, 1.0], rot: [1.0, 0.0, ty.radius(), 1.0], ..Default::default() };
        let solids = vec![Solid::new(inst, ty)];
        let obs = Obstacles { solids: &solids, bodies: &[] };
        let swan = Capsule { radius: 0.4, height: 1.1, step: 0.275, swims: true, needs_water: 0.0 };
        // Bobbing a little under: its head 20 cm down.
        let mut p = Vec3::new(sea.x, wl - 0.2 - swan.height, sea.y);
        for _ in 0..80 {
            p = move_body(&terrain, &obs, p, Vec3::new(0.0, 0.0, 0.05), swan);
        }
        assert!(p.z < sea.y + 2.0 - 0.3, "stopped at the wall: {:.2} m past where it stands", p.z - sea.y - 2.0);
    }

    #[test]
    fn anyone_climbs_out_of_a_dug_pit() {
        let terrain = Terrain::new(1, crate::terrain::default_biomes());
        let c = Vec2::new(500.0, 500.0);
        let floor = terrain.natural_height(c.x, c.y) - 2.0;
        {
            let mut cv = terrain.carve.write();
            cv.dug.push(crate::terrain::Hollow { c: [c.x, c.y], rot: [1.0, 0.0], half: [1.0, 1.0], floor, round: true, edge: crate::terrain::HOLLOW_EDGE });
            cv.reindex();
        }
        let start = Vec3::new(c.x, terrain.height(c.x, c.y), c.y);
        let end = walk(&terrain, start, Vec2::X, 80);
        assert!(end.x - c.x > 2.5, "walked out of the pit: {}", end.x - c.x);
    }
}

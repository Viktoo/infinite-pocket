//! Deterministic scatter (trees, rocks, bushes, grass…) per 64 m chunk, from
//! (seed, chunk). Never stored; story instances suppress scatter under them.
//!
//! Things gather the way they do in nature. Each kind grows in patches of its
//! own (`clump_of`, or a biome's `clump`): trees in groves with clearings
//! between, flowers and tall grass in fields, boulders in boulder fields,
//! with a thinner sprinkle between them. Within a stretch of land one kind
//! leads (a birch stand, then an oak wood), and things in the same patch share
//! k.a (a flower field's colour, a grove's tint). Sticks and mushrooms lie
//! where the trees are; boulders and stones gather at the foot of cliffs.

use super::{CHUNK, Solid, WorldSnapshot, chunk_of};
use crate::noise::{h2, hseq, hseqf, u2f, vnoise2};
use crate::render::GpuInst;
use crate::terrain::{Biome, Terrain, smoothstep};
use glam::Vec3;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

const CELL: f32 = 4.0;
const CELLS: i32 = (CHUNK / CELL) as i32;

#[derive(Clone)]
pub struct ScatterItem {
    pub inst: GpuInst,
    pub max_dist: f32,
    pub solid: bool,
    /// The 4 m scatter cell this item stands in (its identity).
    pub cell: (i32, i32),
}

pub fn max_dist_for(tags: &[String]) -> f32 {
    if tags.iter().any(|t| t == "grass") {
        24.0
    } else if tags.iter().any(|t| t == "small") {
        45.0
    } else if tags.iter().any(|t| ["bush", "flower", "tallgrass", "reed", "fern"].contains(&t.as_str())) {
        110.0
    } else {
        f32::MAX
    }
}


fn scale_range(tag: &str) -> (f32, f32) {
    match tag {
        "tree" | "pine" => (0.7, 1.3),
        "rock" => (0.5, 1.6),
        "grass" => (0.8, 1.3),
        "flower" => (0.8, 1.25),
        "tallgrass" => (0.85, 1.2),
        "stick" | "stone" | "mushroom" => (0.8, 1.2),
        _ => (0.75, 1.25),
    }
}

/// How a kind of thing gathers: how strongly it grows in patches (0 spread
/// evenly … 1 only in patches) and how wide its patches are (m). A biome's
/// `clump` overrides the strength.
pub fn clump_of(tag: &str) -> (f32, f32) {
    match tag {
        "tree" | "palm" => (0.6, 60.0),
        "pine" => (0.45, 80.0),
        "bush" => (0.5, 28.0),
        "flower" => (0.9, 24.0),
        "tallgrass" | "reed" | "reeds" | "fern" => (0.85, 22.0),
        "grass" => (0.45, 30.0),
        "rock" => (0.55, 36.0),
        "stone" => (0.3, 30.0),
        "mushroom" => (0.7, 14.0),
        // Lie where the trees are (see `density_at`).
        "stick" => (0.0, 30.0),
        _ => (0.5, 40.0),
    }
}

/// Tags that are trees (their patches are the woods litter lies in).
const WOODY: [&str; 3] = ["tree", "pine", "palm"];

/// The patch noise thresholds: where a patch's edge fades in (in noise
/// units), and how much denser than average a patch's heart is so that, on
/// average, a clumped kind is as common as its density says.
const PATCH_LO: f32 = 0.5;
const PATCH_HI: f32 = 0.6;
pub const PATCH_BOOST: f32 = 2.6;

/// A stable seed for a tag (the same across runs and machines).
fn tag_seed(seed: u32, tag: &str) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for b in tag.bytes() {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    crate::noise::pcg(h ^ seed)
}

/// How far into one of `tag`'s patches (x, z) is: 0 outside … 1 in its heart.
pub fn patch_depth(seed: u32, tag: &str, x: f32, z: f32) -> f32 {
    let size = clump_of(tag).1;
    let ts = tag_seed(seed, tag);
    let (u, v) = (x / size, z / size);
    // A second, finer layer frays the edges so patches aren't blobs.
    let n = 0.7 * vnoise2(u, v, ts) + 0.3 * vnoise2(u * 2.7 + 5.3, v * 2.7 + 1.7, ts.wrapping_add(1));
    smoothstep(PATCH_LO, PATCH_HI, n)
}

/// Which stand (0 … 1) (x, z) belongs to for `tag`: stretches of land a
/// little wider than its patches, with wandering borders. Things in one stand
/// share their leading type and their k.a.
pub fn stand_of(seed: u32, tag: &str, x: f32, z: f32) -> f32 {
    let ss = clump_of(tag).1 * 1.2;
    let ts = tag_seed(seed, tag);
    let wx = x + ss * 0.6 * (vnoise2(x / ss * 1.7, z / ss * 1.7, ts.wrapping_add(2)) - 0.5);
    let wz = z + ss * 0.6 * (vnoise2(x / ss * 1.7, z / ss * 1.7, ts.wrapping_add(3)) - 0.5);
    u2f(h2((wx / ss).floor() as i32, (wz / ss).floor() as i32, ts.wrapping_add(4)))
}

/// A biome's patch strength for a tag (its own `clump`, else the kind's).
fn strength(biome: &Biome, tag: &str) -> f32 {
    biome.clump.get(tag).copied().unwrap_or_else(|| clump_of(tag).0).clamp(0.0, 1.0)
}

/// Items per 100 m² of `tag` at (x, z) in `biome` (whose plain density is
/// `dens`), and how deep in its patch the point is. `foot`: how much the
/// point lies at the foot of a cliff (`Terrain::height_and_foot`).
pub fn density_at(seed: u32, biome: &Biome, tag: &str, dens: f32, x: f32, z: f32, foot: f32) -> (f32, f32) {
    let c = strength(biome, tag);
    let depth = if c > 0.0 { patch_depth(seed, tag, x, z) } else { 0.0 };
    let mut d = dens * ((1.0 - c) + c * depth * PATCH_BOOST);
    match tag {
        "stick" | "mushroom" => {
            // Fallen from trees: in the woods, hardly any in the open.
            let wood = WOODY
                .iter()
                .filter(|w| biome.scatter.get(**w).is_some_and(|d| *d > 0.0))
                .map(|w| strength(biome, w) * patch_depth(seed, w, x, z) + (1.0 - strength(biome, w)))
                .fold(None, |a: Option<f32>, b| Some(a.map_or(b, |a| a.max(b))));
            if let Some(wood) = wood {
                d *= 0.2 + 1.6 * wood;
            }
        }
        "rock" | "stone" => {
            // Fallen from cliffs: heaped at their feet.
            d *= 1.0 + foot * if tag == "rock" { 6.0 } else { 4.0 };
        }
        _ => {}
    }
    (d, depth)
}

/// Ground cover: small growth that fills a field more than one to a cell.
pub fn is_cover(tag: &str) -> bool {
    matches!(tag, "flower" | "tallgrass" | "grass" | "reed" | "reeds" | "fern")
}

/// Extra cover in a cell (in its other 2 m quarters) at most, and how far
/// the extras show: they thicken a field around the viewer, and the cell's
/// own item carries the field into the distance.
const COVER_EXTRA: u32 = 3;
const COVER_EXTRA_DIST: f32 = 40.0;

/// Cells of the extra cover live in their own range, far from any real cell
/// (`SUB + 4·cell + quarter` on each axis), so their identities (taken,
/// burnt, saved with the world) never meet a cell's.
const SUB: i32 = 1 << 30;

/// The identity of quarter `q` (1 … 3) of a cell.
pub fn sub_cell(cell: (i32, i32), q: u32) -> (i32, i32) {
    (SUB + cell.0 * 4 + (q % 2) as i32, SUB + cell.1 * 4 + (q / 2) as i32)
}

/// The cell an identity belongs to (itself, or the cell a quarter is in).
pub fn parent_cell(id: (i32, i32)) -> (i32, i32) {
    if id.0 >= SUB / 2 {
        ((id.0 - SUB).div_euclid(4), (id.1 - SUB).div_euclid(4))
    } else {
        id
    }
}

/// The middle (x, z) of a cell or a cell's quarter, in metres.
pub fn cell_centre(id: (i32, i32)) -> (f32, f32) {
    if id.0 >= SUB / 2 {
        let axis = |v: i32| ((v - SUB).div_euclid(4) as f32 + 0.5 * (v - SUB).rem_euclid(4) as f32 + 0.25) * CELL;
        (axis(id.0), axis(id.1))
    } else {
        ((id.0 as f32 + 0.5) * CELL, (id.1 as f32 + 0.5) * CELL)
    }
}

/// The seed scatter draws from, for a terrain.
pub fn scatter_seed(t: &Terrain) -> u32 {
    t.seed ^ 0x5CA7_7E12
}

/// Trees per 100 m² at (x, z) in `biome`, patches and all.
pub fn trees_at(t: &Terrain, biome: &Biome, x: f32, z: f32) -> f32 {
    biome.scatter.iter().filter(|(k, d)| **d > 0.0 && WOODY.iter().any(|w| k.contains(w))).map(|(k, d)| density_at(scatter_seed(t), biome, k, *d, x, z, 0.0).0).sum()
}

/// Generate one chunk's scatter.
pub fn generate(snap: &WorldSnapshot, cx: i32, cz: i32) -> Vec<ScatterItem> {
    let t = &snap.terrain;
    let seed = scatter_seed(t);
    let mut out = Vec::new();
    if snap.scatter.is_empty() {
        return out;
    }
    // Story footprints in and around this chunk suppress scatter.
    let blockers: Vec<(Vec3, f32)> = snap
        .near_chunk((cx, cz))
        .filter_map(|p| snap.type_of(p.type_id).map(|ty| (p.pos, ty.ct.meta.bounds[0].max(ty.ct.meta.bounds[2]) * p.scale + 1.5)))
        .collect();
    let cliffy = t.biomes.iter().any(|b| b.cliffs > 0.0);
    // What of each tag grows where: on dry ground, at the water's edge, under
    // it or floating on it (each type's `meta.grows`).
    let grows: HashMap<&str, Vec<(u32, crate::lang::ir::Grows)>> =
        snap.scatter.iter().map(|(tag, ids)| (tag.as_str(), ids.iter().filter_map(|id| snap.type_of(*id).map(|ty| (*id, ty.ct.meta.grows().clone()))).collect())).collect();
    let water_over = |x: f32, z: f32, h: f32| t.water(x, z) - h;
    // One thing of `tag` at (x, z), named `id` (its hashes come from it too),
    // on ground `h` high (found here when not given). `extra`: cover filling
    // a quarter, by the ground of its cell's own item, which found the slope
    // gentle enough two metres away; a quarter far above or below that is
    // on a rock face, not in the field.
    let place = |id: (i32, i32), x: f32, z: f32, tag: &str, biome: &Biome, depth: f32, extra: Option<f32>, h: Option<f32>| -> Option<ScatterItem> {
        let (gx, gz) = id;
        let h = h.unwrap_or_else(|| t.height(x, z));
        // Only the kinds that grow in this much water (or this far above it).
        let wd = water_over(x, z, h);
        let cands: Vec<&(u32, crate::lang::ir::Grows)> = grows.get(tag)?.iter().filter(|(_, g)| g.fits(wd)).collect();
        if cands.is_empty() {
            return None;
        }
        // Most things in a stand are its leading kind; a few are not.
        let stand = stand_of(seed, tag, x, z);
        let lead = cands.len() > 1 && hseqf(seed, gx, gz, 9) < 0.75;
        let pick = if lead { ((stand * cands.len() as f32) as usize).min(cands.len() - 1) } else { (hseq(seed, gx, gz, 5) as usize) % cands.len() };
        let (tid, g) = cands[pick];
        let ty = snap.type_of(*tid)?;
        let big = tag != "grass";
        match extra {
            Some(by) if (h - by).abs() > 1.2 => return None,
            None if big && t.normal(x, z).y < 0.78 => return None,
            _ => {}
        }
        let pos2 = Vec3::new(x, 0.0, z);
        if (pos2 - Vec3::new(snap.spawn.x, 0.0, snap.spawn.z)).length() < 4.0 {
            return None;
        }
        if blockers.iter().any(|(c, r)| (Vec3::new(c.x, 0.0, c.z) - pos2).length() < *r) {
            return None;
        }
        // Nothing grows on a road, a street or a path (grass tufts only up
        // to its edge).
        if t.on_road(x, z, if big { 0.8 } else { 0.0 }) {
            return None;
        }
        let (s0, s1) = scale_range(tag);
        let mut scale = s0 + (s1 - s0) * hseqf(seed, gx, gz, 6);
        // Trees grow tallest in the heart of a wood, smaller at its edge.
        if WOODY.contains(&tag) && strength(biome, tag) > 0.2 {
            scale *= 0.88 + 0.22 * depth;
        }
        let rot = hseqf(seed, gx, gz, 7) * std::f32::consts::TAU;
        let sink = if ty.has_tag("rock") {
            0.25
        } else if ty.has_tag("small") {
            0.0
        } else {
            0.08
        };
        // Floating, it rides on the water; else it stands on the ground (the bed).
        let y = if g.surface { h + wd - ty.bottom * scale - 0.02 } else { h - ty.bottom * scale - sink * scale };
        let pseed = (u2f(hseq(seed, gx, gz, 8)) * 97.0).floor();
        // k.a: the stand's own look, shared across it; k.b: how deep in its patch.
        let k = [pseed, scale, stand, depth, 0.5, 0.5, 0.5, 0.5];
        let inst = GpuInst {
            pos_scale: [x, y, z, scale],
            rot: [rot.cos(), rot.sin(), ty.radius() * scale, 1.0],
            k0: [k[0], k[1], k[2], k[3]],
            k1: [k[4], k[5], k[6], k[7]],
            info: ty.gpu_info(),
            ..Default::default()
        };
        let far = max_dist_for(&ty.ct.meta.tags);
        Some(ScatterItem { inst, max_dist: if extra.is_some() { far.min(COVER_EXTRA_DIST) } else { far }, solid: ty.solid, cell: id })
    };
    caves(snap, cx, cz, &grows, &mut out);
    for j in 0..CELLS {
        for i in 0..CELLS {
            let gx = cx * CELLS + i;
            let gz = cz * CELLS + j;
            let x = (gx as f32 + 0.1 + 0.8 * hseqf(seed, gx, gz, 1)) * CELL;
            let z = (gz as f32 + 0.1 + 0.8 * hseqf(seed, gx, gz, 2)) * CELL;
            let w = t.weights(x, z);
            // Pick a biome in proportion to its weight, so borders blend.
            let mut r = hseqf(seed, gx, gz, 3);
            let mut bi = 0;
            for (k, wk) in w.iter().enumerate().take(t.biomes.len()) {
                if r < *wk {
                    bi = k;
                    break;
                }
                r -= *wk;
                bi = k;
            }
            let biome = &t.biomes[bi];
            let roll = hseqf(seed, gx, gz, 4);
            let mut acc = 0.0;
            let mut chosen: Option<(&String, f32)> = None;
            let fallen = cliffy && ["rock", "stone"].iter().any(|k| biome.scatter.get(*k).is_some_and(|d| *d > 0.0));
            let (h, foot) = if fallen { let (h, f) = t.height_and_foot(x, z); (h, f) } else { (t.height(x, z), 0.0) };
            let h = Some(h);
            // Only what grows here, wet or dry, shares the cell.
            let wd = water_over(x, z, h.unwrap_or(0.0));
            let here: Vec<(&String, f32, f32)> = biome
                .scatter
                .iter()
                .filter(|(tag, d)| **d > 0.0 && grows.get(tag.as_str()).is_some_and(|c| c.iter().any(|(_, g)| g.fits(wd))))
                .map(|(tag, dens)| {
                    let (d, depth) = density_at(seed, biome, tag, *dens, x, z, foot);
                    (tag, d, depth)
                })
                .collect();
            let total: f32 = here.iter().map(|(_, d, _)| d * CELL * CELL / 100.0).sum();
            let norm = total.max(1.0);
            for (tag, dens, depth) in &here {
                acc += dens * CELL * CELL / 100.0 / norm;
                if roll < acc {
                    chosen = Some((tag, *depth));
                    break;
                }
            }
            let Some((tag, depth)) = chosen else { continue };
            let cover = is_cover(tag);
            let Some(item) = place((gx, gz), x, z, tag, biome, depth, None, h) else { continue };
            out.push(item);
            // In the thick of a field, cover fills the cell's other quarters.
            if let (true, Some(ground)) = (cover, h) {
                let thick = strength(biome, tag) * depth;
                for q in 1..=COVER_EXTRA {
                    let id = sub_cell((gx, gz), q);
                    if hseqf(seed, id.0, id.1, 10) >= thick * 0.9 {
                        continue;
                    }
                    let sx = (gx as f32 + 0.5 * (q % 2) as f32 + 0.05 + 0.4 * hseqf(seed, id.0, id.1, 1)) * CELL;
                    let sz = (gz as f32 + 0.5 * (q / 2) as f32 + 0.05 + 0.4 * hseqf(seed, id.0, id.1, 2)) * CELL;
                    if let Some(extra) = place(id, sx, sz, tag, biome, depth, Some(ground), None) {
                        out.push(extra);
                    }
                }
            }
        }
    }
    out
}

/// What grows in caves (`meta.grows.at: "cave"`): on their floors, out of
/// the light, as thick as the biome over them says for its tag. Not in the
/// rooms of shapes set into the ground. Each such thing's identity is its
/// cell's own quarter 0 (see `sub_cell`).
fn caves(snap: &WorldSnapshot, cx: i32, cz: i32, grows: &HashMap<&str, Vec<(u32, crate::lang::ir::Grows)>>, out: &mut Vec<ScatterItem>) {
    let t = &snap.terrain;
    let seed = scatter_seed(t) ^ 0xCA7E;
    let cave_kinds: HashMap<&str, Vec<u32>> = grows
        .iter()
        .filter_map(|(tag, c)| {
            let ids: Vec<u32> = c.iter().filter(|(_, g)| g.fits_cave(-1.0)).map(|(id, _)| *id).collect();
            (!ids.is_empty()).then_some((*tag, ids))
        })
        .collect();
    if cave_kinds.is_empty() {
        return;
    }
    let (x0, z0) = (cx as f32 * CHUNK, cz as f32 * CHUNK);
    // A cave reaching into this chunk is seen at one of its 16 m tiles.
    if !(0..4).any(|j| (0..4).any(|i| t.caves_near(x0 + 8.0 + 16.0 * i as f32, z0 + 8.0 + 16.0 * j as f32))) {
        return;
    }
    for j in 0..CELLS {
        for i in 0..CELLS {
            let (gx, gz) = (cx * CELLS + i, cz * CELLS + j);
            let x = (gx as f32 + 0.1 + 0.8 * hseqf(seed, gx, gz, 1)) * CELL;
            let z = (gz as f32 + 0.1 + 0.8 * hseqf(seed, gx, gz, 2)) * CELL;
            if !t.caves_near(x, z) {
                continue;
            }
            let Some(floor) = t.cave_floor_under(x, z) else { continue };
            let at = Vec3::new(x, floor + 0.3, z);
            if t.carve.read().ground.caves_at(x, z).any(|c| c.kind == crate::inground::KIND && c.dist(at) < 0.0) {
                continue;
            }
            // The biome over it, by weight.
            let w = t.weights(x, z);
            let bi = (0..t.biomes.len()).max_by(|a, b| w[*a].total_cmp(&w[*b])).unwrap_or(0);
            let biome = &t.biomes[bi];
            let here: Vec<(&str, f32)> = biome.scatter.iter().filter_map(|(tag, d)| cave_kinds.get(tag.as_str()).map(|_| (tag.as_str(), d * CELL * CELL / 100.0))).filter(|(_, d)| *d > 0.0).collect();
            let roll = hseqf(seed, gx, gz, 4);
            let mut acc = 0.0;
            let Some(tag) = here.iter().find(|(_, d)| {
                acc += d.min(1.0);
                roll < acc
            }).map(|(tag, _)| *tag) else { continue };
            let ids = &cave_kinds[tag];
            let Some(ty) = snap.type_of(ids[(hseq(seed, gx, gz, 5) as usize) % ids.len()]) else { continue };
            let (s0, s1) = scale_range(tag);
            let scale = s0 + (s1 - s0) * hseqf(seed, gx, gz, 6);
            // Room for it under the roof.
            if t.void(Vec3::new(x, floor + (ty.top - ty.bottom) * scale + 0.1, z)) > 0.0 {
                continue;
            }
            let rot = hseqf(seed, gx, gz, 7) * std::f32::consts::TAU;
            let k = [(u2f(hseq(seed, gx, gz, 8)) * 97.0).floor(), scale, hseqf(seed, gx, gz, 9), 1.0, 0.5, 0.5, 0.5, 0.5];
            let inst = GpuInst {
                pos_scale: [x, floor - ty.bottom * scale - 0.03 * scale, z, scale],
                rot: [rot.cos(), rot.sin(), ty.radius() * scale, 1.0],
                k0: [k[0], k[1], k[2], k[3]],
                k1: [k[4], k[5], k[6], k[7]],
                info: ty.gpu_info(),
                ..Default::default()
            };
            out.push(ScatterItem { inst, max_dist: max_dist_for(&ty.ct.meta.tags), solid: ty.solid, cell: sub_cell((gx, gz), 0) });
        }
    }
}

/// How the live layer changes the static one: placed objects and scatter
/// items that now live as things (hidden here), and per-cell looks
/// (charred, wet) or cells whose small item burnt away.
#[derive(Default, Clone)]
pub struct Overlay {
    pub hidden: std::collections::HashSet<i64>,
    pub taken: std::collections::HashSet<(i32, i32)>,
    pub cell_fx: HashMap<(i32, i32), [f32; 4]>,
    pub cell_gone: std::collections::HashSet<(i32, i32)>,
    pub version: u64,
    /// How lit the land's lamps are (0 by day … 1 at night).
    pub lamps: f32,
    /// Placed objects being worked on, and how bright they pulse now.
    pub pulse: HashMap<i64, f32>,
}

impl Overlay {
    /// Whether a scatter item is still there (not taken or burnt away).
    pub fn shows(&self, cell: (i32, i32)) -> bool {
        !self.taken.contains(&cell) && !self.cell_gone.contains(&cell)
    }
}

/// Per-chunk scatter cache, invalidated per chunk when its inputs change.
#[derive(Default)]
pub struct ScatterCache {
    /// Per chunk: its signature, its items, and the box (min x, min z, max
    /// x, max z) its solid items' bounding spheres reach.
    chunks: HashMap<(i32, i32), (u64, Arc<Vec<ScatterItem>>, [f32; 4])>,
    pub overlay: Overlay,
}

fn signature(snap: &WorldSnapshot, c: (i32, i32)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    snap.scatter_epoch.hash(&mut h);
    // Ground dug in it: what stood there settles into the hollow.
    let (x0, z0) = (c.0 as f32 * CHUNK, c.1 as f32 * CHUNK);
    snap.terrain.carve_sig(x0, z0, x0 + CHUNK, z0 + CHUNK).hash(&mut h);
    for p in snap.near_chunk(c) {
        p.id.hash(&mut h);
    }
    h.finish()
}

impl ScatterCache {
    pub fn get(&mut self, snap: &WorldSnapshot, c: (i32, i32)) -> Arc<Vec<ScatterItem>> {
        self.get_with_reach(snap, c).0
    }

    /// A chunk's items and the box its solid items reach.
    fn get_with_reach(&mut self, snap: &WorldSnapshot, c: (i32, i32)) -> (Arc<Vec<ScatterItem>>, [f32; 4]) {
        let sig = signature(snap, c);
        if let Some((s, v, reach)) = self.chunks.get(&c) {
            if *s == sig {
                return (v.clone(), *reach);
            }
        }
        let v = Arc::new(generate(snap, c.0, c.1));
        let mut reach = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
        for it in v.iter().filter(|it| it.solid) {
            let (m, r) = (it.inst.center(), it.inst.radius());
            reach = [reach[0].min(m.x - r), reach[1].min(m.z - r), reach[2].max(m.x + r), reach[3].max(m.z + r)];
        }
        self.chunks.insert(c, (sig, v.clone(), reach));
        (v, reach)
    }

    /// Drop chunks far from the player.
    pub fn trim(&mut self, center: Vec3, keep: f32) {
        let cc = chunk_of(center.x, center.z);
        let r = (keep / CHUNK).ceil() as i32 + 1;
        self.chunks.retain(|k, _| (k.0 - cc.0).abs() <= r && (k.1 - cc.1).abs() <= r);
    }

    /// The scatter item standing in a cell (or a cell's quarter), if any
    /// (ignores the overlay).
    pub fn item_at(&mut self, snap: &WorldSnapshot, cell: (i32, i32)) -> Option<ScatterItem> {
        let home = parent_cell(cell);
        let c = (home.0.div_euclid(CELLS), home.1.div_euclid(CELLS));
        self.get(snap, c).iter().find(|it| it.cell == cell).cloned()
    }

    /// Scatter items (shown ones) whose position is within `radius` of `p`.
    pub fn items_near(&mut self, snap: &WorldSnapshot, p: Vec3, radius: f32) -> Vec<ScatterItem> {
        let mut out = Vec::new();
        // Only the chunks the circle touches (usually one).
        let lo = chunk_of(p.x - radius, p.z - radius);
        let hi = chunk_of(p.x + radius, p.z + radius);
        for cz in lo.1..=hi.1 {
            for cx in lo.0..=hi.0 {
                for it in self.get(snap, (cx, cz)).iter() {
                    let d = it.inst.pos() - p;
                    if d.x * d.x + d.z * d.z <= radius * radius && self.overlay.shows(it.cell) {
                        out.push(it.clone());
                    }
                }
            }
        }
        out
    }

    /// Solid scatter + story instances whose bounds come within `radius` of `p`.
    pub fn solids_near(&mut self, snap: &WorldSnapshot, p: Vec3, radius: f32) -> Vec<Solid> {
        let mut out = Vec::new();
        let c = chunk_of(p.x, p.z);
        for dz in -1..=1 {
            for dx in -1..=1 {
                let cc = (c.0 + dx, c.1 + dz);
                let (items, reach) = self.get_with_reach(snap, cc);
                // Most of the time no solid in the chunk comes near.
                let (gx, gz) = ((reach[0] - p.x).max(p.x - reach[2]).max(0.0), (reach[1] - p.z).max(p.z - reach[3]).max(0.0));
                for it in items.iter().filter(|_| gx * gx + gz * gz <= radius * radius) {
                    if !it.solid || (it.inst.center() - p).length() - it.inst.radius() > radius || !self.overlay.shows(it.cell) {
                        continue;
                    }
                    if let Some(ty) = snap.type_of(it.inst.info[0]) {
                        out.push(Solid::new(it.inst, ty.clone()));
                    }
                }
                if let Some(v) = snap.by_chunk.get(&cc) {
                    for &i in v {
                        let pl = &snap.instances[i];
                        let Some(ty) = snap.type_of(pl.type_id) else { continue };
                        if !ty.solid || self.overlay.hidden.contains(&pl.id) {
                            continue;
                        }
                        let gi = pl.gpu(ty, 1.0);
                        if (gi.center() - p).length() - gi.radius() > radius {
                            continue;
                        }
                        out.push(Solid::new(gi, ty.clone()));
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Over a wide stretch, a clumped kind is about as common as its density says.
    #[test]
    fn patches_keep_the_density() {
        for tag in ["tree", "pine", "flower", "tallgrass", "rock", "bush", "cactus"] {
            let (mut sum, mut n) = (0.0f64, 0);
            for j in 0..400 {
                for i in 0..400 {
                    sum += patch_depth(77, tag, i as f32 * 9.7 - 2000.0, j as f32 * 9.7 - 2000.0) as f64;
                    n += 1;
                }
            }
            let mean = (sum / n as f64) as f32 * PATCH_BOOST;
            eprintln!("{tag}: mean density × {mean:.3}");
            assert!((0.85..1.15).contains(&mean), "{tag}: {mean}");
        }
    }

    /// The same density, clumped, comes in patches: some 10 m squares thick
    /// with it, others bare; spread evenly, every square is alike.
    #[test]
    fn clumped_kinds_come_in_patches() {
        let t = Terrain::new(5, crate::terrain::default_biomes());
        let mut b = t.biomes[0].clone();
        let spread = |b: &Biome| -> (f32, f32) {
            let v: Vec<f32> = (0..2500).map(|k| density_at(5, b, "flower", 1.0, (k % 50) as f32 * 10.0, (k / 50) as f32 * 10.0, 0.0).0).collect();
            let mean = v.iter().sum::<f32>() / v.len() as f32;
            let bare = v.iter().filter(|d| **d < 0.2 * mean).count() as f32 / v.len() as f32;
            let sd = (v.iter().map(|d| (d - mean).powi(2)).sum::<f32>() / v.len() as f32).sqrt();
            (sd / mean.max(1e-6), bare)
        };
        let (cv, bare) = spread(&b);
        assert!(cv > 1.0 && bare > 0.4, "fields with bare ground between: cv {cv}, bare {bare}");
        b.clump.insert("flower".into(), 0.0);
        let (cv, bare) = spread(&b);
        assert!(cv < 1e-3 && bare == 0.0, "spread evenly: cv {cv}, bare {bare}");
    }

    /// Neighbours mostly share a stand (one kind leads there); far apart they don't.
    #[test]
    fn stands_hold_together() {
        let (mut near, mut far) = (0, 0);
        for k in 0..2000 {
            let (x, z) = (k as f32 * 31.7, (k * 7 % 997) as f32 * 13.1);
            near += (stand_of(3, "tree", x, z) == stand_of(3, "tree", x + 4.0, z + 3.0)) as usize;
            far += (stand_of(3, "tree", x, z) == stand_of(3, "tree", x + 900.0, z - 700.0)) as usize;
        }
        assert!(near > 1700, "neighbours share a stand: {near} of 2000");
        assert!(far < 100, "far apart they don't: {far} of 2000");
    }

    #[test]
    fn quarters_have_their_own_names() {
        for cell in [(0, 0), (-1, -1), (37, -12), (-40_000, 90_000)] {
            let (cx, cz) = cell_centre(cell);
            assert_eq!(((cx / CELL).floor() as i32, (cz / CELL).floor() as i32), cell);
            assert_eq!(parent_cell(cell), cell);
            let mut seen = std::collections::HashSet::new();
            for q in 1..=COVER_EXTRA {
                let id = sub_cell(cell, q);
                assert!(seen.insert(id), "quarters differ");
                assert_ne!(id, cell);
                assert_eq!(parent_cell(id), cell, "{id:?} is in {cell:?}");
                // Its middle lies in its own quarter of the cell.
                let (x, z) = cell_centre(id);
                let (fx, fz) = (x / CELL - cell.0 as f32, z / CELL - cell.1 as f32);
                assert!((0.0..1.0).contains(&fx) && (0.0..1.0).contains(&fz), "{fx}, {fz}");
                assert_eq!(((fx * 2.0) as u32, (fz * 2.0) as u32), (q % 2, q / 2));
            }
        }
    }
}

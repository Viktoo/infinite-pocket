//! The world as the main thread sees it: an immutable snapshot of one world
//! version (types + pipeline + story instances + characters + look), plus
//! procedural scatter derived from it.

pub mod characters;
pub mod collide;
pub mod cull;
pub mod describe;
pub mod scatter;
pub mod species;
pub mod weather;

use crate::lang::CompiledType;
use crate::render::GpuInst;
use crate::render::gpu::ScenePipeline;
use crate::terrain::{Biome, Palette, Terrain};
use glam::Vec3;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

pub const CHUNK: f32 = 64.0;
pub const REGION_CHUNKS: i32 = 4;
pub const REGION: f32 = CHUNK * REGION_CHUNKS as f32;

pub fn chunk_of(x: f32, z: f32) -> (i32, i32) {
    ((x / CHUNK).floor() as i32, (z / CHUNK).floor() as i32)
}

pub fn region_of(x: f32, z: f32) -> (i32, i32) {
    ((x / REGION).floor() as i32, (z / REGION).floor() as i32)
}

pub fn region_center(r: (i32, i32)) -> Vec3 {
    Vec3::new((r.0 as f32 + 0.5) * REGION, 0.0, (r.1 as f32 + 0.5) * REGION)
}

/// Universe-wide look: palette and biome table. Produced at `pocket new`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Look {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub palette: Palette,
    #[serde(default = "crate::terrain::default_biomes")]
    pub biomes: Vec<Biome>,
    /// The whole land, written at genesis from the player's prompt: what
    /// every region and request is told the universe is. Empty in older
    /// worlds, which keep using the prompt itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub land: String,
    /// The prompt's own situation, where the traveler begins: only the
    /// starting region is planned around it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub start: String,
    /// The world's weather kinds, written at genesis (empty: mild, nothing falls).
    #[serde(default, skip_serializing_if = "weather::Climate::is_default")]
    pub climate: weather::Climate,
    /// How many rivers run through the land, 0 (none, a desert) … 1 (a
    /// land of streams). Older worlds, saved without it, get some.
    #[serde(default = "d_rivers")]
    pub rivers: f32,
    /// How many caves the hills hold, 0 … 1 (older worlds: a few).
    #[serde(default = "d_caves")]
    pub caves: f32,
    /// How hard things fall, m/s² (Earth 9.81, the Moon 1.62).
    #[serde(default = "d_gravity")]
    pub gravity: f32,
    /// How much breathable air there is out in the open, 0 (none: the
    /// Moon, deep space) … 1 (Earth's). Fire needs it, sound carries in
    /// it, and bodies that breathe suffocate without it.
    #[serde(default = "d_air")]
    pub air: f32,
    /// The sea's level (m, 0 in most lands), or none (null): a land with no
    /// sea, where low ground is only low ground (a cave world, a desert).
    #[serde(default = "d_sea", deserialize_with = "number_or_none")]
    pub sea: Option<f32>,
    /// How far under the land's own surface the ground water lies (m), if
    /// any: ground dug deeper than it fills with water up to it (0.5 a
    /// marsh, 3 a meadow, 30 a desert; none: holes stay dry).
    #[serde(default, skip_serializing_if = "Option::is_none", deserialize_with = "number_or_none")]
    pub groundwater: Option<f32>,
    /// The land's large shape, where the world gives one: coasts, bays,
    /// ranges at their real places (see `landforms`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub landforms: Vec<crate::landforms::Landform>,
}

fn d_gravity() -> f32 {
    crate::sim::physics::GRAVITY
}

/// A number, or none for anything else (null, "none"), so one odd word
/// doesn't lose the whole look.
fn number_or_none<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f32>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_f64().map(|v| v as f32))
}

fn d_sea() -> Option<f32> {
    Some(crate::terrain::WATER_LEVEL)
}

fn d_air() -> f32 {
    1.0
}

fn d_caves() -> f32 {
    0.3
}

fn d_rivers() -> f32 {
    0.6
}

impl Look {
    /// Keep what genesis wrote in bounds.
    pub fn sanitize(&mut self) {
        self.gravity = if self.gravity.is_finite() { self.gravity.clamp(0.5, 30.0) } else { d_gravity() };
        self.air = if self.air.is_finite() { self.air.clamp(0.0, 1.0) } else { d_air() };
        self.sea = self.sea.map(|s| if s.is_finite() { s.clamp(-50.0, 50.0) } else { crate::terrain::WATER_LEVEL });
        self.groundwater = self.groundwater.filter(|d| d.is_finite()).map(|d| d.clamp(0.2, 200.0));
        crate::landforms::sanitize(&mut self.landforms);
    }
}

impl Default for Look {
    fn default() -> Self {
        Look { name: String::new(), palette: Palette::default(), biomes: crate::terrain::default_biomes(), land: String::new(), start: String::new(), climate: Default::default(), rivers: 0.0, caves: 0.0, gravity: d_gravity(), air: d_air(), sea: d_sea(), groundwater: None, landforms: Vec::new() }
    }
}

/// A validated, active object type.
#[derive(Debug)]
pub struct TypeEntry {
    pub id: u32,
    pub ct: Arc<CompiledType>,
    pub builtin: bool,
    pub solid: bool,
    /// Lowest solid point in local space (for ground placement).
    pub bottom: f32,
    /// Highest solid point in local space (as probed, at rest).
    pub top: f32,
    /// Bounding sphere: centre height (local) and radius (unscaled).
    pub sphere_cy: f32,
    pub sphere_r: f32,
}

impl TypeEntry {
    pub fn name(&self) -> &str {
        &self.ct.meta.name
    }
    pub fn has_tag(&self, t: &str) -> bool {
        self.ct.meta.tags.iter().any(|x| x == t)
    }
    pub fn radius(&self) -> f32 {
        self.sphere_r
    }
    /// Growth a body pushes through low down (a bush, flowers): only what
    /// stands above half a metre gets in the way.
    pub fn is_growth(&self) -> bool {
        ["bush", "flower", "grass", "shrub", "fern", "reeds"].iter().any(|t| self.has_tag(t))
    }
    /// The light it gives (its `light` property as written), 0 for none.
    pub fn light(&self) -> f32 {
        self.ct.meta.props.iter().find(|(k, _)| k == "light").map(|(_, v)| *v).unwrap_or(0.0)
    }

    /// Shader `info`: type id, sphere centre height, box half-extents x/z (f32 bits).
    pub fn gpu_info(&self) -> [u32; 4] {
        let b = self.ct.meta.bounds;
        [self.id, self.sphere_cy.to_bits(), b[0].to_bits(), b[2].to_bits()]
    }

    /// Tight bounding sphere from the bounds box and the probed vertical extent.
    pub fn sphere(bounds: [f32; 3], bottom: f32, top: f32) -> (f32, f32) {
        let b = bounds;
        let spacing = (b[1] * 1.5 + 0.3) * 2.0 / 12.0;
        let lo = (bottom - spacing).max(-b[1]);
        let hi = (top + spacing).min(b[1]).max(lo + 0.01);
        let half = (hi - lo) * 0.5;
        ((lo + hi) * 0.5, (b[0] * b[0] + b[2] * b[2] + half * half).sqrt() + 0.02)
    }
}

/// The types (CPU) and compiled pipeline (GPU) of one world version.
#[derive(Debug, Default)]
pub struct SceneTypes {
    pub types: HashMap<u32, Arc<TypeEntry>>,
    pub pipeline: Option<Arc<ScenePipeline>>,
}

/// A stored, placed instance (story layer).
#[derive(Clone, Debug)]
pub struct Placed {
    pub id: i64,
    pub type_id: u32,
    pub pos: Vec3,
    pub rot_y: f32,
    pub scale: f32,
    pub params: [f32; 8],
    pub version: i64,
}

impl Placed {
    pub fn gpu(&self, ty: &TypeEntry, fade: f32) -> GpuInst {
        let mut k = self.params;
        k[1] = self.scale;
        GpuInst {
            pos_scale: [self.pos.x, self.pos.y, self.pos.z, self.scale],
            rot: [self.rot_y.cos(), self.rot_y.sin(), ty.sphere_r * self.scale, fade],
            k0: [k[0], k[1], k[2], k[3]],
            k1: [k[4], k[5], k[6], k[7]],
            info: ty.gpu_info(),
            ..Default::default()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Persona {
    pub name: String,
    #[serde(default)]
    pub age: u32,
    #[serde(default)]
    pub appearance: String,
    #[serde(default)]
    pub personality: String,
    #[serde(default)]
    pub goals: String,
    #[serde(default)]
    pub voice: String,
    /// How their voice sounds, as numbers (see `voice::recipe::Recipe::of`),
    /// over what the words in `voice` suggest.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub tone: species::LookMap,
    #[serde(default)]
    pub relationships: Vec<String>,
    #[serde(default)]
    pub home: String,
    /// Look sliders by name (the body's `meta.body.look`), e.g. height in
    /// metres, build, skin, shirt and trousers hue for people.
    #[serde(default)]
    pub look: species::LookMap,
    /// Species name ("" = human).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub species: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub variety: String,
    /// What they wear (layer type names), besides their variety's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub layers: Vec<String>,
    /// Their own temper, when it differs from their species' (born into a
    /// line that drifted).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temper: Option<species::Temper>,
    /// Their own size against their species' (a big tom: 1.5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<f32>,
    /// Their own body, when theirs was reshaped (else their species').
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub body: String,
    /// Personality as numbers 0..1 (sociable, playful, curious, brave,
    /// generous, crafty), when whoever wrote them gave them; otherwise they
    /// are read from the words.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub traits: std::collections::BTreeMap<String, f32>,
    /// What they are after in life, as goals (written with their region):
    /// words, and where it can be, a want the world can check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aims: Vec<AimSpec>,
}

/// One aim as generation writes it: `{"text": "win Mara's heart", "want":
/// {"kind": "affection", "with": "Mara", "at_least": 0.6}}`.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AimSpec {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub why: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub want: Option<serde_json::Value>,
}

impl Persona {
    /// Their own size against their species', kept in sense.
    pub fn size_mul(&self) -> f32 {
        self.size.filter(|s| s.is_finite()).unwrap_or(1.0).clamp(0.5, 4.0)
    }
}

#[derive(Clone, Debug)]
pub struct CharacterDef {
    pub id: i64,
    pub persona: Persona,
    pub home: Vec3,
    /// Saved dynamic state (position etc.) at load time.
    pub state: characters::SavedState,
    pub version: i64,
}

/// One region's story metadata (for status bar and prompts).
#[derive(Clone, Debug, Default)]
pub struct RegionInfo {
    pub name: String,
    pub mood: String,
    pub facts: Vec<String>,
    /// Roads laid between the settlement's buildings (older worlds; now
    /// they are ground edits like the rest).
    pub roads: Vec<crate::terrain::Road>,
    /// How the settlement shaped the land: canals, squares, roads, lagoons.
    pub ground: Vec<crate::ground::Edit>,
    /// Hollows it dug into the land: cave dwellings, mines.
    pub caves: Vec<crate::ground::Cave>,
}

/// One immutable world version, everything the main thread needs.
#[derive(Clone)]
pub struct WorldSnapshot {
    pub version: i64,
    pub scene: Arc<SceneTypes>,
    pub terrain: Arc<Terrain>,
    pub look: Arc<Look>,
    pub instances: Vec<Placed>,
    pub by_chunk: HashMap<(i32, i32), Vec<usize>>,
    pub characters: Vec<Arc<CharacterDef>>,
    pub regions: HashMap<(i32, i32), Arc<RegionInfo>>,
    /// Changes only when scatter inputs (terrain/look/scatter types) change.
    pub scatter_epoch: u64,
    /// Scatter type ids per tag.
    pub scatter: HashMap<String, Vec<u32>>,
    pub figure_type: Option<u32>,
    pub species: Arc<species::SpeciesBook>,
    pub spawn: Vec3,
    /// A quick first picture (land and plants only); the whole world flips in later.
    pub partial: bool,
}

impl WorldSnapshot {
    pub fn index(instances: &[Placed]) -> HashMap<(i32, i32), Vec<usize>> {
        let mut m: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
        for (i, p) in instances.iter().enumerate() {
            m.entry(chunk_of(p.pos.x, p.pos.z)).or_default().push(i);
        }
        m
    }

    #[cfg(test)]
    /// Invariant behind atomic flips: everything this snapshot references is in
    /// it, and its pipeline was built from exactly its types.
    pub fn check_consistent(&self) -> Result<(), String> {
        for p in &self.instances {
            if !self.scene.types.contains_key(&p.type_id) {
                return Err(format!("instance {} uses type {} missing from the snapshot", p.id, p.type_id));
            }
        }
        for ids in self.scatter.values() {
            for id in ids {
                if !self.scene.types.contains_key(id) {
                    return Err(format!("scatter type {id} missing"));
                }
            }
        }
        if let Some(pipe) = &self.scene.pipeline {
            let mut list: Vec<(u32, &crate::lang::CompiledType)> = self.scene.types.values().map(|t| (t.id, t.ct.as_ref())).collect();
            list.sort_by_key(|x| x.0);
            if crate::render::shader::key(&list) != pipe.key {
                return Err("pipeline was built from a different type set".into());
            }
        }
        Ok(())
    }

    pub fn type_of(&self, id: u32) -> Option<&Arc<TypeEntry>> {
        self.scene.types.get(&id)
    }

    /// A body type by name (built-in bodies first, then the newest).
    pub fn body_type(&self, name: &str) -> Option<&Arc<TypeEntry>> {
        let mut found: Option<&Arc<TypeEntry>> = None;
        for t in self.scene.types.values() {
            if t.ct.meta.body.is_some() && t.ct.meta.name == name && found.is_none_or(|f| !f.builtin && (t.builtin || t.id > f.id)) {
                found = Some(t);
            }
        }
        found.or_else(|| self.figure_type.and_then(|f| self.type_of(f)))
    }

    /// A body type with exactly this name (no fallback).
    pub fn body_named(&self, name: &str) -> Option<&Arc<TypeEntry>> {
        self.body_type(name).filter(|t| t.ct.meta.name == name)
    }

    /// A body and the bodies it was written from, nearest first ("cat
    /// (Whiskers)", "cat", "quadruped").
    pub fn body_line(&self, name: &str) -> Vec<String> {
        let mut out = vec![name.to_string()];
        let mut at = name.to_string();
        while out.len() < 8 {
            let Some(from) = self.body_named(&at).and_then(|t| t.ct.meta.body.as_ref()).and_then(|b| b.from.clone()) else { break };
            if out.contains(&from) {
                break;
            }
            out.push(from.clone());
            at = from;
        }
        out
    }

    /// The oldest body of a body's line (itself, unless written from another).
    pub fn body_root(&self, name: &str) -> String {
        self.body_line(name).pop().unwrap_or_else(|| name.to_string())
    }

    /// Would a layer made for `fits` sit on `body`? Made for it, or for a
    /// body it was written from (a quadruped's collar on a cat's own body).
    pub fn layer_fits(&self, fits: &str, body: &str) -> bool {
        fits == body || self.body_line(body).iter().any(|b| b == fits)
    }

    pub fn region_name(&self, r: (i32, i32)) -> Option<&str> {
        self.regions.get(&r).map(|x| x.name.as_str()).filter(|s| !s.is_empty())
    }

    /// Story instances in a chunk and its neighbours.
    pub fn near_chunk(&self, c: (i32, i32)) -> impl Iterator<Item = &Placed> {
        let mut out = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(v) = self.by_chunk.get(&(c.0 + dx, c.1 + dz)) {
                    out.extend(v.iter().map(|i| &self.instances[*i]));
                }
            }
        }
        out.into_iter()
    }
}

/// Instance with everything the CPU needs to evaluate it.
#[derive(Clone)]
pub struct Solid {
    pub inst: GpuInst,
    pub ty: Arc<TypeEntry>,
    /// Cuts past the instance's own few, and bores (a live thing shot
    /// full of holes).
    pub more: Arc<[[f32; 4]]>,
    pub bores: Arc<[[f32; 8]]>,
}

impl Solid {
    pub fn sdf(&self, p: Vec3) -> f32 {
        let c = self.inst.center();
        let ds = (p - c).length() - self.inst.radius();
        if ds > 0.5 {
            return ds;
        }
        // Its bounds box is much tighter than the sphere for tall or flat
        // shapes (a tree, a wall), and still never more than the true distance.
        let l = Vec3::from(self.inst.to_local(p));
        let b = Vec3::from_array(self.ty.ct.meta.bounds);
        let q = l.abs() - b;
        let db = (q.max(Vec3::ZERO).length() + q.max_element().min(0.0)) * self.inst.pos_scale[3];
        if db > 0.5 {
            return db;
        }
        self.exact_sdf(p)
    }

    /// The shape's own distance, without the bounding shortcuts (which are
    /// only lower bounds: too small for a wide body passing close).
    pub fn exact_sdf(&self, p: Vec3) -> f32 {
        let d = self.inst.sdf(&self.ty.ct, p);
        if self.more.is_empty() && self.bores.is_empty() {
            return d;
        }
        let lp = self.inst.to_local(p);
        let s = self.inst.pos_scale[3];
        let d = self.more.iter().fold(d, |d, c| d.max(-crate::render::cut_sdf(c, lp) * s));
        self.bores.iter().fold(d, |d, c| d.max(-crate::render::bore_sdf(c, lp) * s))
    }

    /// A placed or scattered shape: no cuts past its own.
    pub fn new(inst: GpuInst, ty: Arc<TypeEntry>) -> Solid {
        Solid { inst, ty, more: Arc::new([]), bores: Arc::new([]) }
    }
}

#[cfg(test)]
mod tests {
    /// A land keeps the usual sea unless it says it has none (null, or a
    /// word), and old worlds have no ground water.
    #[test]
    fn a_land_may_have_no_sea() {
        let look = |j: &str| serde_json::from_str::<super::Look>(j).unwrap();
        assert_eq!(look("{}").sea, Some(0.0));
        assert_eq!(look("{}").groundwater, None);
        assert_eq!(look(r#"{"sea": null, "groundwater": 4}"#).sea, None);
        assert_eq!(look(r#"{"sea": "none"}"#).sea, None);
        assert_eq!(look(r#"{"groundwater": 4}"#).groundwater, Some(4.0));
        let none = look(&serde_json::to_string(&look(r#"{"sea": null}"#)).unwrap());
        assert_eq!(none.sea, None, "saved and loaded again");
    }
}

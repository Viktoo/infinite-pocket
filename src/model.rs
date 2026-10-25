//! The authoritative world model. Owned by the committer thread, which
//! serialises every change: placement checks (step 5), shader build + GPU
//! parity (step 6), the SQLite commit, and producing the next snapshot (flip).

use crate::db::{self, Db};
use crate::lang::probe::{ProbeReport, default_k, probe, sample_points};
use crate::lang::{CompiledType, Diag, Stage, compile, format_diags};
use crate::render::gpu::{Gpu, ScenePipeline};
use crate::render::{GpuInst, shader};
use crate::terrain::Terrain;
use crate::world::characters::SavedState;
use crate::world::{CharacterDef, Look, Persona, Placed, RegionInfo, SceneTypes, TypeEntry, WorldSnapshot};
use anyhow::Result;
use glam::Vec3;
use parking_lot::Mutex;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const BUILTIN_SOURCES: &[&str] = &[
    include_str!("builtin/figure.js"),
    include_str!("builtin/quadruped.js"),
    include_str!("builtin/nightwalker.js"),
    include_str!("builtin/phantom.js"),
    include_str!("builtin/tree.js"),
    include_str!("builtin/pine.js"),
    include_str!("builtin/rock.js"),
    include_str!("builtin/bush.js"),
    include_str!("builtin/grass.js"),
    include_str!("builtin/flower.js"),
    include_str!("builtin/tallgrass.js"),
    include_str!("builtin/flame.js"),
    include_str!("builtin/stick.js"),
    include_str!("builtin/stone.js"),
    include_str!("builtin/mushroom.js"),
    include_str!("builtin/door.js"),
    include_str!("builtin/hatch.js"),
    include_str!("builtin/lamppost.js"),
    include_str!("builtin/bridge.js"),
    include_str!("builtin/walkway.js"),
    include_str!("builtin/bench.js"),
    include_str!("builtin/boat.js"),
    include_str!("builtin/shot.js"),
    include_str!("builtin/slug.js"),
    include_str!("builtin/conjuring.js"),
];

/// Live positions the committer must not build on top of.
#[derive(Default, Clone)]
pub struct Live {
    pub player: Vec3,
    pub characters: Vec<Vec3>,
    /// Types the sim's live things and cells use right now. Types nothing
    /// uses are left out of the shader (see `WorldModel::active_types`).
    pub types: HashSet<u32>,
}

pub type LiveRef = Arc<Mutex<Live>>;

#[derive(Clone)]
struct TypeRec {
    entry: Arc<TypeEntry>,
    version: Option<i64>,
    interior: Vec<[f32; 3]>,
    /// When this session committed it; new types stay in the shader for a
    /// while even before the sim reports a thing using them.
    born: Option<Instant>,
}

/// How long a newly committed type is kept without any reported use.
const NEW_TYPE_GRACE: Duration = Duration::from_secs(120);

#[derive(Clone)]
struct InstRec {
    placed: Placed,
    removed: Option<i64>,
}

/// A type that passed steps 1–4.
#[derive(Clone)]
pub struct NewType {
    pub ct: Arc<CompiledType>,
    pub report: Arc<ProbeReport>,
}

#[derive(Clone, Debug)]
pub enum TypeRef {
    Existing(u32),
    New(usize),
}

#[derive(Clone, Debug)]
pub struct Placement {
    pub ty: TypeRef,
    pub x: f32,
    pub z: f32,
    /// Explicit height of the instance origin; None = sit on the ground.
    pub y: Option<f32>,
    /// Sit on the floor of the hollow under the spot (a cave's), not the surface.
    pub under: bool,
    pub rot_y: f32,
    pub scale: f32,
    pub params: [f32; 8],
}

pub struct RegionCommit {
    pub r: (i32, i32),
    pub plan_json: String,
    pub info: RegionInfo,
    pub characters: Vec<(Persona, Vec3)>,
    /// The settlement's middle, and how its ways look: roads between its
    /// buildings (colour, half width) and what lights them.
    pub settlement: Option<Settlement>,
}

#[derive(Default)]
pub struct Settlement {
    pub x: f32,
    pub z: f32,
    /// None: no roads. A colour of None: trodden earth in the land's colours.
    pub road: Option<(Option<[f32; 3]>, f32)>,
    pub lamps: Option<TypeRef>,
    /// How it shapes its land (lagoons, islands, canals, streets, squares),
    /// in order, each with the building type lined up along it, if any.
    pub ground: Vec<(crate::settle::LandPlan, Option<TypeRef>)>,
    /// What spans its cuts where its ways cross them (None: the built-in bridge).
    pub bridge: Option<TypeRef>,
    /// The one vehicle its people share (a boat, a cart), set by its middle.
    pub vehicle: Option<TypeRef>,
    /// Hollows it digs into the land: cave dwellings, mines, cellars in a hill.
    pub caves: Vec<crate::settle::CaveSpec>,
    /// Ways off the ground between what holds them up (rope bridges
    /// between trees, boardwalks along a cliff).
    pub walkways: Vec<crate::settle::WalkwaySpec>,
}

pub struct CommitRequest {
    pub kind: &'static str,
    pub summary: String,
    pub new_types: Vec<NewType>,
    pub placements: Vec<Placement>,
    /// Move placements a little to resolve conflicts instead of failing.
    pub nudge: bool,
    pub region: Option<RegionCommit>,
    pub look: Option<Look>,
}

pub struct CommitOk {
    pub snapshot: Arc<WorldSnapshot>,
    pub placed: usize,
    pub dropped: Vec<String>,
}

pub struct WorldModel {
    pub db: Arc<Db>,
    pub gpu: Option<Arc<Gpu>>,
    pub seed: u32,
    pub bible: String,
    pub look: Arc<Look>,
    pub species: Arc<crate::world::species::SpeciesBook>,
    terrain: Arc<Terrain>,
    types: BTreeMap<u32, TypeRec>,
    insts: Vec<InstRec>,
    reverted: HashSet<i64>,
    pub current: Option<i64>,
    chars: Vec<Arc<CharacterDef>>,
    regions: HashMap<(i32, i32), Arc<RegionInfo>>,
    pipelines: Vec<Arc<ScenePipeline>>,
    pub spawn: Vec3,
    pub live: LiveRef,
    /// Types of the saved live things when the world was loaded. Kept for the
    /// whole session so nothing is dropped before the sim has loaded them.
    saved_thing_types: HashSet<u32>,
    /// Versions that reshaped a thing (each makes a one-off type).
    reshape_versions: HashSet<i64>,
    /// The box each type's solid fills, by type id (see `inground::solid_box`).
    solid_boxes: Mutex<HashMap<u32, crate::inground::SolidBox>>,
}

fn meta_json(ct: &CompiledType, bottom: f32, top: f32) -> String {
    serde_json::json!({ "name": ct.meta.name, "bounds": ct.meta.bounds, "tags": ct.meta.tags, "props": ct.meta.props, "bottom": bottom, "top": top }).to_string()
}

fn type_entry(id: u32, ct: Arc<CompiledType>, bottom: f32, top: f32, builtin: bool) -> Arc<TypeEntry> {
    let solid = !ct.meta.tags.iter().any(|t| t == "nonsolid" || t == "grass");
    // A shape that reads its live state (poses, growth, doors) can move
    // anywhere inside its bounds: the bounding sphere must cover all of them.
    let b = ct.meta.bounds[1];
    let (sb, st) = if ct.meta.tags.iter().any(|t| t == "figure") {
        (0.0, b)
    } else if ct.wgsl.contains("k.s") {
        (-b, b)
    } else {
        (bottom, top)
    };
    let (sphere_cy, sphere_r) = TypeEntry::sphere(ct.meta.bounds, sb, st);
    Arc::new(TypeEntry { id, ct, builtin, solid, bottom, top, sphere_cy, sphere_r })
}

/// Choose a gentle spawn point near the origin (on land or in the water:
/// the traveler swims).
pub fn find_spawn(t: &Terrain) -> Vec3 {
    spawn_near(t, 0.0, 0.0)
}

/// For a newly made land: near the middle of a region, so only that one
/// region is in sight and loading waits for it alone; the nearest region
/// whose middle lies in the first biome (where the land's "start" is). A
/// land shaped by landforms begins at the origin, where they put its start.
pub fn find_first_spawn(t: &Terrain) -> Vec3 {
    // A land given its shape (landforms) is laid round where the traveler
    // begins: the origin.
    if t.lands.is_some() {
        return spawn_near(t, 0.0, 0.0);
    }
    let reg = crate::world::REGION;
    let mid = |i: i32, j: i32| (i as f32 * reg + reg / 2.0, j as f32 * reg + reg / 2.0);
    let first = (0..=16i32).flat_map(|ring| {
        (-ring..=ring).flat_map(move |i| (-ring..=ring).map(move |j| (i, j))).filter(move |(i, j)| i.abs().max(j.abs()) == ring)
    }).find(|&(i, j)| {
        let (x, z) = mid(i, j);
        t.biome_at(x, z) == 0
    });
    let (i, j) = first.unwrap_or((0, 0));
    let (x, z) = mid(i, j);
    spawn_near(t, x, z)
}

/// The nearest dry ground to `p` (itself if dry), on the ground.
pub fn dry_near(t: &Terrain, p: Vec3) -> Vec3 {
    for k in 0..2000 {
        let a = k as f32 * 2.399;
        let r = (k as f32).sqrt() * 1.5;
        let (x, z) = (p.x + a.cos() * r, p.z + a.sin() * r);
        let h = t.height(x, z);
        if h >= t.water(x, z) + 0.3 {
            return Vec3::new(x, h, z);
        }
    }
    Vec3::new(p.x, t.height(p.x, p.z), p.z)
}

fn spawn_near(t: &Terrain, mx: f32, mz: f32) -> Vec3 {
    let mut best = Vec3::new(mx, 0.0, mz);
    let mut best_score = f32::MAX;
    // Spiral outwards for the gentlest spot close by, dry if there is any:
    // with only water round about, the traveler starts afloat in it.
    for i in 0..400 {
        let a = i as f32 * 2.399;
        let r = (i as f32).sqrt() * 9.0;
        let (x, z) = (mx + a.cos() * r, mz + a.sin() * r);
        let h = t.height(x, z);
        let wl = t.water(x, z);
        let n = t.normal(x, z);
        let mut score = r * 0.02 + (1.0 - n.y) * 30.0;
        if (mx, mz) != (0.0, 0.0) && r > 18.0 {
            // Further out, more regions come into sight.
            score += 5.0;
        }
        if h < wl + 1.5 {
            score += 100.0;
        }
        if score < best_score {
            best_score = score;
            best = Vec3::new(x, h, z);
        }
    }
    afloat(t, best)
}

/// A spot as the traveler starts there: on the ground, or at the top of
/// water too deep to stand in.
pub fn afloat(t: &Terrain, p: Vec3) -> Vec3 {
    let h = t.height(p.x, p.z);
    Vec3::new(p.x, h.max(t.water(p.x, p.z) - 1.5), p.z)
}

impl WorldModel {
    /// Load everything from the database and recompile every type from source.
    pub fn load(db: Arc<Db>, gpu: Option<Arc<Gpu>>, live: LiveRef) -> Result<WorldModel> {
        let u = db.universe()?;
        let mut look: Look = serde_json::from_str(&u.look_json).unwrap_or_default();
        crate::terrain::add_litter(&mut look.biomes);
        let terrain = Arc::new(Terrain::new(u.seed, look.biomes.clone()).with_water(look.sea, look.groundwater).with_lands(&look.landforms));
        terrain.set_rivers(look.rivers, look.palette.sand, look.caves);
        let mut m = WorldModel {
            db: db.clone(),
            gpu,
            seed: u.seed,
            bible: u.bible,
            look: Arc::new(look),
            species: Arc::new(crate::world::species::SpeciesBook::load(&db.with(|c| crate::db::species_rows(c)).unwrap_or_default(), db.kv_get("species.world").as_deref())),
            terrain,
            types: BTreeMap::new(),
            insts: Vec::new(),
            reverted: HashSet::new(),
            current: None,
            chars: Vec::new(),
            regions: HashMap::new(),
            pipelines: Vec::new(),
            spawn: Vec3::ZERO,
            live,
            saved_thing_types: HashSet::new(),
            reshape_versions: HashSet::new(),
            solid_boxes: Mutex::new(HashMap::new()),
        };
        m.saved_thing_types = db
            .with(|c| {
                let mut st = c.prepare("SELECT DISTINCT type_id FROM things")?;
                let rows = st.query_map([], |r| r.get::<_, i64>(0))?;
                Ok(rows.filter_map(|r| r.ok()).map(|id| id as u32).collect())
            })
            .unwrap_or_default();
        // The rivers round where the traveler begins.
        if let Some(s) = db.kv_get("spawn").and_then(|s| serde_json::from_str::<[f32; 3]>(&s).ok()) {
            m.terrain.lay_rivers(s[0], s[2]);
        }
        m.spawn = match db.kv_get("spawn").and_then(|s| serde_json::from_str::<[f32; 3]>(&s).ok()) {
            Some(s) => Vec3::from(s),
            None => {
                let s = find_spawn(&m.terrain);
                m.terrain.lay_rivers(s.x, s.z);
                let s = afloat(&m.terrain, s);
                db.kv_set("spawn", &serde_json::to_string(&s.to_array())?)?;
                s
            }
        };

        // Built-in types are stored like any other. Worlds made by older
        // versions get new built-ins added and changed ones updated in place.
        // A built-in's probe never changes, so it is done once per run (a
        // run that opens many worlds, like the tests, probes each once).
        static PROBED: Mutex<Vec<Option<(f32, f32)>>> = Mutex::new(Vec::new());
        let rows = db.types()?;
        for (i, src) in BUILTIN_SOURCES.iter().enumerate() {
            let ct = compile(src).map_err(|d| anyhow::anyhow!("builtin type failed: {}", format_diags(&d)))?;
            let existing = rows.iter().find(|r| r.status == "builtin" && r.name == ct.meta.name);
            if existing.is_some_and(|r| r.code == *src) {
                continue;
            }
            let known = PROBED.lock().get(i).copied().flatten();
            let (bottom, top) = match known {
                Some(b) => b,
                None => {
                    let rep = probe(&ct).map_err(|d| anyhow::anyhow!("builtin {} failed probe: {}", ct.meta.name, format_diags(&d)))?;
                    let mut p = PROBED.lock();
                    p.resize(BUILTIN_SOURCES.len(), None);
                    p[i] = Some((rep.bottom, rep.top));
                    (rep.bottom, rep.top)
                }
            };
            let meta = meta_json(&ct, bottom, top);
            match existing {
                Some(r) => db.with(|c| {
                    c.execute("UPDATE types SET code = ?1, meta_json = ?2 WHERE id = ?3", rusqlite::params![src, meta, r.id])?;
                    Ok(())
                })?,
                None => {
                    db.with(|c| db::add_type(c, None, &ct.meta.name, src, &meta, "builtin", ""))?;
                }
            }
        }
        let versions = db.versions()?;
        for v in &versions {
            if let Some(r) = v.reverts {
                m.reverted.insert(r);
            }
        }
        m.current = versions.last().map(|v| v.id);
        m.reshape_versions = versions.iter().filter(|v| is_reshape(&v.kind, &v.summary)).map(|v| v.id).collect();
        for r in db.types()? {
            if r.status != "ok" && r.status != "builtin" {
                continue;
            }
            let ct = match compile(&r.code) {
                Ok(ct) => Arc::new(ct),
                Err(d) => {
                    crate::log::error(format!("stored type {} no longer compiles: {}", r.name, format_diags(&d)));
                    continue;
                }
            };
            let stored = db
                .with(|c| Ok(c.query_row("SELECT meta_json FROM types WHERE id = ?1", [r.id], |row| row.get::<_, String>(0))?))
                .ok()
                .and_then(|j| serde_json::from_str::<serde_json::Value>(&j).ok());
            let get = |k: &str| stored.as_ref().and_then(|v| v.get(k)).and_then(|b| b.as_f64()).map(|x| x as f32);
            let (bottom, top) = match (get("bottom"), get("top")) {
                (Some(b), Some(t)) => (b, t),
                _ => probe(&ct).map(|p| (p.bottom, p.top)).unwrap_or((-ct.meta.bounds[1], ct.meta.bounds[1])),
            };
            let builtin = r.status == "builtin";
            let version = if builtin { None } else { r.version };
            m.types.insert(r.id as u32, TypeRec { entry: type_entry(r.id as u32, ct, bottom, top, builtin), version, interior: Vec::new(), born: None });
        }
        for r in db.instances()? {
            m.insts.push(InstRec {
                placed: Placed { id: r.id, type_id: r.type_id as u32, pos: Vec3::new(r.x, r.y, r.z), rot_y: r.rot_y, scale: r.scale, params: r.params, version: r.version },
                removed: r.removed,
            });
        }
        for r in db.regions()? {
            if r.status != "done" {
                continue;
            }
            if r.version.is_some_and(|v| m.reverted.contains(&v)) {
                continue;
            }
            let info = region_info_from_plan(&r.plan_json);
            m.regions.insert((r.rx, r.rz), Arc::new(info));
        }
        // The shaped land first: homes stand on it.
        m.apply_ground();
        for c in db.characters()? {
            let persona: Persona = serde_json::from_str(&c.persona_json).unwrap_or_default();
            let state: SavedState = serde_json::from_str(&c.state_json).unwrap_or_default();
            let mut home = Vec3::new(c.home_x, m.terrain.height(c.home_x, c.home_z), c.home_z);
            // Older worlds may have homes inside buildings: move them to the doorstep for good.
            // (A people of the water keeps its home in the water.)
            let swims = !persona.species.is_empty() && m.species.get(&persona.species).is_some_and(|s| s.water > 0.0);
            let fixed = m.clear_spot(home, &[], swims);
            if (fixed - home).length() > 0.01 {
                home = fixed;
                let _ = db.with(|cn| {
                    cn.execute("UPDATE characters SET home_x = ?1, home_z = ?2 WHERE id = ?3", rusqlite::params![home.x as f64, home.z as f64, c.id])?;
                    Ok(())
                });
            }
            m.chars.push(Arc::new(CharacterDef { id: c.id, persona, home, state, version: c.version.unwrap_or(0) }));
        }
        Ok(m)
    }

    /// Solid story instances near `p` (plus extra, not yet committed ones).
    fn story_solids_near(&self, p: Vec3, extra: &[(Placed, Arc<TypeEntry>)]) -> Vec<crate::world::Solid> {
        let mut out = Vec::new();
        let near = |pl: &Placed, e: &TypeEntry| (pl.pos - p).length() < e.radius() * pl.scale + 4.0;
        for i in self.insts.iter().filter(|i| self.inst_active(i)) {
            if let Some(t) = self.types.get(&i.placed.type_id) {
                if t.entry.solid && near(&i.placed, &t.entry) {
                    out.push(crate::world::Solid::new(i.placed.gpu(&t.entry, 1.0), t.entry.clone()));
                }
            }
        }
        for (pl, e) in extra {
            if e.solid && near(pl, e) {
                out.push(crate::world::Solid::new(pl.gpu(e, 1.0), e.clone()));
            }
        }
        out
    }

    /// The nearest spot to `p` where a person can stand (not inside a building).
    fn clear_spot(&self, p: Vec3, extra: &[(Placed, Arc<TypeEntry>)], swims: bool) -> Vec3 {
        let solids = self.story_solids_near(p, extra);
        if solids.is_empty() {
            return p;
        }
        let obs = crate::world::collide::Obstacles { solids: &solids, bodies: &[] };
        crate::world::collide::free_spot(&self.terrain, &obs, p, crate::world::collide::NPC_RADIUS + 0.3, 40.0, swims)
    }

    /// Make the world anew from what its tables say now (a guest's copy,
    /// written to by the host), keeping the land as it is when its making
    /// didn't change (the pits and ways made in play stay on it).
    pub fn reload(&mut self) -> Result<Arc<WorldSnapshot>> {
        let mut fresh = WorldModel::load(self.db.clone(), self.gpu.clone(), self.live.clone())?;
        let same_land = fresh.seed == self.seed && serde_json::to_string(&*fresh.look).ok() == serde_json::to_string(&*self.look).ok();
        if same_land {
            fresh.terrain = self.terrain.clone();
            fresh.apply_ground();
        }
        *self = fresh;
        self.snapshot().map_err(|d| anyhow::anyhow!("building the world shader failed: {}", format_diags(&d)))
    }

    pub fn terrain(&self) -> Arc<Terrain> {
        self.terrain.clone()
    }

    fn type_active(&self, t: &TypeRec, inst_types: &HashSet<u32>) -> bool {
        match t.version {
            None => true,
            Some(v) => !self.reverted.contains(&v) || inst_types.contains(&t.entry.id),
        }
    }

    fn inst_active(&self, i: &InstRec) -> bool {
        if self.reverted.contains(&i.placed.version) {
            return false;
        }
        match i.removed {
            None => true,
            Some(r) => self.reverted.contains(&r),
        }
    }

    fn active_instances(&self) -> Vec<Placed> {
        self.insts.iter().filter(|i| self.inst_active(i)).map(|i| i.placed.clone()).collect()
    }

    /// Types that go into the shader. A reshape makes a one-off type per
    /// edit, so earlier versions of a reshaped thing that nothing uses any
    /// more are left out: every type costs build time. Other types stay even
    /// unused (things grow into them, spawn them or are made by name).
    fn active_types(&self) -> Vec<&TypeRec> {
        let used: HashSet<u32> = self.insts.iter().filter(|i| self.inst_active(i)).map(|i| i.placed.type_id).collect();
        let live = self.live.lock().types.clone();
        let in_use = |t: &TypeRec| {
            let id = t.entry.id;
            !t.version.is_some_and(|v| self.reshape_versions.contains(&v))
                || used.contains(&id)
                || live.contains(&id)
                || self.saved_thing_types.contains(&id)
                || t.born.is_some_and(|b| b.elapsed() < NEW_TYPE_GRACE)
        };
        self.types.values().filter(|t| self.type_active(t, &used) && in_use(t)).collect()
    }

    /// Scatter type ids per tag: universe types replace built-ins for a tag.
    /// A reshaped thing's one-off type keeps its source's tags ("base",
    /// "rock") but is that one thing only: it never scatters.
    fn scatter_map(&self, types: &[&TypeRec]) -> HashMap<String, Vec<u32>> {
        let mut tags: HashSet<String> = HashSet::new();
        for b in &self.look.biomes {
            tags.extend(b.scatter.keys().cloned());
        }
        let mut m = HashMap::new();
        for tag in tags {
            let has = |t: &&&TypeRec| t.entry.has_tag(&tag) && t.entry.has_tag("base") && !t.version.is_some_and(|v| self.reshape_versions.contains(&v));
            let custom: Vec<u32> = types.iter().filter(has).map(|t| t.entry.id).collect();
            let ids = if !custom.is_empty() {
                custom
            } else {
                types.iter().filter(|t| t.entry.builtin && t.entry.has_tag(&tag)).map(|t| t.entry.id).collect()
            };
            if !ids.is_empty() {
                m.insert(tag, ids);
            }
        }
        m
    }

    fn pipeline_for(&mut self, types: &[&TypeRec]) -> Result<Option<Arc<ScenePipeline>>, Vec<Diag>> {
        let Some(gpu) = self.gpu.clone() else { return Ok(None) };
        let list: Vec<(u32, &CompiledType)> = types.iter().map(|t| (t.entry.id, t.entry.ct.as_ref())).collect();
        let key = shader::key(&list);
        if let Some(p) = self.pipelines.iter().find(|p| p.key == key) {
            let p = p.clone();
            self.pipelines.retain(|q| q.key != key);
            self.pipelines.push(p.clone());
            return Ok(Some(p));
        }
        let src = shader::assemble(&list);
        let t0 = std::time::Instant::now();
        let p = gpu.build_pipeline(&src, key).map_err(|e| {
            let _ = std::fs::write(std::env::temp_dir().join("pocket-failed.wgsl"), &src);
            vec![Diag::new(Stage::Gpu, 0, e)]
        })?;
        crate::log::info(format!("built pipeline with {} types in {:.0} ms", list.len(), t0.elapsed().as_secs_f64() * 1000.0));
        self.pipelines.push(p.clone());
        if self.pipelines.len() > 8 {
            self.pipelines.remove(0);
        }
        Ok(Some(p))
    }

    /// A quick first snapshot for big universes: built-in and base types only,
    /// no story instances or characters. The full one follows via a flip.
    pub fn snapshot_base(&mut self) -> Result<Arc<WorldSnapshot>, Vec<Diag>> {
        let types: Vec<TypeRec> = self.active_types().into_iter().filter(|t| t.entry.builtin || t.entry.has_tag("base")).cloned().collect();
        let refs: Vec<&TypeRec> = types.iter().collect();
        let pipeline = self.pipeline_for(&refs)?;
        let full = self.make_snapshot(&refs, pipeline);
        Ok(Arc::new(WorldSnapshot {
            version: full.version,
            scene: full.scene.clone(),
            terrain: full.terrain.clone(),
            look: full.look.clone(),
            instances: Vec::new(),
            by_chunk: Default::default(),
            characters: Vec::new(),
            regions: full.regions.clone(),
            scatter_epoch: full.scatter_epoch,
            scatter: full.scatter.clone(),
            figure_type: full.figure_type,
            species: full.species.clone(),
            spawn: full.spawn,
            partial: true,
        }))
    }

    /// Number of active types (decides whether startup uses `snapshot_base`).
    pub fn active_type_count(&self) -> usize {
        self.active_types().len()
    }

    /// Build the snapshot for the current state (building/caching the pipeline).
    pub fn snapshot(&mut self) -> Result<Arc<WorldSnapshot>, Vec<Diag>> {
        let types: Vec<TypeRec> = self.active_types().into_iter().cloned().collect();
        let refs: Vec<&TypeRec> = types.iter().collect();
        let pipeline = self.pipeline_for(&refs)?;
        Ok(self.make_snapshot(&refs, pipeline))
    }

    fn make_snapshot(&self, types: &[&TypeRec], pipeline: Option<Arc<ScenePipeline>>) -> Arc<WorldSnapshot> {
        let scene = Arc::new(SceneTypes { types: types.iter().map(|t| (t.entry.id, t.entry.clone())).collect(), pipeline });
        let instances = self.active_instances();
        // The ground taken away under placed shapes, and the settlements' ground.
        let hollows: Vec<crate::terrain::Hollow> = instances
            .iter()
            .filter_map(|p| {
                let e = scene.types.get(&p.type_id)?;
                Some(hollow_of(p, &ground_taken(&e.ct.meta, || self.solid_box(e))?))
            })
            .collect();
        self.apply_ground();
        // Hollows under shapes set into the ground, on the land as shaped.
        let sunk: Vec<crate::ground::Cave> = instances
            .iter()
            .filter_map(|p| {
                let e = scene.types.get(&p.type_id)?;
                e.ct.meta.in_ground.is_some().then(|| crate::inground::cave(&self.terrain, p, e, &self.solid_box(e)))
            })
            .collect();
        self.terrain.set_caves(crate::ground::Layer::Shapes, sunk);
        {
            let mut c = self.terrain.carve.write();
            if c.fixed != hollows {
                c.fixed = hollows;
                c.reindex();
            }
        }
        let by_chunk = WorldSnapshot::index(&instances);
        let scatter = self.scatter_map(types);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        serde_json::to_string(self.look.as_ref()).unwrap_or_default().hash(&mut h);
        let mut sm: Vec<_> = scatter.iter().collect();
        sm.sort();
        sm.hash(&mut h);
        let figure_type = types.iter().find(|t| t.entry.builtin && t.entry.has_tag("figure")).map(|t| t.entry.id);
        let characters = self.chars.iter().filter(|c| !self.reverted.contains(&c.version)).cloned().collect();
        Arc::new(WorldSnapshot {
            version: self.current.unwrap_or(0),
            scene,
            terrain: self.terrain.clone(),
            look: self.look.clone(),
            instances,
            by_chunk,
            characters,
            regions: self.regions.clone(),
            scatter_epoch: h.finish(),
            scatter,
            figure_type,
            species: self.species.clone(),
            spawn: self.spawn,
            partial: false,
        })
    }

    /// Lay the settlements' ground edits (canals, squares, roads) on the
    /// land, region by region in a fixed order.
    fn apply_ground(&self) {
        self.terrain.set_ground(crate::ground::Layer::Places, self.places_edits(None));
        self.terrain.set_caves(crate::ground::Layer::Places, self.places_caves(None));
    }

    /// The settlements' caves, region by region (but `except`'s).
    fn places_caves(&self, except: Option<(i32, i32)>) -> Vec<crate::ground::Cave> {
        let mut keys: Vec<&(i32, i32)> = self.regions.keys().filter(|k| Some(**k) != except).collect();
        keys.sort();
        keys.into_iter().flat_map(|k| self.regions[k].caves.iter().cloned()).collect()
    }

    /// The settlements' ground edits, region by region (but `except`'s).
    fn places_edits(&self, except: Option<(i32, i32)>) -> Vec<crate::ground::Edit> {
        let mut keys: Vec<&(i32, i32)> = self.regions.keys().filter(|k| Some(**k) != except).collect();
        keys.sort();
        let raw = |x: f32, z: f32| self.terrain.raw_height(x, z);
        let mut edits = Vec::new();
        for k in keys {
            let r = &self.regions[k];
            if r.ground.is_empty() {
                // Older worlds kept only their roads.
                edits.extend(r.roads.iter().map(|rd| rd.edit(&raw)));
            } else {
                edits.extend(r.ground.iter().cloned());
            }
        }
        edits
    }

    /// The land under a new settlement: the seed's, with the rivers and the
    /// other settlements' ground (`others`) on it.
    fn base_land(&self, others: &[crate::ground::Edit]) -> impl Fn(f32, f32) -> f32 + use<> {
        let t = self.terrain.clone();
        let mut under: Vec<crate::ground::Edit> = t.carve.read().ground.layer(crate::ground::Layer::Land).to_vec();
        under.extend(others.iter().cloned());
        move |x: f32, z: f32| crate::settle::shaped(&under, &|x, z| t.raw_height(x, z), x, z)
    }

    /// A type for a placement: its entry and the points inside it.
    fn entry_of(&mut self, t: &TypeRef, new_entries: &[Arc<TypeEntry>], new_types: &[NewType]) -> Option<(Arc<TypeEntry>, Vec<[f32; 3]>)> {
        match t {
            TypeRef::New(k) => new_entries.get(*k).cloned().zip(new_types.get(*k).map(|nt| nt.report.interior.clone())),
            TypeRef::Existing(id) => {
                let e = self.types.get(id)?.entry.clone();
                let inside = self.interior_of(e.id);
                Some((e, inside))
            }
        }
    }

    /// A built-in type by name.
    /// The box a type's solid fills (worked out once per type).
    fn solid_box(&self, e: &TypeEntry) -> crate::inground::SolidBox {
        if let Some(b) = self.solid_boxes.lock().get(&e.id) {
            return *b;
        }
        let b = crate::inground::solid_box(e);
        self.solid_boxes.lock().insert(e.id, b);
        b
    }

    fn builtin(&self, name: &str) -> Option<TypeRef> {
        self.types.values().find(|t| t.entry.builtin && t.entry.name() == name).map(|t| TypeRef::Existing(t.entry.id))
    }

    /// Build out a settlement around what was placed: buildings along its
    /// ways, roads from doors (to its streets, or between them when it has
    /// none), lamps, bridges where ways cross its cuts, and its vehicle.
    /// Returns its roads and its ground edits, as laid.
    fn settle(&mut self, r: (i32, i32), st: &Settlement, req: &CommitRequest, new_entries: &[Arc<TypeEntry>], placed: &mut Vec<(Placed, Arc<TypeEntry>)>) -> (Vec<crate::terrain::Road>, Vec<crate::ground::Edit>) {
        use crate::settle::{LandPlan, crossings, frontage, is_cut, is_way, realize, road_plan, spurs};
        let plans: Vec<LandPlan> = st.ground.iter().map(|g| g.0.clone()).collect();
        let others = self.places_edits(Some(r));
        let base = self.base_land(&others);
        let laid = realize(&plans, &[], &base, self.terrain.sea);

        // Buildings lined up along its ways and cuts, doors toward them.
        let mut lined = 0;
        for (i, (plan, ty)) in st.ground.iter().enumerate() {
            let (Some(ty), Some((_, sides, every))) = (ty, &plan.frontage) else { continue };
            let Some(edit) = laid.iter().find(|e| e.kind == plan.edit.kind && e.name == plan.edit.name && e.rim == plan.edit.rim) else { continue };
            let crate::ground::Shape::Line { pts, half } = &edit.shape else { continue };
            let Some((entry, interior)) = self.entry_of(ty, new_entries, &req.new_types) else { continue };
            let b = entry.ct.meta.bounds;
            let out = entry.ct.meta.anchors.iter().find(|a| a.kind == "door").map(|a| a.out()).unwrap_or([0.0, 0.0, 1.0]);
            let o = glam::Vec2::new(out[0], out[2]).normalize_or(glam::Vec2::Y);
            let (width, depth) = if o.y.abs() >= o.x.abs() { (b[0] * 2.0, b[2] * 2.0) } else { (b[2] * 2.0, b[0] * 2.0) };
            let walk = if is_cut(edit) { 0.4 } else { 1.0 };
            let clear = half + edit.rim.map(|r| r.1).unwrap_or(0.0) + walk;
            // Not across another way (a street meeting the quay stays open).
            let across = |p: glam::Vec2| laid.iter().any(|e| !std::ptr::eq(e, edit) && (is_way(e) || is_cut(e)) && e.dist(p.x, p.y).0 < width.max(depth) * 0.5 + 0.8);
            for (k, (p, face)) in frontage(pts, clear, *sides, *every, width, depth).into_iter().enumerate() {
                if lined >= 48 {
                    break;
                }
                if across(p) {
                    continue;
                }
                let rot = o.y.atan2(o.x) - face.y.atan2(face.x);
                let pl = Placement { ty: ty.clone(), x: p.x, z: p.y, y: None, under: false, rot_y: rot, scale: 1.0, params: [400.0 + (i * 64 + k) as f32, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5] };
                if let Ok(p) = self.check_place(&pl, &entry, &interior, placed, "a building along the way") {
                    let kids = self.children(&p, &entry, new_entries);
                    placed.push((p, entry.clone()));
                    placed.extend(kids);
                    lined += 1;
                }
            }
        }

        // Roads: from doors to the streets, or between doors when it has none.
        let fronts: Vec<Vec3> = placed.iter().filter(|(p, e)| e.has_tag("building") || e.ct.meta.anchors.iter().any(|a| a.kind == "door") || (e.solid && e.radius() * p.scale > 3.0 && !e.builtin)).filter(|(p, _)| (p.pos.x - st.x).hypot(p.pos.z - st.z) < 130.0).map(|(p, e)| front_of(p, e)).collect();
        let streets: Vec<&crate::ground::Edit> = laid.iter().filter(|e| is_way(e)).collect();
        let roads = match st.road {
            Some((color, half)) => {
                let color = color.unwrap_or_else(|| road_color(&self.look.palette));
                if streets.is_empty() { road_net(&fronts, color, half) } else { spurs(&fronts, &streets, color, half, 40.0) }
            }
            None => Vec::new(),
        };
        let extra: Vec<LandPlan> = roads.iter().map(road_plan).collect();
        let mut laid = if plans.is_empty() && extra.is_empty() { Vec::new() } else { realize(&plans, &extra, &base, self.terrain.sea) };
        // Rivers through the settlement's ways run on under them: cut again
        // after the ways (so they don't dam them) and bridged.
        if laid.iter().any(|e| is_way(e)) {
            let (ox, oz) = (r.0 as f32 * crate::world::REGION, r.1 as f32 * crate::world::REGION);
            let rivers: Vec<crate::ground::Edit> = self.terrain.carve.read().ground.layer(crate::ground::Layer::Land).iter().filter(|e| {
                let b = e.bounds();
                b[2] > ox - 40.0 && b[0] < ox + crate::world::REGION + 40.0 && b[3] > oz - 40.0 && b[1] < oz + crate::world::REGION + 40.0
            }).filter(|e| laid.iter().filter(|w| is_way(w)).any(|w| !crossings(w, e).is_empty())).cloned().collect();
            laid.extend(rivers);
        }
        let mut all = others;
        all.extend(laid.iter().cloned());
        self.terrain.set_ground(crate::ground::Layer::Places, all);

        // Lamps along the roads and streets.
        if let Some(lt) = &st.lamps {
            if let Some((entry, interior)) = self.entry_of(lt, new_entries, &req.new_types) {
                let mut along = roads.clone();
                for e in laid.iter().filter(|e| is_way(e)) {
                    if let crate::ground::Shape::Line { pts, half } = &e.shape {
                        along.extend((0..pts.len().saturating_sub(1)).step_by(4).map(|i| crate::terrain::Road { a: pts[i], b: pts[(i + 4).min(pts.len() - 1)], half: *half, color: [0.0; 3] }));
                    }
                }
                let mut n = 0;
                for (i, (a, b)) in lamp_spots(&along).into_iter().enumerate() {
                    if n >= 16 {
                        break;
                    }
                    let pl = Placement { ty: lt.clone(), x: a, z: b, y: None, under: false, rot_y: 0.0, scale: 1.0, params: [300.0 + i as f32, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5] };
                    if let Ok(p) = self.check_place(&pl, &entry, &interior, placed, "a lamp") {
                        placed.push((p, entry.clone()));
                        n += 1;
                    }
                }
            }
        }

        // Ways off the ground, span by span between what holds their ends up.
        if let Some(wt) = self.builtin("walkway").and_then(|w| self.entry_of(&w, new_entries, &req.new_types)).map(|e| e.0) {
            for (i, w) in st.walkways.iter().enumerate() {
                let rest = |e: Vec3| -> Option<f32> {
                    let g = self.terrain.height(e.x, e.z);
                    if e.y < 0.4 {
                        return Some(g);
                    }
                    self.support_below(e.x, e.z, g + e.y + 1.0, g + e.y - 1.5, placed)
                };
                let (Some(ya), Some(yb)) = (rest(w.a), rest(w.b)) else {
                    crate::log::info(format!("a walkway from ({:.0}, {:.0}) to ({:.0}, {:.0}) has nothing to rest on at an end", w.a.x, w.a.z, w.b.x, w.b.z));
                    continue;
                };
                // Onto what holds each end a little.
                let dir = glam::Vec2::new(w.b.x - w.a.x, w.b.z - w.a.z).normalize_or(glam::Vec2::Y);
                let (a, b) = (Vec3::new(w.a.x - dir.x * 0.3, ya, w.a.z - dir.y * 0.3), Vec3::new(w.b.x + dir.x * 0.3, yb, w.b.z + dir.y * 0.3));
                for (k, (c, len, rise, yaw)) in crate::settle::spans(a, b, w.style).into_iter().enumerate() {
                    let params = [700.0 + (i * 16 + k) as f32, 1.0, ((len * 0.5 - 0.5) / 7.5).clamp(0.0, 1.0), ((w.width * 0.5 - 0.3) / 1.2).clamp(0.0, 1.0), (rise / 16.0 + 0.5).clamp(0.0, 1.0), w.style, 0.5, 0.5];
                    placed.push((Placed { id: 0, type_id: wt.id, pos: c, rot_y: yaw, scale: 1.0, params, version: 0 }, wt.clone()));
                }
            }
        }

        // Bridges where ways cross cuts.
        let bridge = st.bridge.clone().or_else(|| self.builtin("bridge"));
        if let Some((bt, (entry, interior))) = bridge.and_then(|b| Some((b.clone(), self.entry_of(&b, new_entries, &req.new_types)?))) {
            let t = self.terrain.clone();
            let mut n = 0;
            for w in laid.iter().filter(|e| is_way(e)) {
                let wh = match &w.shape {
                    crate::ground::Shape::Line { half, .. } => *half,
                    _ => continue,
                };
                for c in laid.iter().filter(|e| is_cut(e)) {
                    for (p, dir, reach) in crossings(w, c) {
                        if n >= 24 {
                            break;
                        }
                        let half_span = reach + 1.2;
                        let (a, b) = (p - dir * half_span, p + dir * half_span);
                        let y = t.height(a.x, a.y).max(t.height(b.x, b.y));
                        let rot = std::f32::consts::FRAC_PI_2 - dir.y.atan2(dir.x);
                        let ka = ((half_span - 2.0) / 10.0).clamp(0.0, 1.0);
                        let kb = ((wh + 0.4 - 0.75) / 2.25).clamp(0.0, 1.0);
                        let timber = w.color.is_some_and(|c| c[0] > c[2] * 1.4 && c[0] < 0.55);
                        let pl = Placement { ty: bt.clone(), x: p.x, z: p.y, y: Some(y), under: false, rot_y: rot, scale: 1.0, params: [500.0 + n as f32, 1.0, ka, kb, if timber { 1.0 } else { 0.0 }, 0.5, 0.5, 0.5] };
                        if let Ok(p) = self.check_place(&pl, &entry, &interior, placed, "a bridge") {
                            placed.push((p, entry.clone()));
                            n += 1;
                        }
                    }
                }
            }
        }

        // Its vehicle: afloat on the nearest water for a boat, else by the middle.
        if let Some(vt) = &st.vehicle {
            if let Some((entry, interior)) = self.entry_of(vt, new_entries, &req.new_types) {
                let water = entry.ct.meta.drive.as_ref().is_some_and(|d| d.on == "water" || d.on == "under");
                let t = self.terrain.clone();
                let spots: Vec<(f32, f32)> = (0..400)
                    .map(|k| {
                        let a = k as f32 * 2.399;
                        let d = (k as f32).sqrt() * 6.0;
                        (st.x + a.cos() * d, st.z + a.sin() * d)
                    })
                    .filter(|(x, z)| if water { t.water(*x, *z) - t.height(*x, *z) > 0.9 } else { t.height(*x, *z) > t.water(*x, *z) + 0.3 })
                    // A boat is moored by the shore, where people step in.
                    .filter(|(x, z)| !water || (0..8).any(|k| {
                        let a = k as f32 * std::f32::consts::FRAC_PI_4;
                        let (qx, qz) = (x + a.cos() * 2.6, z + a.sin() * 2.6);
                        t.height(qx, qz) > t.water(qx, qz) + 0.2
                    }))
                    .collect();
                for (k, (x, z)) in spots.into_iter().take(30).enumerate() {
                    let pl = Placement { ty: vt.clone(), x, z, y: None, under: false, rot_y: k as f32 * 1.3, scale: 1.0, params: [600.0, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5] };
                    if let Ok(p) = self.check_place(&pl, &entry, &interior, placed, "the settlement's vehicle") {
                        placed.push((p, entry.clone()));
                        break;
                    }
                }
            }
        }
        (roads, laid)
    }

    pub fn type_names(&self) -> Vec<(u32, String, Vec<String>, [f32; 3])> {
        self.active_types().iter().filter(|t| !t.entry.has_tag("figure") && t.entry.ct.meta.body.is_none()).map(|t| (t.entry.id, t.entry.name().to_string(), t.entry.ct.meta.tags.clone(), t.entry.ct.meta.bounds)).collect()
    }

    fn interior_of(&mut self, id: u32) -> Vec<[f32; 3]> {
        let Some(t) = self.types.get_mut(&id) else { return Vec::new() };
        if t.interior.is_empty() {
            if let Ok(rep) = probe(&t.entry.ct) {
                t.interior = rep.interior;
            }
        }
        t.interior.clone()
    }

    // ---- step 5: placement ----

    /// For a sizeable thing seated on a slope (more than `MAX_DROP` of ground
    /// drop under it), a nearby spot where the ground is flatter, if any.
    fn flatter_spot(&self, pl: &Placement, entry: &Arc<TypeEntry>) -> Option<Placement> {
        const MAX_DROP: f32 = 1.5;
        let b = entry.ct.meta.bounds;
        let r = b[0].max(b[2]) * pl.scale.clamp(0.2, 6.0);
        if pl.y.is_some() || !entry.solid || r < 1.5 || entry.ct.meta.hollow.is_some() || entry.ct.meta.in_ground.is_some() {
            return None;
        }
        let t = &self.terrain;
        let drop = |p: &Placement| {
            let (lo, hi) = footprint(t, p, entry);
            hi - lo
        };
        let start = drop(pl);
        if start <= MAX_DROP {
            return None;
        }
        let mut best = (start, None);
        for ring in [0.5f32, 1.0, 1.5] {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::FRAC_PI_4 + ring;
                let mut p2 = pl.clone();
                p2.x += a.cos() * r * ring;
                p2.z += a.sin() * r * ring;
                if t.height(p2.x, p2.z) < t.water(p2.x, p2.z) - 0.6 {
                    continue;
                }
                let d = drop(&p2);
                if d < best.0 - 0.3 {
                    best = (d, Some(p2));
                }
            }
        }
        best.1
    }

    /// The top of whatever solid placed thing stands in the column at (x, z)
    /// between `from` and `to` (heights, `from` above): what a thing set up
    /// there would stand on. Earlier placements of this commit count.
    fn support_below(&self, x: f32, z: f32, from: f32, to: f32, extra: &[(Placed, Arc<TypeEntry>)]) -> Option<f32> {
        let mid = Vec3::new(x, (from + to) * 0.5, z);
        let reach = (from - to) * 0.5;
        let cands: Vec<(GpuInst, Arc<TypeEntry>)> = self
            .insts
            .iter()
            .filter(|i| self.inst_active(i))
            .filter_map(|i| self.types.get(&i.placed.type_id).map(|t| (i.placed.clone(), t.entry.clone())))
            .chain(extra.iter().cloned())
            .filter(|(p, e)| e.solid && (p.pos + Vec3::Y * e.sphere_cy * p.scale - mid).length() < e.sphere_r * p.scale + reach + 1.0)
            .map(|(p, e)| (p.gpu(&e, 1.0), e))
            .collect();
        if cands.is_empty() {
            return None;
        }
        let dist = |y: f32| cands.iter().map(|(g, e)| e.ct.sdf(g.to_local(Vec3::new(x, y, z)), &g.k()) * g.pos_scale[3]).fold(f32::MAX, f32::min);
        let mut y = from;
        while y > to {
            let d = dist(y);
            if d < 0.01 {
                // Up out of it again, to its top.
                let mut top = y;
                while dist(top + 0.02) < 0.01 && top < from {
                    top += 0.02;
                }
                return Some(top);
            }
            y -= d.clamp(0.01, 0.3);
        }
        None
    }

    /// Check one placement against the terrain, the player, characters, the
    /// spawn point and existing instances. Returns the resolved instance.
    fn check_place(&mut self, pl: &Placement, entry: &Arc<TypeEntry>, interior: &[[f32; 3]], extra: &[(Placed, Arc<TypeEntry>)], label: &str) -> Result<Placed, String> {
        let t = self.terrain.clone();
        let scale = pl.scale.clamp(0.2, 6.0);
        // Set into the ground: its floor under the ground before its doorway,
        // turned to the open side of a hill (a height given with it is not
        // for it: it goes into the ground, not over it).
        let seated = (entry.ct.meta.in_ground.is_some() && !pl.under).then(|| crate::inground::seat(&t, entry, &self.solid_box(entry), pl.x, pl.z, pl.rot_y, scale));
        let rot_y = seated.map(|s| s.1).unwrap_or(pl.rot_y);
        let (c, s) = (rot_y.cos(), rot_y.sin());
        let (lo, hi) = footprint(&t, pl, entry);
        let ground = t.height(pl.x, pl.z);
        // On a cave's floor: stood there (the land above doesn't matter).
        if pl.under && pl.y.is_none() {
            if let Some(floor) = t.cave_floor_under(pl.x, pl.z) {
                let mut p2 = pl.clone();
                p2.y = Some(floor - entry.bottom * pl.scale.clamp(0.2, 6.0) + 0.02);
                p2.under = false;
                return self.check_place(&p2, entry, interior, extra, label);
            }
        }
        let y = match pl.y {
            _ if let Some((y, _)) = seated => y,
            // A shape that takes ground away (a cellar, a house in a hillside)
            // stands with its floor (y = 0) on the lowest ground under it;
            // the hollow clears the rest.
            None if entry.ct.meta.hollow.is_some() => lo.min(ground) - 0.02,
            // Seat on the lowest point, raised a little on slopes so less of the
            // uphill side is buried (the downhill gap stays small).
            None => lo.min(ground) + ((hi - lo) * 0.3).min(0.4) - entry.bottom * scale - 0.12,
            // In a cave, on its floor: the land above doesn't matter.
            Some(y) if t.void(Vec3::new(pl.x, y + entry.bottom * scale + 0.3, pl.z)) < 0.0 => y,
            // A world written by hand stands it exactly where it says (its
            // y = 0 at that level: on a deck, piles sunk into the sand).
            Some(y) if crate::brain::authored() => y,
            Some(y) => {
                let base = y + entry.bottom * scale;
                if base - hi > 0.5 {
                    // Held up there by something standing already (a platform
                    // in a tree, a ledge, stilts, a roof): set on its top.
                    match self.support_below(pl.x, pl.z, base + 0.8, base - 1.5, extra) {
                        Some(top) => top - entry.bottom * scale + 0.01,
                        None => return Err(format!("{label} floats {:.1} m above the ground with nothing under it (its base is at y={:.1}, the terrain at y={:.1}); stand it on something placed before it (a platform, a branch, a ledge, stilts), or omit its height to sit it on the ground", base - hi, base, hi)),
                    }
                } else if lo - base > 3.0 {
                    return Err(format!("{label} is buried {:.1} m below the ground", lo - base));
                } else {
                    y
                }
            }
        };
        // A boat set on water floats on it (as `vehicle` keeps it).
        let afloat = entry.ct.meta.drive.as_ref().is_some_and(|d| d.on == "water");
        let wl = t.water(pl.x, pl.z);
        let y = if afloat && pl.y.is_none() && ground < wl - 0.1 { wl - ((entry.top - entry.bottom) * scale * 0.25).clamp(0.1, 1.0) - entry.bottom * scale } else { y };
        let under = entry.ct.meta.drive.as_ref().is_some_and(|d| d.on == "under");
        let water_ok = afloat || under || entry.ct.meta.tags.iter().any(|t| matches!(t.as_str(), "water" | "boat" | "bridge" | "pier"));
        // (Something up on a pier or a jetty over the water stands dry.)
        let over = y + entry.bottom * scale > wl + 0.3;
        if ground < wl - 0.6 && !water_ok && !over {
            return Err(format!("{label} would stand in deep water at ({:.0}, {:.0})", pl.x, pl.z));
        }
        let mut params = pl.params;
        params[1] = scale;
        let placed = Placed { id: 0, type_id: entry.id, pos: Vec3::new(pl.x, y, pl.z), rot_y, scale, params, version: 0 };
        let gi = placed.gpu(entry, 1.0);
        let sdf_at = |p: Vec3| entry.ct.sdf(gi.to_local(p), &gi.k()) * scale;
        let live = self.live.lock().clone();
        // Small things (a ball, a lamp) may appear right next to people.
        let small = entry.ct.meta.bounds.iter().fold(0.0f32, |a, b| a.max(*b)) * scale <= 0.6;
        if entry.solid && !small {
            let pp = live.player;
            for h in [0.4, 1.0, 1.6] {
                if sdf_at(Vec3::new(pp.x, t.height(pp.x, pp.z) + h, pp.z)) < 0.6 {
                    return Err(format!("{label} would overlap the player"));
                }
            }
            for cpos in &live.characters {
                if sdf_at(*cpos + Vec3::Y * 0.9) < 0.4 {
                    return Err(format!("{label} would overlap a character"));
                }
            }
            let sp = self.spawn + Vec3::Y * 0.9;
            if sdf_at(sp) < 1.0 {
                return Err(format!("{label} would block the spawn point"));
            }
        }
        // Existing story instances (and earlier placements in this commit).
        let world_pts: Vec<Vec3> = interior
            .iter()
            .map(|p| {
                let lx = p[0] * scale;
                let lz = p[2] * scale;
                Vec3::new(pl.x + c * lx + s * lz, y + p[1] * scale, pl.z - s * lx + c * lz)
            })
            .collect();
        let r_new = entry.radius() * scale;
        let others: Vec<(Placed, Arc<TypeEntry>)> = self
            .insts
            .iter()
            .filter(|i| self.inst_active(i))
            .filter_map(|i| self.types.get(&i.placed.type_id).map(|t| (i.placed.clone(), t.entry.clone())))
            .chain(extra.iter().cloned())
            .collect();
        for (o, oe) in &others {
            if !oe.solid || !entry.solid {
                continue;
            }
            let ro = oe.radius() * o.scale;
            if (o.pos - placed.pos).length() > ro + r_new {
                continue;
            }
            let og = o.gpu(oe, 1.0);
            let ok_k = og.k();
            for p in &world_pts {
                let d = oe.ct.sdf(og.to_local(*p), &ok_k) * o.scale;
                if d < -0.3 {
                    return Err(format!("{label} would overlap the existing {} at ({:.0}, {:.0})", oe.name(), o.pos.x, o.pos.z));
                }
            }
        }
        Ok(placed)
    }

    /// What comes with a placed shape, by its anchors: a door hung in each
    /// doorway, what sits in each slot (a sword on its rack, a pot on the
    /// shelf), each fixed part. Types are found by name among this commit's
    /// new ones and the world's.
    fn children(&self, p: &Placed, entry: &Arc<TypeEntry>, new_entries: &[Arc<TypeEntry>]) -> Vec<(Placed, Arc<TypeEntry>)> {
        let mut out = Vec::new();
        if entry.ct.meta.anchors.is_empty() {
            return out;
        }
        let by_name = |n: &str| -> Option<Arc<TypeEntry>> {
            let n = n.trim().to_lowercase();
            let exact = |e: &TypeEntry| e.name().to_lowercase() == n;
            // A near name will do ("old iron sword" for "iron sword").
            let near = |e: &TypeEntry| {
                let m = e.name().to_lowercase();
                n.len() >= 3 && (m.contains(&n) || n.contains(&m)) && e.ct.meta.body.is_none() && !e.has_tag("building")
            };
            new_entries.iter().rev().find(|e| exact(e)).cloned()
                .or_else(|| self.types.values().filter(|t| exact(&t.entry) && t.entry.ct.meta.body.is_none()).max_by_key(|t| (t.entry.builtin, t.entry.id)).map(|t| t.entry.clone()))
                .or_else(|| new_entries.iter().rev().find(|e| near(e)).cloned())
                .or_else(|| self.types.values().filter(|t| near(&t.entry)).max_by_key(|t| t.entry.id).map(|t| t.entry.clone()))
        };
        let g = p.gpu(entry, 1.0);
        for (i, a) in entry.ct.meta.anchors.iter().enumerate() {
            let at = g.from_local(Vec3::from_array(a.at));
            let yaw = p.rot_y + a.face.to_radians();
            let seed = p.params[0] * 31.0 + i as f32 + 1.0;
            match a.kind.as_str() {
                "door" | "hatch" => {
                    // Its own door (a round door, an iron gate, a stone slab, a
                    // hide flap: anything on a hinge), or the world's.
                    let own = a.holds.as_deref().and_then(by_name).filter(|d| d.ct.meta.joint.is_some());
                    let Some(door) = own.clone().or_else(|| by_name(&a.kind).filter(|d| d.ct.meta.joint.is_some())) else { continue };
                    let [w, h] = a.size.unwrap_or([1.0, 2.0]).map(|v| v * p.scale);
                    let params = match (&own, a.kind.as_str()) {
                        (Some(_), _) => [seed, 1.0, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5],
                        (None, "door") => [seed, 1.0, (w / 2.0).clamp(0.2, 1.1), (h / 4.0).clamp(0.15, 0.85), 0.5, 0.5, 0.5, 0.5],
                        // A hatch: width and depth.
                        _ => [seed, 1.0, (w / 2.0).clamp(0.2, 1.1), (h / 2.0).clamp(0.2, 1.0), 0.5, 0.5, 0.5, 0.5],
                    };
                    let o = if a.kind == "door" {
                        // Hinged at its left edge, seen from outside.
                        let right = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
                        at - right * (w * 0.5)
                    } else {
                        // Hinged along its back edge, lying flat, opening toward `face`.
                        let fwd = Vec3::new(yaw.sin(), 0.0, yaw.cos());
                        at - fwd * (h * 0.5)
                    };
                    out.push((Placed { id: 0, type_id: door.id, pos: o, rot_y: yaw, scale: 1.0, params, version: 0 }, door));
                }
                "slot" | "part" => {
                    let Some(ty) = a.holds.as_deref().and_then(by_name) else { continue };
                    let scale = if a.kind == "part" { p.scale } else { 1.0 };
                    let pos = Vec3::new(at.x, at.y - ty.bottom * scale + 0.01, at.z);
                    let params = [seed, scale, 0.5, 0.5, 0.5, 0.5, 0.5, 0.5];
                    out.push((Placed { id: 0, type_id: ty.id, pos, rot_y: yaw, scale, params, version: 0 }, ty));
                }
                _ => {}
            }
        }
        out
    }

    // ---- step 6 + commit + flip ----

    pub fn commit(&mut self, mut req: CommitRequest) -> Result<CommitOk, Vec<Diag>> {
        // Regions planned side by side can both dream up the same person:
        // the one already living in the land stays, the copy is dropped.
        if let Some(rc) = req.region.as_mut() {
            let taken: HashSet<String> = self.chars.iter().filter(|c| c.persona.species.is_empty()).map(|c| c.persona.name.trim().to_lowercase()).collect();
            rc.characters.retain(|(p, _)| !p.species.is_empty() || !taken.contains(&p.name.trim().to_lowercase()));
        }
        let res = self.commit_inner(req);
        if res.is_err() {
            // A settlement's ground laid for a commit that failed goes again.
            self.apply_ground();
        }
        res
    }

    fn commit_inner(&mut self, req: CommitRequest) -> Result<CommitOk, Vec<Diag>> {
        let reshape = is_reshape(req.kind, &req.summary);
        // Species the brain stored for this commit travel with its snapshot.
        self.species = Arc::new(crate::world::species::SpeciesBook::load(&self.db.with(|c| crate::db::species_rows(c)).unwrap_or_default(), self.db.kv_get("species.world").as_deref()));
        // Provisional entries for new types (ids assigned after the DB insert).
        // Provisional ids match what SQLite will assign (failed types also take ids).
        let db_max = self.db.with(|c| Ok(c.query_row("SELECT COALESCE(MAX(id), 0) FROM types", [], |r| r.get::<_, i64>(0))?)).unwrap_or(0) as u32;
        let mut next_id = db_max.max(self.types.keys().max().copied().unwrap_or(0)) + 1;
        let mut new_entries: Vec<Arc<TypeEntry>> = Vec::new();
        for nt in &req.new_types {
            new_entries.push(type_entry(next_id, nt.ct.clone(), nt.report.bottom, nt.report.top, false));
            next_id += 1;
        }
        // The settlement's ground first: what is placed stands on it.
        if let Some((r, st)) = req.region.as_ref().and_then(|r| r.settlement.as_ref().map(|s| (r.r, s))) {
            if !st.ground.is_empty() {
                let plans: Vec<crate::settle::LandPlan> = st.ground.iter().map(|g| g.0.clone()).collect();
                let others = self.places_edits(Some(r));
                let base = self.base_land(&others);
                let mut edits = others;
                edits.extend(crate::settle::realize(&plans, &[], &base, self.terrain.sea));
                self.terrain.set_ground(crate::ground::Layer::Places, edits);
            }
            if !st.caves.is_empty() {
                let mut caves = self.places_caves(Some(r));
                caves.extend(st.caves.iter().map(|c| c.cave(self.terrain.height(c.mouth.x, c.mouth.y))));
                self.terrain.set_caves(crate::ground::Layer::Places, caves);
            }
        }
        // Step 5: placement.
        let mut placed: Vec<(Placed, Arc<TypeEntry>)> = Vec::new();
        let mut dropped = Vec::new();
        let mut diags = Vec::new();
        for (i, pl) in req.placements.iter().enumerate() {
            let (entry, interior) = match &pl.ty {
                TypeRef::New(k) => match (new_entries.get(*k), req.new_types.get(*k)) {
                    (Some(e), Some(nt)) => (e.clone(), nt.report.interior.clone()),
                    _ => {
                        diags.push(Diag::new(Stage::Place, 0, format!("placement {} refers to a missing type", i + 1)));
                        continue;
                    }
                },
                TypeRef::Existing(id) => match self.types.get(id) {
                    Some(t) => {
                        let e = t.entry.clone();
                        (e, self.interior_of(*id))
                    }
                    None => {
                        diags.push(Diag::new(Stage::Place, 0, format!("placement {} refers to unknown type id {id}", i + 1)));
                        continue;
                    }
                },
            };
            let label = format!("the {} (placement {})", entry.name(), i + 1);
            let flatter = self.flatter_spot(pl, &entry);
            let mut result = match &flatter {
                Some(p2) => self.check_place(p2, &entry, &interior, &placed, &label).or_else(|_| self.check_place(pl, &entry, &interior, &placed, &label)),
                None => self.check_place(pl, &entry, &interior, &placed, &label),
            };
            if result.is_err() && req.nudge {
                let r = entry.radius() * pl.scale;
                for k in 1..=14 {
                    let a = k as f32 * 2.399;
                    let d = (1.0 + k as f32 * 0.5) * r.max(2.0);
                    let mut p2 = pl.clone();
                    p2.x += a.cos() * d;
                    p2.z += a.sin() * d;
                    p2.y = None;
                    if let Ok(ok) = self.check_place(&p2, &entry, &interior, &placed, &label) {
                        result = Ok(ok);
                        break;
                    }
                }
            }
            match result {
                Ok(p) => {
                    let kids = self.children(&p, &entry, &new_entries);
                    placed.push((p, entry));
                    placed.extend(kids);
                }
                Err(e) if req.nudge => dropped.push(e),
                Err(e) => diags.push(Diag::new(Stage::Place, 0, e)),
            }
        }
        if !diags.is_empty() {
            return Err(diags);
        }
        // A settlement's ground, what lines its ways, its roads and lamps,
        // its bridges and its vehicle.
        let mut region_plan = req.region.as_ref().map(|r| r.plan_json.clone());
        let mut region_roads = Vec::new();
        let mut region_edits = Vec::new();
        let mut region_caves = Vec::new();
        if let Some((r, st)) = req.region.as_ref().and_then(|r| r.settlement.as_ref().map(|s| (r.r, s))) {
            let others = self.places_caves(Some(r));
            region_caves = self.terrain.carve.read().ground.cave_layer(crate::ground::Layer::Places).iter().filter(|c| !others.contains(c)).cloned().collect();
            let built = self.settle(r, st, &req, &new_entries, &mut placed);
            region_roads = built.0;
            region_edits = built.1;
            if let Some(plan) = region_plan.as_mut() {
                let mut v: serde_json::Value = serde_json::from_str(plan).unwrap_or_default();
                if let Some(o) = v.as_object_mut() {
                    if !region_roads.is_empty() {
                        o.insert("roads".into(), serde_json::to_value(&region_roads).unwrap_or_default());
                    }
                    if !region_edits.is_empty() {
                        o.insert("edits".into(), serde_json::to_value(&region_edits).unwrap_or_default());
                    }
                    if !region_caves.is_empty() {
                        o.insert("caves".into(), serde_json::to_value(&region_caves).unwrap_or_default());
                    }
                }
                *plan = v.to_string();
            }
        }

        // Step 6: build the next full shader and check GPU/CPU parity.
        let mut look = self.look.clone();
        let mut terrain = self.terrain.clone();
        if let Some(l) = &req.look {
            look = Arc::new(l.clone());
            terrain = Arc::new(Terrain::new(self.seed, l.biomes.clone()).with_water(l.sea, l.groundwater).with_lands(&l.landforms));
            terrain.set_rivers(l.rivers, l.palette.sand, l.caves);
        }
        let cur: Vec<TypeRec> = self.active_types().into_iter().cloned().collect();
        let mut all: Vec<TypeRec> = cur.clone();
        for e in &new_entries {
            all.push(TypeRec { entry: e.clone(), version: Some(-1), interior: Vec::new(), born: None });
        }
        let refs: Vec<&TypeRec> = all.iter().collect();
        let pipeline = self.pipeline_for(&refs)?;
        if let (Some(gpu), Some(pipe)) = (&self.gpu, &pipeline) {
            for (e, nt) in new_entries.iter().zip(&req.new_types) {
                gpu_parity(gpu, pipe, &terrain, e.id, &nt.ct)?;
            }
        }

        // A new look changes the terrain: pick a new spawn point.
        let new_spawn = req.look.as_ref().map(|_| {
            let s = find_first_spawn(&terrain);
            terrain.lay_rivers(s.x, s.z);
            afloat(&terrain, s)
        });

        // Commit to SQLite as one new version.
        let parent = self.current;
        // Characters live beside their houses, never inside them.
        // (Beings that live in water keep the water they were put in.)
        let wet = |p: &Persona| !p.species.is_empty() && self.species.get(&p.species).is_some_and(|s| s.water > 0.0);
        // (Someone a world written by hand stood up on something stays there.)
        let homes: Vec<Vec3> = req.region.as_ref().map(|rc| rc.characters.iter().map(|(p, h)| if h.y != 0.0 { *h } else if wet(p) { self.clear_spot(*h, &placed, true) } else { self.clear_spot(dry_near(&terrain, *h), &placed, false) }).collect()).unwrap_or_default();
        let region = req.region.as_ref();
        let res = self.db.tx(|tx| {
            let v = db::add_version(tx, parent, req.kind, &req.summary, None)?;
            let mut ids = Vec::new();
            for (nt, e) in req.new_types.iter().zip(&new_entries) {
                let id = db::add_type(tx, Some(v), &nt.ct.meta.name, &nt.ct.source, &meta_json(&nt.ct, nt.report.bottom, nt.report.top), "ok", "")?;
                ids.push((e.id, id as u32));
            }
            let mut inst_ids = Vec::new();
            for (p, _) in &placed {
                let tid = ids.iter().find(|(prov, _)| *prov == p.type_id).map(|x| x.1).unwrap_or(p.type_id);
                let id = db::add_instance(tx, tid as i64, v, p.pos.x, p.pos.y, p.pos.z, p.rot_y, p.scale, &p.params)?;
                inst_ids.push((id, tid));
            }
            let mut char_ids = Vec::new();
            if let Some(rc) = region {
                for ((persona, _), home) in rc.characters.iter().zip(&homes) {
                    let id = db::add_character(tx, rc.r, &serde_json::to_string(persona)?, home.x, home.z, v)?;
                    if home.y != 0.0 {
                        let st = crate::world::characters::SavedState { x: home.x, z: home.z, y: Some(home.y), ..Default::default() };
                        tx.execute("UPDATE characters SET state_json = ?1 WHERE id = ?2", rusqlite::params![serde_json::to_string(&st)?, id])?;
                    }
                    char_ids.push(id);
                }
                db::set_region(tx, rc.r.0, rc.r.1, "done", region_plan.as_deref().unwrap_or(&rc.plan_json), Some(v))?;
            }
            if let Some(l) = &req.look {
                tx.execute("UPDATE universe SET palette = ?1 WHERE id = 1", [serde_json::to_string(l)?])?;
            }
            if let Some(s) = new_spawn {
                db::kv_set(tx, "spawn", &serde_json::to_string(&s.to_array())?)?;
            }
            Ok((v, ids, inst_ids, char_ids))
        });
        let (v, ids, inst_ids, char_ids) = res.map_err(|e| vec![Diag::new(Stage::Gpu, 0, format!("database commit failed: {e}"))])?;

        // Apply to memory. Provisional type ids may differ from the DB's; if so,
        // the pipeline must be rebuilt with the real ids.
        let mut remap = false;
        for ((prov, real), e) in ids.iter().zip(&new_entries) {
            if prov != real {
                remap = true;
            }
            let entry = Arc::new(TypeEntry { id: *real, ct: e.ct.clone(), builtin: false, solid: e.solid, bottom: e.bottom, top: e.top, sphere_cy: e.sphere_cy, sphere_r: e.sphere_r });
            self.types.insert(*real, TypeRec { entry, version: Some(v), interior: Vec::new(), born: Some(Instant::now()) });
        }
        for ((p, _), (id, tid)) in placed.iter().zip(&inst_ids) {
            let mut p = p.clone();
            p.id = *id;
            p.type_id = *tid;
            p.version = v;
            self.insts.push(InstRec { placed: p, removed: None });
        }
        if let Some(rc) = req.region {
            for (((persona, _), home), id) in rc.characters.into_iter().zip(&homes).zip(char_ids) {
                let h = Vec3::new(home.x, terrain.height(home.x, home.z), home.z);
                self.chars.push(Arc::new(CharacterDef { id, persona, home: h, state: SavedState::default(), version: v }));
            }
            let mut info = rc.info;
            info.roads = region_roads;
            info.ground = region_edits;
            info.caves = region_caves;
            self.regions.insert(rc.r, Arc::new(info));
        }
        if req.look.is_some() {
            self.look = look;
            self.terrain = terrain;
        }
        if let Some(s) = new_spawn {
            self.spawn = s;
        }
        self.current = Some(v);
        if reshape {
            self.reshape_versions.insert(v);
        }
        let snapshot = if remap {
            self.snapshot()?
        } else {
            let types: Vec<TypeRec> = self.active_types().into_iter().cloned().collect();
            let refs: Vec<&TypeRec> = types.iter().collect();
            self.make_snapshot(&refs, pipeline)
        };
        Ok(CommitOk { snapshot, placed: placed.len(), dropped })
    }

    /// Revert the most recent create that is still in effect.
    pub fn undo(&mut self) -> Result<(Arc<WorldSnapshot>, String), String> {
        let versions = self.db.versions().map_err(|e| e.to_string())?;
        let target = versions.iter().rev().find(|v| v.kind == "create" && !self.reverted.contains(&v.id));
        let Some(target) = target else { return Err("nothing to undo".into()) };
        let tid = target.id;
        let summary = target.summary.clone();
        let parent = self.current;
        let v = self.db.tx(|tx| db::add_version(tx, parent, "undo", &format!("undo: {summary}"), Some(tid))).map_err(|e| e.to_string())?;
        self.reverted.insert(tid);
        self.current = Some(v);
        match self.snapshot() {
            Ok(s) => Ok((s, summary)),
            Err(d) => Err(format_diags(&d)),
        }
    }

    pub fn history(&self) -> Vec<String> {
        let Ok(vs) = self.db.versions() else { return vec![] };
        vs.iter()
            .rev()
            .take(30)
            .map(|v| {
                let mark = if self.reverted.contains(&v.id) { " (undone)" } else { "" };
                let cur = if Some(v.id) == self.current { "▸" } else { " " };
                let age = crate::db::now() - v.created_at;
                let ago = if age < 90.0 {
                    format!("{age:.0}s ago")
                } else if age < 5400.0 {
                    format!("{:.0}m ago", age / 60.0)
                } else if age < 172800.0 {
                    format!("{:.0}h ago", age / 3600.0)
                } else {
                    format!("{:.0}d ago", age / 86400.0)
                };
                format!("{cur}#{} {:<7} {}{}  ({ago})", v.id, v.kind, v.summary, mark)
            })
            .collect()
    }

    pub fn region_names_near(&self, r: (i32, i32)) -> Vec<String> {
        let mut v = Vec::new();
        for dz in -1..=1 {
            for dx in -1..=1 {
                if let Some(i) = self.regions.get(&(r.0 + dx, r.1 + dz)) {
                    v.push(format!("{} ({})", i.name, i.mood));
                }
            }
        }
        v
    }

}

pub fn region_info_from_plan(plan_json: &str) -> RegionInfo {
    let v: serde_json::Value = serde_json::from_str(plan_json).unwrap_or_default();
    RegionInfo {
        name: v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        mood: v.get("mood").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        facts: v.get("facts").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|f| f.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        roads: v.get("roads").and_then(|x| serde_json::from_value(x.clone()).ok()).unwrap_or_default(),
        ground: v.get("edits").and_then(|x| serde_json::from_value(x.clone()).ok()).unwrap_or_default(),
        caves: v.get("caves").and_then(|x| serde_json::from_value(x.clone()).ok()).unwrap_or_default(),
    }
}

/// Globals sufficient for probe dispatches.
pub fn probe_globals(terrain: &Terrain) -> crate::render::Globals {
    let pal = crate::terrain::Palette::default();
    let light = crate::render::sky::lighting(400.0, &pal);
    let sp = crate::render::SceneParams {
        terrain,
        palette: &pal,
        camera: crate::render::Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0, roll: 0.0 },
        width: 1,
        height: 1,
        pixel_aspect: 1.0,
        light,
        time: 0.0,
        frame: 0,
        shadows: false,
        lights: &[],
        weather: Default::default(),
    };
    crate::render::build_globals(&sp, 1, None)
}

pub const PARITY_TOL: f32 = 1e-3;

/// Step 6b: the type's sdf must agree on GPU and CPU at the probe points.
pub fn gpu_parity(gpu: &Gpu, pipe: &ScenePipeline, terrain: &Terrain, tid: u32, ct: &CompiledType) -> Result<(), Vec<Diag>> {
    let pts = sample_points(ct.meta.bounds);
    let k = default_k(0.37);
    let inst = GpuInst { k0: [k[0], k[1], k[2], k[3]], k1: [k[4], k[5], k[6], k[7]], ..Default::default() };
    let input: Vec<[f32; 4]> = pts.iter().map(|p| [p[0], p[1], p[2], 0.0]).collect();
    let out = gpu.probe(pipe, &probe_globals(terrain), 0, tid, &inst, &input).map_err(|e| vec![Diag::new(Stage::Gpu, 0, format!("GPU probe failed: {e}"))])?;
    let mut worst = 0.0f32;
    let mut at = [0.0; 3];
    for (p, g) in pts.iter().zip(&out) {
        let c = ct.sdf(*p, &k);
        let err = (g[0] - c).abs() / c.abs().max(1.0);
        if !(err <= PARITY_TOL) {
            if !(err <= worst) {
                worst = err;
                at = *p;
            }
        }
    }
    if worst > 0.0 {
        return Err(vec![Diag::new(Stage::Gpu, 0, format!("GPU/CPU sdf mismatch for {}: relative error {worst:.2e} at ({:.2}, {:.2}, {:.2})", ct.meta.name, at[0], at[1], at[2]))]);
    }
    Ok(())
}

/// Lowest and highest ground under a placement's footprint (corners and centre).
fn footprint(t: &Terrain, pl: &Placement, entry: &TypeEntry) -> (f32, f32) {
    let scale = pl.scale.clamp(0.2, 6.0);
    let b = entry.ct.meta.bounds;
    let (c, s) = (pl.rot_y.cos(), pl.rot_y.sin());
    let mut lo = f32::MAX;
    let mut hi = f32::MIN;
    for (fx, fz) in [(0.0, 0.0), (1.0, 1.0), (1.0, -1.0), (-1.0, 1.0), (-1.0, -1.0)] {
        let lx = fx * b[0] * scale * 0.7;
        let lz = fz * b[2] * scale * 0.7;
        // local → world is the inverse of to_local
        let wx = pl.x + c * lx + s * lz;
        let wz = pl.z - s * lx + c * lz;
        let h = t.height(wx, wz);
        lo = lo.min(h);
        hi = hi.max(h);
    }
    (lo, hi)
}

/// A commit that reshaped one thing (see `brain::edit_item_type`).
fn is_reshape(kind: &str, summary: &str) -> bool {
    kind == "interp" && summary.starts_with("reshaped: ")
}

/// The ground a shape takes away: its `meta.hollow`, or, for a shape with
/// a doorway, the ground under its floor out to its walls (so no slope pokes
/// through it). None for a shape set into the ground (it takes a hollow of
/// its own, see `inground`).
pub fn ground_taken(m: &crate::lang::ir::Meta, solid: impl FnOnce() -> crate::inground::SolidBox) -> Option<crate::lang::ir::Hollow> {
    if m.in_ground.is_some() {
        return None;
    }
    if let Some(h) = &m.hollow {
        return Some(h.clone());
    }
    let door = m.anchors.iter().find(|a| a.kind == "door")?;
    let b = solid();
    // Centred on the origin: out to the nearer wall on each axis.
    let half = [b.hi.x.min(-b.lo.x), b.hi.z.min(-b.lo.z)];
    Some(crate::lang::ir::Hollow { half, floor: door.at[1] - 0.04 })
}

/// How far a shape's cut slopes in from its edge (m): steep, and inside its
/// box, so the ground meets its walls with no gap.
const SHAPE_EDGE: f32 = 0.1;

/// The ground a placed shape takes away (its `meta.hollow`), in the world.
pub fn hollow_of(p: &Placed, h: &crate::lang::ir::Hollow) -> crate::terrain::Hollow {
    crate::terrain::Hollow {
        c: [p.pos.x, p.pos.z],
        rot: [p.rot_y.cos(), p.rot_y.sin()],
        half: [(h.half[0] * p.scale - SHAPE_EDGE).max(0.05), (h.half[1] * p.scale - SHAPE_EDGE).max(0.05)],
        floor: p.pos.y + h.floor * p.scale,
        round: false,
        edge: SHAPE_EDGE,
    }
}

/// Where a building is walked up to: outside its door, or before its front.
pub fn front_of(p: &Placed, e: &TypeEntry) -> Vec3 {
    let g = p.gpu(e, 1.0);
    match e.ct.meta.anchors.iter().find(|a| a.kind == "door") {
        Some(a) => g.from_local(Vec3::from_array(a.at) + Vec3::from_array(a.out()) * (1.2 / p.scale.max(0.1))),
        None => g.from_local(Vec3::new(0.0, 0.0, e.ct.meta.bounds[2] + 1.2 / p.scale.max(0.1))),
    }
}

/// A road's colour from the land's: trodden earth between sand and rock.
fn road_color(pal: &crate::terrain::Palette) -> [f32; 3] {
    let s = crate::terrain::rgbv(pal.sand);
    let r = crate::terrain::rgbv(pal.rock);
    (s * 0.55 + r * 0.45).to_array()
}

/// Roads joining every front to its nearest neighbours (a spanning tree),
/// and the longest few loops it leaves out skipped. Deterministic.
pub fn road_net(fronts: &[Vec3], color: [f32; 3], half: f32) -> Vec<crate::terrain::Road> {
    let n = fronts.len();
    if n < 2 {
        return Vec::new();
    }
    let mut inside = vec![false; n];
    let mut best = vec![(f32::MAX, 0usize); n];
    inside[0] = true;
    for j in 1..n {
        best[j] = ((fronts[j] - fronts[0]).length(), 0);
    }
    let mut out = Vec::new();
    for _ in 1..n {
        let Some(j) = (0..n).filter(|j| !inside[*j]).min_by(|a, b| best[*a].0.total_cmp(&best[*b].0)) else { break };
        inside[j] = true;
        let (d, from) = best[j];
        if d < 140.0 {
            let (a, b) = (fronts[from], fronts[j]);
            out.push(crate::terrain::Road { a: [a.x, a.z], b: [b.x, b.z], half: half.clamp(0.4, 4.0), color });
        }
        for k in 0..n {
            if !inside[k] {
                let dk = (fronts[k] - fronts[j]).length();
                if dk < best[k].0 {
                    best[k] = (dk, j);
                }
            }
        }
    }
    out
}

/// Spots beside the roads for lamps: about every 16 m, alternating sides.
pub fn lamp_spots(roads: &[crate::terrain::Road]) -> Vec<(f32, f32)> {
    let mut out: Vec<(f32, f32)> = Vec::new();
    for (i, r) in roads.iter().enumerate() {
        let (ax, az, bx, bz) = (r.a[0], r.a[1], r.b[0], r.b[1]);
        let len = (bx - ax).hypot(bz - az);
        if len < 4.0 {
            continue;
        }
        let (dx, dz) = ((bx - ax) / len, (bz - az) / len);
        let k = ((len - 4.0) / 16.0).floor() as usize + 1;
        for j in 0..k {
            let t = (2.0 + j as f32 * 16.0).min(len - 2.0);
            let side = if (i + j) % 2 == 0 { 1.0 } else { -1.0 };
            let off = r.half + 0.8;
            let p = (ax + dx * t - dz * off * side, az + dz * t + dx * off * side);
            if out.iter().all(|q| (q.0 - p.0).hypot(q.1 - p.1) > 8.0) {
                out.push(p);
            }
        }
    }
    out
}

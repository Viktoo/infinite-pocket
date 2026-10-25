//! The living world: actors, live things, properties and rules, behaviour
//! code, characters' minds and their life together. Rendering, terminals and
//! LLM calls live elsewhere; the simulation talks to them through requests
//! (`out`) and notes (`notes`), so the same `Sim` runs inside the game, in
//! `pocket sim` (headless, fast-forward, seeded) and under `pocket act`.
//!
//! Determinism: everything iterates in id order and draws randomness from one
//! seeded generator, so a seed and a scripted LLM replay the same history.

pub mod actions;
pub mod air;
pub mod actor;
pub mod behavior;
pub mod burst;
pub mod beliefs;
pub mod body;
pub mod beings;
pub mod carry;
pub mod dress;
pub mod catchup;
pub mod climb;
pub mod config;
pub mod env;
pub mod exposure;
pub mod fear;
pub mod footing;
pub mod goals;
pub mod hands;
pub mod headless;
pub mod incident;
pub mod inspect;
pub mod interp;
pub mod joint;
pub mod levels;
pub mod life;
pub mod mirror;
pub mod missile;
pub mod motion;
pub mod needs;
pub mod night;
pub mod npc;
pub mod persist;
pub mod phantom;
pub mod physics;
pub mod pick;
pub mod props;
pub mod remains;
pub mod render;
pub mod rooms;
pub mod rules;
pub mod scorer;
pub mod shape;
pub mod social;
pub mod surprise;
pub mod things;
pub mod tool;
pub mod travelers;
pub mod vehicle;
pub mod weather;

use crate::db::Db;
use crate::render::sky;
use crate::world::collide::{Obstacles, move_body};
use crate::world::describe::View;
use crate::world::scatter::ScatterCache;
use crate::world::{Solid, TypeEntry, WorldSnapshot};
use actor::Actor;
use config::SimConfig;
use glam::Vec3;
use npc::Cast;
use props::{Props, TypeProps, Vocab};
use rules::{Rule, RuleSpec};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::Value;
use social::Social;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::sync::Arc;
use things::{ThingId, Things};

pub use actions::{Action, ActErr};

/// Who acts: the player, another traveler, or a character (by character id).
///
/// `Player` is the traveler this game plays: on the host, the world's
/// own traveler (number 0); on a guest's copy, the guest. The other
/// travelers in the world are `Traveler(number)` (see `travelers`): on the
/// host its guests (1, 2, …), on a guest's copy the host (0) and the
/// other guests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActorId {
    Player,
    Traveler(u8),
    Npc(i64),
}

/// The code of traveler 0 seen from a guest's copy (the host); never kept.
const HOST_SEEN_BY_GUEST: i64 = -256;

impl ActorId {
    /// A stable number: 0 for the player, -n for traveler n, the character
    /// id otherwise.
    pub fn code(self) -> i64 {
        match self {
            ActorId::Player => 0,
            ActorId::Traveler(0) => HOST_SEEN_BY_GUEST,
            ActorId::Traveler(n) => -(n as i64),
            ActorId::Npc(i) => i,
        }
    }
    pub fn from_code(c: i64) -> ActorId {
        match c {
            0 => ActorId::Player,
            HOST_SEEN_BY_GUEST => ActorId::Traveler(0),
            c if c < 0 && c > -256 => ActorId::Traveler((-c) as u8),
            c => ActorId::Npc(c),
        }
    }
    pub fn key(self) -> String {
        match self {
            ActorId::Player => "player".into(),
            ActorId::Traveler(n) => format!("player:{n}"),
            ActorId::Npc(i) => format!("npc:{i}"),
        }
    }
    /// A traveler (this game's or another), not a character.
    pub fn is_traveler(self) -> bool {
        matches!(self, ActorId::Player | ActorId::Traveler(_))
    }
    pub fn parse(s: &str) -> Option<ActorId> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("player") || s == "you" || s == "0" {
            return Some(ActorId::Player);
        }
        if let Some(n) = s.strip_prefix("player:") {
            return n.parse::<u8>().ok().map(ActorId::Traveler);
        }
        s.trim_start_matches("npc:").parse::<i64>().ok().filter(|i| *i > 0).map(ActorId::Npc)
    }
}

impl Serialize for ActorId {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            ActorId::Player => s.serialize_str("player"),
            ActorId::Traveler(_) => s.serialize_str(&self.key()),
            ActorId::Npc(i) => s.serialize_i64(*i),
        }
    }
}

impl<'de> Deserialize<'de> for ActorId {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = Value::deserialize(d)?;
        match &v {
            Value::String(s) => ActorId::parse(s).ok_or_else(|| serde::de::Error::custom(format!("not an actor: {s}"))),
            Value::Number(n) => n.as_i64().map(ActorId::from_code).ok_or_else(|| serde::de::Error::custom("bad actor id")),
            _ => Err(serde::de::Error::custom("an actor is \"player\" or a character id")),
        }
    }
}

/// Anything an action can point at.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// A live thing.
    Thing(ThingId),
    /// The player or a character.
    Actor(ActorId),
    /// A placed object of the static world (joins the live layer when touched).
    Instance(i64),
    /// The procedural item in a 4 m scatter cell (a stick, a tree, a tuft…).
    Cell([i32; 2]),
    /// A point in the world.
    Point([f32; 3]),
    /// The nearest thing or person with this name (for plans and agents).
    Name(String),
}

/// Requests for the slow world (LLM). Answers come back through the `on_*` methods.
#[derive(Clone, Debug)]
pub enum Request {
    /// A character decides what to do about an event.
    Decide { cid: i64, event: String, context: String },
    /// A character remembers something.
    Witness { cid: i64, text: String, importance: f32 },
    /// Something happened that no rule or behaviour covers: what does it do?
    Interpret { id: u64, actor: ActorId, text: String, context: String },
    /// Someone makes something new.
    Create { id: u64, by: ActorId, text: String, target: Vec3, yaw: f32, view: Box<View> },
    /// Two characters talk; the player is close enough to overhear.
    Chat { a: i64, b: i64, context: String },
    /// Write a new object type (for spawn() of an unknown name).
    /// `fits`: a layer to be worn on this body.
    BuildType { id: u64, name: String, description: String, size: [f32; 3], props: Vec<(String, f32)>, fits: Option<String> },
    /// The player says something to a character (outside the talk screen).
    Talk { cid: i64, text: String, context: String },
    /// Write the pose keyframes of a gesture nobody knows yet.
    /// `body`: for a species' own version (name@species), what its body can move.
    BuildGesture { id: u64, name: String, body: String },
    /// Rewrite a thing's shape code: `source` changed as `change` says, near
    /// `spot` (JSON, the type's own coordinates), with `cuts` baked in and
    /// maybe another thing worked in (name, source, its size relative to this one).
    EditType { id: u64, name: String, source: String, change: String, spot: String, cuts: Vec<[f32; 4]>, with: Option<(String, String, f32)> },
    /// A species (JSON) still on a generic body gets its own, written from
    /// `template`. Answered through `on_type_built`.
    SpeciesBody { id: u64, species: String, template: String },
    /// Rewrite one being's body (`source`, the body `from`) as `change`
    /// says, as its own body `name`. Answered through `on_type_built`.
    ReshapeBody { id: u64, name: String, from: String, source: String, change: String },
    /// People drawn before bodies had faces and clothes: their look (face,
    /// hair, clothes) read from their words. (id, a JSON line about them).
    Dress { people: Vec<(i64, String)> },
}

impl Request {
    fn costs_llm(&self) -> bool {
        !matches!(self, Request::Witness { .. })
    }
}

/// Something for the player's chat log.
#[derive(Clone, Debug, PartialEq)]
pub enum Note {
    /// Someone says something the player hears (`id` is who, when it is a being).
    Line { id: Option<ActorId>, who: String, text: String },
    /// Something the player sees happen that is for them: an answer to what
    /// they did, a question, a deed done to them.
    Info(String),
    /// Everyday life nearby (a snort, a wave, someone eating): shown a short
    /// while, kept in the journal.
    Ambient(String),
    /// Something that changes the world (a thing made or remade, a birth, a
    /// death, a new kind of creature): always kept, marked.
    Notable(String),
    /// Something made or remade (a thing, a being): kept, marked, and
    /// listed under "made".
    Made(String),
    /// Something notable beyond earshot, and where; `made` if it was a making.
    Far { text: String, at: [f32; 3], made: bool },
    /// One line for an incident (a fire, a plague), updated in place as it
    /// grows and when it ends. `near` if the player is close. An empty
    /// text: it joined another incident, and its line goes.
    Incident { id: u32, text: String, at: [f32; 3], near: bool },
    /// What the player's deed changed that the eye may miss ("the moth:
    /// trust ↓"), under the deed's story.
    Effect(String),
}

impl Note {
    /// Everyday life, unless it is done to or with the player.
    pub fn seen(text: String, involves_player: bool) -> Note {
        if involves_player { Note::Info(text) } else { Note::Ambient(text) }
    }

    /// The words, whatever the kind.
    pub fn text(&self) -> String {
        match self {
            Note::Line { who, text, .. } => format!("{who}: {text}"),
            Note::Info(t) | Note::Ambient(t) | Note::Notable(t) | Note::Made(t) | Note::Far { text: t, .. } | Note::Incident { text: t, .. } => t.clone(),
            Note::Effect(t) => format!("↳ {t}"),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SimEvent {
    pub t: f64,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<ActorId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "ser_pos")]
    pub pos: Option<[f32; 3]>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

/// Positions to a tenth of a metre, without f32 noise in the JSON.
fn ser_pos<S: Serializer>(p: &Option<[f32; 3]>, s: S) -> Result<S::Ok, S::Error> {
    match p {
        Some(v) => v.map(|x| (x as f64 * 10.0).round() / 10.0).serialize(s),
        None => s.serialize_none(),
    }
}

/// Everything that happened, newest last.
#[derive(Default)]
pub struct EventLog {
    pub recent: VecDeque<SimEvent>,
    pub sink: Option<Box<dyn Write + Send>>,
    pub total: u64,
    /// Events not yet saved to the database.
    pub unsaved: Vec<SimEvent>,
    pub counts: BTreeMap<String, u64>,
}

impl EventLog {
    pub fn push(&mut self, e: SimEvent) {
        if let Some(w) = self.sink.as_mut() {
            if let Ok(line) = serde_json::to_string(&e) {
                let _ = writeln!(w, "{line}");
            }
        }
        *self.counts.entry(e.kind.clone()).or_default() += 1;
        self.total += 1;
        self.unsaved.push(e.clone());
        if self.unsaved.len() > 4000 {
            self.unsaved.drain(..1000);
        }
        self.recent.push_back(e);
        while self.recent.len() > 3000 {
            self.recent.pop_front();
        }
    }

    /// Recent events about a subject ("thing:3", "npc:2", …).
    pub fn about(&self, subject: &str, actor: Option<ActorId>, n: usize) -> Vec<SimEvent> {
        let mut v: Vec<SimEvent> = self
            .recent
            .iter()
            .rev()
            .filter(|e| e.subject.as_deref() == Some(subject) || (actor.is_some() && e.actor == actor))
            .take(n)
            .cloned()
            .collect();
        v.reverse();
        v
    }
}

/// A queued LLM request waiting for budget.
struct Queued {
    req: Request,
    dist: f32,
    at: f64,
    /// How much it matters, 0..1.
    weight: f32,
}

pub struct Sim {
    pub cfg: SimConfig,
    /// What each body wears (kg), tallied once a frame while actors step.
    worn_mass: Option<HashMap<ActorId, f32>>,
    /// Which part each layer sits on, on the body it was made for (by layer
    /// and body type): see `render::layer_shift`.
    layer_parts: std::sync::Mutex<HashMap<(u32, u32), Option<usize>>>,
    pub db: Arc<Db>,
    pub snap: Arc<WorldSnapshot>,
    pub cache: ScatterCache,
    pub seed: u64,
    rng: u64,
    /// Game time in seconds (one day = sky::DAY_SECONDS).
    pub t: f64,
    pub player: Actor,
    /// What the traveler chose about their own body (see `hands`).
    pub own: hands::OwnBody,
    /// The other travelers here, playing together (see `travelers`).
    pub travelers: BTreeMap<u8, travelers::Traveler>,
    /// What travelers who aren't here hold and wear, kept for them.
    pub stowed: BTreeMap<u8, Vec<things::Thing>>,
    /// A guest's copy of the host's world (see `mirror`).
    pub mirror: mirror::Mirror,
    pub cast: Cast,
    pub things: Things,
    pub field: env::Field,
    pub vocab: Arc<Vocab>,
    pub rules: Arc<Vec<Rule>>,
    pub universe_rules: Vec<RuleSpec>,
    pub type_props: TypeProps,
    pub social: Social,
    pub log: EventLog,
    /// Requests ready for the brain.
    pub out: Vec<Request>,
    queue: Vec<Queued>,
    budget: f32,
    pub notes: Vec<Note>,
    /// Notes by where they happened, for other travelers (see `note_near`).
    placed: Vec<(Vec3, f32, Note)>,
    placed_host: std::collections::HashSet<usize>,
    pub has_llm: bool,
    /// How far up or down the player looks (radians): where "that hill" is.
    pub look_pitch: f32,
    /// The character the player is talking to (they stand still and face the player).
    pub talking_to: Option<i64>,
    /// How open each speaking character's mouth is now (0..1), from their
    /// voice as it plays (the app keeps it).
    pub mouths: HashMap<i64, f32>,
    /// Every face held at one mouth and eyes-shut (portraits for checking).
    pub face_hold: Option<(f32, f32)>,
    pub interp: interp::Interp,
    acc_rules: f32,
    acc_behavior: f32,
    acc_regions: f32,
    acc_life: f32,
    /// Where new things just appeared, and when: a flash of light (see `render`).
    pub flashes: Vec<(Vec3, f64)>,
    /// Blasts lighting up what is around them: (where, when, how far it reached).
    pub blasts: Vec<(Vec3, f64, f32)>,
    /// Who lit what is lit (it goes off by their hand).
    pub lit_by: HashMap<things::ThingId, ActorId>,
    /// When things that go off were last looked over for fire.
    pub burst_check: f64,
    pub region_seen: HashMap<(i32, i32), f64>,
    next_req: u64,
    /// The things the player recently did (for dialogue context).
    pub recent_player: VecDeque<(f64, String)>,
    /// Actions queued for the player by an agent (`pocket act`).
    pub player_plan: VecDeque<actions::Action>,
    /// Placed objects that appeared in the last flips: (id, when).
    fresh: Vec<(i64, f64)>,
    /// Placed objects someone is known to have made, and who (told by `on_created`).
    pub made: std::collections::HashMap<i64, ActorId>,
    /// Night, corruption and the traveler's charges (see `night`).
    pub night: night::NightState,
    /// Happenings with one cause, told as one story (see `incident`).
    pub incidents: incident::Incidents,
    /// What people want and have promised (see `goals`).
    pub goals: goals::Goals,
    /// What each holds true (see `beliefs`).
    pub beliefs: beliefs::Beliefs,
    /// Which property crossings are news (from the vocabulary).
    pub watch: Arc<Vec<env::Watch>>,
    /// Requests dropped for waiting too long (see `SimConfig::queue_wait`).
    pub dropped: u64,
    /// What was made heard since the game last took them (see `audio`).
    pub sounds: Vec<crate::audio::Cue>,
    /// When each body last bumped into something (a bump is heard once).
    bumped: HashMap<ActorId, f64>,
    /// The last step's length (s): how fast a walk's delta was.
    frame_dt: f32,
    /// Pits were dug since the last save.
    pub dug_dirty: bool,
    /// The enterable shapes (see `rooms`).
    pub rooms: Vec<rooms::Room>,
    /// Stairs and ladders (see `climb`).
    pub climbs: Vec<climb::Climb>,
    /// The ways between levels (see `levels`), and each body's next place
    /// on its way there, remembered for a moment: (where to, when, next).
    pub levels: levels::Levels,
    level_ways: parking_lot::Mutex<HashMap<ActorId, (Vec3, f64, Vec3)>>,
    /// How fast each vehicle in use is going (m/s, forward; see `vehicle`).
    pub drives: std::collections::BTreeMap<things::ThingId, f32>,
    /// Beasts harnessed to what they pull (a cart's horse, a sledge's dogs).
    pub hitched: std::collections::BTreeMap<things::ThingId, ActorId>,
    /// Characters driving somewhere: when they last got anywhere, and where.
    pub driving: std::collections::HashMap<ActorId, (f64, glam::Vec3)>,
    /// The way a character is steering along (waypoints, the next one).
    pub routes: std::collections::HashMap<ActorId, (Vec<glam::Vec2>, usize)>,
    /// The world's weather over time, and what it is now (see `weather`).
    pub weather: crate::world::weather::Weather,
    pub wx: crate::world::weather::Now,
    pub weather_st: weather::WeatherState,
}

impl Sim {
    pub fn new(db: Arc<Db>, snap: Arc<WorldSnapshot>, player_pos: Vec3, player_yaw: f32, t: f64, seed: u64) -> Sim {
        let cfg = SimConfig::load(|k| db.kv_get(k));
        let mut sim = Sim {
            cfg,
            worn_mass: None,
            layer_parts: Default::default(),
            db: db.clone(),
            snap: snap.clone(),
            cache: ScatterCache::default(),
            seed,
            rng: seed ^ 0x9E37_79B9_7F4A_7C15,
            t,
            player: Actor::new(player_pos, player_yaw),
            own: Default::default(),
            travelers: BTreeMap::new(),
            stowed: BTreeMap::new(),
            mirror: Default::default(),
            cast: Cast::default(),
            things: Things::default(),
            field: env::Field::default(),
            vocab: Arc::new(Vocab::builtin()),
            rules: Arc::new(Vec::new()),
            universe_rules: Vec::new(),
            type_props: TypeProps::default(),
            social: Social::default(),
            log: EventLog::default(),
            out: Vec::new(),
            queue: Vec::new(),
            budget: 2.0,
            notes: Vec::new(),
            placed: Vec::new(),
            placed_host: Default::default(),
            has_llm: false,
            look_pitch: -0.12,
            talking_to: None,
            mouths: HashMap::new(),
            face_hold: None,
            interp: interp::Interp::default(),
            acc_rules: 0.0,
            acc_behavior: 0.0,
            acc_regions: 1.0,
            acc_life: 0.0,
            flashes: Vec::new(),
            blasts: Vec::new(),
            lit_by: HashMap::new(),
            burst_check: 0.0,
            region_seen: HashMap::new(),
            next_req: 1,
            recent_player: VecDeque::new(),
            player_plan: VecDeque::new(),
            fresh: Vec::new(),
            made: std::collections::HashMap::new(),
            night: night::NightState::default(),
            incidents: incident::Incidents::default(),
            goals: goals::Goals::default(),
            beliefs: beliefs::Beliefs::default(),
            watch: Arc::new(Vec::new()),
            dropped: 0,
            sounds: Vec::new(),
            bumped: HashMap::new(),
            frame_dt: 1.0 / 60.0,
            drives: Default::default(),
            hitched: Default::default(),
            driving: Default::default(),
            routes: Default::default(),
            dug_dirty: false,
            rooms: rooms::rooms_of(&snap),
            climbs: climb::climbs_of(&snap),
            levels: Default::default(),
            level_ways: Default::default(),
            weather: crate::world::weather::Weather::new(&snap.look.climate, snap.terrain.seed),
            wx: Default::default(),
            weather_st: Default::default(),
        };
        sim.levels = levels::levels_of(&snap, &sim.climbs, &sim.rooms);
        sim.player.dims = traveler_dims(&snap);
        sim.load_own_body();
        sim.load_universe_rules();
        persist::load_gestures(&sim.db);
        sim.cast.sync(&snap, seed);
        persist::load(&mut sim);
        sim.step_weather();
        sim.fit_bodies();
        sim.seed_goals();
        sim.social.seed_from_personas(&sim.cast, &snap.species);
        sim.dress_new();
        sim.sync_overlay();
        sim
    }

    // ------------------------------------------------------------ basics

    /// Uniform random in [0, 1) from the simulation's seeded generator.
    pub fn rand(&mut self) -> f32 {
        self.rng = self.rng.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.rng >> 40) as u32) as f32 / (1u32 << 24) as f32
    }

    pub fn next_id(&mut self) -> u64 {
        let id = self.next_req;
        self.next_req += 1;
        id
    }

    /// The last step's length (s).
    pub fn frame_dt(&self) -> f32 {
        self.frame_dt
    }

    pub fn hour(&self) -> f32 {
        sky::day_phase(self.t) * 24.0
    }

    pub fn night(&self) -> bool {
        sky::is_night(self.t)
    }

    /// Types used by live things and cells (reported to the model so it can
    /// leave unused ones out of the shader).
    pub fn types_in_use(&self) -> std::collections::HashSet<u32> {
        self.things.live().map(|t| t.type_id).chain(self.field.cells.values().map(|c| c.type_id)).collect()
    }

    pub fn type_entry(&self, id: u32) -> Option<Arc<TypeEntry>> {
        self.snap.type_of(id).cloned()
    }

    /// A type and its initial properties at scale 1.
    pub fn type_info(&mut self, id: u32) -> Option<(Arc<TypeEntry>, Arc<Props>)> {
        let ty = self.snap.type_of(id)?.clone();
        let base = self.type_props.get(&self.vocab, &ty);
        Some((ty, base))
    }

    /// A type by name (case-insensitive), newest first.
    pub fn type_by_name(&self, name: &str) -> Option<Arc<TypeEntry>> {
        let n = name.trim().to_lowercase();
        let mut best: Option<&Arc<TypeEntry>> = None;
        for t in self.snap.scene.types.values() {
            if t.name().to_lowercase() == n && best.is_none_or(|b| t.id > b.id) {
                best = Some(t);
            }
        }
        best.cloned()
    }

    pub fn actor(&self, id: ActorId) -> Option<&Actor> {
        match id {
            ActorId::Player => Some(&self.player),
            ActorId::Traveler(n) => self.travelers.get(&n).map(|t| &t.a),
            ActorId::Npc(c) => self.cast.get(c).map(|n| &n.a),
        }
    }

    pub fn actor_mut(&mut self, id: ActorId) -> Option<&mut Actor> {
        match id {
            ActorId::Player => Some(&mut self.player),
            ActorId::Traveler(n) => self.travelers.get_mut(&n).map(|t| &mut t.a),
            ActorId::Npc(c) => self.cast.get_mut(c).map(|n| &mut n.a),
        }
    }

    pub fn actor_ids(&self) -> Vec<ActorId> {
        let mut v = vec![ActorId::Player];
        v.extend(self.travelers.keys().map(|n| ActorId::Traveler(*n)));
        v.extend(self.cast.npcs.iter().filter(|n| n.here()).map(|n| ActorId::Npc(n.def.id)));
        v
    }

    pub fn actor_name(&self, id: ActorId) -> String {
        match id {
            ActorId::Player => "the traveler".into(),
            ActorId::Traveler(n) => self.travelers.get(&n).map(|t| t.name.clone()).unwrap_or_else(|| "a traveler".into()),
            ActorId::Npc(c) => self.cast.get(c).map(|n| n.name().to_string()).unwrap_or_else(|| "someone".into()),
        }
    }

    /// How far an actor can reach to pick something up or use it.
    pub fn reach_of(&self, id: ActorId) -> f32 {
        self.actor(id).map(|a| a.dims.reach).unwrap_or(actor::REACH)
    }

    /// Body height of an actor (m).
    pub fn actor_height(&self, id: ActorId) -> f32 {
        self.actor(id).map(|a| a.dims.height).unwrap_or(1.75)
    }

    pub fn thing_name(&self, id: ThingId) -> String {
        if let Some(c) = self.things.get(id).and_then(|t| t.origin.remains) {
            return self.remains_name(c);
        }
        self.things.get(id).and_then(|t| self.snap.type_of(t.type_id)).map(|t| t.name().to_string()).unwrap_or_else(|| "thing".into())
    }

    pub fn dist_to_player(&self, p: Vec3) -> f32 {
        let d = p - self.player.pos;
        (d.x * d.x + d.z * d.z).sqrt()
    }

    // ------------------------------------------------------------ events

    #[allow(clippy::too_many_arguments)]
    pub fn event(&mut self, kind: &str, actor: Option<ActorId>, subject: Option<String>, text: impl Into<String>, pos: Option<Vec3>, data: Value) {
        let e = SimEvent { t: (self.t * 1000.0).round() / 1000.0, kind: kind.into(), actor, subject, text: text.into(), pos: pos.map(|p| [(p.x * 10.0).round() / 10.0, (p.y * 10.0).round() / 10.0, (p.z * 10.0).round() / 10.0]), data };
        self.log.push(e);
    }

    /// Something is heard. Kept only for a game listening; a headless run
    /// lets the oldest go.
    pub fn cue(&mut self, at: Vec3, from: Option<ActorId>, what: crate::audio::Heard) {
        if self.sounds.len() >= 256 {
            self.sounds.drain(..64);
        }
        self.sounds.push(crate::audio::Cue { at, from, what });
    }

    /// A being's `i`th noise, heard from where it is.
    pub fn cue_noise(&mut self, cid: i64, i: usize, gain: f32) {
        let Some(n) = self.cast.get(cid) else { return };
        let Some(call) = crate::audio::call::species_call(&n.species, i) else { return };
        // Its register: its kind's, bent by how big this one is.
        let ratio = (n.a.dims.mass / n.species.mass.max(0.1)).max(0.01).powf(1.0 / 3.0);
        let pitch = crate::audio::individual_pitch(cid, ratio);
        let at = n.a.pos + Vec3::Y * n.a.dims.height * 0.75;
        let mass = n.species.mass;
        self.cue(at, Some(ActorId::Npc(cid)), crate::audio::Heard::Call { call, mass, pitch, gain });
    }

    /// Tell the player, if they are close enough to notice.
    /// Notable news from beyond earshot is still told, as far off.
    /// With other travelers here, each is told as near or far as it is to
    /// them (see `drain_placed`).
    pub fn note_near(&mut self, at: Vec3, range: f32, n: Note) {
        if !self.travelers.is_empty() {
            self.placed.push((at, range, n.clone()));
            self.placed_host.insert(self.notes.len());
        }
        if let Some(n) = told_at(n, self.dist_to_player(at), at, range) {
            self.notes.push(n);
        }
    }

    /// Notes told by where they happened (for the other travelers, each
    /// judged from where they are), and which of the notes not yet drained
    /// came from them (so they aren't passed on twice).
    pub fn drain_placed(&mut self) -> (Vec<(Vec3, f32, Note)>, std::collections::HashSet<usize>) {
        (std::mem::take(&mut self.placed), std::mem::take(&mut self.placed_host))
    }

    /// Characters within `range` of `at` who can see it remember it.
    pub fn witness(&mut self, at: Vec3, range: f32, text: &str, importance: f32, except: &[ActorId]) {
        let ids: Vec<i64> = self.cast.npcs.iter().filter(|n| n.here() && (n.a.pos - at).length() < range && !n.a.asleep && !except.contains(&ActorId::Npc(n.def.id))).map(|n| n.def.id).collect();
        for cid in ids {
            if let Some(n) = self.cast.get_mut(cid) {
                if n.recently_witnessed(text, self.t) {
                    continue;
                }
                n.curiosity_bump(importance);
                n.hear_news(self.t, text, importance);
            }
            self.out.push(Request::Witness { cid, text: text.to_string(), importance });
        }
    }

    // ------------------------------------------------------------ LLM budget

    /// Queue a request that costs LLM budget; nearer ones go first.
    pub fn request(&mut self, req: Request, at: Vec3) {
        self.request_weighted(req, at, 0.5);
    }

    /// Queue a request that costs LLM budget, weighted by how much it
    /// matters (0..1: a surprise, trouble, someone talking to them).
    pub fn request_weighted(&mut self, req: Request, at: Vec3, weight: f32) {
        if !self.has_llm {
            return;
        }
        if !req.costs_llm() {
            self.out.push(req);
            return;
        }
        let dist = self.dist_to_travelers(at);
        self.queue.push(Queued { req, dist, at: self.t, weight: weight.clamp(0.0, 1.0) });
    }

    /// Player-initiated requests skip the queue.
    pub fn request_now(&mut self, req: Request) {
        if self.has_llm {
            self.out.push(req);
        }
    }

    fn pump_requests(&mut self, dt: f32) {
        let per_s = self.cfg.llm_per_min / 60.0;
        self.budget = (self.budget + per_s * dt).min((self.cfg.llm_per_min / 6.0).max(1.0));
        let now = self.t;
        let wait = self.cfg.queue_wait as f64;
        let before = self.queue.len();
        self.queue.retain(|q| now - q.at < wait);
        self.dropped += (before - self.queue.len()) as u64;
        // What matters most goes first; nearer breaks ties.
        self.queue.sort_by(|a, b| (a.dist / (0.25 + a.weight)).total_cmp(&(b.dist / (0.25 + b.weight))));
        while self.budget >= 1.0 && !self.queue.is_empty() {
            let q = self.queue.remove(0);
            self.budget -= 1.0;
            self.out.push(q.req);
        }
    }

    /// Requests still waiting for budget.
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    // ------------------------------------------------------------ world

    /// Solid things near `p`: static solids (scatter and placed objects not in
    /// the live layer) plus anchored or large live things.
    pub fn solids_near(&mut self, p: Vec3, r: f32) -> Vec<Solid> {
        let snap = self.snap.clone();
        let mut out = self.cache.solids_near(&snap, p, r);
        for id in self.things.near(p, r + 12.0) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() {
                continue;
            }
            let Some(ty) = snap.type_of(t.type_id) else { continue };
            if !ty.solid || t.props[props::P_SOLID] <= 0.0 {
                continue;
            }
            let big = t.anchored || ty.radius() * t.scale > 0.45;
            if !big {
                continue;
            }
            let gi = render::thing_inst(t, ty, [0.0; 4]);
            if (gi.center() - p).length() - gi.radius() > r {
                continue;
            }
            let mut s = Solid::new(gi, ty.clone());
            s.more = t.more_cuts().into();
            s.bores = t.shape.bores.as_slice().into();
            out.push(s);
        }
        out
    }

    /// Move an actor by `delta` with collisions (terrain, solids, other bodies).
    pub fn walk(&mut self, id: ActorId, delta: Vec3) {
        // On a ladder, or walking onto one (people take hold on their way, see `step_toward`).
        let held = self.actor(id).is_some_and(|a| a.ladder.is_some());
        if (held || !matches!(id, ActorId::Npc(_))) && self.climb_walk(id, delta) {
            return;
        }
        let Some(from) = self.actor(id).map(|a| a.pos) else { return };
        let own = self.actor(id).map(|a| a.dims.radius).unwrap_or(0.35);
        let solids = self.solids_near(from, 4.0 + own);
        let mine = self.actor(id).and_then(|a| a.riding);
        let mut bodies: Vec<(Vec3, f32)> = Vec::new();
        let others = std::iter::once((ActorId::Player, &self.player)).chain(self.cast.npcs.iter().filter(|n| n.here()).map(|n| (ActorId::Npc(n.def.id), &n.a)));
        for (o, a) in others {
            if o != id && Some(o) != mine && a.riding != Some(id) && a.carried_by.is_none() && a.aboard.is_none() && a.alt < 1.0 && (a.pos - from).length() < 4.0 + a.dims.radius {
                bodies.push((a.pos, footing::walk_radius(o, a)));
            }
        }
        let obs = Obstacles { solids: &solids, bodies: &bodies };
        let cap = self.capsule(id);
        let r = cap.radius;
        let to = move_body(&self.snap.terrain, &obs, from, delta, cap);
        // What it stands on there, from the same look around (see `footing`).
        let grounded = self.actor(id).is_some_and(|a| a.grounded && a.vy == 0.0);
        let support = grounded.then(|| crate::world::collide::support_under(&self.snap.terrain, &solids, to, r, to.y + cap.step));
        if let Some(a) = self.actor_mut(id) {
            a.moved = (to - from).length();
            a.pos = to;
            a.support = support.map(|g| (to, g));
        }
        self.bump_sound(id, from, delta, to, r, &solids);
    }

    /// Walking into something solid is heard as physics says: by how
    /// squarely and fast it was met, and by both bodies. A body meeting a
    /// heavy, hard thing makes a dull thud at most; brushing past growth is
    /// a rustle (the foley's), not a hit; only dry, dead growth met head-on
    /// snaps. Heard once, not again while leaning on it.
    fn bump_sound(&mut self, id: ActorId, from: Vec3, delta: Vec3, to: Vec3, r: f32, solids: &[Solid]) {
        use crate::audio::call::Material;
        let want = Vec3::new(delta.x, 0.0, delta.z).length();
        let got = Vec3::new(to.x - from.x, 0.0, to.z - from.z).length();
        if want < 1e-4 || got > want * 0.4 {
            return;
        }
        let calm = if id == ActorId::Player { 0.8 } else { 3.0 };
        let fresh = self.bumped.get(&id).is_none_or(|t| self.t - t > calm);
        self.bumped.insert(id, self.t);
        if !fresh {
            return;
        }
        let dir = Vec3::new(delta.x, 0.0, delta.z) / want;
        let ahead = from + dir * (r + 0.2);
        let gap = |s: &Solid| (Vec3::new(s.inst.center().x, ahead.y, s.inst.center().z) - ahead).length() - s.inst.radius();
        let Some(hit) = solids.iter().filter(|s| gap(s) < 0.8).min_by(|a, b| gap(a).total_cmp(&gap(b))) else { return };
        let c = hit.inst.center();
        let head_on = dir.dot(Vec3::new(c.x - from.x, 0.0, c.z - from.z).normalize_or_zero()).max(0.0);
        let speed = (want / self.frame_dt.max(1e-3)).min(12.0) * head_on;
        let ty = hit.ty.clone();
        let scale = hit.inst.pos_scale[3].max(0.01);
        let props = self.type_props.get(&self.vocab, &ty);
        let mat = Material::of(&props, &ty.ct.meta.tags, Material::from_meta(&ty.ct.meta.sound).as_ref());
        let mass = (props[props::P_MASS] * scale.powi(3)).max(0.01);
        let body = self.actor(id).map(|a| a.dims.mass).unwrap_or(70.0);
        // Growth gives way: it rustles as it's pushed (the foley hears that).
        // Only dry, dead growth met squarely snaps.
        if mat.leafy > 0.5 && !(mat.dry > 0.6 && head_on > 0.75) {
            return;
        }
        if speed < 0.8 {
            return;
        }
        self.cue(ahead + Vec3::Y * 0.8, None, crate::audio::Heard::Hit { mat, mass, speed, by: Some((Material::FLESH, body)) });
    }

    /// A new world version arrived (creation, region, undo).
    pub fn flip(&mut self, snap: Arc<WorldSnapshot>) {
        let alive: std::collections::HashSet<i64> = snap.instances.iter().map(|p| p.id).collect();
        let before: std::collections::HashSet<i64> = self.snap.instances.iter().map(|p| p.id).collect();
        // (A first full world after a quick start is not news.)
        if !before.is_empty() {
            for p in &snap.instances {
                if !before.contains(&p.id) {
                    self.fresh.push((p.id, self.t));
                }
            }
        }
        // Things whose source was undone go away with it.
        let gone: Vec<ThingId> = self.things.live().filter(|t| t.origin.instance.is_some_and(|i| !alive.contains(&i)) || snap.type_of(t.type_id).is_none()).map(|t| t.id).collect();
        for id in gone {
            self.release(id);
            self.things.forget(id);
        }
        self.snap = snap.clone();
        // What waited through a quick start comes now.
        if !snap.partial && !self.things.waiting.is_empty() {
            let w = std::mem::take(&mut self.things.waiting);
            persist::load_things(self, Some(&w));
        }
        self.rooms = rooms::rooms_of(&snap);
        self.climbs = climb::climbs_of(&snap);
        self.levels = levels::levels_of(&snap, &self.climbs, &self.rooms);
        self.level_ways.lock().clear();
        for id in self.actor_ids() {
            self.off_ladder(id);
        }
        self.weather.refresh(&snap.look.climate, snap.terrain.seed);
        self.type_props.clear();
        self.cast.sync(&snap, self.seed);
        self.fit_bodies();
        self.seed_goals();
        self.player.dims = traveler_dims(&snap);
        self.social.seed_from_personas(&self.cast, &snap.species);
        self.dress_new();
        self.sync_overlay();
    }

    /// Detach a thing from whoever holds it.
    pub fn release(&mut self, id: ThingId) {
        let holders: Vec<ActorId> = self.things.get(id).map(|t| [t.holder, t.co_holder].into_iter().flatten().collect()).unwrap_or_default();
        for h in holders {
            if let Some(a) = self.actor_mut(h) {
                if a.held == Some(id) {
                    a.held = None;
                }
            }
        }
        if let Some(t) = self.things.get_mut(id) {
            t.holder = None;
            t.co_holder = None;
            t.hold_tilt = None;
        }
    }

    /// Keep the static layer's view (hidden instances, taken scatter, cell
    /// looks) in step with the live layer.
    pub fn sync_overlay(&mut self) {
        let o = &mut self.cache.overlay;
        o.hidden = self.things.by_instance.keys().copied().collect();
        o.taken = self.things.taken.keys().copied().collect();
        o.cell_fx.clear();
        o.cell_gone.clear();
        for (c, cell) in &self.field.cells {
            let p = &cell.props;
            if p[props::P_FUEL] <= 0.0 && cell.small {
                o.cell_gone.insert(*c);
                continue;
            }
            let mut fx = [0.0; 4];
            fx[crate::render::FX_CHAR] = p[props::P_CHAR];
            fx[crate::render::FX_WET] = p[props::P_WET].min(1.0) * 0.6;
            if fx.iter().any(|v| *v > 0.02) {
                o.cell_fx.insert(*c, fx);
            }
        }
        o.version = o.version.wrapping_add(1);
    }

    // ------------------------------------------------------------ the tick

    pub fn step(&mut self, dt: f32) {
        let dt = dt.clamp(0.0, 0.25);
        if dt > 0.0 {
            self.frame_dt = dt;
        }
        self.t += dt as f64;
        // The rivers round the travelers, as they walk.
        for p in self.traveler_spots() {
            self.snap.terrain.lay_rivers_soon(p.x, p.z);
        }
        self.step_weather();
        let dark = sky::lighting(self.t, &self.snap.look.palette).night;
        self.cache.overlay.lamps = dark;
        self.mark_work();
        self.step_actors(dt);
        self.step_travelers(dt);
        self.place_hands();
        self.update_riders();
        self.step_social(dt);
        self.step_physics(dt);
        self.step_motions(dt);
        self.step_bursts();
        self.step_joints(dt);
        self.acc_behavior += dt;
        let bstep = 1.0 / self.cfg.behavior_hz;
        if self.acc_behavior >= bstep {
            let d = self.acc_behavior.min(1.0);
            self.acc_behavior = 0.0;
            self.step_behavior(d);
        }
        self.acc_rules += dt;
        let rstep = 1.0 / self.cfg.rules_hz;
        if self.acc_rules >= rstep {
            let d = self.acc_rules.min(1.0);
            self.acc_rules = 0.0;
            self.step_rules(d);
        }
        self.acc_life += dt;
        if self.acc_life >= 60.0 {
            self.acc_life = 0.0;
            self.step_life();
            self.step_remains();
            self.fade_fear(60.0);
        }
        // The first step after loading: who is about at this hour, before anyone is drawn.
        if self.night.was_night.is_none() {
            self.step_night();
        }
        self.acc_regions += dt;
        if self.acc_regions >= 1.0 {
            self.acc_regions = 0.0;
            self.step_regions();
            self.step_incidents();
            self.ask_species_bodies();
            self.ask_wardrobe();
            self.step_night();
            self.tell_weather();
            self.feel_bodies(1.0);
            self.breathe(1.0);
            self.step_fear();
            self.step_found_bodies();
            self.step_goals(1.0);
        }
        self.pump_requests(dt);
    }

    /// Take everything ready for the brain.
    pub fn drain_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.out)
    }

    pub fn drain_notes(&mut self) -> Vec<Note> {
        // (What was told by place goes with them: take that first, if wanted.)
        self.placed.clear();
        self.placed_host.clear();
        std::mem::take(&mut self.notes)
    }

    /// People near something that just appeared notice it, as surprised as
    /// it makes them (unless they saw a character make it: that is told by
    /// `on_created`). The most striking thing is taken in first.
    pub fn notice_fresh(&mut self) {
        let t = self.t;
        let (due, wait): (Vec<(i64, f64)>, Vec<(i64, f64)>) = self.fresh.drain(..).partition(|(_, at)| t - at >= 0.5);
        self.fresh = wait;
        let mut seen = Vec::new();
        for (id, _) in due {
            let by = self.made.get(&id).copied();
            if by.is_some_and(|w| w != ActorId::Player) {
                continue;
            }
            let Some(p) = self.snap.instances.iter().find(|p| p.id == id).cloned() else { continue };
            if self.dist_to_travelers(p.pos) > 70.0 {
                continue;
            }
            let Some(ty) = self.snap.type_of(p.type_id).cloned() else { continue };
            let sight = self.sight_of(&ty, p.scale, surprise::Arrival::FromNowhere);
            seen.push((self.plain_surprise(sight), p.pos, ty.name().to_string(), sight, by.is_some()));
        }
        seen.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, at, name, sight, by_traveler) in seen {
            // The traveler asked for it out loud, right there: that is seen too.
            let there = by_traveler && self.dist_to_player(at) < 30.0;
            let near: Vec<(i64, Vec3)> = self.cast.npcs.iter().filter(|n| n.here() && !n.a.asleep && (n.a.pos - at).length() < 60.0).map(|n| (n.def.id, n.a.pos)).collect();
            for (cid, pos) in near {
                let dir = compass(at - pos);
                let (memory, tell) = if there {
                    (format!("A {name} appeared {dir} of me, out of nowhere, right where the traveler stood."), format!("A {name} just appeared {dir} of you, out of nowhere, right where the traveler is standing."))
                } else {
                    (format!("A {name} appeared {dir} of me, out of nowhere, after the traveler arrived."), format!("A {name} just appeared {dir} of you, out of nowhere."))
                };
                let what = format!("the {name}");
                let news = surprise::News { at, sight, memory: &memory, importance: 0.5, event: "new_building", tell: &tell, what: &what };
                self.startle_one(cid, &news);
            }
        }
    }

    /// Things that must always hold. Returns what is wrong (empty: all well).
    pub fn check_invariants(&self) -> Vec<String> {
        let mut bad = Vec::new();
        for t in self.things.live() {
            let Some(ty) = self.snap.type_of(t.type_id) else {
                bad.push(format!("thing {} has no type", t.id));
                continue;
            };
            if !t.pos.is_finite() || t.props.iter().any(|v| !v.is_finite()) {
                bad.push(format!("thing {} has a non-finite position or property", t.id));
            }
            if !t.held() && !t.anchored {
                let g = self.snap.terrain.height(t.pos.x, t.pos.z);
                let lowest = t.pos.y + ty.bottom * t.scale;
                if lowest < g - 0.5 - ty.radius() * t.scale && g > self.snap.terrain.water(t.pos.x, t.pos.z) {
                    bad.push(format!("thing {} ({}) sank {:.1} m into the ground", t.id, ty.name(), g - lowest));
                }
            }
            for h in [t.holder, t.co_holder].into_iter().flatten() {
                match self.actor(h) {
                    Some(a) if a.held == Some(t.id) => {}
                    _ => bad.push(format!("thing {} says {} holds it, but they don't", t.id, h.key())),
                }
            }
            if t.holder.is_some() && t.holder == t.co_holder {
                bad.push(format!("thing {} is held twice by the same person", t.id));
            }
        }
        for a in self.actor_ids() {
            if let Some(id) = self.actor(a).and_then(|x| x.held) {
                match self.things.get(id) {
                    Some(t) if t.holder == Some(a) || t.co_holder == Some(a) => {}
                    _ => bad.push(format!("{} holds thing {id}, which doesn't know it", a.key())),
                }
            }
        }
        if self.things.len() > self.cfg.max_things {
            bad.push(format!("{} live things, over the cap of {}", self.things.len(), self.cfg.max_things));
        }
        bad
    }

    /// The player did something worth remembering in conversations.
    pub fn player_did(&mut self, what: String) {
        self.recent_player.push_back((self.t, what));
        while self.recent_player.len() > 4 {
            self.recent_player.pop_front();
        }
    }
}

/// The traveler's body: a person of the universe's chosen height (1.75 m
/// unless the world says otherwise).
pub fn traveler_dims(snap: &WorldSnapshot) -> crate::world::species::Dims {
    let human = snap.species.of("human");
    let Some(body) = snap.body_type(&human.body).and_then(|t| t.ct.meta.body.clone()) else { return Default::default() };
    let h = snap.species.traveler_height.filter(|h| h.is_finite()).unwrap_or(1.75).clamp(0.3, 12.0);
    let own = h.clamp(1.3, 2.0);
    let mut sliders = [0.5; crate::lang::ir::MAX_SLIDERS];
    sliders[..5].copy_from_slice(&[own, 1.0, 0.3, 0.6, 0.1]);
    if let Some(i) = body.slider("height") {
        sliders[i] = own;
    }
    let mut d = npc::body_dims(&body, &human, &sliders, h / own);
    d.human_arms = d.arms && snap.body_root(&human.body) == "figure";
    d
}

/// A note about something at `at`, as one `dist` from it hears it: whole
/// within `range`; beyond it, only what changes the world, as far off.
pub fn told_at(n: Note, dist: f32, at: Vec3, range: f32) -> Option<Note> {
    if dist <= range {
        return Some(n);
    }
    match n {
        Note::Notable(text) => Some(Note::Far { text, at: at.to_array(), made: false }),
        Note::Made(text) => Some(Note::Far { text, at: at.to_array(), made: true }),
        _ => None,
    }
}

/// Short compass word for a direction.
pub fn compass(d: Vec3) -> &'static str {
    let a = d.x.atan2(d.z).to_degrees().rem_euclid(360.0);
    ["north", "north-east", "east", "south-east", "south", "south-west", "west", "north-west"][((a + 22.5) / 45.0) as usize % 8]
}

/// "a" or "an".
pub fn article(name: &str) -> &'static str {
    if name.starts_with(['a', 'e', 'i', 'o', 'u', 'A', 'E', 'I', 'O', 'U']) { "an" } else { "a" }
}

#[cfg(test)]
mod tests;

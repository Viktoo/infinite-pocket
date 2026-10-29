//! Characters' minds. Cheap code runs every tick: needs drift, and when a
//! character has nothing to do it scores a few options (eat, sleep, play,
//! seek company, look at something new, flee a fire, wander) and turns the
//! best into a short plan of shared actions. The LLM is a rare planner: for
//! big moments it writes a goal and steps, and the same executor carries
//! them out.

use super::actions::{Action, ActErr};
use super::actor::{Actor, GestureKind, NPC_RUN, NPC_WALK, PLAYER_SPEED, Task};
use super::props::*;
use super::things::ThingId;
use super::{ActorId, Request, Sim, Target};
use crate::render::GpuInst;
use crate::world::characters::{Decision, Needs, SavedState};
use crate::world::species::{Dims, Species};
use crate::world::{CharacterDef, TypeEntry, WorldSnapshot};
use glam::Vec3;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

/// What a character is about, as the engine reads it (never from the words
/// of `doing`, which are for people).
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Aim {
    #[default]
    Idle,
    /// Pushing back an incident.
    Counter(u32),
    /// Keeping clear of something.
    Avoid,
    /// Going after someone (a predator its prey).
    Chase(ActorId),
    /// Keeping with someone.
    Follow(ActorId),
    /// Anything else.
    Other,
}

/// How someone will push back a trouble: which one, with what, where.
#[derive(Clone, Debug)]
pub struct CounterPlan {
    pub id: u32,
    pub noun: String,
    pub tool: Option<ThingId>,
    pub target: Target,
    pub at: Vec3,
    pub held: Option<ThingId>,
    pub pos: Vec3,
    pub was: Aim,
}

/// Personality as numbers (0..1), read from the persona's words.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Traits {
    pub sociable: f32,
    pub playful: f32,
    pub curious: f32,
    pub brave: f32,
    pub generous: f32,
    pub crafty: f32,
}

impl Traits {
    pub fn from_persona(p: &crate::world::Persona, seed: u32) -> Traits {
        let text = format!("{} {} {} {}", p.personality, p.goals, p.appearance, p.voice).to_lowercase();
        let has = |ws: &[&str]| ws.iter().filter(|w| text.contains(*w)).count() as f32;
        let jitter = |k: u32| (crate::noise::u2f(crate::noise::pcg(seed ^ k.wrapping_mul(0x9E37))) - 0.5) * 0.2;
        let clamp = |v: f32| v.clamp(0.05, 0.95);
        let young = p.age > 0 && p.age < 16;
        Traits {
            sociable: clamp(0.5 + 0.15 * has(&["friendly", "warm", "cheerful", "talkative", "gregarious", "kind", "jovial", "chatty", "outgoing"]) - 0.15 * has(&["shy", "gruff", "quiet", "reserved", "withdrawn", "solitary", "aloof", "suspicious"]) + jitter(1)),
            playful: clamp(0.4 + 0.15 * has(&["playful", "mischievous", "lively", "joyful", "fun", "games", "restless", "adventurous"]) + if young { 0.35 } else { 0.0 } - 0.1 * has(&["stern", "serious", "grim", "weary"]) + jitter(2)),
            curious: clamp(0.45 + 0.15 * has(&["curious", "inquisitive", "scholar", "inventor", "nosy", "explorer", "questions"]) + jitter(3)),
            brave: clamp(0.5 + 0.15 * has(&["brave", "bold", "fearless", "soldier", "guard", "stubborn"]) - 0.15 * has(&["timid", "nervous", "anxious", "fearful", "cautious"]) + jitter(4)),
            generous: clamp(0.5 + 0.15 * has(&["generous", "kind", "giving", "caring", "gentle"]) - 0.2 * has(&["greedy", "stingy", "selfish", "miser"]) + jitter(5)),
            crafty: clamp(0.35 + 0.2 * has(&["smith", "carpenter", "maker", "tinker", "builder", "artist", "craft", "inventor", "potter", "weaver", "mender"]) + jitter(6)),
        }
        .given(&p.traits)
    }

    /// Numbers the persona's author gave outright win over words read.
    fn given(mut self, t: &std::collections::BTreeMap<String, f32>) -> Traits {
        for (k, v) in t {
            let v = v.clamp(0.0, 1.0);
            if !v.is_finite() {
                continue;
            }
            match k.trim().to_lowercase().as_str() {
                "sociable" => self.sociable = v,
                "playful" => self.playful = v,
                "curious" => self.curious = v,
                "brave" => self.brave = v,
                "generous" => self.generous = v,
                "crafty" => self.crafty = v,
                _ => {}
            }
        }
        self
    }
}

impl Traits {
    /// A species' temper sets the starting point; the persona's words still
    /// nudge it (a timid dragon is possible).
    pub fn with_temper(self, sp: &Species, t: crate::world::species::Temper) -> Traits {
        use crate::world::species::{Mind, Social};
        let blend = |own: f32, base: f32| (own - 0.5) * 0.5 + base;
        let sociable = match sp.social {
            Social::Solitary => 0.2,
            Social::Pair => 0.45,
            Social::Pack | Social::Herd => 0.65,
            Social::Village => 0.6,
        };
        let sapient = sp.mind == Mind::Sapient;
        Traits {
            sociable: blend(self.sociable, sociable).clamp(0.0, 1.0),
            playful: blend(self.playful, t.playful).clamp(0.0, 1.0),
            curious: self.curious,
            brave: blend(self.brave, t.bold).clamp(0.0, 1.0),
            generous: if sapient { self.generous } else { 0.2 },
            crafty: if sapient { self.crafty } else { 0.0 },
        }
    }
}

pub struct Npc {
    pub def: Arc<CharacterDef>,
    pub species: Arc<Species>,
    /// Its own temper (its species', or its line's).
    pub temper: crate::world::species::Temper,
    /// The body type it is drawn with.
    pub body_ty: u32,
    /// Look sliders (k.a … k.e).
    pub sliders: [f32; crate::lang::ir::MAX_SLIDERS],
    pub a: Actor,
    pub needs: Needs,
    pub traits: Traits,
    /// What they are doing right now, in a few words.
    pub doing: String,
    /// What they are about, for the engine (`doing` is the words for it).
    pub aim: Aim,
    pub goal: String,
    pub plan: VecDeque<Action>,
    pub plan_from_llm: bool,
    pub think_at: f64,
    pub next_llm: f64,
    pub rng: u32,
    witnessed: VecDeque<(u64, f64)>,
    /// What they saw lately that mattered (when, the memory, how much it
    /// mattered): what they bring up when they stop to talk.
    pub news: VecDeque<(f64, String, f32)>,
    pub last_player_near: f64,
    acc: f32,
    pub last_sim: f64,
    pub decisions: VecDeque<(f64, String)>,
    pub bored_since: f64,
    pub last_line: f64,
    /// What they last said aloud and when: the same words again soon after
    /// are let go.
    pub last_said: (f64, String),
    pub last_work: f64,
    /// When an animal last greeted its person.
    pub last_greet: f64,
    /// Someone to hand the next thing they make to, until this game time.
    pub deliver: Option<(ActorId, f64)>,
    /// A deed they set out to do and what it waits on (see `needs`).
    pub mission: Option<super::needs::Mission>,
    /// Someone who asked them a favour and awaits the answer, until this game time.
    pub asked_by: Option<(ActorId, f64)>,
    /// Someone they agreed to help, until this game time.
    pub helping: Option<(ActorId, f64)>,
    /// The deed in words they are in the middle of (asked, or being made),
    /// so it can be done again if the session ends before it lands.
    pub deed: Option<(String, Option<Target>, f64)>,
    /// Loaded with work under way: fix up what was waiting on an answer.
    pub restored: bool,
    /// Times in a row a walk was blocked and they stepped aside to try again.
    pub detours: u8,
    pub dead: bool,
    pub dressed: bool,
    /// Gestures it was taught (an animal's tricks), done when greeting.
    pub tricks: Vec<String>,
    /// Game time it was born (for the young), or 0.
    pub born: f64,
    pub parents: Vec<i64>,
    pub lineage: i64,
    pub last_birth: f64,
    pub frights: u32,
    /// How used to wonders they are (see `surprise`), as of `habit_at`.
    pub habit: f32,
    pub habit_at: f64,
    /// When something last made them drop everything.
    pub shock_at: f64,
    /// Grown-up fraction (0.3 newborn … 1 adult).
    pub growth: f32,
    /// Out of its hours (see `Species::active`): not drawn, simulated or met.
    pub away: bool,
    /// Their body's properties, like a thing's (empty until the sim fits
    /// it to the vocabulary; see `Sim::fit_bodies`): a touch's
    /// glow, wetness, warmth, and whatever the world's own rules put on them.
    pub props: Props,
    /// How much what is on their body hurts them right now (by its harm).
    pub pain: f32,
    /// Breathes and has no air: suffocating (see `air`).
    pub gasping: bool,
    /// When its touch last landed (it draws back for a while after).
    pub touched_at: f64,
    /// The dead whose bodies they have come upon (or saw die).
    pub seen_dead: Vec<i64>,
    /// When fear of someone near last moved them (see `fear`).
    pub feared_at: f64,
    /// Where something they reached for was out of their reach, and until
    /// when they leave it be (so they look elsewhere, not try again and again).
    pub unreachable: Vec<(Vec3, f64)>,
}

impl Npc {
    /// In the world right now: alive and within its hours.
    pub fn here(&self) -> bool {
        !self.dead && !self.away
    }

    pub fn name(&self) -> &str {
        &self.def.persona.name
    }

    /// How badly hurt they are, as their body carries it: 0 while health
    /// is above `LIMP_FROM`, 1 at none left.
    pub fn hurt(&self) -> f32 {
        let h = self.props.get(P_HEALTH).copied().unwrap_or(1.0);
        ((super::actor::LIMP_FROM - h) / super::actor::LIMP_FROM).clamp(0.0, 1.0)
    }

    /// A glow on them (a touch's dust; it fades).
    pub fn glow(&self) -> f32 {
        self.props.get(P_LIGHT).copied().unwrap_or(0.0)
    }

    pub fn rand(&mut self) -> f32 {
        self.rng = crate::noise::pcg(self.rng);
        crate::noise::u2f(self.rng)
    }

    pub fn saved(&self, t: f64) -> SavedState {
        SavedState { x: self.a.pos.x, z: self.a.pos.z, y: Some(self.a.pos.y), yaw: self.a.yaw, asleep: self.a.asleep, needs: Some(self.needs), held: self.a.held, goal: self.goal.clone(), t, dead: self.dead, dressed: self.dressed, tricks: self.tricks.clone(), born: self.born, parents: self.parents.clone(), lineage: self.lineage, last_birth: self.last_birth, frights: self.frights, habit: super::surprise::habit_now(self.habit, t - self.habit_at), work: self.work(), props: Default::default(), away: self.away, seen_dead: self.seen_dead.clone() }
    }

    pub fn gpu(&self, body: &TypeEntry) -> GpuInst {
        body_inst(&self.a, body, &self.sliders, self.def.id as f32 % 97.0)
    }

    /// Avoid remembering the same thing over and over.
    pub fn recently_witnessed(&mut self, text: &str, t: f64) -> bool {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        text.hash(&mut h);
        let k = h.finish();
        self.witnessed.retain(|(_, at)| t - at < 120.0);
        if self.witnessed.iter().any(|(x, _)| *x == k) {
            return true;
        }
        self.witnessed.push_back((k, t));
        false
    }

    /// Keep something seen as news to tell (the most recent few).
    pub fn hear_news(&mut self, t: f64, text: &str, importance: f32) {
        self.news.push_back((t, text.to_string(), importance));
        while self.news.len() > 8 {
            self.news.pop_front();
        }
    }

    pub fn curiosity_bump(&mut self, amount: f32) {
        self.needs.curiosity = (self.needs.curiosity + amount * self.traits.curious).min(1.0);
    }

    fn log_decision(&mut self, t: f64, s: String) {
        self.decisions.push_back((t, s));
        while self.decisions.len() > 12 {
            self.decisions.pop_front();
        }
    }
}

#[derive(Default)]
pub struct Cast {
    pub npcs: Vec<Npc>,
    index: HashMap<i64, usize>,
    /// When any character last turned to their craft (see `SimConfig::work_gap_secs`).
    pub last_work: f64,
    /// When a character last made something (see `SimConfig::maker_secs`).
    pub last_made: f64,
    /// The species book the cast was last fitted to.
    book: Option<Arc<crate::world::species::SpeciesBook>>,
}

impl Cast {
    /// Add characters that appeared in a new snapshot; when the species
    /// changed (one was written or replaced), everyone takes theirs anew.
    pub fn sync(&mut self, snap: &WorldSnapshot, seed: u64) {
        let fresh_book = !self.book.as_ref().is_some_and(|b| Arc::ptr_eq(b, &snap.species));
        self.book = Some(snap.species.clone());
        if fresh_book {
            for n in self.npcs.iter_mut() {
                let species = snap.species.of(&n.def.persona.species);
                let body_ty = body_for(&n.def.persona, &species, snap).map(|b| b.id).unwrap_or(0);
                if species.as_ref() != n.species.as_ref() || body_ty != n.body_ty {
                    fit(n, snap, seed);
                }
            }
        }
        for def in &snap.characters {
            if !self.index.contains_key(&def.id) {
                self.add(def.clone(), snap, seed);
            }
        }
    }

    /// Bring one character into the cast.
    pub fn add(&mut self, def: Arc<CharacterDef>, snap: &WorldSnapshot, seed: u64) {
        let s = &def.state;
        let pos = if s.x == 0.0 && s.z == 0.0 { def.home } else { Vec3::new(s.x, 0.0, s.z) };
        let t = &snap.terrain;
        let caver = snap.species.get(&def.persona.species).is_some_and(|sp| sp.caves);
        // Where they were (up a tree, down a cave), or home: a cave's floor
        // for those who live in caves, else the ground.
        let y = match s.y.filter(|y| y.is_finite() && s.x != 0.0) {
            Some(y) => t.floor(pos.x, y + 0.3, pos.z).max(y.min(t.height(pos.x, pos.z) + 80.0)),
            None if caver => t.cave_floor_under(pos.x, pos.z).unwrap_or_else(|| t.height(pos.x, pos.z)),
            None => t.height(pos.x, pos.z),
        };
        let pos = Vec3::new(pos.x, y, pos.z);
        let rng = crate::noise::pcg(def.id as u32 ^ 0xC0FFEE ^ seed as u32);
        let mut a = Actor::new(pos, s.yaw);
        a.asleep = s.asleep;
        let traits = Traits::from_persona(&def.persona, rng);
        let needs = s.needs.unwrap_or_else(|| {
            let r = |k: u32| crate::noise::u2f(crate::noise::pcg(rng ^ k)) * 0.4;
            Needs { hunger: 0.1 + r(1), fatigue: r(2) * 0.5, social: 0.2 + r(3), fun: 0.2 + r(4), curiosity: 0.2 + r(5) }
        });
        let mut n = Npc {
            def: def.clone(),
            species: snap.species.of(""),
            temper: Default::default(),
            body_ty: 0,
            sliders: [0.5; crate::lang::ir::MAX_SLIDERS],
            a,
            needs,
            traits,
            doing: if s.away { "away" } else { "idle" }.into(),
            aim: Aim::Idle,
            goal: s.goal.clone(),
            plan: VecDeque::new(),
            plan_from_llm: false,
            think_at: 0.0,
            next_llm: 0.0,
            rng,
            witnessed: VecDeque::new(),
            news: VecDeque::new(),
            last_player_near: f64::MIN,
            acc: 0.0,
            last_sim: s.t,
            decisions: VecDeque::new(),
            bored_since: f64::MAX,
            last_line: f64::MIN,
            last_said: (f64::MIN, String::new()),
            last_work: f64::MIN,
            last_greet: f64::MIN,
            deliver: None,
            unreachable: Vec::new(),
            mission: None,
            asked_by: None,
            helping: None,
            deed: None,
            restored: false,
            detours: 0,
            dead: s.dead,
            dressed: s.dressed,
            tricks: s.tricks.clone(),
            born: s.born,
            parents: s.parents.clone(),
            lineage: if s.lineage != 0 { s.lineage } else { def.id },
            last_birth: s.last_birth,
            frights: s.frights,
            habit: s.habit,
            habit_at: s.t,
            shock_at: f64::MIN,
            // Only the born grow up; beings made by deeds arrive grown.
            growth: if s.born > 0.0 && !s.parents.is_empty() { 0.3 } else { 1.0 },
            away: s.away,
            props: Vec::new(),
            pain: 0.0,
            gasping: false,
            touched_at: f64::MIN,
            seen_dead: s.seen_dead.clone(),
            feared_at: f64::MIN,
        };
        if let Some(w) = s.work.as_ref() {
            n.restore_work(w);
        }
        fit(&mut n, snap, seed);
        self.index.insert(def.id, self.npcs.len());
        self.npcs.push(n);
    }

    pub fn get(&self, id: i64) -> Option<&Npc> {
        self.index.get(&id).map(|i| &self.npcs[*i])
    }

    pub fn get_mut(&mut self, id: i64) -> Option<&mut Npc> {
        self.index.get(&id).copied().map(move |i| &mut self.npcs[i])
    }
}

/// A body's instance: where it stands, its look sliders and its pose.
pub fn body_inst(a: &super::actor::Actor, body: &TypeEntry, sliders: &[f32; crate::lang::ir::MAX_SLIDERS], seed: f32) -> GpuInst {
    let rot = a.yaw;
    let d = &a.dims;
    // Bodies that draw their own height (people) are scaled only by the
    // world's size; others by their whole size.
    let own_height = body.ct.meta.body.as_ref().is_some_and(|b| b.slider("height").is_some());
    let r = if own_height { d.ratio } else { d.scale };
    let mut g = GpuInst {
        pos_scale: [a.pos.x, a.pos.y, a.pos.z, d.scale],
        rot: [rot.cos(), rot.sin(), body.sphere_r * r.max(d.scale), 1.0],
        k0: [seed, d.scale, 0.0, 0.0],
        k1: [0.0, 0.0, 0.0, a.phase],
        info: body.gpu_info(),
        ..Default::default()
    };
    g.set_look(sliders);
    g.set_pose(&a.pose);
    g
}

impl super::Sim {
    /// Where everyone's hands are this frame, from their bodies: a body that
    /// says where its parts are (`parts()`) holds things in its own hands,
    /// however long its arms or wherever they reach.
    pub fn place_hands(&mut self) {
        let snap = &self.snap;
        for n in self.cast.npcs.iter_mut().filter(|n| n.here() && !n.dead) {
            let Some(body) = snap.type_of(n.body_ty) else {
                n.a.hands = None;
                continue;
            };
            if body.ct.parts.is_none() {
                n.a.hands = None;
                continue;
            }
            let g = n.gpu(body);
            let k = g.k();
            let s = g.pos_scale[3];
            n.a.hands = body.ct.part(0, &k).map(|r| [r * s, body.ct.part(1, &k).unwrap_or(r) * s]);
        }
    }
}

/// Fit a character to its species: body, look sliders, size, roles and
/// temper (on arrival, and again when its species changes).
pub fn fit(n: &mut Npc, snap: &WorldSnapshot, seed: u64) {
    let species = snap.species.of(&n.def.persona.species);
    let body = body_for(&n.def.persona, &species, snap);
    let bmeta = body.and_then(|b| b.ct.meta.body.clone()).unwrap_or_default();
    let variety = species.varieties.iter().find(|v| v.name == n.def.persona.variety);
    let rng = crate::noise::pcg(n.def.id as u32 ^ 0xC0FFEE ^ seed as u32);
    // Before their look says how they are dressed, a guess from their words.
    let mut look = n.def.persona.look.clone();
    if bmeta.slider("shape").is_some() && !look.contains_key("shape") {
        let guess = super::dress::guess(&n.def.persona);
        look.extend(guess.into_iter().filter(|(k, _)| variety.is_none_or(|v| !v.look.contains_key(k))));
    }
    n.sliders = crate::world::species::sliders(&bmeta, &species, variety, &look, rng);
    // The young are smaller, all over.
    n.a.dims = body_dims(&bmeta, &species, &n.sliders, snap.species.size_of(&species.name) * n.def.persona.size_mul() * n.growth.clamp(0.2, 1.0));
    n.a.dims.human_arms = n.a.dims.arms && body.is_some_and(|b| snap.body_root(b.name()) == "figure");
    n.a.roles = super::actor::role_mask(&bmeta);
    n.a.species = if species.is_human() { String::new() } else { species.name.clone() };
    n.traits = Traits::from_persona(&n.def.persona, rng);
    n.temper = n.def.persona.temper.unwrap_or(species.temper);
    if !species.is_human() {
        n.traits = n.traits.with_temper(&species, n.temper);
    }
    n.body_ty = body.map(|b| b.id).unwrap_or(0);
    n.species = species;
}

/// The body a character lives in: their own (reshaped) one while it
/// exists, else their species'.
pub fn body_for<'a>(p: &crate::world::Persona, species: &Species, snap: &'a WorldSnapshot) -> Option<&'a Arc<crate::world::TypeEntry>> {
    Some(p.body.as_str()).filter(|b| !b.is_empty()).and_then(|b| snap.body_named(b)).or_else(|| snap.body_type(&species.body))
}

/// Size numbers for a character: people draw their own height (a slider);
/// other bodies are scaled by their species' size. `world` is the universe's
/// size multiplier for the species.
pub fn body_dims(b: &crate::lang::ir::Body, sp: &Species, sliders: &[f32; crate::lang::ir::MAX_SLIDERS], world: f32) -> Dims {
    let (height, scale) = match b.slider("height") {
        Some(i) => (sliders[i], world),
        None => (b.height, sp.size * world),
    };
    let norm = match b.slider("height") {
        Some(_) => 1.75,
        None => b.height * sp.size,
    };
    let mut d = Dims::of(b, height, scale, sp.mass);
    d.mass = sp.mass * (d.height / norm.max(0.01)).powi(3);
    d
}

/// Plans written by the LLM are close to the action JSON but not always
/// exact: accept plain strings for targets and a few common verb aliases.
pub fn parse_step(v: &Value) -> Option<Action> {
    let mut v = v.clone();
    let obj = v.as_object_mut()?;
    if !obj.contains_key("do") {
        for alias in ["action", "verb", "type"] {
            if let Some(x) = obj.remove(alias) {
                obj.insert("do".into(), x);
                break;
            }
        }
    }
    let raw = obj.get("do")?.as_str()?.trim().to_lowercase().replace([' ', '-'], "_");
    let verb = match raw.as_str() {
        "walk_to" | "go_to" | "go" | "approach" | "walk" => "goto",
        "pick_up" | "pickup" | "take" | "grab" | "lift" | "carry" => "hold",
        "put_down" | "set_down" | "release" => "drop",
        "toss" => "throw",
        "speak" | "tell" | "talk" => "say",
        // Making is something you do, in words, like everything else.
        "make" | "build" | "craft" | "create" => "do",
        "hand" | "offer" => "give",
        "put_on" | "dress" | "don" => "wear",
        "mount" | "ride_on" | "climb_on" | "board" | "get_in" | "get_into" | "drive" | "row" | "sail" | "sit_on" | "sit_in" => "ride",
        "sit" | "sit_down" if obj.contains_key("target") || obj.contains_key("on") => "ride",
        "get_down" | "get_off" | "dismount" | "get_out" | "get_up" | "disembark" | "stand_up" | "alight" => "dismount",
        "take_off" | "undress" | "doff" => "take_off",
        "douse" | "put_out" | "extinguish" | "fight_fire" | "beat_out" | "smother" | "rub" | "press" | "work" => "apply",
        "home" | "go_home" => "go_home",
        "run_away" | "run_from" | "escape" | "get_away" | "flee_from" => "flee",
        "close" | "shut" | "unlatch" | "open_door" => "open",
        "swing" | "strike" | "hit" | "attack" | "chop" | "dig" | "stab" | "thrust" | "pour" | "swing_at" => "use",
        // A launcher in hand: shoot at what is named; load it with what is named.
        "shoot" | "shoot_at" | "fire" | "fire_at" | "loose" | "aim" | "load" | "reload" => "use",
        // Light, set off, pull the pin: the thing named itself is used.
        "light" | "ignite" | "set_off" | "pull_pin" | "detonate" | "arm" => "use",
        "throw_at" | "hurl" | "lob" | "fling" => "throw",
        "duck" | "kneel_down" => "crouch",
        "hop" | "leap" => "jump",
        v => v,
    }
    .to_string();
    obj.insert("do".into(), Value::String(verb.clone()));
    if verb == "ride" && !obj.contains_key("target") {
        if let Some(x) = obj.remove("on").or_else(|| obj.remove("at")) {
            obj.insert("target".into(), x);
        }
    }
    if matches!(raw.as_str(), "close" | "shut") {
        obj.insert("close".into(), Value::Bool(true));
    }
    // "swing at the wolf", "dig here": the held tool is worked on the target.
    let lights = matches!(raw.as_str(), "light" | "ignite" | "set_off" | "pull_pin" | "detonate" | "arm");
    if verb == "use" && raw != "use" && !lights {
        if let Some(t) = obj.remove("at").or_else(|| obj.remove("target")) {
            obj.entry("on").or_insert(t);
        }
    }
    // For do and use, `at` is a spot (a point); a plan never names one.
    if matches!(verb.as_str(), "do" | "use") && obj.get("at").is_some_and(|a| !a.is_array()) {
        obj.remove("at");
    }
    if verb == "flee" && !obj.contains_key("from") {
        if let Some(x) = obj.remove("target").or_else(|| obj.remove("at")) {
            obj.insert("from".into(), x);
        }
    }
    for key in ["target", "at", "to", "on", "with", "from"] {
        if let Some(x) = obj.get(key).cloned() {
            let fixed = match x {
                Value::String(s) => Some(json!({ "name": s })),
                Value::Number(n) if key != "at" || verb != "place" => Some(json!({ "thing": n })),
                _ => None,
            };
            if let Some(f) = fixed {
                obj.insert(key.into(), f);
            }
        }
    }
    if verb == "say" && !obj.contains_key("text") {
        if let Some(x) = obj.remove("line").or_else(|| obj.remove("words")) {
            obj.insert("text".into(), x);
        }
    }
    if verb == "gesture" && !obj.contains_key("kind") {
        if let Some(x) = obj.remove("gesture").or_else(|| obj.remove("name")) {
            obj.insert("kind".into(), x);
        }
    }
    if verb == "do" && !obj.contains_key("text") {
        if let Some(x) = obj.remove("what").or_else(|| obj.remove("thing")) {
            obj.insert("text".into(), x);
        }
    }
    // {"do": "ask", "who": ["Rosa", "Ben"], "for": "a grill"}: names into targets.
    if verb == "ask" {
        let who = obj.remove("who").or_else(|| obj.remove("to")).or_else(|| obj.remove("target")).unwrap_or(Value::Null);
        let list: Vec<Value> = match who {
            Value::Array(v) => v.into_iter().filter_map(|x| match x {
                Value::String(s) => Some(json!({ "name": s })),
                Value::Object(_) => Some(x),
                _ => None,
            }).collect(),
            Value::String(s) => vec![json!({ "name": s })],
            o @ Value::Object(_) => vec![o],
            _ => vec![],
        };
        obj.insert("who".into(), Value::Array(list));
        if !obj.contains_key("what") && !obj.contains_key("for") {
            if let Some(x) = obj.remove("text").or_else(|| obj.remove("thing")) {
                obj.insert("what".into(), x);
            }
        }
    }
    // {"do": "create", "text": "a wooden ball"} → "make a wooden ball".
    if let (Some(Value::String(t)), true) = (obj.get("text").cloned(), matches!(raw.as_str(), "make" | "build" | "craft" | "create")) {
        if !t.trim_start().to_lowercase().starts_with(&raw) {
            obj.insert("text".into(), Value::String(format!("{} {}", if raw == "create" { "make" } else { &raw }, t.trim())));
        }
    }
    // Gestures named as verbs ("hug", "wave").
    if GestureKind::parse(&verb).is_some() && !matches!(verb.as_str(), "sit") {
        obj.insert("kind".into(), Value::String(verb.clone()));
        obj.insert("do".into(), Value::String("gesture".into()));
        if let Some(t) = obj.remove("target") {
            obj.insert("to".into(), t);
        }
    }
    serde_json::from_value(v).ok()
}

/// Short spoken lines for when no LLM speaks for them.
pub fn template(kind: &str, other: &str, hour: f32, k: u32) -> String {
    template_line(kind, other, hour, k)
}

pub(super) fn template_line(kind: &str, other: &str, hour: f32, k: u32) -> String {
    let pick = |v: &[&str]| v[(k as usize) % v.len()].replace("{o}", other);
    match kind {
        "greet" => {
            if hour < 11.0 {
                pick(&["Morning, {o}.", "Good morning, {o}!", "Up early, {o}?"])
            } else if hour < 18.0 {
                pick(&["Hello, {o}.", "{o}! Good to see you.", "Afternoon, {o}."])
            } else {
                pick(&["Evening, {o}.", "Still about, {o}?", "Good evening."])
            }
        }
        "catch" => pick(&["Catch!", "Here it comes!", "Yours!", "Heads up!"]),
        "caught" => pick(&["Got it!", "Nice throw!", "Ha!", "Again!"]),
        "missed" => pick(&["Oops!", "Missed it!", "Too high!"]),
        "alarm" => pick(&["Get back from the {o}!", "Careful, the {o}!", "Look out, the {o}!"]),
        "rally" => pick(&["Help me stop the {o}!", "Quick, before the {o} spreads!", "Don't let the {o} reach the houses!", "Stop the {o}, quick!"]),
        "thanks" => pick(&["For me? Thank you.", "Thank you, {o}.", "Oh! That's kind."]),
        "hug" => pick(&["Come here, you.", "Missed you.", "Oh, {o}."]),
        "bored" => pick(&["Nothing ever happens here.", "Hm.", "What to do…"]),
        "cheer" => pick(&["Yes!", "Did you see that?", "Ha! In it goes!"]),
        "yes" => pick(&["Sure!", "Gladly.", "Why not!", "All right."]),
        "no" => pick(&["Not now.", "Maybe later.", "I'd rather not."]),
        _ => pick(&["Hm.", "Well then.", "Right."]),
    }
}

impl Sim {
    // ------------------------------------------------------------ actors

    pub fn step_actors(&mut self, dt: f32) {
        // The player: scripted plans and tasks (agents), pose, footing.
        self.step_plan(ActorId::Player);
        self.run_task(ActorId::Player, dt);
        self.step_footing(ActorId::Player, dt);
        let held_big = self.held_size(ActorId::Player);
        let t = self.t;
        self.step_hands();
        self.player.update_pose(t, dt, held_big);
        let near = self.cfg.near;
        let medium = self.cfg.medium;
        let hz = self.cfg.medium_hz;
        let mut worn: HashMap<ActorId, f32> = HashMap::new();
        for t in self.things.live() {
            if let Some(w) = t.worn {
                *worn.entry(w).or_default() += t.mass();
            }
        }
        self.worn_mass = Some(worn);
        for i in 0..self.cast.npcs.len() {
            let (cid, d) = {
                let n = &self.cast.npcs[i];
                if !n.here() {
                    continue;
                }
                (n.def.id, self.dist_to_travelers(n.a.pos))
            };
            if d <= near {
                self.npc_tick(cid, dt, true);
            } else if d <= medium {
                let n = &mut self.cast.npcs[i];
                n.acc += dt;
                if n.acc >= 1.0 / hz {
                    let step = n.acc.min(2.0);
                    n.acc = 0.0;
                    self.npc_tick(cid, step, false);
                }
            }
        }
        self.worn_mass = None;
    }

    pub fn held_size(&self, who: ActorId) -> Option<bool> {
        if self.carried_of(who).is_some() {
            return Some(true);
        }
        let id = self.actor(who)?.held?;
        let t = self.things.get(id)?;
        let ty = self.snap.type_of(t.type_id)?;
        Some(ty.radius() * t.scale > 0.35 || t.co_holder.is_some())
    }

    /// Give an actor a plan of actions (replacing any current one).
    pub fn plan(&mut self, who: ActorId, actions: Vec<Action>, goal: &str, from_llm: bool) {
        match who {
            ActorId::Player => {
                self.player_plan = actions.into();
            }
            // Other travelers are moved by their own players.
            ActorId::Traveler(_) => {}
            ActorId::Npc(c) => {
                if let Some(n) = self.cast.get_mut(c) {
                    n.plan = actions.into();
                    n.goal = goal.to_string();
                    n.plan_from_llm = from_llm;
                    n.a.task = None;
                }
            }
        }
    }

    fn plan_mut(&mut self, who: ActorId) -> Option<&mut VecDeque<Action>> {
        match who {
            ActorId::Player => Some(&mut self.player_plan),
            ActorId::Traveler(_) => None,
            ActorId::Npc(c) => self.cast.get_mut(c).map(|n| &mut n.plan),
        }
    }

    /// Carry out the next action of a plan when the current task is done.
    /// Returns false when a step failed.
    pub fn step_plan(&mut self, who: ActorId) -> bool {
        for _ in 0..3 {
            if self.actor(who).is_none_or(|a| a.task.is_some()) {
                return true;
            }
            let Some(next) = self.plan_mut(who).and_then(|p| p.pop_front()) else { return true };
            let label = format!("{:?}", next.verb());
            match self.act(who, next.clone()) {
                Ok(o) => {
                    if !o.ok {
                        self.plan_failed(who, &o.msg);
                        return false;
                    }
                }
                Err(ActErr::TooFar { at, dist }) => {
                    if dist > 120.0 {
                        self.plan_failed(who, &format!("{label}: too far ({dist:.0} m)"));
                        return false;
                    }
                    // Standing right under it: out of reach (up a bush, on a roof).
                    let me = self.actor(who).map(|a| a.pos).unwrap_or(at);
                    if Vec3::new(at.x - me.x, 0.0, at.z - me.z).length() < self.reach_of(who) * 0.6 + 0.3 {
                        if let ActorId::Npc(c) = who {
                            let until = self.t + 300.0;
                            if let Some(n) = self.cast.get_mut(c) {
                                n.unreachable.retain(|(_, u)| *u > until - 300.0);
                                n.unreachable.push((at, until));
                            }
                        }
                        self.plan_failed(who, &format!("{label}: out of reach"));
                        return false;
                    }
                    if let Some(p) = self.plan_mut(who) {
                        p.push_front(next);
                    }
                    let run = matches!(who, ActorId::Npc(_)) && dist > 25.0;
                    let deadline = self.t + 90.0;
                    let stop = self.reach_of(who) * 0.6;
                    self.set_task(who, Task::Goto { target: Target::Point(at.to_array()), stop, run, deadline });
                    return true;
                }
                Err(ActErr::Fail(msg)) => {
                    self.plan_failed(who, &msg);
                    return false;
                }
                // A step about something that isn't there: let it go and
                // carry on with the rest, if there is a rest.
                Err(ActErr::Missing(msg)) => {
                    if self.plan_mut(who).is_none_or(|p| p.is_empty()) {
                        self.plan_failed(who, &msg);
                        return false;
                    }
                    if let ActorId::Npc(c) = who {
                        let t = self.t;
                        if let Some(n) = self.cast.get_mut(c) {
                            n.log_decision(t, format!("skipped a step: {msg}"));
                        }
                    }
                    self.event("step_skipped", Some(who), None, msg, self.actor(who).map(|a| a.pos), json!({}));
                    return true;
                }
            }
        }
        true
    }

    fn plan_failed(&mut self, who: ActorId, why: &str) {
        let t = self.t;
        if let Some(p) = self.plan_mut(who) {
            p.clear();
        }
        if let ActorId::Npc(c) = who {
            if let Some(n) = self.cast.get_mut(c) {
                n.log_decision(t, format!("plan failed: {why}"));
                n.goal.clear();
                n.think_at = t + 2.0;
            }
        }
        self.event("plan_failed", Some(who), None, why.to_string(), self.actor(who).map(|a| a.pos), json!({}));
    }

    /// Advance an actor's current task. True while it is still running.
    pub fn run_task(&mut self, who: ActorId, dt: f32) -> bool {
        let Some(task) = self.actor(who).and_then(|a| a.task.clone()) else { return false };
        // A long way to go: characters take their own mount if it is near.
        if let (ActorId::Npc(_), Task::Goto { target, .. }) = (who, &task) {
            if self.actor(who).is_some_and(|a| a.riding.is_none()) {
                let far = self.resolve(target, who).map(|r| r.pos).zip(self.actor(who).map(|a| a.pos)).is_some_and(|(p, m)| Vec3::new(p.x - m.x, 0.0, p.z - m.z).length() > 50.0);
                // Once per trip: a mount that says no isn't asked again every step.
                let asked = matches!(task, Task::Goto { deadline, .. } if self.interp.mount_asked.get(&who) == Some(&deadline.to_bits()));
                if far && !asked {
                    if let (Some(m), Task::Goto { deadline, .. }) = (self.own_mount(who, 15.0), &task) {
                        self.interp.mount_asked.insert(who, deadline.to_bits());
                        let _ = self.ride(who, m);
                    }
                }
            }
        }
        // In something with seats: driven, carried along, or got out of.
        if let Task::Goto { target, stop, deadline, .. } = &task {
            if self.actor(who).is_some_and(|a| a.aboard.is_some()) {
                let going = self.goto_aboard(who, target, *stop, *deadline, dt);
                if !going {
                    if let Some(a) = self.actor_mut(who) {
                        if a.task.as_ref() == Some(&task) {
                            a.task = None;
                        }
                    }
                }
                return self.actor(who).is_some_and(|a| a.task.is_some());
            }
            // A long way, or over water: take a free vehicle that is near.
            if let ActorId::Npc(c) = who {
                let dest = self.resolve(target, who).map(|r| r.pos);
                let asked = self.interp.mount_asked.get(&who) == Some(&deadline.to_bits());
                let free = self.actor(who).is_some_and(|a| !a.seated() && a.carried_by.is_none());
                if let (Some(dest), false, true) = (dest, asked, free) {
                    // Looked for once per trip.
                    self.interp.mount_asked.insert(who, deadline.to_bits());
                    if let Some(v) = self.vehicle_for(who, dest) {
                        if let Some(n) = self.cast.get_mut(c) {
                            n.plan.push_front(Action::Goto { target: target.clone(), run: false });
                            n.plan.push_front(Action::Ride { target: v.clone() });
                            n.plan.push_front(Action::Goto { target: v, run: false });
                            n.a.task = None;
                        }
                        return false;
                    }
                }
            }
        }
        // What carries them: their own legs (or wings), or their mount.
        let mover = self.actor(who).and_then(|a| a.riding).unwrap_or(who);
        let (speed_walk, speed_run) = self.speeds(mover);
        let flyer = self.can_fly(mover);
        let fly = self.fly_speed(mover);
        let airborne = self.actor(mover).is_some_and(|a| a.alt > 0.3);
        let now = self.t;
        let me = self.actor(who).map(|a| a.pos).unwrap_or_default();
        let mut done = false;
        let mut failed = false;
        match &task {
            Task::Goto { target, stop, run, deadline } => {
                let dest = self.resolve(target, who).map(|r| r.pos);
                match dest {
                    None => failed = true,
                    Some(p) => {
                        let flat = Vec3::new(p.x - me.x, 0.0, p.z - me.z);
                        // Someone already stands on the spot: beside them will do.
                        let taken = matches!(target, Target::Point(_)) && flat.length() <= *stop + 1.5 && self.spot_taken(who, p);
                        if flat.length() <= *stop || taken {
                            if airborne {
                                // Come down first.
                                self.fly_toward(mover, p, fly, dt, true);
                            } else {
                                done = true;
                                if let Some(a) = self.actor_mut(who) {
                                    a.face(flat, 6.0);
                                }
                            }
                        } else if now > *deadline {
                            failed = true;
                        } else if flyer && (airborne || flat.length() > 20.0) {
                            self.fly_toward(mover, p, fly, dt, true);
                        } else {
                            let sp = if *run { speed_run.max(speed_walk) } else { speed_walk };
                            if !self.step_toward(mover, p, sp, dt) {
                                failed = true;
                            }
                        }
                    }
                }
            }
            Task::Move { dir, until } => {
                if now >= *until {
                    done = true;
                } else {
                    let p = me + *dir * 5.0;
                    self.step_toward(mover, p, speed_walk, dt);
                }
            }
            Task::Wait { until } => done = now >= *until,
            Task::Follow { who: other, dist, until } => {
                if now >= *until {
                    done = true;
                } else if let Some(p) = self.actor(*other).map(|a| a.pos) {
                    let d = (p - me).length();
                    if flyer && (airborne || d > 20.0) && d > *dist {
                        self.fly_toward(mover, p, fly, dt, true);
                    } else if d > *dist {
                        let sp = if d > 6.0 { speed_run.max(speed_walk) } else { speed_walk.max(1.6) };
                        self.step_toward(mover, p, sp, dt);
                    } else if let Some(a) = self.actor_mut(who) {
                        a.face(p - me, dt * 4.0);
                    }
                } else {
                    failed = true;
                }
            }
            Task::Face { target, until } => {
                if now >= *until {
                    done = true;
                } else if let Some(p) = self.resolve(target, who).map(|r| r.pos) {
                    if let Some(a) = self.actor_mut(who) {
                        a.face(p - me, dt * 4.0);
                    }
                }
            }
        }
        if done || failed {
            if let Some(a) = self.actor_mut(who) {
                if a.task.as_ref() == Some(&task) {
                    a.task = None;
                }
            }
            // Characters get down when they get there.
            if let (Task::Goto { .. }, ActorId::Npc(c)) = (&task, who) {
                if done && mover != who && self.cast.get(c).is_some_and(|n| !matches!(n.plan.front(), Some(Action::Goto { .. }))) {
                    let _ = self.dismount(who);
                }
            }
            if let (true, ActorId::Npc(c)) = (done, who) {
                if let Some(n) = self.cast.get_mut(c) {
                    n.detours = 0;
                }
            }
            if failed {
                if let ActorId::Npc(c) = who {
                    // Blocked on the way: step aside (one way, then the
                    // other) and try again before giving up on the plan.
                    let aside = match &task {
                        Task::Goto { target, run, .. } if self.cast.get(c).is_some_and(|n| n.detours < 2) => {
                            let goal = self.resolve(target, who).map(|r| r.pos).unwrap_or(me);
                            let away = Vec3::new(goal.x - me.x, 0.0, goal.z - me.z);
                            let d = away.normalize_or_zero();
                            let tries = self.cast.get(c).map(|n| n.detours).unwrap_or(0);
                            let side = Vec3::new(-d.z, 0.0, d.x) * if tries == 0 { 3.0 } else { -6.0 } - d * 1.0;
                            let p = me + side;
                            // Close by, whatever blocks them is what they came to.
                            (away.length() > 4.0 && self.can_be_at(who, p, 0.2) && self.open_at(who, p)).then(|| (Action::Goto { target: Target::Point(p.to_array()), run: false }, Action::Goto { target: target.clone(), run: *run }))
                        }
                        _ => None,
                    };
                    if let Some(n) = self.cast.get_mut(c) {
                        match aside {
                            Some((step, again)) => {
                                n.detours += 1;
                                n.plan.push_front(again);
                                n.plan.push_front(step);
                            }
                            None => {
                                n.detours = 0;
                                n.plan.clear();
                                n.think_at = now + 1.0;
                            }
                        }
                    }
                }
            }
        }
        !(done || failed)
    }

    /// Walking and running speeds (m/s): the traveler's stride, or the
    /// species' own.
    pub fn speeds(&self, who: ActorId) -> (f32, f32) {
        // Armour and loads slow a body down.
        let worn: f32 = match &self.worn_mass {
            Some(m) => m.get(&who).copied().unwrap_or(0.0),
            None => self.things.live().filter(|t| t.worn == Some(who)).map(|t| t.mass()).sum(),
        };
        let k = (1.0 - worn / (self.strength(who) * 3.0)).clamp(0.5, 1.0);
        let (w, r) = self.base_speeds(who);
        (w * k, r * k)
    }

    fn base_speeds(&self, who: ActorId) -> (f32, f32) {
        match who {
            ActorId::Player | ActorId::Traveler(_) => {
                let s = PLAYER_SPEED * (self.player.dims.height / 1.75).sqrt();
                (s, s * 1.7)
            }
            ActorId::Npc(c) => match self.cast.get(c) {
                // In the water, its own swimming pace (a dart to get away).
                Some(n) if n.a.swimming && n.species.moves.swim > 0.0 => (n.species.moves.swim, n.species.moves.swim * 1.6),
                Some(n) if !n.species.is_human() => (n.species.moves.walk.max(0.2), n.species.moves.run.max(n.species.moves.walk)),
                _ => (NPC_WALK, NPC_RUN),
            },
        }
    }

    /// Walk towards a point. False when stuck.
    fn step_toward(&mut self, who: ActorId, p: Vec3, speed: f32, dt: f32) -> bool {
        let Some(me) = self.actor(who).map(|a| a.pos) else { return false };
        // Through doorways, up stairs and round what is in the way: the
        // next step on the way.
        let dest = p;
        let p = self.way_to(who, p);
        let speed = speed * self.gait_factor(who);
        // Up or down a ladder on the way.
        if let Some(r) = self.ladder_step(who, p, speed * dt) {
            return r;
        }
        let d = Vec3::new(p.x - me.x, 0.0, p.z - me.z);
        let len = d.length();
        // A swimmer going into or out of a house under the water swims at
        // the height of its way (the doorway, the floor), and near the
        // doorway first goes down to it rather than into the wall above.
        let swimming = self.actor(who).is_some_and(|a| a.swimming);
        let housed = swimming && who != ActorId::Player && (self.room_at(me + Vec3::Y * 0.3).is_some() || self.room_at(dest + Vec3::Y * 0.3).is_some());
        // Near what it goes for under the water, a swimmer dives to it.
        let under = swimming && who != ActorId::Player && len < 15.0 && dest.y < self.snap.terrain.water(dest.x, dest.z) - 1.0;
        let dive_to = (housed || under).then_some(p.y);
        if let Some(a) = self.actor_mut(who) {
            a.dive_to = dive_to;
        }
        if housed && dive_to.is_some_and(|y| me.y - y > 0.6) && len < 2.5 {
            return true;
        }
        if len < 1e-3 {
            return true;
        }
        let dir = d / len;
        // Keeping room from others on the way: leaning off anyone too near,
        // round one straight ahead.
        let dir = match who {
            ActorId::Npc(_) => {
                let push = self.crowd(who, me);
                let back = push.dot(dir);
                let mut side = push - dir * back;
                if back < 0.0 && side.length() < back.abs() * 0.3 {
                    side = Vec3::new(-dir.z, 0.0, dir.x) * -back;
                }
                (dir + side * 1.5 + dir * back.max(-0.5) * 0.5).normalize_or(dir)
            }
            _ => dir,
        };
        if let Some(a) = self.actor_mut(who) {
            a.face(dir, dt * 6.0);
            a.asleep = false;
        }
        let step = dir * (speed * dt).min(len);
        self.walk(who, step);
        self.kick_things(who, step);
        let moved = self.actor(who).map(|a| a.moved).unwrap_or(0.0);
        let blocked = moved < speed * dt * 0.15 && len >= 0.3;
        if let ActorId::Npc(_) = who {
            let ahead = me + dir * (self.capsule(who).radius + 0.3);
            if blocked {
                // A shut door in the way: open it. Something low: duck under it.
                if let Some(door) = self.shut_door_near(ahead, 1.2) {
                    if self.can_work_hinges(who) {
                        let _ = self.open(who, &door, Some(false));
                    }
                } else if self.actor(who).is_some_and(|a| !a.crouching) && self.crouch_fits(who, ahead) {
                    self.crouch(who, Some(true));
                } else if self.actor(who).is_some_and(|a| a.stuck > 0.8) && self.crowd(who, ahead) == Vec3::ZERO {
                    // Nobody in the way, and still no further: what the way
                    // went through is shut.
                    self.way_blocked(who, ahead);
                }
            } else if self.actor(who).is_some_and(|a| a.crouching) && self.room_to_stand(who, me) && self.room_to_stand(who, ahead) {
                self.crouch(who, Some(false));
            }
        }
        if let Some(a) = self.actor_mut(who) {
            a.phase += moved * a.dims.stride;
            a.stuck = if blocked { a.stuck + dt } else { 0.0 };
            // Someone in the way for a moment is not a wall.
            a.stuck < 1.5
        } else {
            false
        }
    }

    // ------------------------------------------------------------ one character

    fn npc_tick(&mut self, cid: i64, dt: f32, near: bool) {
        let t = self.t;
        let night = self.night();
        let hour = self.hour();
        let player = self.player.pos;
        let Some(n) = self.cast.get_mut(cid) else { return };
        n.last_sim = t;
        // Needs drift, each at its species' pace.
        let tr = n.traits;
        let rate = n.species.needs;
        let sapient = n.species.mind == crate::world::species::Mind::Sapient;
        // People sleep at night; other species keep their own hours.
        let night = if n.species.is_human() { night } else { n.species.sleeps_at(hour) };
        n.needs.hunger = (n.needs.hunger + dt / 900.0 * rate.hunger).min(1.0);
        if n.a.asleep {
            n.needs.fatigue = (n.needs.fatigue - dt / 300.0).max(0.0);
        } else {
            n.needs.fatigue = (n.needs.fatigue + dt / 1100.0 * rate.fatigue).min(1.0);
        }
        n.needs.social = (n.needs.social + dt / 420.0 * (0.4 + tr.sociable) * rate.social).min(1.0);
        n.needs.fun = (n.needs.fun + dt / 520.0 * (0.4 + tr.playful) * rate.fun).min(1.0);
        n.needs.curiosity = (n.needs.curiosity - dt / 400.0 / rate.curiosity.max(0.1)).max(0.0);
        // Held in someone's arms: still until it wants down.
        // A fish out of water: nothing else matters to it.
        if self.stranded_tick(cid, dt) && !self.cast.get(cid).is_some_and(|n| n.a.carried()) {
            return;
        }
        if self.carried_tick(cid, dt) {
            return;
        }
        // Talking with a traveler: stand and face them.
        let talker = if self.talking_to == Some(cid) {
            Some((player, "the traveler".to_string()))
        } else {
            self.travelers.values().find(|t| t.talking_to == Some(cid)).map(|t| (t.a.pos, t.name.clone()))
        };
        if let Some((at, who)) = talker {
            if let Some(n) = self.cast.get_mut(cid) {
                n.a.task = None;
                n.a.asleep = false;
                n.a.face(at - n.a.pos, dt * 4.0);
                n.doing = format!("talking with {who}");
                n.needs.social = (n.needs.social - dt / 60.0).max(0.0);
            }
            let held_big = self.held_size(ActorId::Npc(cid));
            if let Some(n) = self.cast.get_mut(cid) {
                n.a.update_pose(t, dt, held_big);
            }
            return;
        }
        // A traveler comes close (the nearest, when several are about).
        let Some(n) = self.cast.get(cid) else { return };
        let (who, at) = self.nearest_traveler(n.a.pos).unwrap_or((ActorId::Player, player));
        let who_name = match who {
            ActorId::Traveler(_) => format!("{}, a traveler,", self.actor_name(who)),
            _ => "The traveler".into(),
        };
        let Some(n) = self.cast.get_mut(cid) else { return };
        let dp = (n.a.pos - at).length();
        if dp < 9.0 && !n.a.asleep {
            if t - n.last_player_near > 240.0 {
                n.last_player_near = t;
                let ctx = format!("{who_name} has come within a few metres of you. It is {}.", crate::render::sky::time_label(t));
                if self.has_llm && near && sapient {
                    let context = self.decide_context(cid, &ctx);
                    self.request(Request::Decide { cid, event: "player_near".into(), context }, at);
                }
            }
        }
        // Night: home and to bed.
        let Some(n) = self.cast.get_mut(cid) else { return };
        if night && !n.a.asleep && n.a.task.is_none() && n.mission.is_none() && !n.plan.iter().any(|a| matches!(a, Action::Sleep)) && !self.social.busy(ActorId::Npc(cid)) && n.species.want.is_empty() && !n.species.hostile {
            let home = n.def.home;
            // With the dark about, people sleep by the nearest light.
            let home = if sapient { self.night_shelter(home) } else { home };
            let Some(n) = self.cast.get_mut(cid) else { return };
            n.plan = vec![Action::GoHome, Action::Sleep].into();
            n.goal = "go home to sleep".into();
            n.doing = "heading home".into();
            if (n.a.pos - home).length() < 1.5 {
                n.plan = vec![Action::Sleep].into();
            }
        }
        if !night {
            if let Some(n) = self.cast.get_mut(cid) {
                if n.a.asleep {
                    n.a.asleep = false;
                    n.think_at = t + 1.0 + n.rand() as f64 * 3.0;
                    n.doing = "waking up".into();
                }
            }
        }
        if self.cast.get(cid).is_some_and(|n| n.restored) {
            self.after_load(cid);
        }
        self.need_tick(cid);
        let asleep = self.cast.get(cid).is_some_and(|n| n.a.asleep);
        // Some beings move only while the traveler isn't looking, however
        // near (the edge of the eye doesn't count: there it can be seen creeping).
        // Drawing back after its touch it may be seen going.
        let drawing_back = self.cast.get(cid).is_some_and(|n| n.species.touch.any() && self.t - n.touched_at < super::night::TOUCH_GAP);
        let watched = if drawing_back || !self.cast.get(cid).is_some_and(|n| n.species.moves_unseen && n.a.riding.is_none()) {
            None
        } else {
            self.actor(ActorId::Npc(cid)).and_then(|a| self.watched_by(a.pos, a.dims.radius))
        };
        if let Some((by, by_at)) = watched {
            // Within arm's reach looking doesn't save you: it reaches out.
            if self.watched_reach(cid, by) {
                self.touch(cid, by);
            }
            if let Some(n) = self.cast.get_mut(cid) {
                n.a.face(by_at - n.a.pos, dt * 2.0);
                n.a.update_pose(t, dt, None);
            }
            return;
        }
        // Going after something: look again every second or so, mid-stride
        // (its quarry moves, steps into the light, comes within reach).
        if !asleep && self.cast.get(cid).is_some_and(|n| !n.species.want.is_empty() && t >= n.think_at) && !self.social.busy(ActorId::Npc(cid)) {
            if !self.pursue_want(cid) {
                if let Some(n) = self.cast.get_mut(cid) {
                    n.think_at = t + 2.0;
                }
            }
        }
        // Meaning harm: its reflexes look again every second or so (its
        // quarry moves, comes within reach, a weapon lies near).
        if !asleep && self.cast.get(cid).is_some_and(|n| n.species.hostile && t >= n.think_at) && self.talking_to != Some(cid) {
            if !self.hunt(cid) {
                if let Some(n) = self.cast.get_mut(cid) {
                    n.think_at = t + 1.5;
                }
            }
        }
        if !asleep {
            let me = ActorId::Npc(cid);
            self.step_plan(me);
            self.run_task(me, dt);
            let idle = self.cast.get(cid).is_some_and(|n| n.a.task.is_none() && n.plan.is_empty());
            if idle && !self.social.busy(me) && t >= self.cast.get(cid).map(|n| n.think_at).unwrap_or(0.0) {
                self.decide(cid);
            }
            if near {
                self.make_room(me, dt);
            }
        }
        self.grow(cid);
        let held_big = self.held_size(ActorId::Npc(cid));
        let me = ActorId::Npc(cid);
        // A flyer with nothing to do comes down.
        if self.actor(me).is_some_and(|a| a.alt > 0.0 && a.task.is_none() && a.riding.is_none()) && self.rider_of(me).is_none() {
            self.settle(me, dt);
        }
        if let Some(n) = self.cast.get_mut(cid) {
            n.a.update_pose(t, dt, held_big);
        }
        self.step_footing(me, dt);
    }

    // ------------------------------------------------------------ choosing

    /// How they would push back a going incident (a fire, a spreading curse)
    /// near them or their home, if they are willing: people grown and
    /// sapient who are brave, whose home is at stake, or who see someone
    /// they know already at it. Nothing here knows what the trouble is: they
    /// imagine working each tool to hand against it (their hands, what they
    /// hold, what lies about) by the world's rules, and take the one that
    /// pushes it back most. With no tool that helps, they don't try. Nothing
    /// is done yet.
    pub(super) fn counter_plan(&mut self, cid: i64) -> Option<CounterPlan> {
        let me = ActorId::Npc(cid);
        let n = self.cast.get(cid)?;
        if self.mind_of(me) != crate::world::species::Mind::Sapient || n.growth < 0.8 || n.a.riding.is_some() {
            return None;
        }
        let (pos, home, brave, held, was) = (n.a.pos, n.def.home, n.traits.brave, n.a.held, n.aim.clone());
        let pick = |s: &Sim, p: Vec3, r: f32| s.incidents_near(p, r).into_iter().find(|i| !i.live.is_empty()).map(|i| (i.id, i.cause.subject.clone(), i.live.clone(), i.prop.clone(), i.words.noun.clone()));
        let near_home = pick(self, home, 40.0);
        let home_threatened = near_home.is_some();
        let (id, cause, live, prop, noun) = pick(self, pos, 50.0).or(near_home)?;
        let pi = self.vocab.id(&prop)?;
        // Courage is catching: someone they know is already at it.
        let neighbour_fights = self.cast.npcs.iter().any(|o| o.def.id != cid && o.here() && o.aim == Aim::Counter(id) && (o.a.pos - pos).length() < 40.0 && self.social.rel(me, ActorId::Npc(o.def.id)).is_some_and(|r| r.familiarity > 0.2));
        if !(brave > 0.45 || home_threatened || neighbour_fights) {
            return None;
        }
        // Where to work: what started it while it goes on, else the part
        // nearest what is at stake that nobody else has gone for.
        let guard = if home_threatened && (pos - home).length() > 30.0 { home } else { pos };
        let claimed: Vec<String> = self
            .cast
            .npcs
            .iter()
            .filter(|o| o.def.id != cid)
            .filter_map(|o| match o.plan.back() {
                Some(Action::Apply { to: Some(t), .. }) => Some(Self::part_key(t)),
                _ => None,
            })
            .collect();
        let parts: Vec<(Target, Vec3)> = self.trouble_parts(pos, 60.0).into_iter().filter(|(t, _)| live.contains(&Self::part_key(t))).collect();
        let free: Vec<(Target, Vec3)> = parts.iter().filter(|(t, _)| !claimed.contains(&Self::part_key(t))).cloned().collect();
        let parts = if free.is_empty() { parts } else { free };
        let (target, at) = parts.iter().find(|(t, _)| Self::part_key(t) == cause).or_else(|| parts.iter().min_by(|a, b| (a.1 - guard).length().total_cmp(&(b.1 - guard).length()))).cloned()?;
        let (before, _, _) = self.part_props(&target)?;
        // Imagine each tool to hand at work on it; take the one that pushes it back most.
        let mut tools: Vec<(Option<ThingId>, Props, f32)> = vec![(None, self.body_props(me), 0.0)];
        if let Some(h) = held.and_then(|h| self.things.get(h)) {
            tools.push((Some(h.id), h.props.clone(), 0.0));
        }
        for id in self.things.near(pos, 12.0) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() || t.worn.is_some() || t.anchored || t.mass() > STRENGTH || self.vocab.harm(&t.props) > 0.0 || Some(id) == held {
                continue;
            }
            tools.push((Some(id), t.props.clone(), (t.pos - pos).length()));
        }
        let mut best: Option<(Option<ThingId>, f32)> = None;
        for (tool, mut props, fetch) in tools {
            props[P_FORCE] = 1.0;
            let (_, after) = self.foresee(&props, &before, 2.0);
            let gain = before[pi] - after[pi];
            let score = gain / (1.0 + fetch / 8.0);
            if gain > 0.05 && best.is_none_or(|b| score > b.1) {
                best = Some((tool, score));
            }
        }
        let (tool, _) = best?;
        Some(CounterPlan { id, noun, tool, target, at, held, pos, was })
    }

    /// Set out to push the trouble back as planned.
    pub(super) fn do_counter(&mut self, cid: i64, p: CounterPlan) {
        let me = ActorId::Npc(cid);
        let CounterPlan { id, noun, tool, target, at, held, pos, was } = p;
        if was != Aim::Counter(id) {
            let k = self.cast.get_mut(cid).map(|n| n.rand() * 100.0).unwrap_or(0.0) as u32;
            let line = template_line("rally", &noun, self.hour(), k);
            self.say(me, &line, None);
        }
        let mut steps = Vec::new();
        if let Some(t) = tool.filter(|t| Some(*t) != held) {
            if held.is_some() {
                steps.push(Action::Drop);
            }
            steps.push(Action::Hold { target: Target::Thing(t) });
        }
        if (at - pos).length() > 2.0 {
            steps.push(Action::Goto { target: target.clone(), run: true });
        }
        steps.push(Action::Apply { with: tool.map(Target::Thing), to: Some(target), secs: None });
        self.plan(me, steps, &format!("stop the {noun}"), false);
        self.set_aim(cid, Aim::Counter(id), &format!("fighting the {noun}"));
        if was != Aim::Counter(id) {
            self.add_goal(cid, super::goals::Want::Over { incident: id }, &format!("stop the {noun}"), "it threatens us", 0.9, None, None, super::goals::Source::Incident);
        }
    }

    /// Harm close by (what burns, what is cursed…), nearest first, with
    /// its size and name.
    pub(super) fn hazards_near(&self, pos: Vec3, r: f32) -> Vec<(Vec3, f32, String)> {
        let vocab = self.vocab.clone();
        let mut v: Vec<(Vec3, f32, String)> = Vec::new();
        for t in self.things.live().filter(|t| !t.held() && t.worn.is_none() && (t.pos - pos).length() < r + 4.0 && vocab.harm(&t.props) > 0.05) {
            let size = self.snap.type_of(t.type_id).map(|ty| ty.radius() * t.scale).unwrap_or(0.5).clamp(0.2, 4.0);
            v.push((t.pos, size, self.thing_name(t.id)));
        }
        for c in self.field.cells.values().filter(|c| (c.pos - pos).length() < r + 4.0 && vocab.harm(&c.props) > 0.05) {
            v.push((c.pos, c.size.clamp(0.2, 4.0), self.snap.type_of(c.type_id).map(|t| t.name().to_string()).unwrap_or_default()));
        }
        v.retain(|(p, size, _)| (*p - pos).length() < r + size);
        v.sort_by(|a, b| (a.0 - pos).length().total_cmp(&(b.0 - pos).length()));
        v
    }

    /// Whether the maker clock is due and this character is the one to answer
    /// it: the person near the traveler who has gone longest without working.
    pub(super) fn maker_turn(&self, cid: i64) -> bool {
        let t = self.t;
        let gap = self.cfg.maker_secs as f64;
        if !self.has_llm || gap <= 0.0 || t - self.cast.last_made < gap || t - self.cast.last_work < gap * 0.5 || self.interp.building.len() >= 2 {
            return false;
        }
        let ready = |n: &Npc| {
            n.here()
                && !n.a.asleep
                && n.species.mind == crate::world::species::Mind::Sapient
                && n.a.task.is_none()
                && n.plan.is_empty()
                && n.mission.is_none()
                && self.talking_to != Some(n.def.id)
                && !self.social.busy(ActorId::Npc(n.def.id))
                && self.dist_to_travelers(n.a.pos) < 60.0
        };
        self.cast.npcs.iter().filter(|n| ready(n)).min_by(|a, b| a.last_work.total_cmp(&b.last_work).then(a.def.id.cmp(&b.def.id))).is_some_and(|n| n.def.id == cid)
    }

    /// Small loose things lying about: live ones and placed ones not yet touched.
    pub fn loose_near(&mut self, p: Vec3, range: f32) -> usize {
        let near = self.things.near(p, range);
        let live = near.into_iter().filter(|id| self.things.get(*id).is_some_and(|t| !t.held() && !t.anchored && t.mass() <= STRENGTH)).count();
        let placed = self
            .snap
            .near_chunk(crate::world::chunk_of(p.x, p.z))
            .filter(|pl| (pl.pos - p).length() < range && !self.things.by_instance.contains_key(&pl.id))
            .filter(|pl| self.snap.type_of(pl.type_id).is_some_and(|ty| !ty.has_tag("building") && !ty.has_tag("landmark") && ty.radius() < 1.0))
            .count();
        live + placed
    }

    pub fn set_doing(&mut self, cid: i64, what: &str) {
        self.set_aim(cid, Aim::Other, what);
    }

    /// What they are about, and the words for it.
    pub fn set_aim(&mut self, cid: i64, aim: Aim, what: &str) {
        if let Some(n) = self.cast.get_mut(cid) {
            n.doing = what.to_string();
            n.aim = aim;
        }
    }

    /// Go somewhere near home: on dry land, or in the water as much as the
    /// species lives in it (an otter now on the bank, now in the river; a
    /// fish only in the water).
    pub(super) fn wander(&mut self, cid: i64, home: Vec3, radius: f32) {
        let who = ActorId::Npc(cid);
        let (wet, caver) = match self.cast.get_mut(cid) {
            Some(n) => (n.species.water > 0.0 && n.rand() < n.species.water, n.species.caves),
            None => return,
        };
        // Those who live in caves keep to their floors.
        if caver {
            for _ in 0..8 {
                let (a, r) = match self.cast.get_mut(cid) {
                    Some(n) => (n.rand() * std::f32::consts::TAU, 1.0 + n.rand() * radius.min(10.0)),
                    None => return,
                };
                let p = home + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
                if let Some(f) = self.snap.terrain.cave_floor_under(p.x, p.z) {
                    self.plan(who, vec![Action::Goto { target: Target::Point([p.x, f, p.z]), run: false }], "", false);
                    return;
                }
            }
            return;
        }
        for _ in 0..6 {
            let (a, r) = match self.cast.get_mut(cid) {
                Some(n) => (n.rand() * std::f32::consts::TAU, 3.0 + n.rand() * radius),
                None => return,
            };
            let p = home + Vec3::new(a.cos() * r, 0.0, a.sin() * r);
            let t = &self.snap.terrain;
            let in_water = t.height(p.x, p.z) < t.water(p.x, p.z) - 0.3;
            let there = if wet { self.can_be_at(who, p, 0.3) } else { t.height(p.x, p.z) > t.water(p.x, p.z) + 0.3 };
            // Somewhere it could stand: not in a wall or a house's middle.
            if in_water == wet && there && self.open_at(who, p) {
                self.plan(ActorId::Npc(cid), vec![Action::Goto { target: Target::Point([p.x, 0.0, p.z]), run: false }], "", false);
                return;
            }
        }
    }

    /// The most liked awake person nearby (and how much they are liked).
    pub fn best_company(&self, cid: i64, range: f32) -> Option<(ActorId, f32)> {
        let me = ActorId::Npc(cid);
        let pos = self.cast.get(cid)?.a.pos;
        let mut best: Option<(f32, ActorId, f32)> = None;
        for o in self.actor_ids() {
            if o == me {
                continue;
            }
            let Some(a) = self.actor(o) else { continue };
            let d = (a.pos - pos).length();
            if d > range || a.asleep {
                continue;
            }
            if o == ActorId::Player && self.talking_to.is_some() {
                continue;
            }
            let aff = self.social.rel(me, o).map(|r| r.affection - r.rivalry).unwrap_or(0.0);
            if aff < -0.2 || self.social.busy(o) {
                continue;
            }
            let score = aff + 0.3 - d / range * 0.3;
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, o, aff));
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// The nearest live thing or scatter item whose properties match.
    pub fn nearest_matching(&mut self, p: Vec3, range: f32, f: impl Fn(&[f32]) -> bool) -> Option<(Target, Vec3)> {
        let mut best: Option<(f32, Target, Vec3)> = None;
        for id in self.things.near(p, range) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() || t.anchored {
                continue;
            }
            if !f(&t.props) {
                continue;
            }
            let d = (t.pos - p).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Thing(id), t.pos));
            }
        }
        let snap = self.snap.clone();
        // Placed things not yet touched (a settlement's tools and toys).
        for pl in snap.near_chunk(crate::world::chunk_of(p.x, p.z)) {
            let d = (pl.pos - p).length();
            if d > range || self.things.by_instance.contains_key(&pl.id) || best.as_ref().is_some_and(|b| d >= b.0) {
                continue;
            }
            let Some(ty) = snap.type_of(pl.type_id) else { continue };
            let base = scaled((*self.type_props.get(&self.vocab, ty)).clone(), pl.scale);
            if base[P_MASS] > STRENGTH || !f(&base) {
                continue;
            }
            best = Some((d, Target::Instance(pl.id), pl.pos));
        }
        for it in self.cache.items_near(&snap, p, range.min(20.0)) {
            let Some(ty) = snap.type_of(it.inst.info[0]) else { continue };
            let mut base = (*self.type_props.get(&self.vocab, ty)).clone();
            if let Some(c) = self.field.cells.get(&it.cell) {
                base = c.props.clone();
            }
            if !f(&base) {
                continue;
            }
            let d = (it.inst.pos() - p).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Cell([it.cell.0, it.cell.1]), it.inst.pos()));
            }
        }
        best.map(|b| (b.1, b.2))
    }

    /// A small loose thing worth picking up (not food, not alive, not already
    /// in someone's pile at home).
    pub(super) fn loose_thing(&mut self, p: Vec3, home: Vec3, range: f32) -> Option<(Target, Vec3)> {
        let homes: Vec<Vec3> = self.cast.npcs.iter().map(|n| n.def.home).collect();
        let near_a_home = |q: Vec3| homes.iter().any(|h| (*h - q).length() < 5.0);
        let mut best: Option<(f32, Target, Vec3)> = None;
        for id in self.things.near(p, range) {
            let Some(t) = self.things.get(id) else { continue };
            if t.held() || t.anchored || t.mass() > 3.0 || t.props[P_EDIBLE] > 0.0 || self.vocab.harm(&t.props) > 0.0 || near_a_home(t.pos) {
                continue;
            }
            let d = (t.pos - p).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Thing(id), t.pos));
            }
        }
        let snap = self.snap.clone();
        let vocab = self.vocab.clone();
        for it in self.cache.items_near(&snap, p, range) {
            let Some(ty) = snap.type_of(it.inst.info[0]) else { continue };
            // Loose is a matter of properties: small, not living, not food, harmless.
            let pr = scaled((*self.type_props.get(&vocab, ty)).clone(), it.inst.pos_scale[3]);
            let loose = pr[P_MASS] <= 3.0 && pr[P_ALIVE] <= 0.0 && pr[P_EDIBLE] <= 0.0 && vocab.harm(&pr) <= 0.0;
            if !loose || near_a_home(it.inst.pos()) || self.field.cells.contains_key(&it.cell) {
                continue;
            }
            let d = (it.inst.pos() - p).length();
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Cell([it.cell.0, it.cell.1]), it.inst.pos()));
            }
        }
        let _ = home;
        best.map(|b| (b.1, b.2))
    }

    /// Something made recently that this character hasn't looked at yet.
    pub(super) fn novelty_near(&self, cid: i64, p: Vec3, range: f32) -> Option<(Target, Vec3, String)> {
        let mut best: Option<(f32, Target, Vec3, String)> = None;
        for t in self.things.live() {
            if self.t - t.born > 600.0 || t.held() {
                continue;
            }
            let d = (t.pos - p).length();
            if d > range {
                continue;
            }
            let Some(ty) = self.snap.type_of(t.type_id) else { continue };
            if ty.builtin || self.social.has_seen(cid, ty.name()) {
                continue;
            }
            if best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Thing(t.id), t.pos, ty.name().to_string()));
            }
        }
        best.map(|b| (b.1, b.2, b.3))
    }

    /// Something that looks made for throwing things at (a hoop, a goal, a
    /// target, a basket, a bell…), by its tags or name.
    pub(super) fn throwing_mark(&self, p: Vec3, range: f32) -> Option<(Target, Vec3)> {
        // What people aim at is a property ("mark"), whatever it is called.
        let mut best: Option<(f32, Target, Vec3)> = None;
        let vocab = self.vocab.clone();
        let mut marks: HashMap<u32, bool> = HashMap::new();
        for pl in &self.snap.instances {
            let d = (pl.pos - p).length();
            if d > range {
                continue;
            }
            let Some(ty) = self.snap.type_of(pl.type_id) else { continue };
            let fits = *marks.entry(ty.id).or_insert_with(|| super::props::type_props(&vocab, ty)[P_MARK] >= 0.4);
            if fits && best.as_ref().is_none_or(|b| d < b.0) {
                let target = match self.things.by_instance.get(&pl.id) {
                    Some(t) => Target::Thing(*t),
                    None => Target::Instance(pl.id),
                };
                best = Some((d, target, pl.pos));
            }
        }
        for t in self.things.live() {
            let d = (t.pos - p).length();
            if d > range || t.held() {
                continue;
            }
            if t.props[P_MARK] >= 0.4 && best.as_ref().is_none_or(|b| d < b.0) {
                best = Some((d, Target::Thing(t.id), t.pos));
            }
        }
        best.map(|b| (b.1, b.2))
    }

    // ------------------------------------------------------------ the LLM

    /// Ask the planner about an event (rate-limited per character).
    pub fn ask(&mut self, cid: i64, event: &str, what: &str) {
        self.ask_planner(cid, event, what, false);
    }

    /// The traveler said something to a character and they answered: they
    /// may now do what was asked (make it and hand it over, show the way…).
    pub fn asked(&mut self, cid: i64, said: &str, replied: &str) {
        let what = format!(
            "The traveler just said to you: \"{said}\". You answered: \"{replied}\". If they asked you to do, make, fetch, give or show something and you agreed, it is a promise: set \"promise\" and start on it now (to make something for them, a \"do\" step that makes it, then a \"give\" step to the traveler; it is handed over once made). If you refused, or nothing was asked, reply with no steps and no promise. Set \"say\" to null: you have already answered."
        );
        self.ask_planner(cid, "asked", &what, true);
    }

    /// Ask the planner what to do about an event. Returns whether it was asked.
    pub(super) fn ask_planner(&mut self, cid: i64, event: &str, what: &str, now: bool) -> bool {
        self.ask_planner_weighted(cid, event, what, now, if now { 0.9 } else { 0.5 })
    }

    /// The same, with how much it matters against other waiting requests
    /// (a free moment matters less than anything that happened).
    pub(super) fn ask_planner_weighted(&mut self, cid: i64, event: &str, what: &str, now: bool, weight: f32) -> bool {
        use crate::world::species::Mind;
        // Sapient minds plan often, simple ones (most animals) now and then,
        // instinct never: all of it a setting, not a law.
        let every = match self.mind_of(ActorId::Npc(cid)) {
            Mind::Sapient => self.cfg.plan_secs,
            Mind::Simple if self.cfg.simple_plan_secs > 0.0 => self.cfg.simple_plan_secs,
            _ => return false,
        };
        if !self.has_llm {
            return false;
        }
        let t = self.t;
        let Some(n) = self.cast.get_mut(cid) else { return false };
        if t < n.next_llm && !now {
            return false;
        }
        n.next_llm = t + every as f64;
        let pos = n.a.pos;
        if self.dist_to_travelers(pos) > self.cfg.near && !self.cfg.medium_llm {
            return false;
        }
        let context = self.decide_context(cid, what);
        self.request_weighted(Request::Decide { cid, event: event.into(), context }, pos, weight);
        true
    }

    pub fn ask_planner_now(&mut self, cid: i64, event: &str, what: &str) -> bool {
        self.ask_planner(cid, event, what, true)
    }

    /// What a character knows right now, for the planner.
    pub fn decide_context(&mut self, cid: i64, what: &str) -> String {
        let Some(n) = self.cast.get(cid) else { return what.to_string() };
        let me = ActorId::Npc(cid);
        let pos = n.a.pos;
        let held = n.a.held.map(|h| format!("{}{}", self.thing_name(h), self.arms_note(h).map(|w| format!(" ({w})")).unwrap_or_default()));
        let needs = n.needs;
        let goal = n.goal.clone();
        let mut things = Vec::new();
        let mut shown = std::collections::BTreeSet::new();
        for id in self.things.near(pos, 25.0).into_iter().take(12) {
            let Some(t) = self.things.get(id) else { continue };
            let Some(ty) = self.snap.type_of(t.type_id) else { continue };
            // Whatever stands out about it, in the world's own terms (a
            // universe's curse as much as fire): properties well off their default.
            let mut notes: Vec<String> = Vec::new();
            for (i, v) in t.props.iter().enumerate() {
                if i == P_MASS || i == P_SOLID || self.vocab.engine_only(i) {
                    continue;
                }
                let d = self.vocab.defaults.get(i).copied().unwrap_or(0.0);
                let scale = if i == P_TEMP { 30.0 } else { d.abs().max(1.0) * 0.3 };
                if (v - d).abs() > scale {
                    notes.push(format!("{} {}", self.vocab.names[i], if v.abs() >= 10.0 { format!("{v:.0}") } else { format!("{v:.1}") }));
                    shown.insert(i);
                }
            }
            if t.mass() > STRENGTH && !t.anchored {
                notes.push("needs two to carry".into());
            }
            let by = t.origin.made_by.clone().map(|m| format!(", made by {m}")).unwrap_or_default();
            let tname = if t.origin.remains.is_some() { self.thing_name(id) } else { ty.name().to_string() };
            things.push(format!("{tname} ({:.0} m{}{}{})", (t.pos - pos).length(), if notes.is_empty() { "" } else { ", " }, notes.join(", "), by));
        }
        for pl in &self.snap.instances {
            let d = (pl.pos - pos).length();
            if d < 30.0 && !self.things.by_instance.contains_key(&pl.id) {
                if let Some(ty) = self.snap.type_of(pl.type_id) {
                    things.push(format!("{} ({d:.0} m)", ty.name()));
                }
            }
        }
        things.truncate(16);
        let mut people = Vec::new();
        for o in self.actor_ids() {
            if o == me {
                continue;
            }
            let Some(a) = self.actor(o) else { continue };
            let d = (a.pos - pos).length();
            if d > 40.0 {
                continue;
            }
            let mut rel = self.social.rel(me, o).map(|r| r.describe()).unwrap_or_else(|| "a stranger".into());
            if let Some(f) = super::social::fear_words(self.social.fear(me, o)) {
                rel = format!("{rel}, you are {}", f.replace(" them", ""));
            }
            if let Some(w) = self.armed(o) {
                rel = format!("{rel}, holding {}", super::actions::the(&w));
            }
            let doing = match o {
                ActorId::Npc(c) => self.cast.get(c).map(|x| x.doing.clone()).unwrap_or_default(),
                ActorId::Player => "the traveler".into(),
                ActorId::Traveler(_) => "another traveler".into(),
            };
            people.push(format!("{} ({d:.0} m, {rel}{}{})", self.actor_name(o), if doing.is_empty() { "" } else { ", " }, doing));
        }
        let recent: Vec<String> = self.log.recent.iter().rev().filter(|e| e.pos.is_some_and(|p| (Vec3::from(p) - pos).length() < 40.0) && self.t - e.t < 300.0).take(6).map(|e| e.text.clone()).collect();
        let twist = self.drive_line(cid).map(|l| format!("\n{l}")).unwrap_or_default();
        let meanings: Vec<String> = shown.iter().map(|i| format!("{}: {}", self.vocab.names[*i], self.vocab.meanings[*i])).collect();
        let twist = if meanings.is_empty() { twist } else { format!("{twist}\nWhat those numbers mean: {}.", meanings.join("; ")) };
        let twist = format!("{twist}{}", self.trouble_line(pos).map(|l| format!("\n{l}")).unwrap_or_default());
        let twist = format!("{twist}{}", self.body_line(cid).map(|l| format!("\nOn your own body: {l}.")).unwrap_or_default());
        let twist = format!("{twist}{}", self.goals_line(cid).map(|l| format!("\nYour goals: {l}.")).unwrap_or_default());
        let twist = format!("{twist}{}", self.beliefs_line(cid).map(|l| format!("\nWhat you believe: {l}.")).unwrap_or_default());
        let twist = format!("{twist}{}", self.goals_done_line(cid).map(|l| format!("\nLately: {l}.")).unwrap_or_default());
        let twist = format!("{twist}{}", self.menu_line(cid).map(|l| format!("\nThings you could do now (best first): {l}.")).unwrap_or_default());
        format!(
            "{what}{twist}\nIt is {}. {}\nYou hold: {}. Your current goal: {}.\nYou feel: hunger {:.1}, tiredness {:.1}, loneliness {:.1}, boredom {:.1}, curiosity {:.1} (0 = fine, 1 = urgent).\nThings around you: {}.\nPeople around you: {}.\nRecently near you: {}.",
            crate::render::sky::time_label(self.t),
            self.weather_line(cid),
            held.unwrap_or_else(|| "nothing".into()),
            if goal.is_empty() { "none".into() } else { goal },
            needs.hunger,
            needs.fatigue,
            needs.social,
            needs.fun,
            needs.curiosity,
            if things.is_empty() { "nothing in particular".into() } else { things.join("; ") },
            if people.is_empty() { "nobody".into() } else { people.join("; ") },
            if recent.is_empty() { "nothing".into() } else { recent.join("; ") },
        )
    }

    /// A planner decision arrived.
    pub fn on_decision(&mut self, cid: i64, d: Decision) {
        let me = ActorId::Npc(cid);
        let t = self.t;
        let night = self.night();
        let Some(n) = self.cast.get_mut(cid) else { return };
        if n.a.asleep && night {
            return;
        }
        let summary = d.goal.clone().unwrap_or_else(|| d.action.clone());
        n.log_decision(t, format!("decided: {summary}"));
        let mut steps: Vec<Action> = d.steps.iter().filter_map(parse_step).take(10).collect();
        self.decision_goals(cid, &d, &steps);
        let said = d.say.as_ref().is_some_and(|s| !s.trim().is_empty());
        let mut steps = self.need_decision(cid, std::mem::take(&mut steps), said);
        // Making something and then giving it: the thing takes a while to be
        // made, so the giving waits until it is in their hands.
        if let Some(i) = steps.iter().position(|a| matches!(a, Action::Do { .. })) {
            if let Some(j) = steps.iter().skip(i + 1).position(|a| matches!(a, Action::Give { .. })).map(|j| j + i + 1) {
                if let Action::Give { to } = steps.remove(j) {
                    if let Some(Target::Actor(other)) = self.resolve(&to, me).map(|r| r.target) {
                        if let Some(n) = self.cast.get_mut(cid) {
                            n.deliver = Some((other, t + 300.0));
                        }
                    }
                }
            }
        }
        let line = d.say.clone().or(d.line.clone()).filter(|l| !l.trim().is_empty());
        if steps.is_empty() {
            match d.action.as_str() {
                "approach" | "approach_player" | "greet" => {
                    steps.push(Action::Goto { target: Target::Actor(ActorId::Player), run: false });
                    if let Some(l) = &line {
                        steps.push(Action::Say { text: l.clone(), to: Some(Target::Actor(ActorId::Player)) });
                    }
                    steps.push(Action::Follow { target: Target::Actor(ActorId::Player), secs: Some(0.1) });
                }
                "watch" | "look" => {
                    let until = t + 8.0;
                    if let Some(n) = self.cast.get_mut(cid) {
                        n.a.task = Some(Task::Face { target: Target::Actor(ActorId::Player), until });
                    }
                }
                "go_home" | "home" | "sleep" => steps.push(Action::GoHome),
                _ => {}
            }
        } else if let Some(l) = &line {
            // A plan with its own line (often the same words, said on
            // arrival) says it then, not twice.
            if !steps.iter().any(|a| matches!(a, Action::Say { .. })) {
                self.say(me, l, None);
            }
        }
        if !steps.is_empty() {
            let goal = d.goal.clone().unwrap_or_else(|| d.action.clone());
            self.plan(me, steps, &goal, true);
            self.set_doing(cid, &goal);
            let pos = self.cast.get(cid).map(|n| n.a.pos);
            self.event("decided", Some(me), None, format!("{} decided to {goal}", self.actor_name(me)), pos, json!({ "steps": d.steps }));
        }
    }

    /// The player spoke to a character outside the talk screen (agents).
    pub fn player_talks(&mut self, cid: i64, text: &str) {
        if !self.speaks(ActorId::Npc(cid)) {
            self.animal_answers(cid);
            return;
        }
        let context = self.decide_context(cid, "The traveler is talking to you.");
        self.request_now(Request::Talk { cid, text: text.to_string(), context });
    }

    /// A line of dialogue arrived (from the brain) for a character.
    pub fn npc_said(&mut self, cid: i64, text: &str) {
        let me = ActorId::Npc(cid);
        let Some(pos) = self.actor(me).map(|a| a.pos) else { return };
        let name = self.actor_name(me);
        self.event("said", Some(me), Some("player".into()), format!("{name} said: \"{text}\""), Some(pos), json!({ "text": text }));
    }

    /// Notice gestures and ask the planner about big ones.
    pub fn npc_note_event(&mut self, cid: i64, what: &str) {
        let t = self.t;
        if let Some(n) = self.cast.get_mut(cid) {
            n.log_decision(t, what.to_string());
        }
    }

    pub fn say_template(&mut self, who: ActorId, kind: &str, other: &str) {
        let k = match who {
            ActorId::Npc(c) => self.cast.get_mut(c).map(|n| {
                n.rng = crate::noise::pcg(n.rng);
                n.rng
            }),
            _ => None,
        }
        .unwrap_or(0);
        let t = self.t;
        if let ActorId::Npc(c) = who {
            if let Some(n) = self.cast.get_mut(c) {
                if t - n.last_line < 3.0 {
                    return;
                }
                n.last_line = t;
            }
        }
        let line = template_line(kind, other, self.hour(), k);
        self.say(who, &line, None);
    }
}

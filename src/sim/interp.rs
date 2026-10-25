//! The interpreter: when an action falls outside the rules and the things'
//! own code ("I rub the stone on the lantern"), the LLM decides what happens.
//! It must answer as primitives (property changes, state, new things,
//! removals), never only words, so its inventions obey the same rules as
//! everything else. Answers are cached by (what was done, to what, with
//! what): the same cause gives the same effect, and the world is learnable.

use super::actions::{ActErr, Outcome, Resolved};
use super::props::*;
use super::things::{Origin, ThingId};
use super::render::FLASH_FOR;
use super::{ActorId, Note, Request, Sim, Target};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Change {
    /// "held", "target" or "actor".
    pub target: String,
    #[serde(default)]
    pub props: BTreeMap<String, f32>,
    #[serde(default)]
    pub state: BTreeMap<String, f32>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Make {
    pub name: String,
    /// Replace the "held" or "target" thing with the new one.
    #[serde(default)]
    pub replace: Option<String>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub size_m: Option<[f32; 3]>,
    #[serde(default)]
    pub props: BTreeMap<String, f32>,
}

/// What a deed does to a being: what it wears, its look sliders, its needs
/// and feelings change, it can learn a trick, its body can be reshaped (its
/// own from then on), and (where the world has its own forces) it can
/// become another species.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct BeingFx {
    /// Added to its needs (hunger, fatigue, social, fun, curiosity).
    #[serde(default)]
    pub needs: BTreeMap<String, f32>,
    /// Added to how it feels about the actor (affection, trust).
    #[serde(default)]
    pub feel: BTreeMap<String, f32>,
    /// Look sliders set (within the body's ranges).
    #[serde(default)]
    pub look: BTreeMap<String, f32>,
    /// Its size changes by this factor (2 = twice as big).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grow: Option<f32>,
    /// Layers put on it (made if nobody has made one yet).
    #[serde(default)]
    pub wear: Vec<Make>,
    /// Layers taken off it, by name.
    #[serde(default)]
    pub take_off: Vec<String>,
    /// A gesture it learns (a trick), done now and when greeting.
    #[serde(default)]
    pub learn: Option<String>,
    /// Another species it turns into.
    #[serde(default, rename = "become")]
    pub turn_into: Option<String>,
    /// What changes about its body's shape (a poofy tail): its body is
    /// rewritten, for this being only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reshape: Option<String>,
    /// Health it takes (0..1: a bruise 0.05, a deep cut 0.2, a grave wound
    /// 0.5, 1 kills), less what the being wears softens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hurt: Option<f32>,
    /// What the hurt was, as it would end "they were …" ("stabbed in the eye
    /// with a stick"): remembered by whoever saw, and told if it kills.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub how: Option<String>,
    /// Done to the actor's own body (stab myself, eat the poison berries),
    /// not to what they point at.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub on_self: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NewBeing {
    pub species: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Its look sliders (its species' body's), from the description.
    #[serde(default)]
    pub look: BTreeMap<String, f32>,
    /// Its size against its species' (1 = usual).
    #[serde(default)]
    pub size: Option<f32>,
}

/// Make something new in the world (a well, a lighthouse on that hill, a
/// stool): handed to the builder, which places it in view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MakeFx {
    /// What to make and where, in words ("a stone well by the path").
    pub text: String,
}

/// Cut a piece out of a thing at the spot that was touched.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct CutFx {
    #[serde(default = "target_role")]
    pub target: String,
    /// Radius (or half-width) in metres.
    #[serde(default = "cut_size")]
    pub size_m: f32,
    /// "round" (default) or "square".
    #[serde(default)]
    pub shape: String,
}

/// Rewrite a thing's shape code, maybe working another thing into it.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ReshapeFx {
    #[serde(default = "target_role")]
    pub target: String,
    /// The changed thing's name ("hut with a stick on the wall").
    pub name: String,
    /// What changes about its shape, for the builder.
    pub change: String,
    /// "held": the held thing becomes part of it (and is used up).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub with: Option<String>,
}

/// Ground taken away where the deed points: a hole, pit, crater, trench,
/// a cave's mouth dug into a hillside.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct GroundFx {
    /// How deep, in metres.
    #[serde(default = "two")]
    pub depth_m: f32,
    /// How wide across, in metres.
    #[serde(default = "three")]
    pub wide_m: f32,
    /// How long, for a trench or a ditch (along the way the actor looks);
    /// None for a round hole.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_m: Option<f32>,
    /// "sheer" (default: straight sides) or "sloped" (a bowl anyone can
    /// walk down into and out of).
    #[serde(default)]
    pub sides: String,
    /// What it is, when more than a hole: a way laid along the land ("canal",
    /// "ditch", "moat" are cut; "road", "path", "street" are laid level;
    /// "causeway", "dyke", "embankment" are built up), or a "tunnel" (a cave,
    /// a mine, a burrow) dug in. Empty: a hole, a pit, a trench.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub way: String,
    /// What covers a way (cobbles, gravel, packed earth), rgb 0–255.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[f32; 3]>,
    /// The hollow holds water to its brim (a pond, a lake, a pool): it
    /// rises in it until it would spill over the lowest point of its rim.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill: bool,
}

fn two() -> f32 {
    2.0
}
fn three() -> f32 {
    3.0
}

fn target_role() -> String {
    "target".into()
}
fn cut_size() -> f32 {
    0.25
}

/// What the interpreter may answer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct InterpEffect {
    #[serde(default)]
    pub narration: String,
    /// The target gives what is made without changing (a fruit tree, an
    /// oven, a larder, the sea to a fisher): it can be done again and again.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub source: bool,
    #[serde(default)]
    pub changes: Vec<Change>,
    #[serde(default)]
    pub create: Vec<Make>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub make: Vec<MakeFx>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cut: Vec<CutFx>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reshape: Vec<ReshapeFx>,
    /// The land itself dug away where the deed points.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ground: Vec<GroundFx>,
    /// What it does to a person or creature (the target).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub being: Option<BeingFx>,
    /// New beings brought into the world (only where the world allows it).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub beings: Vec<NewBeing>,
    /// A short line the actor says.
    #[serde(default)]
    pub say: Option<String>,
    /// A character couldn't do it as things are: the one thing it needs
    /// (a place, a thing, someone, a time), for them to go and meet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub needs: Option<super::needs::Need>,
    /// The weather made to change (the traveler's word, this world's magic).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<WeatherFx>,
    /// false for one-off results that should not be reused.
    #[serde(default = "yes")]
    pub cache: bool,
}

/// Weather made on purpose: a kind of weather and how many game hours it holds.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct WeatherFx {
    #[serde(flatten)]
    pub kind: crate::world::weather::Kind,
    #[serde(default = "six")]
    pub hours: f32,
}

fn six() -> f32 {
    6.0
}

impl InterpEffect {
    /// Something is still to be built before it all shows (a new thing, a reshape).
    fn builds(&self) -> bool {
        !self.make.is_empty() || !self.create.is_empty() || !self.reshape.is_empty()
    }

    /// Nothing in the world changes: a refusal or a "nothing happens".
    fn changes_nothing(&self) -> bool {
        self.changes.is_empty() && self.create.is_empty() && self.remove.is_empty() && self.make.is_empty() && self.cut.is_empty() && self.reshape.is_empty() && self.ground.is_empty() && self.being.is_none() && self.beings.is_empty() && self.weather.is_none()
    }
}

fn yes() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct PendingInterp {
    pub actor: ActorId,
    pub text: String,
    pub held: Option<ThingId>,
    pub target: Option<Target>,
    /// The spot on the target that was touched (world).
    pub hit: Option<glam::Vec3>,
    pub key: String,
    pub at: f64,
    /// Where the doer stood and looked when asking: what it makes appears there.
    pub aim: Option<Aim>,
}

/// A pose to make things from: feet, eye, and where the eye looks.
#[derive(Clone, Copy, Debug, Default)]
pub struct Aim {
    pub pos: glam::Vec3,
    pub eye: glam::Vec3,
    pub yaw: f32,
    pub pitch: f32,
}

#[derive(Clone, Debug, Default)]
pub struct PendingBuild {
    pub name: String,
    /// Whose deed it is (None: the world's own, like a rule's new thing).
    pub by: Option<ActorId>,
    /// Things waiting for this type: (thing, transform it rather than spawn next to it).
    pub then: Vec<(ThingId, bool)>,
    /// Or a place to put a new one (and who to hand it to).
    pub place: Option<(glam::Vec3, Option<ActorId>, Option<ThingId>)>,
    /// Things being reshaped into it (by whom, and what is worked into
    /// them, used up when it is done), and what the change was.
    pub reshape: Vec<(ThingId, ActorId, Option<ThingId>)>,
    pub change: String,
    /// A layer to put on someone when it exists: (wearer, by whom).
    pub wear_on: Option<(ActorId, ActorId)>,
    /// A character whose body this is, reshaped (see `reshape_being`).
    pub body_of: Option<i64>,
}

#[derive(Default)]
pub struct Interp {
    pub pending: BTreeMap<u64, PendingInterp>,
    /// Creations in progress: request id → (who, when, what, where it
    /// will appear: fixed when asked, so it can be marked while it's made).
    pub creating: HashMap<u64, (ActorId, f64, String, glam::Vec3)>,
    pub building: HashMap<u64, PendingBuild>,
    pub building_names: HashMap<String, u64>,
    /// What a deed's story says, held back until the build it waits on is
    /// done (build or creation id → (who, line)), so it isn't told too soon.
    /// With it, the player's deed's effect line (see `effect_line`).
    pub stories: HashMap<u64, Vec<(ActorId, String, Option<String>)>>,
    /// Deeds' "interpreted" events, held back the same way: (who, text, data).
    pub held_events: HashMap<u64, Vec<(ActorId, String, Value)>>,
    /// Gestures being written: request id → (who, toward whom, name).
    pub gestures: HashMap<u64, (ActorId, Option<Target>, String)>,
    /// Species' own versions of gestures already asked for (name@species).
    pub variants_asked: std::collections::HashSet<String>,
    /// Species whose own body was asked for (see `ask_species_bodies`).
    pub bodies_asked: std::collections::HashSet<String>,
    /// People whose look has been asked for (see `dress::ask_wardrobe`).
    pub dress_asked: std::collections::HashSet<i64>,
    /// The trip (by its deadline) for which a character last asked its mount.
    pub mount_asked: HashMap<ActorId, u64>,
    pub hits: u64,
}

/// Slow work in progress, one kind per pending map in `Interp`: what the
/// game shows as being worked on. A new kind of slow request gets a kind here.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WorkKind {
    /// A deed the world is deciding the outcome of.
    Doing,
    /// Something new being made in the world (/ or a character's make).
    Conjuring,
    /// A new kind of thing being written.
    Making,
    /// A thing's shape being rewritten.
    Reshaping,
    /// A gesture nobody knew, being worked out.
    Learning,
}

impl WorkKind {
    pub fn verb(self) -> &'static str {
        match self {
            WorkKind::Doing => "doing",
            WorkKind::Conjuring => "conjuring",
            WorkKind::Making => "making",
            WorkKind::Reshaping => "reshaping",
            WorkKind::Learning => "learning",
        }
    }
}

/// One piece of slow work: its request id, kind, whose it is and what it
/// is, where it is happening (or will appear) and what it is being done to.
/// `appears`: something new will come out of nothing at `at` (a conjuring,
/// or a deed's new kind of thing being written), not a change to something.
#[derive(Clone, Debug, PartialEq)]
pub struct Work {
    pub id: u64,
    pub kind: WorkKind,
    pub who: Option<ActorId>,
    pub what: String,
    pub at: Option<glam::Vec3>,
    pub on: Option<Target>,
    pub appears: bool,
}

/// New loose things appear next to a target whose shape doesn't change (no
/// cut, reshape, removal or replacement of it): likely "a piece came off" told
/// only in words.
fn pieces_without_change(fx: &InterpEffect, p: &PendingInterp) -> bool {
    if fx.source {
        return false;
    }
    let on_thing = matches!(p.target, Some(Target::Thing(_) | Target::Instance(_) | Target::Cell(_)));
    let is_target = |r: &str| matches!(r.trim().to_lowercase().as_str(), "target" | "object" | "other");
    let loose = fx.create.iter().any(|m| m.replace.is_none());
    let reshaped = fx.cut.iter().any(|c| is_target(&c.target)) || fx.reshape.iter().any(|r| is_target(&r.target)) || fx.remove.iter().any(|r| is_target(r)) || fx.create.iter().any(|m| m.replace.as_deref().is_some_and(is_target));
    on_thing && loose && !reshaped
}

/// The cache key: who acts (the traveler makes outright, characters by the
/// world's laws), the words, normalised, and the kinds of things involved.
fn cache_key(who: ActorId, text: &str, held: &str, target: &str) -> String {
    let t: String = text.to_lowercase().split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty() && !matches!(*w, "the" | "a" | "an" | "my" | "i" | "to")).collect::<Vec<_>>().join(" ");
    let by = if who == ActorId::Player { "traveler" } else { "character" };
    format!("{by}|{t}|{}|{}", held.to_lowercase(), target.to_lowercase())
}

/// "heat ↑" for a change worth telling.
fn shift(name: &str, d: f32) -> Option<String> {
    (d.is_finite() && d.abs() >= 0.02).then(|| format!("{} {}", name.replace('_', " "), if d > 0.0 { '↑' } else { '↓' }))
}

/// The player's deed in one short line: what it changed that the eye may
/// miss (a property, a feeling, a need, a small cut), e.g. "the lantern-moth:
/// trust ↓, affection ↓". None when there is nothing like that. Every part of
/// an answer is named here, so a new kind of effect decides whether it shows.
fn effect_line(fx: &InterpEffect, moved: Vec<(String, Vec<String>)>) -> Option<String> {
    // The weather shows by itself.
    let InterpEffect { narration: _, changes: _, cut: _, create: _, remove: _, make: _, reshape: _, ground: _, beings: _, say: _, needs: _, cache: _, weather: _, source: _, being } = fx;
    // `changes`, `cut` and the being's needs and feelings are in `moved`.
    // New, removed and reshaped things, new beings and words show by themselves.
    if let Some(b) = being {
        let BeingFx { needs: _, feel: _, look: _, grow: _, wear: _, take_off: _, learn: _, turn_into: _, reshape: _, hurt: _, how: _, on_self: _ } = b;
        // Its look, size, layers, a trick, a new shape and a new species show
        // by themselves; a hurt is in `moved` (and the traveler's on their bar).
    }
    let parts: Vec<String> = moved.into_iter().map(|(what, shifts)| format!("{what}: {}", shifts.join(", "))).collect();
    (!parts.is_empty()).then(|| parts.join(" · "))
}

impl Sim {
    /// A thing's state in a few coarse words, for cache keys: the
    /// properties well away from their defaults, bucketed ("[fire:4 wet:0]").
    pub fn state_sig(&self, p: &[f32]) -> String {
        let mut v = Vec::new();
        for (i, x) in p.iter().enumerate() {
            if i == P_MASS || i == P_SOLID || i == P_FORCE || i == P_TEMP {
                continue;
            }
            let d = self.vocab.defaults.get(i).copied().unwrap_or(0.0);
            let unit = d.abs().max(1.0);
            if (x - d).abs() > unit * 0.2 {
                v.push(format!("{}:{}", self.vocab.names[i], ((x / unit) * 4.0).round() as i32));
            }
        }
        let hot = p.get(P_TEMP).copied().unwrap_or(15.0);
        if hot > 100.0 {
            v.push(format!("temp:{}", (hot / 200.0).round() as i32));
        }
        if v.is_empty() { String::new() } else { format!(" [{}]", v.join(" ")) }
    }

    /// Ask the world what an action does (cached, else the LLM).
    pub fn interpret(&mut self, who: ActorId, text: &str, target: Option<Resolved>, hit: Option<glam::Vec3>) -> Result<Outcome, ActErr> {
        self.interpret_noted(who, text, target, hit, None)
    }

    /// The same, with a note for the interpreter only (what to settle), kept
    /// out of the deed the player sees.
    pub fn interpret_noted(&mut self, who: ActorId, text: &str, target: Option<Resolved>, hit: Option<glam::Vec3>, note: Option<&str>) -> Result<Outcome, ActErr> {
        let held = self.actor(who).and_then(|a| a.held);
        let held_name = held.map(|h| self.thing_name(h)).unwrap_or_default();
        let target_name = target.as_ref().map(|r| r.name.clone()).unwrap_or_default();
        // The same words on a thing in another state are another deed (a
        // soaked lantern doesn't light like a dry one): the key carries both
        // things' state, coarsely.
        let held_state = held.and_then(|h| self.things.get(h)).map(|t| self.state_sig(&t.props)).unwrap_or_default();
        let target_state = match target.as_ref().map(|r| r.target.clone()) {
            Some(t @ (Target::Thing(_) | Target::Cell(_))) => self.part_props(&t).map(|(p, _, _)| self.state_sig(&p)).unwrap_or_default(),
            _ => String::new(),
        };
        let key = cache_key(who, text, &format!("{held_name}{held_state}"), &format!("{target_name}{target_state}"));
        let name = self.actor_name(who);
        self.deed_started(who, text, target.as_ref().map(|r| r.target.clone()));
        if let Some(mut fx) = super::persist::cached_interp(&self.db, &key) {
            self.interp.hits += 1;
            // The story is told of whoever does it now.
            fx.narration = fx.narration.replace("{who}", &name);
            let p = PendingInterp { actor: who, text: text.to_string(), held, target: target.map(|r| r.target), hit, key, at: self.t, aim: Some(self.aim_of(who)) };
            let (msg, wait) = self.apply_interp(&p, &fx);
            if !fx.builds() {
                self.deed_landed(who);
            }
            self.deed_done(who, text);
            self.deed_event(who, format!("{name}: {text} → {msg}"), json!({ "cached": true }), wait);
            // The player hears the story from the notes (as with a fresh answer),
            // so the outcome only says what was tried, not the story again.
            let told = wait.is_some() || who == ActorId::Player;
            return Ok(if told { Outcome { ok: true, msg: format!("{name} tries to {text}…"), pending: None, thing: None } } else { Outcome::ok(msg) });
        }
        if !self.has_llm {
            let at = self.actor(who).map(|a| a.pos);
            self.event("nothing", Some(who), None, format!("{name} tried to {text}; nothing happened"), at, json!({}));
            return Ok(Outcome { ok: false, msg: "Nothing happens.".into(), pending: None, thing: None });
        }
        let id = self.next_id();
        let mut context = self.interp_context(who, held, target.as_ref(), hit);
        if let Some(n) = note {
            if let Ok(mut v) = serde_json::from_str::<Value>(&context) {
                v["note"] = json!(n);
                context = v.to_string();
            }
        }
        let req = Request::Interpret { id, actor: who, text: text.to_string(), context };
        if who == ActorId::Player {
            self.request_now(req);
        } else {
            let at = self.actor(who).map(|a| a.pos).unwrap_or_default();
            self.request(req, at);
        }
        self.interp.pending.insert(id, PendingInterp { actor: who, text: text.to_string(), held, target: target.map(|r| r.target), hit, key, at: self.t, aim: Some(self.aim_of(who)) });
        Ok(Outcome { ok: true, msg: format!("{name} tries to {text}…"), pending: Some(id), thing: None })
    }

    /// A person or creature as the interpreter sees it.
    fn describe_being(&self, a: ActorId, by: ActorId) -> Value {
        let wears: Vec<String> = self.worn_by(a).into_iter().map(|id| self.thing_name(id)).collect();
        let mut v = json!({ "person": self.actor_name(a), "wears": wears, "body": self.body_name(a) });
        if let ActorId::Npc(c) = a {
            if let Some(n) = self.cast.get(c) {
                let body = self.snap.type_of(n.body_ty).and_then(|t| t.ct.meta.body.clone());
                let look: serde_json::Map<String, Value> = body.map(|b| b.look.iter().enumerate().map(|(i, (name, lo, hi))| (name.clone(), json!({ "now": n.sliders.get(i).copied().unwrap_or(0.5), "range": [lo, hi] }))).collect()).unwrap_or_default();
                v["species"] = json!(n.species.name);
                v["mind"] = json!(n.species.mind);
                v["speech"] = json!(n.species.speech);
                v["look_sliders"] = Value::Object(look);
                v["tricks"] = json!(n.tricks);
                v["needs"] = json!({ "hunger": n.needs.hunger, "fatigue": n.needs.fatigue, "social": n.needs.social, "fun": n.needs.fun });
                v["feels_about_actor"] = json!(self.social.rel(a, by).map(|r| r.describe()).unwrap_or_else(|| "a stranger".into()));
                v["distance_m"] = json!(self.actor(by).map(|x| ((x.pos - n.a.pos).length() * 10.0).round() / 10.0));
            }
        }
        v["world_has_magic"] = json!(self.world_has_magic());
        v["beings_can_be_made"] = json!(self.cfg.create_beings);
        v["species_here"] = json!(self.snap.species.list.iter().map(|s| s.name.clone()).collect::<Vec<_>>());
        v
    }

    /// Each species with its body's look sliders and their usual ranges, for
    /// making a new being that looks as described.
    fn species_looks(&self) -> Value {
        let mut out = serde_json::Map::new();
        for sp in &self.snap.species.list {
            let body = self.snap.body_type(&sp.body).and_then(|b| b.ct.meta.body.clone());
            let look: serde_json::Map<String, Value> = body.map(|b| b.look.iter().map(|(name, lo, hi)| (name.clone(), json!({ "range": [lo, hi], "usual": sp.look.get(name) }))).collect()).unwrap_or_default();
            out.insert(sp.name.clone(), json!({ "body": sp.body, "look": look }));
        }
        Value::Object(out)
    }

    fn describe_thing(&self, id: ThingId) -> Value {
        let Some(t) = self.things.get(id) else { return Value::Null };
        let ty = self.snap.type_of(t.type_id);
        let base = ty.map(|ty| scaled(type_props(&self.vocab, ty), t.scale));
        json!({
            "name": self.thing_name(id),
            "tags": ty.map(|t| t.ct.meta.tags.clone()).unwrap_or_default(),
            "size_m": ty.map(|ty| { let b = ty.ct.meta.bounds; [b[0] * 2.0 * t.scale, b[1] * 2.0 * t.scale, b[2] * 2.0 * t.scale] }),
            "props": named(&self.vocab, &t.props),
            "changed_from_new": base.map(|b| diff(&self.vocab, &t.props, &b)),
            "state": t.state,
            "made_by": t.origin.made_by,
            "cuts": t.shape.cuts.len(),
            "history": t.shape.edits.iter().rev().take(5).map(|e| format!("{} ({})", e.what, e.by)).collect::<Vec<_>>(),
        })
    }

    fn interp_context(&mut self, who: ActorId, held: Option<ThingId>, target: Option<&Resolved>, hit: Option<glam::Vec3>) -> String {
        let me = self.actor(who).cloned();
        let mut v = json!({
            "actor": self.actor_name(who),
            "actor_is": if who == ActorId::Player { "the traveler" } else { "a character" },
            "time": crate::render::sky::time_label(self.t),
            "weather": self.wx.words(),
            "held": held.map(|h| self.describe_thing(h)),
        });
        if who == ActorId::Player {
            let wears: Vec<String> = self.worn_by(who).into_iter().map(|id| self.thing_name(id)).collect();
            v["self"] = json!({ "body": self.body_name(who), "look_sliders": self.own_look_sliders(), "wears": wears });
        }
        if let Some(r) = target {
            let tv = match &r.target {
                Target::Actor(a) => self.describe_being(*a, who),
                Target::Point(p) => json!({ "ground_or_far_point": p, "distance_m": (glam::Vec3::from(*p) - self.actor(who).map(|a| a.pos).unwrap_or_default()).length().round() }),
                other => match self.liven(other) {
                    Some(id) => {
                        let mut d = self.describe_thing(id);
                        if let (Some(t), Some(a)) = (self.things.get(id), self.actor(who)) {
                            d["distance_m"] = json!(((t.pos - a.pos).length() * 10.0).round() / 10.0);
                        }
                        if let Some(spot) = self.touch_spot(id, hit, who).and_then(|p| self.describe_spot(id, p)) {
                            d["touched_at"] = spot;
                        }
                        d
                    }
                    None => json!({ "name": r.name }),
                },
            };
            v["target"] = tv;
        }
        if let Some(m) = me {
            let mut near: Vec<String> = Vec::new();
            for id in self.things.near(m.pos, 8.0).into_iter().take(10) {
                near.push(self.thing_name(id));
            }
            v["nearby"] = json!(near);
            let beings: Vec<Value> = self.cast.npcs.iter().filter(|n| n.here() && ActorId::Npc(n.def.id) != who && (n.a.pos - m.pos).length() < 15.0).take(8).map(|n| json!({ "name": n.def.persona.name, "species": n.species.name })).collect();
            if !beings.is_empty() {
                v["beings_nearby"] = json!(beings);
            }
            if let ActorId::Npc(c) = who {
                if let Some(n) = self.cast.get(c) {
                    let pe = &n.def.persona;
                    let about: String = format!("{} {}", pe.personality, pe.goals).chars().take(300).collect();
                    v["actor_about"] = json!({ "species": n.species.name, "who_they_are": about.trim() });
                }
                v["around"] = self.around(m.pos);
            }
        }
        if self.cfg.create_beings {
            v["beings_can_be_made"] = json!(true);
            v["species_looks"] = self.species_looks();
        }
        v["properties"] = json!(self.vocab.writable_names());
        v.to_string()
    }

    /// The LLM's answer arrived.
    pub fn on_interpreted(&mut self, id: u64, result: Result<Value, String>) {
        let Some(p) = self.interp.pending.remove(&id) else { return };
        let name = self.actor_name(p.actor);
        match result.and_then(|v| serde_json::from_value::<InterpEffect>(v).map_err(|e| e.to_string())) {
            Ok(fx) => {
                let (msg, wait) = self.apply_interp(&p, &fx);
                if !fx.builds() {
                    self.deed_landed(p.actor);
                }
                if fx.changes_nothing() {
                    // A "no" depends on the moment; never make it the rule.
                    match &fx.needs {
                        Some(need) => self.on_need(p.actor, &p.text, need, &fx.narration),
                        None => self.deed_fell_through(p.actor, &p.text, &fx.narration),
                    }
                } else {
                    self.deed_done(p.actor, &p.text);
                    if fx.cache && pieces_without_change(&fx, &p) {
                        // A piece came off the target, but its shape stayed: that
                        // answer would show nothing, so don't make it the rule.
                        crate::log::info(format!("not caching '{}': it makes new things from the target but leaves the target's shape as it was", p.text));
                    } else if fx.cache {
                        // Kept without the doer's name, for whoever does it next.
                        let mut keep = fx.clone();
                        keep.narration = keep.narration.replace(&name, "{who}");
                        super::persist::cache_interp(&self.db, &p.key, &keep);
                    }
                }
                self.deed_event(p.actor, format!("{name}: {} → {msg}", p.text), json!({ "effect": fx }), wait);
            }
            Err(e) => {
                self.deed_landed(p.actor);
                crate::log::error(format!("interpretation failed: {e}"));
                self.note_near(self.actor(p.actor).map(|a| a.pos).unwrap_or_default(), 30.0, Note::seen("Nothing seems to happen.".into(), p.actor == ActorId::Player));
            }
        }
    }

    /// What is around a character, by name, for the interpreter to pick a
    /// need from: places and big things, loose things, and people.
    fn around(&self, at: glam::Vec3) -> Value {
        let mut places: Vec<(f32, String)> = Vec::new();
        for i in &self.snap.instances {
            let d = (i.pos - at).length();
            if d < 150.0 {
                if let Some(t) = self.snap.type_of(i.type_id) {
                    let tags = t.ct.meta.tags.join(", ");
                    places.push((d, if tags.is_empty() { t.name().to_string() } else { format!("{} ({tags})", t.name()) }));
                }
            }
        }
        places.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut seen = std::collections::HashSet::new();
        let places: Vec<String> = places.into_iter().filter(|(_, n)| seen.insert(n.clone())).take(24).map(|(d, n)| format!("{n}, {d:.0} m")).collect();
        let mut things: Vec<(f32, String)> = self.things.live().filter(|t| t.holder.is_none()).filter_map(|t| {
            let d = (t.pos - at).length();
            (d < 40.0).then(|| (d, self.thing_name(t.id)))
        }).collect();
        things.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut seen = std::collections::HashSet::new();
        let things: Vec<String> = things.into_iter().filter(|(_, n)| !n.is_empty() && seen.insert(n.clone())).take(12).map(|(d, n)| format!("{n}, {d:.0} m")).collect();
        let mut people: Vec<(f32, String)> = self.cast.npcs.iter().filter(|n| n.here()).map(|n| {
            let d = (n.a.pos - at).length();
            let about: String = n.def.persona.personality.chars().take(80).collect();
            (d, format!("{} ({}, {d:.0} m): {}", n.def.persona.name, n.species.name, about.trim()))
        }).filter(|(d, _)| *d > 0.5 && *d < 300.0).collect();
        people.sort_by(|a, b| a.0.total_cmp(&b.0));
        let people: Vec<String> = people.into_iter().take(10).map(|(_, s)| s).collect();
        json!({ "places": places, "things": things, "people": people })
    }

    /// Apply an interpreter answer. Returns the narration, and the build it
    /// is held back for (if any).
    pub fn apply_interp(&mut self, p: &PendingInterp, fx: &InterpEffect) -> (String, Option<u64>) {
        let pos = self.actor(p.actor).map(|a| a.pos).unwrap_or_default();
        // Changing someone takes their say-so; a refused change changes nothing.
        let being = match (&p.target, &fx.being) {
            (_, Some(bf)) if bf.on_self => Some(p.actor),
            (Some(Target::Actor(a)), _) => Some(*a),
            _ => None,
        };
        if let (Some(b), Some(bf)) = (being, &fx.being) {
            if let Err(why) = self.being_consents(p.actor, b, bf) {
                if p.actor == ActorId::Player {
                    self.notes.push(Note::Info(why.clone()));
                }
                return (why, None);
            }
        }
        if let Some(w) = &fx.weather {
            let mut kind = w.kind.clone();
            if kind.name.trim().is_empty() {
                kind.name = "strange weather".into();
            }
            let by = self.actor_name(p.actor);
            self.make_weather(kind, w.hours, &by);
        }
        // Builds this deed waits on: its story is told when the first is done.
        let mut waits: Vec<u64> = Vec::new();
        let target_thing = p.target.as_ref().and_then(|t| match t {
            Target::Actor(_) | Target::Point(_) | Target::Name(_) => None,
            other => self.liven(other),
        });
        let resolve = |s: &Sim, role: &str| -> Option<ThingId> {
            match role.trim().to_lowercase().as_str() {
                "held" | "tool" | "it" => p.held.filter(|h| s.things.get(*h).is_some()),
                "target" | "object" | "other" => target_thing,
                _ => None,
            }
        };
        // What moved, for the effect line: (what, "prop ↑").
        let mut moved: Vec<(String, Vec<String>)> = Vec::new();
        for c in &fx.changes {
            let Some(id) = resolve(self, &c.target) else { continue };
            let vocab = self.vocab.clone();
            let mut shifts = Vec::new();
            if let Some(t) = self.things.get_mut(id) {
                let (props0, state0) = (t.props.clone(), t.state);
                for (k, v) in &c.props {
                    if let Some(i) = vocab.lookup(k).filter(|i| vocab.writable(*i)) {
                        if v.is_finite() {
                            t.props[i] = *v;
                        }
                    }
                }
                for (k, v) in &c.state {
                    if let Some(i) = crate::lang::ir::state_field(k) {
                        if v.is_finite() {
                            t.state[i as usize] = *v;
                        }
                    }
                }
                sanitize(&mut t.props);
                t.dirty = true;
                t.asleep = false;
                for k in c.props.keys() {
                    if let Some(i) = vocab.lookup(k).filter(|i| vocab.writable(*i)) {
                        shifts.extend(shift(&vocab.names[i], t.props[i] - props0[i]));
                    }
                }
                for k in c.state.keys() {
                    if let Some(i) = crate::lang::ir::state_field(k) {
                        shifts.extend(shift(k, t.state[i as usize] - state0[i as usize]));
                    }
                }
            }
            if !shifts.is_empty() {
                moved.push((format!("the {}", self.thing_name(id)), shifts));
            }
        }
        // Where new things come: in place of what they replace, else where
        // what was worked on was touched, else where the doer pointed (the
        // spot they looked at while it was being decided), else just in front
        // of them. Anything not replacing is set down clear of solids, on the
        // doer's side (never inside the boulder it was pointed at).
        let pointed = match &p.target {
            Some(Target::Point(q)) => Some(glam::Vec3::from(*q)),
            _ => p.hit,
        };
        let ahead = self.actor(p.actor).map(|a| a.pos + a.forward() * 1.2).unwrap_or(pos);
        let worked_on = p.hit.filter(|_| target_thing.is_some()).or_else(|| target_thing.and_then(|t| self.things.get(t).map(|x| x.pos)));
        let mut unspawned = 0;
        for (i, m) in fx.create.iter().take(3).enumerate() {
            let replace = m.replace.as_deref().and_then(|r| resolve(self, r));
            // Several at once lie side by side.
            let side = self.actor(p.actor).map(|a| a.right()).unwrap_or(glam::Vec3::X) * (i as f32 * 0.45);
            let at = match replace.and_then(|r| self.things.get(r).map(|t| t.pos)) {
                Some(at) => at + side,
                None => {
                    let r = match self.type_by_name(&m.name) {
                        Some(ty) => ty.ct.meta.bounds[0].max(ty.ct.meta.bounds[2]),
                        None => m.size_m.map(|s| s[0].max(s[2]) * 0.5).unwrap_or(0.25),
                    };
                    self.clear_spot(worked_on.or(pointed).unwrap_or(ahead) + side, pos, r)
                }
            };
            let was_held_by = replace.and_then(|r| self.things.get(r).and_then(|t| t.holder));
            let made_from: Vec<String> = [p.held, target_thing].into_iter().flatten().map(|i| self.thing_name(i)).collect();
            let origin = Origin { made_by: Some(self.actor_name(p.actor)), made_from: made_from.clone(), ..Default::default() };
            if let Some(r) = replace {
                self.release(r);
                self.things.remove(r);
            }
            match self.type_by_name(&m.name) {
                Some(ty) => {
                    let Some(nid) = self.spawn_thing(ty.id, at, 0.0, 1.0, origin, was_held_by.is_none()) else {
                        unspawned += 1;
                        continue;
                    };
                    {
                        self.record_creation(ty.id, "made", Some(p.actor), &made_from.join(" and "), at);
                        self.apply_make_props(nid, m);
                        if let Some(h) = was_held_by {
                            self.hand_to(h, nid);
                        } else if p.actor != ActorId::Player && self.things.get(nid).is_some_and(|t| t.liftable(1)) {
                            self.made_into_hands(p.actor, nid, true);
                        }
                    }
                }
                None => {
                    let id = self.next_id();
                    let key = m.name.trim().to_lowercase();
                    self.interp.building_names.insert(key, id);
                    self.interp.building.insert(id, PendingBuild { name: m.name.clone(), by: Some(p.actor), place: Some((at, was_held_by, None)), ..Default::default() });
                    let props: Vec<(String, f32)> = m.props.iter().map(|(k, v)| (k.clone(), *v)).collect();
                    let desc = if m.description.is_empty() { format!("{} (made from {})", m.name, made_from.join(" and ")) } else { m.description.clone() };
                    self.request_now(Request::BuildType { id, name: m.name.clone(), description: desc, size: m.size_m.unwrap_or([0.5, 0.5, 0.5]), props, fits: None });
                    waits.push(id);
                }
            }
        }
        // Shape changes, at the spot that was touched.
        let mut why: Vec<String> = Vec::new();
        for c in fx.cut.iter().take(2) {
            if let Some(id) = resolve(self, &c.target) {
                let square = c.shape.trim().eq_ignore_ascii_case("square") || c.shape.trim().eq_ignore_ascii_case("box");
                let name = self.thing_name(id);
                match self.cut(p.actor, id, p.hit, c.size_m, square) {
                    Ok(_) => moved.push((format!("the {name}"), vec![format!("cut {:.0} cm", (c.size_m * 100.0).max(1.0))])),
                    Err(e) => why.push(e),
                }
            }
        }
        for r in fx.reshape.iter().take(1) {
            if let Some(id) = resolve(self, &r.target) {
                let with = r.with.as_deref().and_then(|w| resolve(self, w));
                match self.reshape(p.actor, id, &r.name, &r.change, with, p.hit) {
                    Ok(Some(rid)) => waits.push(rid),
                    Ok(None) => {}
                    Err(e) => why.push(e),
                }
            }
        }
        for g in fx.ground.iter().take(1) {
            let at = match &p.target {
                Some(Target::Point(q)) => Some(glam::Vec3::from(*q)),
                _ => p.hit.or_else(|| target_thing.and_then(|t| self.things.get(t).map(|x| x.pos))),
            };
            if let Err(e) = self.dig_ground(p.actor, at, p.aim, g) {
                why.push(e);
            }
        }
        for m in fx.make.iter().take(1) {
            if !m.text.trim().is_empty() {
                match self.create(p.actor, m.text.trim().to_string(), p.aim) {
                    Ok(o) => waits.extend(o.pending),
                    Err(e) => why.push(e.to_string()),
                }
            }
        }
        if !why.is_empty() {
            crate::log::info(format!("interpretation partly failed: {}", why.join("; ")));
        }
        for r in &fx.remove {
            if let Some(id) = resolve(self, r) {
                self.release(id);
                self.things.remove(id);
            }
        }
        if let (Some(b), Some(bf)) = (being, &fx.being) {
            if let Some(change) = bf.reshape.as_deref() {
                match self.reshape_being(p.actor, b, change) {
                    Ok(Some(rid)) => waits.push(rid),
                    Ok(None) => {}
                    Err(e) => crate::log::info(format!("reshaping a being failed: {e}")),
                }
            }
            self.apply_being(p.actor, b, bf);
            let feel = bf.feel.iter().filter(|(k, _)| b != p.actor && matches!(k.as_str(), "affection" | "love" | "liking" | "trust" | "rivalry" | "anger"));
            let mut shifts: Vec<String> = feel.chain(&bf.needs).filter_map(|(k, v)| shift(k, *v)).collect();
            if bf.hurt.is_some_and(|h| h > 0.0) {
                shifts.push("health ↓".into());
            }
            if !shifts.is_empty() {
                let name = if b == ActorId::Player { "you".to_string() } else { self.actor_name(b).trim_end_matches(|c: char| c.is_ascii_digit()).trim_end().to_string() };
                moved.push((name, shifts));
            }
        }
        for nb in fx.beings.iter().take(2) {
            if let Err(e) = self.make_being(p.actor, nb) {
                why.push(e);
            }
        }
        if let Some(line) = &fx.say {
            if !line.trim().is_empty() {
                self.say(p.actor, line.trim(), None);
            }
        }
        // Only things were to come and none could: the story didn't happen.
        let only_creates = fx.changes.is_empty() && fx.make.is_empty() && fx.cut.is_empty() && fx.reshape.is_empty() && fx.ground.is_empty() && fx.being.is_none() && fx.beings.is_empty() && fx.weather.is_none() && fx.remove.is_empty();
        if unspawned > 0 && unspawned == fx.create.len().min(3) && only_creates {
            crate::log::info(format!("'{}': nothing it made could be placed", p.text));
            let msg = "Nothing comes of it.".to_string();
            self.tell_story(pos, p.actor, &msg, None);
            return (msg, None);
        }
        let msg = if fx.narration.trim().is_empty() { format!("{} {}.", super::physics::cap(&self.actor_name(p.actor)), p.text) } else { fx.narration.trim().to_string() };
        let line = (p.actor == ActorId::Player).then(|| effect_line(fx, moved)).flatten();
        if let Some(first) = waits.first() {
            self.interp.stories.entry(*first).or_default().push((p.actor, msg.clone(), line));
            return (msg, Some(*first));
        }
        self.tell_story(pos, p.actor, &msg, line);
        (msg, None)
    }

    /// A deed's "interpreted" event: now, or once the build it waits on is
    /// done, so nothing (achievements included) counts it before it shows.
    fn deed_event(&mut self, who: ActorId, text: String, data: Value, wait: Option<u64>) {
        match wait {
            Some(id) => self.interp.held_events.entry(id).or_default().push((who, text, data)),
            None => {
                let at = self.actor(who).map(|a| a.pos);
                self.event("interpreted", Some(who), None, text, at, data);
            }
        }
    }

    /// The deeds waiting on build `id` are done, or came to nothing.
    pub(super) fn release_deeds(&mut self, id: u64, ok: bool) {
        for (who, text, mut data) in self.interp.held_events.remove(&id).unwrap_or_default() {
            if !ok {
                data["came_to_nothing"] = json!(true);
            }
            self.deed_event(who, text, data, None);
        }
    }

    /// Everything slow in progress, oldest first.
    pub fn work(&self) -> Vec<Work> {
        let i = &self.interp;
        let mut out: Vec<Work> = Vec::new();
        out.extend(i.pending.iter().map(|(id, p)| {
            let on = p.target.clone().filter(|t| !matches!(t, Target::Point(_) | Target::Name(_)));
            let at = p.hit.or_else(|| p.target.as_ref().and_then(|t| self.work_spot(t)));
            Work { id: *id, kind: WorkKind::Doing, who: Some(p.actor), what: p.text.clone(), at, on, appears: false }
        }));
        out.extend(i.creating.iter().map(|(id, c)| Work { id: *id, kind: WorkKind::Conjuring, who: Some(c.0), what: c.2.clone(), at: Some(c.3), on: None, appears: true }));
        out.extend(i.building.iter().map(|(id, b)| {
            let kind = if b.reshape.is_empty() && b.body_of.is_none() { WorkKind::Making } else { WorkKind::Reshaping };
            let what = match (b.reshape.first(), b.body_of) {
                (Some((thing, _, _)), _) if !b.change.trim().is_empty() => format!("the {}: {}", self.thing_name(*thing), b.change.trim()),
                (_, Some(c)) => format!("{}: {}", self.actor_name(ActorId::Npc(c)), b.change.trim()),
                _ => b.name.clone(),
            };
            let on = match (b.reshape.first(), b.body_of, b.then.first()) {
                (Some((thing, _, _)), _, _) => Some(Target::Thing(*thing)),
                (_, Some(c), _) => Some(Target::Actor(ActorId::Npc(c))),
                (_, _, Some((thing, true))) => Some(Target::Thing(*thing)),
                _ => None,
            };
            let at = b.place.map(|p| p.0).or_else(|| on.as_ref().and_then(|t| self.work_spot(t))).or_else(|| b.then.first().and_then(|(t, _)| self.work_spot(&Target::Thing(*t))));
            let appears = kind == WorkKind::Making && b.place.is_some_and(|p| p.1.is_none());
            Work { id: *id, kind, who: b.by, what, at, on, appears }
        }));
        out.extend(i.gestures.iter().map(|(id, g)| {
            let name = g.2.split('@').next().unwrap_or("").replace('_', " ");
            Work { id: *id, kind: WorkKind::Learning, who: Some(g.0), what: format!("how to {name}"), at: None, on: None, appears: false }
        }));
        out.sort_by_key(|w| w.id);
        out
    }

    /// Where work on a target shows: a thing's or placed object's middle, a
    /// being's chest, a cell's item or a point.
    fn work_spot(&self, t: &Target) -> Option<glam::Vec3> {
        match t {
            Target::Actor(a) => self.actor(*a).map(|a| a.pos + glam::Vec3::Y * a.dims.height * 0.6),
            Target::Cell(c) => {
                let (x, z) = crate::world::scatter::cell_centre((c[0], c[1]));
                Some(glam::Vec3::new(x, self.snap.terrain.height(x, z) + 0.5, z))
            }
            Target::Point(p) => Some(glam::Vec3::from(*p)),
            _ => self.target_shape(t).map(|(pos, ty, scale)| pos + glam::Vec3::Y * ty.sphere_cy * scale),
        }
    }

    /// A deed's story, told to those near and remembered by who saw it.
    fn tell_story(&mut self, pos: glam::Vec3, who: ActorId, msg: &str, effect: Option<String>) {
        // Your own deeds are answers to you; anyone else's change the world.
        let n = if who == ActorId::Player { Note::Info(msg.to_string()) } else { Note::Notable(msg.to_string()) };
        self.note_near(pos, 30.0, n);
        if let Some(e) = effect {
            self.note_near(pos, 30.0, Note::Effect(e));
        }
        self.witness(pos, 15.0, &format!("I saw {}: {msg}", self.actor_name(who)), 0.35, &[who]);
    }

    /// Tell the stories held back for a build that is done (at `at`), if
    /// any. Returns whether there were any.
    pub(super) fn tell_held_stories(&mut self, id: u64, at: glam::Vec3) -> bool {
        self.release_deeds(id, true);
        let Some(stories) = self.interp.stories.remove(&id) else { return false };
        let any = !stories.is_empty();
        for (who, msg, effect) in stories {
            self.tell_story(at, who, &msg, effect);
        }
        any
    }

    /// A way or a tunnel made by a deed (see `GroundFx::way`): from the doer
    /// along the way they look (or to where they point), as long as asked
    /// (a person does a stretch at a time; the traveler as much as they
    /// ask). A cut joining water fills with it. Saved with the live world.
    fn lay_way(&mut self, who: ActorId, at: Option<glam::Vec3>, aim: Option<Aim>, g: &GroundFx) -> Result<(), String> {
        use crate::ground::{Edit, Height, Layer, Mode, Shape};
        let (me, fwd) = self.actor(who).map(|a| (a.pos, a.forward())).ok_or("no one to dig")?;
        let ok = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { lo };
        let traveler = who == ActorId::Player;
        let word = g.way.trim().to_lowercase();
        let t = self.snap.terrain.clone();
        let fwd = aim.map(|a| glam::Vec3::new(a.yaw.sin(), 0.0, a.yaw.cos())).unwrap_or(fwd);
        let start = glam::Vec2::new(me.x, me.z) + glam::Vec2::new(fwd.x, fwd.z) * 1.2;
        let wanted = g.long_m.unwrap_or(12.0);
        let long = ok(wanted, 2.0, if traveler { 400.0 } else { 8.0 });
        let end = match at {
            Some(p) if glam::Vec2::new(p.x - me.x, p.z - me.z).length() > 2.0 => {
                let d = glam::Vec2::new(p.x, p.z) - start;
                start + d.normalize_or(glam::Vec2::Y) * d.length().min(long)
            }
            _ => start + glam::Vec2::new(fwd.x, fwd.z).normalize_or(glam::Vec2::Y) * long,
        };
        let wide = ok(g.wide_m, 0.8, if traveler { 60.0 } else { 4.0 });
        let depth = ok(g.depth_m, 0.3, if traveler { 30.0 } else { 2.5 });
        if ["tunnel", "cave", "mine", "burrow", "adit", "den"].iter().any(|w| word.contains(w)) {
            let h = t.height(start.x, start.y);
            let cave = crate::ground::Cave::dug(&word, "", glam::Vec3::new(start.x, h, start.y), end - start, (end - start).length(), depth.max(2.0), wide.max(2.5));
            let mut made = t.carve.read().ground.cave_layer(Layer::Made).to_vec();
            made.push(cave);
            if made.len() > 60 {
                made.remove(0);
            }
            t.set_caves(Layer::Made, made);
            self.dug_dirty = true;
            return Ok(());
        }
        let cut = ["canal", "ditch", "moat", "channel", "trench", "drain", "dyke ditch", "race", "sluice", "leat"].iter().any(|w| word.contains(w));
        let raised = ["causeway", "dyke", "dike", "embankment", "levee", "dam", "rampart", "mound"].iter().any(|w| word.contains(w));
        let raw = |x: f32, z: f32| t.natural_height(x, z);
        let (pts, hs) = crate::ground::lay(&[start.to_array(), end.to_array()], 3.0, 9.0, &raw);
        let low = hs.iter().copied().fold(f32::MAX, f32::min);
        // Water at either end runs in: a canal from the river is the river's.
        let wet = |p: glam::Vec2| (t.water(p.x, p.y) - t.height(p.x, p.y) > 0.3).then(|| t.water(p.x, p.y));
        let water = if cut { wet(start).into_iter().chain(wet(end)).reduce(f32::max).filter(|w| *w > low - depth - 0.1 && *w > t.sea + 0.05) } else { None };
        let (mode, to) = if cut {
            // The bed level, as deep as asked under its lowest point (and
            // under the water it joins, so it holds some).
            let bed = (low - depth).min(water.map(|w| w - 0.8).unwrap_or(f32::MAX));
            (Mode::Lower, Height::At(bed))
        } else if raised {
            (Mode::Raise, Height::Along(hs.iter().map(|h| h.max(t.water(start.x, start.y)) + depth).collect()))
        } else {
            (Mode::Level, Height::Along(hs.clone()))
        };
        let color = g.color.map(|c| c.map(|v| (v / 255.0).clamp(0.0, 1.0))).or((!cut && !raised).then(|| {
            let s = crate::terrain::rgbv(self.snap.look.palette.sand);
            let r = crate::terrain::rgbv(self.snap.look.palette.rock);
            (s * 0.55 + r * 0.45).to_array()
        }));
        // Where it runs lowest the ground water seeps in first.
        let bottom = if cut && water.is_none() { pts.iter().zip(&hs).min_by(|a, b| a.1.total_cmp(b.1)).map(|(p, _)| glam::Vec2::from_array(*p)) } else { None };
        let edit = Edit { pattern: None,
            kind: word,
            name: String::new(),
            shape: Shape::Line { pts, half: wide * 0.5 },
            mode,
            to,
            edge: if cut { (depth * 0.6).clamp(0.3, 3.0) } else { 1.5 },
            color,
            rim: None,
            water: water.map(Height::At),
        };
        let mut made = t.carve.read().ground.layer(Layer::Made).to_vec();
        made.push(edit);
        if made.len() > 200 {
            made.remove(0);
        }
        t.set_ground(Layer::Made, made);
        self.dug_dirty = true;
        if let Some(p) = bottom {
            self.seep(p, long.min(120.0) + wide);
        }
        Ok(())
    }

    /// Take the ground away for a deed (see `GroundFx`), at `at` or, with
    /// nowhere pointed at, just ahead of the doer. The traveler digs as big
    /// as they ask; anyone else only what a person could dig.
    fn dig_ground(&mut self, who: ActorId, at: Option<glam::Vec3>, aim: Option<Aim>, g: &GroundFx) -> Result<(), String> {
        if !g.way.trim().is_empty() {
            return self.lay_way(who, at, aim, g);
        }
        let me = self.actor(who).map(|a| (a.pos, a.forward())).ok_or("no one to dig")?;
        let ok = |v: f32, lo: f32, hi: f32| if v.is_finite() { v.clamp(lo, hi) } else { lo };
        let (most_deep, most_wide) = if who == ActorId::Player { (40.0, 80.0) } else { (2.5, 4.0) };
        let depth = ok(g.depth_m, 0.3, most_deep);
        let wide = ok(g.wide_m, 0.5, most_wide);
        let long = g.long_m.map(|l| ok(l, wide, most_wide * 1.5));
        let sloped = g.sides.trim().eq_ignore_ascii_case("sloped");
        let edge = if sloped { (depth * 1.4).max(crate::terrain::HOLLOW_EDGE) } else { (depth * 0.08).max(crate::terrain::HOLLOW_EDGE) };
        let fwd = aim.map(|a| glam::Vec3::new(a.yaw.sin(), 0.0, a.yaw.cos())).unwrap_or(me.1);
        let at = at.unwrap_or(me.0 + fwd * (wide * 0.5 + 2.0));
        // A trench runs the way the doer looks at it.
        let along = glam::Vec2::new(at.x - me.0.x, at.z - me.0.z).try_normalize().unwrap_or(glam::Vec2::new(fwd.x, fwd.z).normalize_or(glam::Vec2::Y));
        let half = |w: f32| (w * 0.5 - edge).max(w * 0.25);
        let terrain = self.snap.terrain.clone();
        let floor = terrain.height(at.x, at.z) - depth;
        let hollow = crate::terrain::Hollow {
            c: [at.x, at.z],
            rot: [along.y, along.x],
            half: [half(wide), long.map(half).unwrap_or(half(wide))],
            floor,
            round: long.is_none(),
            edge,
        };
        self.open_hollow(at, hollow);
        let reach = long.unwrap_or(wide).max(wide) + edge * 2.0 + 6.0;
        if g.fill {
            self.pour(glam::Vec2::new(at.x, at.z), f32::MAX, reach);
        } else {
            self.seep(glam::Vec2::new(at.x, at.z), reach);
        }
        Ok(())
    }

    /// Water fills the hollow at `at` (see `ground::basin`), up to `up_to`
    /// at most, looked for within `reach`: a pond dug to hold water, or the
    /// ground water seeping into a hole dug below it. Saved with the live
    /// world.
    pub(super) fn pour(&mut self, at: glam::Vec2, up_to: f32, reach: f32) {
        use crate::ground::Layer;
        let t = self.snap.terrain.clone();
        let Some(edit) = crate::ground::basin(&|x, z| t.height(x, z), at.to_array(), up_to, reach) else { return };
        let mut made = t.carve.read().ground.layer(Layer::Made).to_vec();
        made.push(edit);
        if made.len() > 200 {
            made.remove(0);
        }
        t.set_ground(Layer::Made, made);
        self.dug_dirty = true;
    }

    /// The ground water, where the land has any, seeps into ground dug
    /// below it near `at`.
    pub(super) fn seep(&mut self, at: glam::Vec2, reach: f32) {
        if let Some(level) = self.snap.terrain.groundwater_at(at.x, at.y) {
            self.pour(at, level, reach);
        }
    }

    /// Take the ground away in `hollow` (a pit, a crater): what is placed
    /// there settles with it, what lies there drops in. Dug again where a
    /// hollow was dug before, that one grows to take it in.
    pub(super) fn open_hollow(&mut self, at: glam::Vec3, hollow: crate::terrain::Hollow) {
        let edge = hollow.edge;
        let floor = hollow.floor;
        let terrain = self.snap.terrain.clone();
        // What stands there, and the ground under it before: placed things
        // (buildings, made things) never look at the ground again by
        // themselves, so they are brought live to settle with it.
        let span = hollow.half[0].max(hollow.half[1]) * 1.5 + edge + 1.0;
        let placed: Vec<i64> = self.snap.instances.iter().filter(|q| glam::Vec2::new(q.pos.x - at.x, q.pos.z - at.z).length() < span).map(|q| q.id).collect();
        for i in placed {
            self.promote_instance(i);
        }
        let standing: Vec<(ThingId, f32)> = self.things.near(at, span).into_iter().filter_map(|id| self.things.get(id).filter(|t| !t.held() && t.worn.is_none()).map(|t| (id, terrain.height(t.pos.x, t.pos.z)))).collect();
        {
            let mut c = terrain.carve.write();
            // Dug again where a hollow was dug before: that one grows to take
            // it in (deeper, wider) rather than another piling up on it.
            match c.dug.iter_mut().rev().find(|h| h.half[0].max(h.half[1]) > 1.1 && h.outside(at.x, at.z) < 0.0) {
                Some(h) => {
                    h.floor = h.floor.min(floor);
                    h.edge = h.edge.max(edge);
                    let r = hollow.half[0].max(hollow.half[1]);
                    let (dx, dz) = (at.x - h.c[0], at.z - h.c[1]);
                    if h.round {
                        h.half[0] = h.half[0].max((dx * dx + dz * dz).sqrt() + r).min(60.0);
                    } else {
                        let (lx, lz) = (h.rot[0] * dx - h.rot[1] * dz, h.rot[1] * dx + h.rot[0] * dz);
                        h.half = [h.half[0].max(lx.abs() + r).min(60.0), h.half[1].max(lz.abs() + r).min(90.0)];
                    }
                }
                None => {
                    c.dug.push(hollow);
                    crate::terrain::Carve::trim_dug(&mut c.dug);
                }
            }
        }
        terrain.carve.write().reindex();
        self.dug_dirty = true;
        // What lay there drops in; what is fixed in place sinks with the ground.
        for (id, was) in standing {
            let Some(t) = self.things.get_mut(id) else { continue };
            let drop = was - terrain.height(t.pos.x, t.pos.z);
            if drop <= 0.01 {
                continue;
            }
            if t.anchored {
                t.pos.y -= drop;
                t.dirty = true;
            } else {
                t.asleep = false;
                t.rest_t = 0.0;
            }
        }
    }

    fn apply_make_props(&mut self, id: ThingId, m: &Make) {
        let vocab = self.vocab.clone();
        if let Some(t) = self.things.get_mut(id) {
            for (k, v) in &m.props {
                if let (Some(i), true) = (vocab.lookup(k).filter(|i| vocab.writable(*i)), v.is_finite()) {
                    t.props[i] = *v;
                }
            }
            sanitize(&mut t.props);
        }
    }

    /// Put a thing in someone's hands (if they are free).
    pub fn hand_to(&mut self, who: ActorId, id: ThingId) {
        if self.actor(who).is_some_and(|a| a.held.is_some()) {
            return;
        }
        let strength = self.strength(who);
        if let Some(t) = self.things.get_mut(id) {
            if t.anchored || t.mass() > strength {
                return;
            }
            t.holder = Some(who);
            t.asleep = false;
        }
        if let Some(a) = self.actor_mut(who) {
            a.held = Some(id);
        }
    }

    /// A character's own small make goes into their hands (when it appeared
    /// `near` them), and on to whoever they made it for: fetched first if it
    /// appeared further off.
    fn made_into_hands(&mut self, who: ActorId, id: ThingId, near: bool) {
        if near {
            self.hand_to(who, id);
        }
        let ActorId::Npc(c) = who else { return };
        self.need_made(who, Target::Thing(id), true);
        let t = self.t;
        let Some(n) = self.cast.get_mut(c) else { return };
        let Some((to, until)) = n.deliver.take() else { return };
        if t > until {
            return;
        }
        let give = super::Action::Give { to: Target::Actor(to) };
        let steps = match n.a.held {
            Some(h) if h == id => vec![give],
            Some(_) => vec![super::Action::Drop, super::Action::Hold { target: Target::Thing(id) }, give],
            None => vec![super::Action::Hold { target: Target::Thing(id) }, give],
        };
        self.plan(who, steps, "hand it over", true);
    }

    /// A type the world asked for was written (or failed).
    pub fn on_type_built(&mut self, id: u64, type_id: Option<u32>) {
        let Some(b) = self.interp.building.remove(&id) else { return };
        if let Some(c) = b.body_of {
            self.on_body_reshaped(id, c, b, type_id);
            return;
        }
        for who in b.place.iter().filter_map(|p| p.1).chain(b.reshape.iter().map(|r| r.1)) {
            self.deed_landed(who);
        }
        self.interp.building_names.remove(&b.name.trim().to_lowercase());
        let Some(tid) = type_id else {
            crate::log::info(format!("no type for '{}'", b.name));
            self.release_deeds(id, false);
            if self.interp.stories.remove(&id).is_some() && b.reshape.is_empty() {
                if let Some((at, _, _)) = b.place {
                    self.note_near(at, 30.0, Note::Info("Nothing comes of it.".into()));
                }
            }
            for (thing, _, _) in &b.reshape {
                if let Some(at) = self.things.get(*thing).map(|t| t.pos) {
                    let name = self.thing_name(*thing);
                    self.note_near(at, 30.0, Note::Info(format!("The {name} stays as it was.")));
                }
            }
            return;
        };
        for (thing, transform) in b.then {
            let from = self.thing_name(thing);
            let at = self.things.get(thing).map(|t| t.pos).unwrap_or_default();
            self.record_creation(tid, "changed", b.by, &from, at);
            self.spawn_or_transform(thing, tid, transform, &from);
        }
        let mut told = false;
        for (thing, who, with) in b.reshape {
            let at = self.things.get(thing).map(|t| t.pos).unwrap_or_default();
            let from = self.thing_name(thing);
            self.record_creation(tid, "reshaped", Some(who), &from, at);
            told = told || self.tell_held_stories(id, at);
            self.set_shape_type(thing, tid, who, &b.change, true, !told);
            if let Some(w) = with {
                self.use_up(w);
            }
        }
        if let Some((wearer, by)) = b.wear_on {
            let at = self.actor(wearer).map(|a| a.pos).unwrap_or_default();
            self.record_creation(tid, "made", Some(by), "", at);
            let origin = Origin { made_by: Some(self.actor_name(by)), ..Default::default() };
            if let Some(nid) = self.spawn_thing(tid, at, 0.0, 1.0, origin, false) {
                if let Err(e) = self.wear(by, wearer, nid) {
                    crate::log::info(format!("couldn't put it on: {e:?}"));
                    self.things.remove(nid);
                }
            }
        }
        if let Some((at, holder, _)) = b.place {
            // Credit whose deed it was, not only who ends up holding it.
            let maker = b.by.or(holder);
            let origin = Origin { made_by: maker.map(|h| self.actor_name(h)), ..Default::default() };
            if let Some(nid) = self.spawn_thing(tid, at, 0.0, 1.0, origin, holder.is_none()) {
                if holder.is_none() {
                    self.flash(at);
                }
                self.record_creation(tid, if maker.is_some() { "made" } else { "changed" }, maker, "", at);
                let name = self.thing_name(nid);
                if let Some(h) = holder {
                    self.hand_to(h, nid);
                } else if let Some(by) = b.by.filter(|w| *w != ActorId::Player) {
                    // Made by someone close by: into their hands, as with a
                    // kind of thing the world already knew.
                    let near = self.actor(by).is_some_and(|a| (a.pos - at).length() < 4.0);
                    if near && self.things.get(nid).is_some_and(|t| t.liftable(1)) {
                        self.made_into_hands(by, nid, true);
                    }
                }
                self.event("made", maker, Some(format!("thing:{nid}")), format!("{} {name} came into being", super::physics::cap(super::article(&name))), Some(at), json!({}));
                if !self.tell_held_stories(id, at) {
                    self.note_near(at, 30.0, Note::Made(format!("{} {name} appears.", super::physics::cap(super::article(&name)))));
                }
            }
        }
    }

    /// Where something `r` across (half-width) can be set down near `want`
    /// without being inside a solid: `want` itself if clear, else stepping
    /// toward `from` (the doer) until it is, else just short of the doer.
    pub(super) fn clear_spot(&mut self, want: glam::Vec3, from: glam::Vec3, r: f32) -> glam::Vec3 {
        let r = r.clamp(0.05, 3.0);
        let mut to = from - want;
        to.y = 0.0;
        let len = to.length();
        let dir = if len > 1e-3 { to / len } else { glam::Vec3::ZERO };
        let solids = self.solids_near(want, len + r + 1.0);
        let clear = |s: &Sim, q: glam::Vec3| {
            let g = s.snap.terrain.height(q.x, q.z);
            // Not set down in water, where it would sink out of reach.
            if s.snap.terrain.water(q.x, q.z) > g + 0.05 {
                return false;
            }
            // Probe low and at its middle: it stands on the ground.
            [0.15, r.min(1.0)].iter().all(|h| solids.iter().all(|so| so.sdf(glam::Vec3::new(q.x, g + h, q.z)) >= r * 0.8))
        };
        let mut d = 0.0;
        while d <= (len - 0.4).max(0.0) {
            let q = want + dir * d;
            if clear(self, q) {
                return q;
            }
            d += 0.2;
        }
        if dir == glam::Vec3::ZERO { want } else { from - dir * (0.4 + r).min(len) }
    }

    /// Something new appeared here: a flash of light.
    pub(super) fn flash(&mut self, at: glam::Vec3) {
        let now = self.t;
        self.flashes.retain(|f| now - f.1 < FLASH_FOR);
        self.flashes.push((at, now));
    }

    /// Count a kind of thing made in the world's record of creations.
    pub fn record_creation(&mut self, tid: u32, kind: &str, by: Option<ActorId>, from: &str, at: glam::Vec3) {
        let name = self.snap.type_of(tid).map(|t| t.name().to_string()).unwrap_or_default();
        self.note_creation(&format!("type:{tid}"), kind, &name, by, from, at);
    }

    /// Count something made, by key (`type:<id>`, `being:<id>`).
    pub fn note_creation(&mut self, key: &str, kind: &str, name: &str, by: Option<ActorId>, from: &str, at: glam::Vec3) {
        if matches!(by, Some(ActorId::Npc(_))) {
            self.cast.last_made = self.t;
        }
        let maker = by.map(|b| self.actor_name(b)).unwrap_or_else(|| "the world".into());
        let t = self.t;
        let _ = self.db.with(|c| crate::db::note_creation(c, key, kind, name, &maker, from, t, at.x, at.z));
    }

    /// A creation request finished: `instances` are the new placed objects.
    pub fn on_created(&mut self, id: u64, instances: &[i64]) {
        let Some((who, _, _, _)) = self.interp.creating.remove(&id) else { return };
        self.deed_landed(who);
        let maker = self.actor_name(who);
        for i in instances {
            super::persist::set_made_by(&self.db, *i, &maker);
            self.made.insert(*i, who);
        }
        let Some(first) = instances.first().copied() else {
            self.release_deeds(id, false);
            if self.interp.stories.remove(&id).is_some() {
                let at = self.actor(who).map(|a| a.pos).unwrap_or_default();
                self.note_near(at, 30.0, Note::seen("Nothing comes of it.".into(), who == ActorId::Player));
            }
            self.event("create_failed", Some(who), None, format!("{maker} couldn't make it"), None, json!({}));
            return;
        };
        let placed = self.snap.instances.iter().find(|p| p.id == first).and_then(|p| self.snap.type_of(p.type_id).map(|t| (t.clone(), p.pos, p.scale)));
        let (name, pos) = placed.as_ref().map(|(t, p, _)| (t.name().to_string(), *p)).unwrap_or(("thing".into(), self.player.pos));
        // The traveler's things appear out of nowhere; a character's are made by hand.
        let how = if who == ActorId::Player { super::surprise::Arrival::FromNowhere } else { super::surprise::Arrival::Seen };
        let sight = match &placed {
            Some((ty, _, scale)) => self.sight_of(ty, *scale, how),
            None => super::surprise::Sight { how, extent: 1.0, strange: 0.0 },
        };
        let surprise = (self.plain_surprise(sight) * 100.0).round() / 100.0;
        self.flash(pos);
        if let Some(tid) = self.snap.instances.iter().find(|p| p.id == first).map(|p| p.type_id) {
            self.record_creation(tid, "built", Some(who), "", pos);
        }
        self.event("made", Some(who), Some(format!("instance:{first}")), format!("{maker} made {} {name}", super::article(&name)), Some(pos), json!({ "instances": instances, "surprise": surprise }));
        // Who saw it made knows who made it (the traveler's makings come out of nowhere).
        if who != ActorId::Player {
            let claim = super::beliefs::Claim::Made { name: name.clone(), by: maker.clone() };
            self.notice_claim(pos, 40.0, &[who], &claim, super::beliefs::Source::Saw, 0.95, 0.5);
        }
        let told = self.tell_held_stories(id, pos);
        if who != ActorId::Player {
            if !told {
                self.note_near(pos, 40.0, Note::Made(format!("{} made {} {name}.", super::physics::cap(&maker), super::article(&name))));
            }
            let memory = format!("{maker} made {} {name}.", super::article(&name));
            let tell = format!("{} just made {} {name} near you.", super::physics::cap(&maker), super::article(&name));
            let what = format!("the {name}");
            let news = super::surprise::News { at: pos, sight, memory: &memory, importance: 0.5, event: "something_made", tell: &tell, what: &what };
            self.startle(40.0, &news, &[who]);
            // Small things go straight into the maker's hands.
            match self.promote_instance(first) {
                Some(tid) if self.things.get(tid).is_some_and(|t| t.liftable(1)) => {
                    let near = self.actor(who).is_some_and(|a| (a.pos - pos).length() < 8.0);
                    self.made_into_hands(who, tid, near);
                }
                Some(tid) => self.need_made(who, Target::Thing(tid), false),
                None => self.need_made(who, Target::Instance(first), false),
            }
        }
    }

    /// A gesture's keyframes arrived: learn it, keep it, and do it.
    pub fn on_gesture_built(&mut self, id: u64, result: Result<Value, String>) {
        let Some((who, to, name)) = self.interp.gestures.remove(&id) else { return };
        let g = result.and_then(|v| {
            let mut g: super::actor::CustomGesture = serde_json::from_value(v).map_err(|e| e.to_string())?;
            g.name = name.clone();
            Ok(g)
        });
        match g.and_then(|g| super::actor::register_gesture(g.clone()).map(|k| (k, g))) {
            Ok((k, g)) => {
                super::persist::save_gesture(&self.db, &g);
                // A species' own version is used from now on; the shared one already played.
                if !g.name.contains('@') {
                    let _ = self.act(who, super::Action::Gesture { kind: k.name(), to });
                }
            }
            Err(e) => {
                crate::log::info(format!("gesture '{name}' not learned: {e}"));
                self.note_near(self.actor(who).map(|a| a.pos).unwrap_or_default(), 20.0, Note::seen(format!("(nobody quite knows how to {name})"), who == ActorId::Player));
            }
        }
    }

    /// Requests whose answers never came (stale) are dropped.
    pub fn expire_pending(&mut self) {
        let now = self.t;
        self.interp.pending.retain(|_, p| now - p.at < 300.0);
        self.interp.creating.retain(|_, c| now - c.1 < 600.0);
        let Interp { stories, held_events, creating, building, .. } = &mut self.interp;
        stories.retain(|id, _| creating.contains_key(id) || building.contains_key(id));
        held_events.retain(|id, _| creating.contains_key(id) || building.contains_key(id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Picking a lemon off a tree that gives many: the tree stays as it is,
    /// and the answer is kept as the rule; a piece told only in words is not.
    #[test]
    fn a_source_gives_without_changing() {
        let p = PendingInterp { actor: ActorId::Player, text: "pick a lemon".into(), held: None, target: Some(Target::Thing(7)), hit: None, key: String::new(), at: 0.0, aim: None };
        let lemon: InterpEffect = serde_json::from_str(r#"{ "create": [ { "name": "lemon" } ], "cache": true }"#).unwrap();
        assert!(pieces_without_change(&lemon, &p));
        let from_tree: InterpEffect = serde_json::from_str(r#"{ "create": [ { "name": "lemon" } ], "source": true, "cache": true }"#).unwrap();
        assert!(!pieces_without_change(&from_tree, &p));
    }
}

//! What host and guests say to each other. Plain data only (postcard can't
//! carry self-describing values such as JSON), so the game's own types are
//! turned into these at the edge.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A traveler's number in a world: 0 is the world's owner (the host), the
/// rest are guests, each kept by their token across visits.
pub type TravelerNo = u8;

/// The first thing a guest says.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Hello {
    pub protocol: u32,
    pub release: String,
    pub commit: String,
    pub name: String,
    pub token: String,
    pub code: String,
}

/// Guest → host.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Up {
    Hello(Hello),
    /// Where the guest's body is and how it stands: they move it
    /// themselves (the host checks it), about 20 times a second.
    Me(Body),
    /// They changed how they look.
    Look(BTreeMap<String, f32>),
    /// They do something (an action, as JSON, naming actors as the guest
    /// knows them).
    Act(String),
    /// They say something to someone (a being's id).
    Talk { cid: i64, text: String },
    /// Who they are talking with now (the being stands and faces them).
    Talking(Option<i64>),
    /// Something said to everyone ("/say").
    Say(String),
    /// Seated in what they drive: throttle and brake, steering, up and down.
    Steer { throttle: f32, steer: f32, climb: f32 },
    /// On what they ride: the way to go (flat, length up to 1), and where
    /// they look up or down.
    Ride { dir: [f32; 3], pitch: f32 },
    /// Leaving.
    Bye,
}

/// Host → guest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Down {
    /// Not let in, and why (a wrong code, another build).
    Refused(String),
    /// Let in: who they are, and the world follows in chunks.
    Welcome(Welcome),
    /// A piece of the world's file (deflated), in order.
    WorldChunk(Vec<u8>),
    /// The last piece came.
    WorldDone,
    /// Who else is here now (sent on joining and whenever it changes).
    Roster(Vec<Traveler>),
    /// The others' bodies, about 20 times a second (not the guest's own),
    /// and the host's game time.
    Bodies { t: f64, bodies: Vec<(TravelerNo, Body)> },
    /// The host moved the guest (pulled out of the ground, carried, put
    /// back): take this place.
    Place(Body),
    /// Seated in or on something the host moves: where the guest is now.
    Seated(Body),
    /// A line for the log.
    Say(String),
    /// The live world near the guest: what changed since last sent.
    World(Box<WorldNear>),
    /// Rows of the world's own tables (types, buildings, regions, people,
    /// species…) changed by the host: the guest keeps them and, when
    /// `rebuild`, makes its world anew from them.
    Rows { tables: Vec<Rows>, rebuild: bool },
    /// The guest's own lot: how hurt, fallen or not, glowing, and what they
    /// sit in or on (a vehicle's id and seat, a mount's host code).
    Own { wounds: f32, fallen: bool, glow: f32, aboard: Option<i64>, seat_no: u8, riding: Option<i64> },
    /// Something for the log, as it is said to this guest.
    Note(Note),
    /// How what they did went (`verb` as the action's; the message names
    /// them by name).
    Acted { verb: String, ok: bool, msg: String, thing: bool },
    /// A being's reply as it comes (to whoever spoke; everyone hears it).
    Token { cid: i64, text: String },
    /// The reply is done (`ok` false: none came).
    ReplyDone { cid: i64, ok: bool },
    /// Sounds near the guest, to be heard where they are.
    Heard(Vec<Sound>),
    /// The host is leaving or closed the game.
    Bye(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Welcome {
    pub you: TravelerNo,
    /// The world's name, as the host's worlds list shows it.
    pub world: String,
    /// The guest's name, as the host's world knows them.
    pub name: String,
    /// The host's name.
    pub host: String,
    /// The world's file, before deflating, and deflated (for progress).
    pub bytes: u64,
    pub packed: u64,
    /// Where the guest stands and faces (where they left, or the start).
    pub body: Body,
    pub look: BTreeMap<String, f32>,
    /// How hurt they are (0 whole … 1 fallen).
    pub wounds: f32,
    /// The host's game time.
    pub t: f64,
    /// The host's world's seed for this session (people look the same by it).
    pub seed: u64,
    /// The host's build (release, commit): it may differ from the guest's.
    pub build: (String, String),
}

/// Someone in the world.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Traveler {
    pub no: TravelerNo,
    pub name: String,
    pub look: BTreeMap<String, f32>,
}

/// A body as it is this moment: enough to draw it and to move it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Body {
    pub pos: [f32; 3],
    pub yaw: f32,
    /// Where the eyes look, up or down.
    pub pitch: f32,
    pub pose: [f32; 9],
    /// Walk phase.
    pub phase: f32,
    pub moved: f32,
    pub crouch: f32,
    pub alt: f32,
    pub flags: u8,
}

/// A thing as the guest draws it: everything once (and again when it
/// changes), its motion often.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ThingFull {
    pub id: i64,
    pub type_id: u32,
    pub scale: f32,
    pub params: [f32; 8],
    pub look: Vec<f32>,
    pub props: Vec<f32>,
    /// Who holds, holds with, or wears it (the host's actor codes).
    pub holder: Option<i64>,
    pub co_holder: Option<i64>,
    pub worn: Option<i64>,
    pub anchored: bool,
    pub point_first: bool,
    pub born: f64,
    /// Where it came from and its shape (cuts, joint), as the world keeps them.
    pub origin: String,
    pub shape: String,
    pub m: ThingMove,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ThingMove {
    pub id: i64,
    pub pos: [f32; 3],
    pub vel: [f32; 3],
    pub yaw: f32,
    pub state: [f32; 8],
    /// Lying tilted (x, y, z, w), or [0; 4] upright.
    pub tilt: [f32; 4],
    pub asleep: bool,
}

/// A being, once (and when it changes): who they are.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct NpcFull {
    pub id: i64,
    pub persona: String,
    pub home: [f32; 3],
    pub version: i64,
    pub props: Vec<f32>,
    pub m: NpcMove,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct NpcMove {
    pub id: i64,
    pub body: Body,
    pub growth: f32,
    /// Away (out of its hours), dead, being talked with.
    pub away: bool,
    pub dead: bool,
    pub held: Option<i64>,
}

/// A scatter cell that is alive with something (burning, wet, charred).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CellState {
    pub at: (i32, i32),
    pub type_id: u32,
    pub pos: [f32; 3],
    pub size: f32,
    pub small: bool,
    pub water: bool,
    pub active: bool,
    pub props: Vec<f32>,
}

/// What changed near a guest since the last time.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct WorldNear {
    pub t: f64,
    pub things: Vec<ThingFull>,
    pub moved: Vec<ThingMove>,
    pub things_gone: Vec<i64>,
    pub npcs: Vec<NpcFull>,
    pub npcs_moved: Vec<NpcMove>,
    pub npcs_props: Vec<(i64, Vec<f32>)>,
    pub npcs_gone: Vec<i64>,
    pub cells: Vec<CellState>,
    pub cells_gone: Vec<(i32, i32)>,
    /// Scatter cells used up (eaten, burnt), and when, when that changed.
    pub spent: Option<Vec<((i32, i32), f64)>>,
    /// Flashes where something appeared, and blasts (where, when, how far).
    pub flashes: Vec<([f32; 3], f64)>,
    pub blasts: Vec<([f32; 3], f64, f32)>,
    /// Where things are being conjured now (and whether by this guest),
    /// when that changed.
    pub conjuring: Option<Vec<([f32; 3], bool)>>,
    /// The weather's state (JSON), when it changed.
    pub weather: Option<String>,
}

/// A table's changed rows (whole rows, by column) and gone keys.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Rows {
    pub table: String,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Val>>,
    /// Keys (the table's key columns) of rows that are gone.
    pub gone: Vec<Vec<Val>>,
}

/// One value in a row.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Val {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

/// A log line for one guest.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Note {
    /// Someone speaks (`id` the host's actor code, when a being), with the
    /// voice's tone for the guest's own voices.
    Line { id: Option<i64>, who: String, text: String },
    Info(String),
    Ambient(String),
    Notable(String),
    Made(String),
    Far { text: String, at: [f32; 3], made: bool },
    Incident { id: u32, text: String, at: [f32; 3], near: bool },
    Effect(String),
}

/// A sound, where it is and who made it (the host's actor code).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Sound {
    pub at: [f32; 3],
    pub from: Option<i64>,
    pub what: Heard,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum Heard {
    /// A call, as JSON parts.
    Call { call: String, mass: f32, pitch: f32, gain: f32 },
    Hit { mat: [f32; 4], mass: f32, speed: f32, by: Option<([f32; 4], f32)> },
}

pub const RUNNING: u8 = 1;
pub const CROUCHING: u8 = 2;
pub const SWIMMING: u8 = 4;
pub const GROUNDED: u8 = 8;
pub const ASLEEP: u8 = 16;
pub const CONJURING: u8 = 32;

impl Body {
    pub fn of(a: &crate::sim::actor::Actor, pitch: f32) -> Body {
        let mut flags = 0;
        for (on, f) in [(a.running, RUNNING), (a.crouching, CROUCHING), (a.swimming, SWIMMING), (a.grounded, GROUNDED), (a.asleep, ASLEEP), (a.conjuring, CONJURING)] {
            if on {
                flags |= f;
            }
        }
        Body { pos: a.pos.to_array(), yaw: a.yaw, pitch, pose: a.pose, phase: a.phase, moved: a.moved, crouch: a.crouch, alt: a.alt, flags }
    }

    /// Put this onto an actor (not its place in seats or hands: those are
    /// the world's).
    pub fn onto(&self, a: &mut crate::sim::actor::Actor) {
        a.pos = glam::Vec3::from_array(self.pos);
        a.yaw = self.yaw;
        a.pose = self.pose;
        a.phase = self.phase;
        a.moved = self.moved;
        a.crouch = self.crouch;
        a.alt = self.alt;
        a.running = self.flags & RUNNING != 0;
        a.crouching = self.flags & CROUCHING != 0;
        a.swimming = self.flags & SWIMMING != 0;
        a.grounded = self.flags & GROUNDED != 0;
        a.asleep = self.flags & ASLEEP != 0;
        a.conjuring = self.flags & CONJURING != 0;
    }

    /// Part way from `self` to `b` (0..1), for drawing between two.
    pub fn lerp(&self, b: &Body, k: f32) -> Body {
        let mix = |x: f32, y: f32| x + (y - x) * k;
        let mut out = *b;
        for i in 0..3 {
            out.pos[i] = mix(self.pos[i], b.pos[i]);
        }
        let dy = (b.yaw - self.yaw + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        out.yaw = self.yaw + dy * k;
        out.pitch = mix(self.pitch, b.pitch);
        for i in 0..9 {
            out.pose[i] = mix(self.pose[i], b.pose[i]);
        }
        out.crouch = mix(self.crouch, b.crouch);
        out.alt = mix(self.alt, b.alt);
        // The walk phase runs on; a wrap shouldn't spin the legs backwards.
        out.phase = if (b.phase - self.phase).abs() < 3.0 { mix(self.phase, b.phase) } else { b.phase };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_go_through_whole() {
        let mut look = BTreeMap::new();
        look.insert("skin".to_string(), 0.4);
        let m = Down::Welcome(Welcome { you: 2, world: "a bay".into(), name: "Ana".into(), host: "Hal".into(), bytes: 10, packed: 5, body: Body { pos: [1.0, 2.0, 3.0], flags: RUNNING | GROUNDED, ..Default::default() }, look, wounds: 0.0, t: 12.5, seed: 1, build: ("dev".into(), "abc".into()) });
        let back: Down = crate::net::decode(&crate::net::encode(&m)).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn a_body_turns_the_short_way_between_two() {
        let a = Body { yaw: 3.0, ..Default::default() };
        let b = Body { yaw: -3.0, ..Default::default() };
        let mid = a.lerp(&b, 0.5);
        assert!((mid.yaw.sin() - std::f32::consts::PI.sin()).abs() < 0.05, "{}", mid.yaw);
    }
}

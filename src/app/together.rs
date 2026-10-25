//! Playing together, as the game runs it. Hosting: guests are let in, given
//! the world, and their bodies kept in it as they move them; everyone is
//! told where everyone else is. As a guest: the host's world is the one
//! here, the guest moves their own body, and the others are drawn where the
//! host says, a moment behind so they move smoothly between what was heard.

use super::{ACCENT, App, DIM};
use crate::net::host::{Arrival, Host};
use crate::net::rows::Watch;
use crate::net::wire::{self, Body, Down, Traveler, TravelerNo, Up, Welcome};
use crate::net::Link;
use crate::sim::mirror::{Seen, from_host};
use crate::sim::{ActorId, Note};
use glam::Vec3;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// How often bodies are sent.
const BODY_EVERY: Duration = Duration::from_millis(50);
/// Messages waiting for a guest past which the steady ones (bodies, the
/// world near them, sounds) skip a turn: a connection slower than what is
/// sent catches up rather than falling ever further behind.
const BACKLOG: usize = 20;
/// How far behind what was heard the others are drawn (so there are two
/// to draw between).
const BEHIND: Duration = Duration::from_millis(110);
/// The farthest a guest may move between two of their messages (m): more
/// is put back.
const MAX_STEP: f32 = 25.0;
/// How many slow things (deeds in words, makings, replies) a guest may have
/// under way at once.
const GUEST_AT_ONCE: usize = 3;

pub enum Together {
    Host(Hosting),
    Guest(Guesting),
}

pub struct Hosting {
    pub host: Host,
    /// The world's file (copied for each guest who joins).
    path: PathBuf,
    world: String,
    name: String,
    guests: BTreeMap<TravelerNo, Guest>,
    last_bodies: Instant,
    /// The roster changed: tell everyone.
    roster_dirty: bool,
    /// Keeps every guest's copy of the world's own tables the same.
    watch: crossbeam_channel::Sender<Watch>,
    /// Beings answering a guest now: who asked, and what.
    talks: HashMap<i64, (TravelerNo, String)>,
    /// Calling out on the local network, and the way in from outside (ngrok).
    _beacon: Option<crate::net::discover::Beacon>,
    tunnel: Option<crate::net::ngrok::Tunnel>,
    /// The tunnel's state last told in the log.
    told_tunnel: Option<crate::net::ngrok::State>,
    /// Guests sent away (by token): not let back in while this hosting lasts.
    sent_away: std::collections::HashSet<String>,
    /// Each guest's talk with each being (theirs, the being's), for the next reply.
    convs: HashMap<(TravelerNo, i64), Vec<(bool, String)>>,
}

struct Guest {
    name: String,
    token: String,
    link: Link<Up, Down>,
    /// The world has gone to them (they can be sent the rest).
    ready: Arc<AtomicBool>,
    told_ready: bool,
    /// What they were last told of the world near them, and of themselves.
    seen: Seen,
    own: Option<(f32, bool, f32, Option<i64>, u8, Option<i64>)>,
    /// How they steer what they sit in or ride, and when they last said.
    controls: Option<(Up, Instant)>,
}

pub struct Guesting {
    link: Link<Down, Up>,
    pub you: TravelerNo,
    roster: Vec<Traveler>,
    /// The host's name.
    host: String,
    /// What was heard of each other body, and when.
    heard: HashMap<TravelerNo, VecDeque<(Instant, Body)>>,
    last_sent: Instant,
    /// The look last told the host.
    look: crate::world::species::LookMap,
    /// Our own name in the host's world.
    me: String,
    /// Who we last told the host we talk with.
    talking: Option<i64>,
    /// How we last said we steer, and when.
    steering: Option<(Up, Instant)>,
}

impl Hosting {
    /// Host `world` (its file at `path`) as `name`; over the internet too
    /// (an ngrok tunnel) when `internet`.
    pub fn new(host: Host, path: PathBuf, world: String, name: String, internet: bool) -> Hosting {
        let beacon = crate::net::discover::Beacon::start(&name, &world, host.port);
        let tunnel = if internet { crate::net::ngrok::Tunnel::start(host.port) } else { None };
        let (watch, rx) = crossbeam_channel::unbounded();
        let p = path.clone();
        let _ = std::thread::Builder::new().name("host-rows".into()).spawn(move || crate::net::rows::watch(p, rx));
        Hosting { host, path, world, name, guests: BTreeMap::new(), last_bodies: Instant::now(), roster_dirty: false, watch, talks: HashMap::new(), convs: HashMap::new(), _beacon: beacon, sent_away: Default::default(), told_tunnel: (internet && tunnel.is_none()).then(|| crate::net::ngrok::State::Failed(String::new())), tunnel }
    }

    /// Where guests join: the public address when there is one, else this
    /// machine's on the local network.
    pub fn address(&self) -> String {
        match self.tunnel.as_ref().map(|t| t.state()) {
            Some(crate::net::ngrok::State::Up(url)) => url,
            _ => self.host.lan_address(),
        }
    }

    /// What to give someone to join: the address and the code.
    pub fn invite(&self) -> String {
        format!("{} {}", self.address(), self.host.code)
    }

    /// How many are here besides the host.
    pub fn guests(&self) -> usize {
        self.guests.len()
    }

    pub fn names(&self) -> Vec<String> {
        self.guests.values().map(|g| g.name.clone()).collect()
    }
}

impl Guesting {
    pub fn new(link: Link<Down, Up>, w: &Welcome, roster: Vec<Traveler>) -> Guesting {
        Guesting { link, you: w.you, host: w.host.clone(), roster, heard: HashMap::new(), last_sent: Instant::now(), look: w.look.clone(), me: w.name.clone(), talking: None, steering: None }
    }

    /// Do something in the host's world.
    pub fn act(&self, json: String) {
        self.link.send(Up::Act(json));
    }

    /// Say something to a being in the host's world.
    pub fn talk(&self, cid: i64, text: String) {
        self.link.send(Up::Talk { cid, text });
    }

    /// Say something to everyone.
    pub fn say(&self, text: String) {
        self.link.send(Up::Say(text));
    }

    /// How we steer what we sit in or ride, for the host to move it.
    pub fn steer(&self, c: Up) {
        self.link.send(c);
    }

    /// The host's name, as they gave it.
    pub fn host_name(&self) -> String {
        self.roster.iter().find(|t| t.no == 0).map(|t| t.name.clone()).unwrap_or_else(|| self.host.clone())
    }
}

impl App {
    /// Each frame: whatever playing together needs.
    pub(super) fn together_tick(&mut self) {
        match self.together.take() {
            Some(Together::Host(mut h)) => {
                self.host_tick(&mut h);
                self.together = Some(Together::Host(h));
            }
            Some(Together::Guest(mut g)) => {
                let go_on = self.guest_tick(&mut g);
                if go_on {
                    self.together = Some(Together::Guest(g));
                }
            }
            None => {}
        }
    }

    /// Playing as a guest in someone's world.
    pub fn is_guest(&self) -> bool {
        matches!(self.together, Some(Together::Guest(_)))
    }

    fn host_tick(&mut self, h: &mut Hosting) {
        // The way in from outside, as it comes up (or doesn't).
        let ts = h.tunnel.as_ref().map(|t| t.state());
        if ts != h.told_tunnel && ts != Some(crate::net::ngrok::State::Starting) {
            match &ts {
                Some(crate::net::ngrok::State::Up(_)) => self.say(None, "Open over the internet (F5 copies the invite).", ACCENT),
                Some(crate::net::ngrok::State::Failed(why)) => self.say(None, &format!("Only on this network: {why}."), DIM),
                _ => {}
            }
            h.told_tunnel = ts;
        }
        if h.told_tunnel == Some(crate::net::ngrok::State::Failed(String::new())) {
            h.told_tunnel = Some(crate::net::ngrok::State::Failed("ngrok isn't installed".into()));
            self.say(None, "Only on this network: to let friends join from anywhere, install ngrok (ngrok.com/download), sign in once, and host again.", DIM);
        }
        for a in h.host.arrivals() {
            self.let_in(h, a);
        }
        // What each guest says.
        let mut gone = Vec::new();
        let mut talks = Vec::new();
        let mut said = Vec::new();
        for (&no, g) in h.guests.iter_mut() {
            for m in g.link.drain() {
                match m {
                    c @ (Up::Steer { .. } | Up::Ride { .. }) => g.controls = Some((c, Instant::now())),
                    Up::Talk { cid, text } => talks.push((no, cid, text)),
                    Up::Talking(cid) => {
                        if let Some(t) = self.sim.travelers.get_mut(&no) {
                            t.talking_to = cid;
                        }
                    }
                    Up::Say(text) => said.push((g.name.clone(), text)),
                    Up::Me(b) => self.guest_moved(no, b, &*g),
                    Up::Look(look) => {
                        if let Some(t) = self.sim.travelers.get_mut(&no) {
                            t.own.look = look;
                            h.roster_dirty = true;
                        }
                    }
                    Up::Bye => g.link.close(),
                    Up::Act(json) => self.guest_acts(no, &json, &*g),
                    Up::Hello(_) => {}
                }
            }
            if g.link.is_closed() {
                gone.push(no);
            }
            if !g.told_ready && g.ready.load(Ordering::Relaxed) {
                h.roster_dirty = true;
            }
        }
        // Guests steering what they sit in or ride (the last word holds a moment).
        let dt = self.sim.frame_dt();
        for (&no, g) in h.guests.iter_mut() {
            let who = ActorId::Traveler(no);
            match g.controls.as_ref() {
                Some((_, at)) if at.elapsed() > Duration::from_millis(400) => g.controls = None,
                Some((Up::Steer { throttle, steer, climb }, _)) => self.sim.steer(who, *throttle, *steer, *climb, dt),
                Some((Up::Ride { dir, pitch }, _)) => self.sim.drive_as(who, Vec3::from_array(*dir), *pitch, dt),
                _ => {}
            }
        }
        for (no, cid, text) in talks {
            self.guest_talks(h, no, cid, text);
        }
        for (who, text) in said {
            self.say(Some(&who), &text, ACCENT);
            for g in h.guests.values().filter(|g| g.told_ready && g.name != who) {
                g.link.send(Down::Note(wire::Note::Line { id: None, who: who.clone(), text: text.clone() }));
            }
        }
        for no in gone {
            if let Some(g) = h.guests.remove(&no) {
                h.talks.retain(|_, (n, _)| *n != no);
                h.convs.retain(|(n, _), _| *n != no);
                let _ = h.watch.send(Watch::Remove(no));
                self.save_guest(no, &g);
                self.sim.remove_traveler(no);
                let why = g.link.why().filter(|w| w != "closed").map(|w| format!(" ({w})")).unwrap_or_default();
                self.say(None, &format!("{} left.{why}", g.name), DIM);
                h.roster_dirty = true;
            }
        }
        if h.roster_dirty {
            h.roster_dirty = false;
            let roster = self.roster(h);
            for g in h.guests.values_mut() {
                if g.ready.load(Ordering::Relaxed) {
                    if !g.told_ready {
                        g.told_ready = true;
                        self.say(None, &format!("{} is here.", g.name), ACCENT);
                    }
                    g.link.send(Down::Roster(roster.clone()));
                }
            }
        }
        if h.last_bodies.elapsed() >= BODY_EVERY && !h.guests.is_empty() {
            h.last_bodies = Instant::now();
            let mut all: Vec<(TravelerNo, Body)> = vec![(0, Body::of(&self.sim.player, self.pitch))];
            all.extend(self.sim.travelers.values().map(|t| (t.no, Body::of(&t.a, t.pitch))));
            for (&no, g) in h.guests.iter_mut() {
                if !g.told_ready || g.link.backlog() > BACKLOG {
                    continue;
                }
                let others = all.iter().filter(|(n, _)| *n != no).copied().collect();
                g.link.send(Down::Bodies { t: self.sim.t, bodies: others });
                // The world near them, as it changed.
                let Some(at) = self.sim.travelers.get(&no).map(|t| t.a.pos) else { continue };
                let w = self.sim.world_near(&mut g.seen, at, no);
                if w != (wire::WorldNear { t: w.t, ..Default::default() }) {
                    g.link.send(Down::World(Box::new(w)));
                }
                let own = self.sim.own_of(no);
                if own.is_some() && own != g.own {
                    g.own = own;
                    let (wounds, fallen, glow, aboard, seat_no, riding) = own.unwrap_or_default();
                    g.link.send(Down::Own { wounds, fallen, glow, aboard, seat_no, riding });
                }
                // Seated, the host moves them: where they are now.
                if let Some(t) = self.sim.travelers.get(&no).filter(|t| t.a.carried()) {
                    g.link.send(Down::Seated(Body::of(&t.a, t.pitch)));
                }
            }
        }
    }

    /// What the host's log is told goes to the guests too: about the host
    /// by their name; what is said to the host alone ("you…") stays theirs.
    /// What happened somewhere is told to each guest as near or far as it
    /// is to them; `skip` are the notes among `notes` that were (the host's
    /// own telling of them).
    pub(super) fn share_notes(&self, notes: &[Note], placed: &[(Vec3, f32, Note)], skip: &std::collections::HashSet<usize>) {
        let Some(Together::Host(h)) = &self.together else { return };
        if h.guests.is_empty() {
            return;
        }
        for (no, g) in h.guests.iter().filter(|(_, g)| g.told_ready) {
            let Some(me) = self.sim.travelers.get(no).map(|t| t.a.pos) else { continue };
            let flat = |p: Vec3| ((p.x - me.x).powi(2) + (p.z - me.z).powi(2)).sqrt();
            let unplaced = notes.iter().enumerate().filter(|(i, _)| !skip.contains(i)).map(|(_, n)| n.clone());
            let near = placed.iter().filter_map(|(at, range, n)| crate::sim::told_at(n.clone(), flat(*at), *at, *range));
            for n in unplaced.chain(near) {
                // An incident is near or far as it is to them.
                let n = match n {
                    Note::Incident { id, text, at, .. } => Note::Incident { id, text, at, near: flat(Vec3::from_array(at)) <= 60.0 },
                    n => n,
                };
                if let Some(w) = wire_note(&n, &h.name) {
                    g.link.send(Down::Note(w));
                }
            }
        }
    }

    /// Sounds go to the guests who can hear them.
    pub(super) fn share_sounds(&self) {
        let Some(Together::Host(h)) = &self.together else { return };
        if h.guests.is_empty() || self.sim.sounds.is_empty() {
            return;
        }
        for (no, g) in h.guests.iter().filter(|(_, g)| g.told_ready && g.link.backlog() <= BACKLOG) {
            let Some(at) = self.sim.travelers.get(no).map(|t| t.a.pos) else { continue };
            let near: Vec<wire::Sound> = self.sim.sounds.iter().filter(|c| (c.at - at).length() < 80.0).map(wire_sound).collect();
            if !near.is_empty() {
                g.link.send(Down::Heard(near));
            }
        }
    }

    /// Everyone here, for the guests: the host as traveler 0.
    fn roster(&self, h: &Hosting) -> Vec<Traveler> {
        let mut r = vec![Traveler { no: 0, name: h.name.clone(), look: self.sim.own.look.clone() }];
        for (no, g) in &h.guests {
            let look = self.sim.travelers.get(no).map(|t| t.own.look.clone()).unwrap_or_default();
            r.push(Traveler { no: *no, name: g.name.clone(), look });
        }
        r
    }

    /// A guest let in: their number, their body in the world, and the
    /// world sent to them (saved first, so it is as it is now).
    fn let_in(&mut self, h: &mut Hosting, a: Arrival) {
        let Arrival { hello, link, from } = a;
        // The host playing in their own world from another window.
        let token = if hello.token == self.settings.player().1 { format!("{}+", hello.token) } else { hello.token.clone() };
        if h.sent_away.contains(&token) {
            link.send(Down::Refused(format!("{} sent you away from this world for now.", h.name)));
            link.close();
            return;
        }
        let known = self.db.travelers();
        let row = known.iter().find(|r| r.token == token).cloned();
        let Some(no) = row.as_ref().map(|r| r.no).or_else(|| (1..=u8::MAX).find(|n| !known.iter().any(|r| r.no == *n))) else {
            link.send(Down::Refused("This world has had too many travelers.".into()));
            link.close();
            return;
        };
        // Back from somewhere else (a dropped connection): the old one goes.
        if let Some(old) = h.guests.remove(&no) {
            old.link.close();
            self.save_guest(no, &old);
            self.sim.remove_traveler(no);
        }
        crate::log::info(format!("{} joins from {from} as traveler {no}", hello.name));
        self.say(None, &format!("{} is joining…", hello.name), DIM);
        let (r, c, w) = crate::net::this_build();
        if let Some(n) = crate::net::build_note(&format!("{} is", hello.name), (r, c, w), (&hello.release, &hello.commit, w)) {
            self.say(None, &n, DIM);
        }
        // Back: where they left, as they were. New: before the host.
        let (at, yaw) = match &row {
            Some(r) => (Vec3::from_array(r.pos), r.yaw),
            None => self.beside_me(),
        };
        let look: crate::world::species::LookMap = row.as_ref().and_then(|r| serde_json::from_str(&r.look_json).ok()).unwrap_or_default();
        let wounds = row.as_ref().map(|r| r.wounds).unwrap_or(0.0);
        self.sim.add_traveler(no, &hello.name, &look, at, yaw);
        if let Some(t) = self.sim.travelers.get_mut(&no) {
            t.wounds = wounds;
        }
        let g = Guest { name: hello.name.clone(), token, link, ready: Arc::new(AtomicBool::new(false)), told_ready: false, seen: Seen::default(), own: None, controls: None };
        self.save_guest(no, &g);
        // The copy they get is the world as it is now.
        self.save_player();
        self.save_characters();
        let body = self.sim.travelers.get(&no).map(|t| Body::of(&t.a, 0.0)).unwrap_or_default();
        let welcome = Welcome { you: no, world: h.world.clone(), name: hello.name.clone(), host: h.name.clone(), bytes: 0, packed: 0, body, look, wounds, t: self.sim.t, seed: self.sim.seed, build: (crate::db::RELEASE.into(), crate::db::COMMIT.into()) };
        let (tx, path, r, watch) = (g.link.sender(), h.path.clone(), g.ready.clone(), h.watch.clone());
        let name = hello.name.clone();
        let _ = std::thread::Builder::new().name("host-send-world".into()).spawn(move || match crate::net::host::guest_copy(&path) {
            Ok((packed, bytes, had)) => {
                let _ = tx.send(Down::Welcome(Welcome { bytes, packed: packed.len() as u64, ..welcome }));
                for c in packed.chunks(crate::net::host::CHUNK) {
                    if tx.send(Down::WorldChunk(c.to_vec())).is_err() {
                        return;
                    }
                }
                let _ = tx.send(Down::WorldDone);
                // From here the world's tables are kept the same for them.
                let _ = watch.send(Watch::Add(no, tx, had));
                r.store(true, Ordering::Relaxed);
            }
            Err(e) => {
                crate::log::error(format!("sending the world to {name}: {e:#}"));
                let _ = tx.send(Down::Refused(format!("The host couldn't send the world: {e}")));
            }
        });
        h.guests.insert(no, g);
    }

    /// Keep a guest as they are now (where, how they look, how hurt), for
    /// when they come back.
    fn save_guest(&self, no: TravelerNo, g: &Guest) {
        let Some(t) = self.sim.travelers.get(&no) else { return };
        let row = crate::db::TravelerRow { no, token: g.token.clone(), name: g.name.clone(), pos: t.a.pos.to_array(), yaw: t.a.yaw, look_json: serde_json::to_string(&t.own.look).unwrap_or_else(|_| "{}".into()), wounds: if t.fallen.is_some() { 0.0 } else { t.wounds }, last_seen: crate::db::now() };
        if let Err(e) = self.db.save_traveler(&row) {
            crate::log::error(format!("saving traveler {no}: {e:#}"));
        }
    }

    /// Every guest kept as they are now (with the regular saves).
    pub(super) fn save_guests(&self) {
        if let Some(Together::Host(h)) = &self.together {
            for (no, g) in &h.guests {
                self.save_guest(*no, g);
            }
        }
    }

    /// Where a newcomer arrives: a couple of steps before the host, facing
    /// them (out of anything solid).
    fn beside_me(&mut self) -> (Vec3, f32) {
        use crate::world::collide::{Obstacles, PLAYER_RADIUS, free_spot};
        let me = self.sim.player.pos;
        let yaw = self.sim.player.yaw;
        let want = me + Vec3::new(yaw.sin(), 0.0, yaw.cos()) * 2.5;
        let want = Vec3::new(want.x, self.snap.terrain.height(want.x, want.z).max(me.y - 1.0), want.z);
        let solids = self.sim.solids_near(want, 6.0);
        let obs = Obstacles { solids: &solids, bodies: &[] };
        let at = free_spot(&self.snap.terrain, &obs, want, PLAYER_RADIUS, 20.0, true);
        let face = me - at;
        (at, face.x.atan2(face.z))
    }

    /// Whether guest `no` may have the host's LLM do more for them now: not
    /// past what the host lets each guest spend, and not too much at once.
    /// The reason when not.
    fn guest_may(&self, no: TravelerNo, talks: &HashMap<i64, (TravelerNo, String)>) -> Result<(), String> {
        let Some(llm) = self.llm.as_ref() else { return Err("the host's world has no LLM: you can walk, look and take things".into()) };
        if self.budget_paused || llm.over_budget() {
            return Err("the host's budget is spent for now".into());
        }
        let limit = self.settings.guest_budget_usd;
        if llm.spent_for(no) >= limit {
            return Err(format!("the host's limit for you (${limit:.2}) is spent: you can still walk, look and take things"));
        }
        let busy = self.sim.work().iter().filter(|w| w.who == Some(ActorId::Traveler(no))).count() + talks.values().filter(|(n, _)| *n == no).count();
        if busy >= GUEST_AT_ONCE {
            return Err("wait for what you are doing to come first".into());
        }
        Ok(())
    }

    /// A guest says something to a being: it answers them (the host's LLM,
    /// on what the guest may spend), and everyone near hears both.
    fn guest_talks(&mut self, h: &mut Hosting, no: TravelerNo, cid: i64, text: String) {
        let Some(g) = h.guests.get(&no) else { return };
        let refuse = |why: String| {
            g.link.send(Down::Note(wire::Note::Info(format!("({why})"))));
            g.link.send(Down::ReplyDone { cid, ok: false });
        };
        if let Err(why) = self.guest_may(no, &h.talks) {
            return refuse(why);
        }
        let name = self.sim.actor_name(ActorId::Npc(cid));
        if h.talks.contains_key(&cid) || self.replying == Some(cid) {
            return refuse(format!("{name} is answering someone else"));
        }
        let me = g.name.clone();
        let text: String = text.chars().take(600).collect();
        self.say(Some(&me), &text, ACCENT);
        for (n, o) in &h.guests {
            if *n != no && o.told_ready {
                o.link.send(Down::Note(wire::Note::Line { id: None, who: me.clone(), text: text.clone() }));
            }
        }
        let at = self.sim.travelers.get(&no).map(|t| t.a.pos);
        self.sim.event("said", Some(ActorId::Traveler(no)), Some(ActorId::Npc(cid).key()), format!("{me} said to {name}: \"{text}\""), at, serde_json::json!({ "text": text }));
        let context = self.guest_talk_context(no);
        let history = h.convs.get(&(no, cid)).cloned().unwrap_or_default();
        h.convs.entry((no, cid)).or_default().push((true, text.clone()));
        h.talks.insert(cid, (no, text.clone()));
        self.brain.send_as(crate::brain::Cmd::Talk { cid, text, context, history }, Some(no));
    }

    /// What a being is told of the moment when a guest speaks to it.
    fn guest_talk_context(&self, no: TravelerNo) -> String {
        let Some(t) = self.sim.travelers.get(&no) else { return String::new() };
        let p = t.a.pos;
        let place = self.snap.region_name(crate::world::region_of(p.x, p.z)).map(str::to_string).unwrap_or_else(|| "the open land".into());
        let held = t.a.held.map(|h| format!(" {} is holding {}.", t.name, crate::sim::actions::the(&self.sim.thing_name(h)))).unwrap_or_default();
        format!("It is {} in {place}. Speaking with you now is {}, another traveler (not the one your memories call the traveler).{held}", crate::render::sky::time_label(self.sim.t), t.name)
    }

    /// What the brain says, as the guests should hear it. True when it was
    /// a reply to a guest, wholly handled here.
    pub(super) fn share_event(&mut self, ev: &crate::brain::Event) -> bool {
        use crate::brain::Event;
        let Some(Together::Host(h)) = self.together.as_mut() else { return false };
        match ev {
            Event::Token { cid, text } => {
                for g in h.guests.values().filter(|g| g.told_ready) {
                    g.link.send(Down::Token { cid: *cid, text: text.clone() });
                }
                false
            }
            Event::ReplyDone { cid, ok } => {
                for g in h.guests.values().filter(|g| g.told_ready) {
                    g.link.send(Down::ReplyDone { cid: *cid, ok: *ok });
                }
                let Some((no, _)) = h.talks.remove(cid) else { return false };
                // Theirs: the reply goes into their talk, not the host's.
                let (cid, ok) = (*cid, *ok);
                let reply = self.log.iter_mut().rev().find(|l| l.streaming == Some(cid)).map(|l| {
                    l.streaming = None;
                    l.text.clone()
                });
                if let Some(Together::Host(h)) = self.together.as_mut() {
                    let c = h.convs.entry((no, cid)).or_default();
                    match (ok, reply) {
                        (true, Some(r)) => c.push((false, r)),
                        _ => {
                            c.pop();
                        }
                    }
                }
                self.voice_reply_done(cid, ok);
                if let Some(line) = self.sim_talk.remove(&cid) {
                    if ok {
                        self.sim.npc_said(cid, line.trim());
                    }
                }
                self.dirty = true;
                true
            }
            _ => false,
        }
    }

    /// A guest does something: done as them, and they are told how it went.
    fn guest_acts(&mut self, no: TravelerNo, json: &str, g: &Guest) {
        let a = serde_json::from_str::<serde_json::Value>(json).map(|mut v| {
            as_host(&mut v, no);
            v
        });
        let a: Option<crate::sim::Action> = a.ok().and_then(|v| serde_json::from_value(v).ok());
        let Some(a) = a else {
            g.link.send(Down::Acted { verb: String::new(), ok: false, msg: "the host's game didn't understand that".into(), thing: false });
            return;
        };
        let verb = a.verb().to_string();
        // A deed in words is the host's LLM's work: within what they may spend.
        if verb == "do" {
            if let Err(why) = self.guest_may(no, &HashMap::new()) {
                g.link.send(Down::Acted { verb, ok: false, msg: why, thing: false });
                return;
            }
        }
        let r = self.sim.act(ActorId::Traveler(no), a);
        let (ok, msg, thing) = match r {
            Ok(o) => (o.ok, o.msg, o.thing.is_some()),
            Err(e) => (false, e.to_string(), false),
        };
        g.link.send(Down::Acted { verb, ok, msg, thing });
    }

    /// A guest moved themselves: kept, unless it is too far to be a step.
    fn guest_moved(&mut self, no: TravelerNo, b: Body, g: &Guest) {
        let Some(t) = self.sim.travelers.get_mut(&no) else { return };
        // Seated or carried, where they are is the host's; where they look is theirs.
        if t.a.carried() {
            t.pitch = b.pitch;
            return;
        }
        let to = Vec3::from_array(b.pos);
        if !to.is_finite() || (to - t.a.pos).length() > MAX_STEP {
            g.link.send(Down::Place(Body::of(&t.a, t.pitch)));
            return;
        }
        b.onto(&mut t.a);
        t.pitch = b.pitch;
    }

    /// One frame as a guest: what the host says, the others drawn between
    /// what was heard, and our own body sent. False when the game is over.
    fn guest_tick(&mut self, g: &mut Guesting) -> bool {
        let now = Instant::now();
        for m in g.link.drain() {
            match m {
                Down::Roster(r) => {
                    self.take_roster(g, r);
                }
                Down::Bodies { t, bodies } => {
                    // The host's clock: followed gently, jumped to when far off.
                    let d = t - self.sim.t;
                    if d.abs() > 2.0 { self.sim.t = t } else { self.sim.t += d * 0.1 }
                    for (no, b) in bodies {
                        let q = g.heard.entry(no).or_default();
                        q.push_back((now, b));
                        while q.len() > 8 {
                            q.pop_front();
                        }
                    }
                }
                Down::Place(b) => {
                    b.onto(&mut self.sim.player);
                    self.pitch = b.pitch;
                }
                Down::Say(s) => self.say(None, &s, DIM),
                Down::World(w) => self.sim.take_world(*w),
                Down::Rows { tables, rebuild } => match crate::net::rows::apply(&self.db, &tables) {
                    Ok(ground) => {
                        if rebuild {
                            self.brain.send(crate::brain::Cmd::Reload);
                        } else if ground {
                            crate::sim::persist::apply_made_ground(&mut self.sim);
                        }
                    }
                    Err(e) => crate::log::error(format!("taking in the host's world: {e:#}")),
                },
                Down::Own { wounds, fallen, glow, aboard, seat_no, riding } => {
                    self.sim.take_own(wounds, fallen, glow);
                    self.sim.take_seat(aboard, seat_no, riding);
                }
                Down::Seated(b) => self.sim.take_seated(b),
                Down::Token { cid, text } => self.on_event(crate::brain::Event::Token { cid, text }),
                Down::ReplyDone { cid, ok } => self.on_event(crate::brain::Event::ReplyDone { cid, ok }),
                Down::Acted { verb, ok, msg, thing } => {
                    let told = matches!(verb.as_str(), "use" | "do" | "eat" | "answer" | "give" | "gesture" | "propose" | "pet" | "ride" | "dismount") || (matches!(verb.as_str(), "hold" | "drop") && !thing);
                    if !ok {
                        self.say(None, &format!("Can't: {}.", super::you_as(&msg, &g.me)), DIM);
                    } else if told {
                        self.say(None, &super::you_as(&msg, &g.me), super::TEXT);
                    }
                }
                Down::Note(n) => {
                    let n = guest_note(n, g.you, &g.me);
                    self.tell(n);
                }
                Down::Heard(sounds) => {
                    for s in sounds {
                        if let Some(c) = cue_of(s, g.you) {
                            self.sim.sounds.push(c);
                        }
                    }
                }
                Down::Bye(why) => {
                    self.leave_world(why);
                    return false;
                }
                Down::Refused(why) => {
                    self.leave_world(why);
                    return false;
                }
                Down::Welcome(_) | Down::WorldChunk(_) | Down::WorldDone => {}
            }
        }
        if g.link.is_closed() {
            let why = g.link.why().filter(|w| w != "closed").map(|w| format!(" ({w})")).unwrap_or_default();
            self.leave_world(format!("The connection to {} was lost{why}.", g.host_name()));
            return false;
        }
        let at = now.checked_sub(BEHIND).unwrap_or(now);
        for (no, q) in &g.heard {
            let Some(t) = self.sim.travelers.get_mut(no) else { continue };
            let b = between(q, at);
            b.onto(&mut t.a);
            t.pitch = b.pitch;
        }
        // Who we talk with, for them to face us (at once: before the words).
        if g.talking != self.sim.talking_to {
            g.talking = self.sim.talking_to;
            g.link.send(Up::Talking(g.talking));
        }
        if g.last_sent.elapsed() >= BODY_EVERY {
            g.last_sent = now;
            g.link.send(Up::Me(Body::of(&self.sim.player, self.pitch)));
            // The look they chose, when it changes, for the others to see.
            if g.look != self.sim.own.look {
                g.look = self.sim.own.look.clone();
                g.link.send(Up::Look(g.look.clone()));
            }
        }
        true
    }

    /// Who is here, as the host says: the others come and go.
    fn take_roster(&mut self, g: &mut Guesting, r: Vec<Traveler>) {
        for t in r.iter().filter(|t| t.no != g.you) {
            match self.sim.travelers.get_mut(&t.no) {
                Some(have) => {
                    have.name = t.name.clone();
                    have.own.look = t.look.clone();
                }
                None => {
                    let at = g.heard.get(&t.no).and_then(|q| q.back()).map(|(_, b)| Vec3::from_array(b.pos)).unwrap_or(self.sim.player.pos);
                    self.sim.add_traveler(t.no, &t.name, &t.look, at, 0.0);
                    if !g.roster.iter().any(|o| o.no == t.no) {
                        self.say(None, &format!("{} is here.", t.name), ACCENT);
                    }
                }
            }
        }
        let gone: Vec<TravelerNo> = self.sim.travelers.keys().copied().filter(|n| !r.iter().any(|t| t.no == *n)).collect();
        for no in gone {
            if let Some(t) = self.sim.remove_traveler(no) {
                self.say(None, &format!("{} left.", t.name), DIM);
            }
            g.heard.remove(&no);
        }
        g.roster = r;
    }

    /// The game together is over: back to the worlds, saying why.
    fn leave_world(&mut self, why: String) {
        crate::log::info(format!("left the host's world: {why}"));
        self.left_because = Some(why);
        self.to_worlds = true;
        self.quit = true;
    }

    /// Leaving the game: guests are told (as a host) or the host is (as a guest).
    pub(super) fn together_end(&mut self) {
        match self.together.take() {
            Some(Together::Host(h)) => {
                for g in h.guests.values() {
                    g.link.send(Down::Bye(format!("{} closed the world.", h.name)));
                    g.link.close();
                }
                h.host.stop();
                // A moment for the goodbyes to go out.
                let until = Instant::now() + Duration::from_millis(300);
                while Instant::now() < until && h.guests.values().any(|g| !g.link.is_closed()) {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            Some(Together::Guest(g)) => {
                g.link.send(Up::Bye);
                g.link.close();
                std::thread::sleep(Duration::from_millis(50));
            }
            None => {}
        }
    }

    /// Just joined: standing where the host says, as the host keeps us,
    /// with the others here.
    pub fn arrive_as_guest(&mut self, w: &Welcome, roster: Vec<Traveler>) {
        w.body.onto(&mut self.sim.player);
        self.pitch = w.body.pitch;
        self.sim.mirror.on = true;
        self.sim.mirror.you = w.you;
        self.sim.own.look = w.look.clone();
        self.sim.t = w.t;
        self.cam_y = self.sim.player.pos.y + self.sim.player.dims.eye;
        if let Some(Together::Guest(mut g)) = self.together.take() {
            self.take_roster(&mut g, roster);
            let host = g.host_name();
            self.together = Some(Together::Guest(g));
            self.say(None, &format!("You're in {host}'s world. They keep it, and its words come from them."), ACCENT);
            let (r, c, n) = crate::net::this_build();
            if let Some(note) = crate::net::build_note(&format!("{host} is"), (r, c, n), (&w.build.0, &w.build.1, n)) {
                self.say(None, &note, DIM);
            }
        }
    }

    /// The invite (address and code) to the clipboard, and said in the log.
    pub(super) fn copy_invite(&mut self) {
        let line = match &self.together {
            Some(Together::Host(h)) => h.invite(),
            Some(Together::Guest(g)) => format!("in {}'s world: ask them for the invite", g.host_name()),
            None => return,
        };
        let guest = self.is_guest();
        let copied = !guest && arboard::Clipboard::new().and_then(|mut c| c.set_text(line.clone())).is_ok();
        // The address is never written on screen: it can be read off a recording.
        self.say(None, &if copied { "Copied the invite. Whoever joins chooses Play together → Join with an invite, and pastes it.".to_string() } else if guest { line } else { "Couldn't copy the invite: Esc → Invite friends shows it.".to_string() }, ACCENT);
    }

    /// The host sends a guest away ("/kick NAME"): they leave the world at
    /// once (kept as they were, as on any leaving) and can't come back
    /// while this hosting lasts.
    pub(super) fn send_away(&mut self, name: &str) {
        let Some(Together::Host(h)) = self.together.as_mut() else {
            self.say(None, "Only the host can send someone away.", DIM);
            return;
        };
        let want = name.trim().to_lowercase();
        let found = h.guests.iter().find(|(_, g)| g.name.to_lowercase() == want).or_else(|| h.guests.iter().find(|(_, g)| g.name.to_lowercase().starts_with(&want))).map(|(n, g)| (*n, g.name.clone()));
        let Some((no, who)) = found.filter(|_| !want.is_empty()) else {
            let names = h.names();
            let here = if names.is_empty() { "nobody else is here".to_string() } else { format!("here: {}", names.join(", ")) };
            self.say(None, &format!("No guest called {name} ({here})."), DIM);
            return;
        };
        if let Some(g) = h.guests.get(&no) {
            h.sent_away.insert(g.token.clone());
            g.link.send(Down::Bye(format!("{} sent you away from this world.", h.name)));
            g.link.close();
        }
        self.say(None, &format!("You sent {who} away; they can't come back until you host again."), DIM);
    }

    /// Something said to everyone playing ("/say").
    pub(super) fn say_to_all(&mut self, text: String) {
        let text: String = text.chars().take(400).collect();
        match &self.together {
            Some(Together::Guest(g)) => {
                g.say(text.clone());
                self.say(Some("you"), &text, ACCENT);
            }
            Some(Together::Host(h)) => {
                for g in h.guests.values().filter(|g| g.told_ready) {
                    g.link.send(Down::Note(wire::Note::Line { id: None, who: h.name.clone(), text: text.clone() }));
                }
                self.say(Some("you"), &text, ACCENT);
            }
            None => {}
        }
    }

    /// A guest steering what they sit in or ride: told to the host (who
    /// moves it), now and then while it holds. False when not a guest.
    pub(super) fn guest_steers(&mut self, c: Up) -> bool {
        let Some(Together::Guest(g)) = self.together.as_mut() else { return false };
        if g.steering.as_ref().is_none_or(|(last, at)| *last != c || at.elapsed() > Duration::from_millis(100)) {
            g.steer(c.clone());
            g.steering = Some((c, Instant::now()));
        }
        true
    }

    /// Playing in someone's world: our name there, and the host's.
    pub(super) fn guest_names(&self) -> Option<(String, String)> {
        match &self.together {
            Some(Together::Guest(g)) => Some((g.me.clone(), g.host_name())),
            _ => None,
        }
    }

    /// Who else is here, for the top line.
    pub(super) fn together_line(&self) -> Option<String> {
        match &self.together {
            Some(Together::Host(h)) => {
                let who = match h.guests() {
                    0 => "nobody has joined yet".to_string(),
                    _ => format!("with {}", h.names().join(", ")),
                };
                Some(format!("hosting · code {} · {who} · F5 copies the invite", h.host.code))
            }
            Some(Together::Guest(g)) => Some(format!("in {}'s world", g.host_name())),
            None => None,
        }
    }

}

/// An action from a guest, naming actors as the host knows them: the
/// guest's "player" is traveler `no`, their traveler 0 the host's player.
fn as_host(v: &mut serde_json::Value, no: TravelerNo) {
    match v {
        serde_json::Value::String(s) if s == "player" => *s = ActorId::Traveler(no).key(),
        serde_json::Value::String(s) if s == "player:0" => *s = "player".into(),
        serde_json::Value::Array(a) => a.iter_mut().for_each(|x| as_host(x, no)),
        serde_json::Value::Object(o) => o.values_mut().for_each(|x| as_host(x, no)),
        _ => {}
    }
}

/// An action as a guest sends it.
pub(super) fn action_json(a: &crate::sim::Action) -> String {
    serde_json::to_string(a).unwrap_or_default()
}

/// A note of the host's as a guest hears it: none when it was said to the
/// host alone ("You fall."), and the host by their name.
fn wire_note(n: &Note, host: &str) -> Option<wire::Note> {
    let personal = |t: &str| {
        let l = format!(" {} ", t.to_lowercase());
        l.starts_with(" you ") || l.contains(" you ") || l.contains(" your ") || l.contains(" you.") || l.contains(" you,")
    };
    let named = |t: &str| t.replace("the traveler's", &format!("{host}'s")).replace("The traveler", host).replace("the traveler", host);
    Some(match n {
        Note::Line { id, who, text } => wire::Note::Line { id: id.map(|a| a.code()), who: who.clone(), text: text.clone() },
        Note::Info(t) | Note::Effect(t) if personal(t) => return None,
        Note::Notable(t) | Note::Made(t) | Note::Ambient(t) if personal(t) => return None,
        Note::Info(t) => wire::Note::Info(named(t)),
        Note::Effect(t) => wire::Note::Effect(named(t)),
        Note::Ambient(t) => wire::Note::Ambient(named(t)),
        Note::Notable(t) => wire::Note::Notable(named(t)),
        Note::Made(t) => wire::Note::Made(named(t)),
        Note::Far { text, at, made } => wire::Note::Far { text: named(text), at: *at, made: *made },
        Note::Incident { id, text, at, near } => wire::Note::Incident { id: *id, text: named(text), at: *at, near: *near },
    })
}

/// A note from the host, for this guest's log: about them, said to them
/// ("Mara waves at Bea" → "Mara waves at you").
fn guest_note(n: wire::Note, you: TravelerNo, me: &str) -> Note {
    let y = |t: String| if t.contains(me) { super::you_as(&t, me) } else { t };
    match n {
        wire::Note::Line { id, who, text } => Note::Line { id: id.map(|c| from_host(c, you)), who, text },
        wire::Note::Info(t) => Note::Info(y(t)),
        wire::Note::Ambient(t) => Note::Ambient(y(t)),
        wire::Note::Notable(t) => Note::Notable(y(t)),
        wire::Note::Made(t) => Note::Made(y(t)),
        wire::Note::Far { text, at, made } => Note::Far { text: y(text), at, made },
        wire::Note::Incident { id, text, at, near } => Note::Incident { id, text: y(text), at, near },
        wire::Note::Effect(t) => Note::Effect(y(t)),
    }
}

fn wire_sound(c: &crate::audio::Cue) -> wire::Sound {
    use crate::audio::Heard;
    let mat = |m: &crate::audio::call::Material| [m.hard, m.dry, m.ring, m.leafy];
    let what = match &c.what {
        Heard::Call { call, mass, pitch, gain } => wire::Heard::Call { call: serde_json::to_string(call).unwrap_or_default(), mass: *mass, pitch: *pitch, gain: *gain },
        Heard::Hit { mat: m, mass, speed, by } => wire::Heard::Hit { mat: mat(m), mass: *mass, speed: *speed, by: by.as_ref().map(|(m, x)| (mat(m), *x)) },
    };
    wire::Sound { at: c.at.to_array(), from: c.from.map(|a| a.code()), what }
}

fn cue_of(s: wire::Sound, you: TravelerNo) -> Option<crate::audio::Cue> {
    use crate::audio::Heard;
    let mat = |m: [f32; 4]| crate::audio::call::Material { hard: m[0], dry: m[1], ring: m[2], leafy: m[3] };
    let what = match s.what {
        wire::Heard::Call { call, mass, pitch, gain } => {
            let v: serde_json::Value = serde_json::from_str(&call).ok()?;
            Heard::Call { call: crate::audio::call::Call::from_value(&v), mass, pitch, gain }
        }
        wire::Heard::Hit { mat: m, mass, speed, by } => Heard::Hit { mat: mat(m), mass, speed, by: by.map(|(m, x)| (mat(m), x)) },
    };
    Some(crate::audio::Cue { at: Vec3::from_array(s.at), from: s.from.map(|c| from_host(c, you)), what })
}

/// A body at `at`, between the two heard around it (or the last, held).
fn between(q: &VecDeque<(Instant, Body)>, at: Instant) -> Body {
    let mut prev: Option<&(Instant, Body)> = None;
    for s in q {
        if s.0 >= at {
            return match prev {
                Some(p) => {
                    let span = (s.0 - p.0).as_secs_f32().max(1e-3);
                    p.1.lerp(&s.1, ((at - p.0).as_secs_f32() / span).clamp(0.0, 1.0))
                }
                None => s.1,
            };
        }
        prev = Some(s);
    }
    q.back().map(|s| s.1).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::model::{Live, WorldModel};
    use parking_lot::Mutex;

    /// A game on `db` (drawn on the CPU), hosting or a guest, with an LLM
    /// that always says the same.
    fn game(db: Arc<Db>, together: Option<Together>, llm: bool) -> App {
        let live = Arc::new(Mutex::new(Live::default()));
        let mut model = WorldModel::load(db.clone(), None, live.clone()).unwrap();
        let snap = model.snapshot().unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let llm = llm.then(|| crate::llm::Llm::scripted(db.clone(), Arc::new(|sys: &str, _: &[crate::llm::Msg]| if sys.contains("You are a character") { "Welcome, stranger.".into() } else { "{}".into() })));
        let brain = crate::brain::Brain::start(model, llm.clone(), tx, false);
        App::new(super::super::Setup { db, brain, events: rx, render: crate::render::cpu::spawn(), snap, live, truecolor: true, size: (100, 34), llm, genesis: false, together, seed: None })
    }

    /// Tick both, and take in what each one's brain says.
    fn tick_both(games: &mut [&mut App]) {
        for g in games.iter_mut() {
            g.tick(1.0 / 60.0);
            while let Ok(ev) = g.events.try_recv() {
                g.on_event(ev);
            }
        }
    }

    /// Tick until `done`, or fail after a while.
    fn until(games: &mut [&mut App], what: &str, mut done: impl FnMut(&mut [&mut App]) -> bool) {
        let t0 = Instant::now();
        while !done(games) {
            assert!(t0.elapsed() < Duration::from_secs(15), "waited too long for {what}");
            tick_both(games);
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A world with someone in it, hosted, and a guest joined to it.
    fn pair(tag: &str, port: u16) -> (App, App, PathBuf) {
        pair_with(tag, port, false, &|_, _| {})
    }

    /// As `pair`, with an LLM when `llm`, and `setup` adding to the world first.
    fn pair_with(tag: &str, port: u16, llm: bool, setup: &dyn Fn(&Arc<Db>, Vec3)) -> (App, App, PathBuf) {
        let path = std::env::temp_dir().join(format!("pocket-together-{tag}-{}.pocket", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Db::create(&path, 4242, "a quiet test valley", &serde_json::to_string(&crate::world::Look::default()).unwrap()).unwrap();
        db.kv_set("genesis", "done").unwrap();
        let spawn = {
            let live = Arc::new(Mutex::new(Live::default()));
            WorldModel::load(db.clone(), None, live).unwrap().snapshot().unwrap().spawn
        };
        let persona = crate::world::Persona { name: "Mara".into(), ..Default::default() };
        db.tx(|tx| {
            let v = crate::db::add_version(tx, None, "region", "test region", None)?;
            crate::db::add_character(tx, crate::world::region_of(spawn.x, spawn.z), &serde_json::to_string(&persona)?, spawn.x + 3.0, spawn.z + 3.0, v)?;
            Ok(())
        })
        .unwrap();
        setup(&db, spawn);
        let host = Host::start(port, "lime-otter-42".into()).unwrap();
        let addr = format!("127.0.0.1:{}", host.port);
        let mut h = game(db, Some(Together::Host(Hosting::new(host, path.clone(), "Test Vale".into(), "Hal".into(), false))), llm);

        // The guest joins on another thread while the host's game runs.
        let joining = std::thread::spawn(move || crate::net::guest::join(&addr, "lime-otter-42", "Bea", "tok-bea", |_, _| {}));
        let t0 = Instant::now();
        while !joining.is_finished() {
            assert!(t0.elapsed() < Duration::from_secs(15), "the join took too long");
            h.tick(1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(5));
        }
        let j = joining.join().unwrap().unwrap();
        assert_eq!(j.welcome.you, 1);
        let gdb = Db::from_bytes(&j.world).unwrap();
        crate::net::guest::as_guest(&gdb, &j.welcome).unwrap();
        let mut g = game(gdb, Some(Together::Guest(Guesting::new(j.link, &j.welcome, j.roster.clone()))), false);
        g.arrive_as_guest(&j.welcome, j.roster);
        assert!(g.is_guest());
        (h, g, path)
    }

    #[test]
    fn two_players_see_each_other_walk() {
        let port = 43000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        let (mut h, mut g, path) = pair("walk", port);
        let start = h.sim.player.pos;

        // Each comes to see the other.
        until(&mut [&mut h, &mut g], "each to see the other", |gs| gs[0].sim.travelers.contains_key(&1) && gs[1].sim.travelers.contains_key(&0));
        assert_eq!(g.sim.travelers[&0].name, "Hal");
        assert_eq!(h.sim.travelers[&1].name, "Bea");
        assert!(h.log.iter().any(|l| l.text.contains("Bea is here")));

        // The guest walks: the host sees them go.
        let from = h.sim.travelers[&1].a.pos;
        g.sim.player.pos = g.sim.player.pos + Vec3::new(3.0, 0.0, 0.0);
        until(&mut [&mut h, &mut g], "the host to see the guest walk", |gs| (gs[0].sim.travelers[&1].a.pos - from).length() > 2.5);
        // The host walks: the guest sees them go, a moment behind.
        h.sim.player.pos = start + Vec3::new(0.0, 0.0, 4.0);
        until(&mut [&mut h, &mut g], "the guest to see the host walk", |gs| (gs[1].sim.travelers[&0].a.pos - (start + Vec3::new(0.0, 0.0, 4.0))).length() < 0.5);

        // A step too far to be a step is put back.
        g.sim.player.pos += Vec3::new(500.0, 0.0, 0.0);
        until(&mut [&mut h, &mut g], "the guest to be put back", |gs| (gs[1].sim.player.pos - gs[0].sim.travelers[&1].a.pos).length() < 30.0);

        // The guest leaves: gone from the host's world.
        g.shutdown();
        until(&mut [&mut h], "the guest to be gone", |gs| !gs[0].sim.travelers.contains_key(&1));
        assert!(h.log.iter().any(|l| l.text.contains("Bea left")));
        // Kept for next time: where she was, by her token.
        let kept = h.db.travelers();
        assert_eq!(kept.len(), 1);
        assert_eq!((kept[0].no, kept[0].token.as_str(), kept[0].name.as_str()), (1, "tok-bea", "Bea"));
        assert!((Vec3::from_array(kept[0].pos) - g.sim.player.pos).length() < 30.0);
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_guest_sees_the_living_world() {
        let port = 45000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        let (mut h, mut g, path) = pair("living", port);
        until(&mut [&mut h, &mut g], "the guest to be in", |gs| gs[1].sim.travelers.contains_key(&0));
        // The same people, looking the same.
        let mara = h.sim.cast.npcs[0].def.id;
        assert_eq!(g.sim.cast.get(mara).map(|n| n.sliders), h.sim.cast.get(mara).map(|n| n.sliders), "Mara looks the same to both");
        // Mara walks off on the host: the guest sees her go.
        let to = h.sim.cast.get(mara).unwrap().a.pos + Vec3::new(5.0, 0.0, 0.0);
        until(&mut [&mut h, &mut g], "Mara to move for the guest", |gs| {
            if let Some(n) = gs[0].sim.cast.get_mut(mara) {
                n.a.pos = to;
                n.think_at = f64::MAX;
            }
            gs[1].sim.cast.get(mara).is_some_and(|n| (n.a.pos - to).length() < 0.5)
        });
        // A stone appears by the host: the guest sees it, and its flash.
        let stone = h.sim.type_by_name("stone").unwrap().id;
        let at = h.sim.player.pos + Vec3::new(1.0, 0.5, 1.0);
        let id = h.sim.spawn_thing(stone, at, 0.0, 1.0, Default::default(), true).unwrap();
        h.sim.flashes.push((at, h.sim.t));
        until(&mut [&mut h, &mut g], "the stone to show", |gs| gs[1].sim.things.get(id).is_some());
        assert!(!g.sim.flashes.is_empty(), "and its flash");
        // The host picks it up: in the host's hand for the guest too.
        h.sim.hand_to(ActorId::Player, id);
        until(&mut [&mut h, &mut g], "the stone in Hal's hand", |gs| gs[1].sim.travelers.get(&0).is_some_and(|t| t.a.held == Some(id)));
        assert_eq!(g.sim.player.held, None, "not in the guest's");
        // The host's world gets a new building: the guest's is made anew with it.
        let (lh, _) = (h.sim.type_by_name("lighthouse").or_else(|| h.sim.type_by_name("stone")).unwrap().id, ());
        let placed = h.db.tx(|tx| {
            let v = crate::db::add_version(tx, None, "make", "a cairn", None)?;
            crate::db::add_instance(tx, lh as i64, v, at.x + 6.0, at.y, at.z, 0.0, 1.0, &[0.5; 8])
        }).unwrap();
        until(&mut [&mut h, &mut g], "the guest's world to have the cairn", |gs| {
            gs[1].snap.instances.iter().any(|p| p.id == placed)
        });
        // The host says something to the world: the guest hears it, the host by name.
        h.sim.notes.push(Note::Ambient("Mara waves at the traveler.".into()));
        h.sim.notes.push(Note::Notable("You feel watched.".into()));
        until(&mut [&mut h, &mut g], "the guest to hear it", |gs| gs[1].log.iter().any(|l| l.text.contains("Mara waves at Hal")));
        assert!(!g.log.iter().any(|l| l.text.contains("You feel watched")), "what is said to the host alone stays theirs");
        // The guest picks up a stone by them: done in the host's world.
        let gp = h.sim.travelers[&1].a.pos;
        let mine = h.sim.spawn_thing(stone, gp + Vec3::new(0.6, 0.3, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
        until(&mut [&mut h, &mut g], "the guest to see the stone by them", |gs| gs[1].sim.things.get(mine).is_some());
        g.player_act(crate::sim::Action::Hold { target: crate::sim::Target::Thing(mine) });
        until(&mut [&mut h, &mut g], "the stone in the guest's hand", |gs| gs[1].sim.player.held == Some(mine));
        assert_eq!(h.sim.travelers[&1].a.held, Some(mine), "the host's world says so");
        assert_eq!(h.sim.things.get(mine).and_then(|t| t.holder), Some(ActorId::Traveler(1)));
        // Hurt on the host, the guest knows it.
        h.sim.wound(ActorId::Traveler(1), 0.3, ActorId::Npc(mara), "pushed");
        until(&mut [&mut h, &mut g], "the guest to feel it", |gs| (gs[1].sim.health() - 0.7).abs() < 0.01);
        g.shutdown();
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn others_are_drawn_between_what_was_heard() {
        let t0 = Instant::now();
        let mut q = VecDeque::new();
        q.push_back((t0, Body { pos: [0.0, 0.0, 0.0], ..Default::default() }));
        q.push_back((t0 + Duration::from_millis(100), Body { pos: [10.0, 0.0, 0.0], ..Default::default() }));
        assert!((between(&q, t0 + Duration::from_millis(50)).pos[0] - 5.0).abs() < 0.01);
        assert_eq!(between(&q, t0 + Duration::from_secs(1)).pos[0], 10.0);
        assert_eq!(between(&q, t0.checked_sub(Duration::from_secs(1)).unwrap_or(t0)).pos[0], 0.0);
    }

    #[test]
    fn a_guest_talks_on_what_the_host_lets_them_spend() {
        let port = 47000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        let (mut h, mut g, path) = pair_with("talk", port, true, &|_, _| {});
        until(&mut [&mut h, &mut g], "the guest to be in", |gs| gs[1].sim.travelers.contains_key(&0));
        let mara = h.sim.cast.npcs[0].def.id;
        // Bea goes up to Mara.
        let by = g.sim.cast.get(mara).unwrap().a.pos + Vec3::new(1.5, 0.0, 0.0);
        g.sim.player.pos = by;
        h.sim.travelers.get_mut(&1).unwrap().a.pos = by;
        h.sim.cast.get_mut(mara).unwrap().think_at = f64::MAX;
        g.set_mode(super::super::Mode::Talk(mara));
        g.sim.talking_to = Some(mara);
        g.send_talk(mara, "Hello there".into());
        until(&mut [&mut h, &mut g], "Mara's answer to reach the guest", |gs| gs[1].log.iter().any(|l| l.text.contains("Welcome, stranger")) && gs[1].replying.is_none());
        assert!(h.log.iter().any(|l| l.speaker.as_deref() == Some("Bea") && l.text == "Hello there"), "the host heard Bea");
        assert!(h.log.iter().any(|l| l.text.contains("Welcome, stranger")), "and Mara's answer");
        assert!(h.conv.get(&mara).is_none_or(|c| c.is_empty()), "Bea's talk isn't the host's");
        assert_eq!(h.sim.travelers[&1].talking_to, Some(mara), "Mara knows who she talks with");
        let spent = h.llm.as_ref().unwrap().spent_for(1);
        assert!(spent > 0.0, "what it cost is counted as Bea's");
        // Past what the host lets a guest spend: told so, plainly.
        h.settings.guest_budget_usd = spent * 0.5;
        g.send_talk(mara, "And again?".into());
        until(&mut [&mut h, &mut g], "the guest to be told", |gs| gs[1].log.iter().any(|l| l.text.contains("limit for you")) && gs[1].replying.is_none());
        g.shutdown();
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_guest_drives_a_cart_from_their_own_game() {
        let port = 49000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        // A hand cart a few steps from the start.
        let place_cart = |db: &Arc<Db>, spawn: Vec3| {
            let src = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/vehicles/cart.js")).unwrap();
            let ct = crate::lang::compile(&src).unwrap();
            let rep = crate::lang::probe::probe(&ct).unwrap();
            let meta = serde_json::json!({ "name": ct.meta.name, "bounds": ct.meta.bounds, "tags": ct.meta.tags, "bottom": rep.bottom, "top": rep.top }).to_string();
            db.tx(|tx| {
                let v = crate::db::add_version(tx, None, "make", "a cart", None)?;
                let t = crate::db::add_type(tx, Some(v), &ct.meta.name, &src, &meta, "ok", "")?;
                crate::db::add_instance(tx, t, v, spawn.x - 6.0, spawn.y - rep.bottom, spawn.z - 6.0, 0.0, 1.0, &[0.5; 8])?;
                Ok(())
            })
            .unwrap();
        };
        let (mut h, mut g, path) = pair_with("cart", port, false, &place_cart);
        until(&mut [&mut h, &mut g], "the guest to be in", |gs| gs[1].sim.travelers.contains_key(&0));
        let cart = h.snap.instances.iter().find(|p| h.snap.type_of(p.type_id).is_some_and(|t| t.name() == "hand cart")).cloned().expect("the cart");
        // Bea goes to it and gets in.
        let by = cart.pos + Vec3::new(1.5, 0.0, 0.0);
        g.sim.player.pos = by;
        h.sim.travelers.get_mut(&1).unwrap().a.pos = by;
        g.player_act(crate::sim::Action::Ride { target: crate::sim::Target::Instance(cart.id) });
        until(&mut [&mut h, &mut g], "the guest to sit in the cart", |gs| gs[1].sim.player.aboard.is_some());
        let id = h.sim.travelers[&1].a.aboard.expect("in the cart on the host");
        assert_eq!(g.sim.player.aboard, Some(id));
        let p0 = h.sim.things.get(id).unwrap().pos;
        // She pushes on: the cart goes in the host's world, and she with it in hers.
        g.keys.down.insert('w');
        until(&mut [&mut h, &mut g], "the cart to go", |gs| gs[0].sim.things.get(id).is_some_and(|t| (t.pos - p0).length() > 1.0));
        let at = h.sim.things.get(id).unwrap().pos;
        until(&mut [&mut h, &mut g], "the guest to be carried along", |gs| (gs[1].sim.player.pos - at).length() < 3.0 && (gs[1].sim.player.pos - by).length() > 0.8);
        g.shutdown();
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_guest_hears_what_happens_near_them_however_far_the_host_is() {
        let port = 51000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        let (mut h, mut g, path) = pair("near", port);
        until(&mut [&mut h, &mut g], "the guest to be in", |gs| gs[1].sim.travelers.contains_key(&0));
        // Bea wanders far off.
        let far = h.sim.player.pos + Vec3::new(150.0, 0.0, 0.0);
        let far = Vec3::new(far.x, h.snap.terrain.height(far.x, far.z), far.z);
        g.sim.player.pos = far;
        h.sim.travelers.get_mut(&1).unwrap().a.pos = far;
        let home = h.sim.player.pos;
        h.sim.note_near(far, 20.0, Note::Ambient("Mara waves at Bea.".into()));
        h.sim.note_near(home, 20.0, Note::Ambient("A dog barks by the well.".into()));
        h.sim.note_near(far, 20.0, Note::Notable("The bridge gives way under Bea.".into()));
        until(&mut [&mut h, &mut g], "the guest to hear what happened by them", |gs| gs[1].log.iter().any(|l| l.text == "Mara waves at you."));
        assert!(g.log.iter().any(|l| l.text.contains("The bridge gives way under you")), "said to her, near");
        assert!(!g.log.iter().any(|l| l.text.contains("dog barks")), "what is only near the host stays the host's");
        assert!(h.log.iter().any(|l| l.text.contains("dog barks")), "the host hears it");
        assert!(!h.log.iter().any(|l| l.text.contains("Mara waves")), "the host doesn't hear what is far from them");
        assert!(h.log.iter().any(|l| l.text.contains("The bridge gives way under Bea")), "but is told of the bridge, far off");
        g.shutdown();
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_host_sends_a_guest_away() {
        let port = 53000 + (std::process::id() % 1000) as u16 * 7 % 9000;
        let (mut h, mut g, path) = pair("kick", port);
        until(&mut [&mut h, &mut g], "the guest to be in", |gs| gs[1].sim.travelers.contains_key(&0));
        h.send_away("nobody");
        assert!(h.log.iter().any(|l| l.text.contains("No guest called nobody (here: Bea)")));
        h.send_away("be");
        until(&mut [&mut h, &mut g], "Bea to be gone", |gs| !gs[0].sim.travelers.contains_key(&1) && gs[1].quit);
        assert_eq!(g.left_because.as_deref(), Some("Hal sent you away from this world."));
        assert!(g.to_worlds, "back to her worlds, told why");
        // She can't come back while this hosting lasts.
        let addr = match &h.together {
            Some(Together::Host(x)) => format!("127.0.0.1:{}", x.host.port),
            _ => unreachable!(),
        };
        let again = std::thread::spawn(move || crate::net::guest::join(&addr, "lime-otter-42", "Bea", "tok-bea", |_, _| {}).err().map(|e| e.to_string()));
        let t0 = Instant::now();
        while !again.is_finished() {
            assert!(t0.elapsed() < Duration::from_secs(15));
            h.tick(1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(5));
        }
        let why = again.join().unwrap().expect("turned away");
        assert!(why.contains("sent you away"), "{why}");
        g.shutdown();
        h.shutdown();
        let _ = std::fs::remove_file(&path);
    }
}

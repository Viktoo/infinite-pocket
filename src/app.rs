//! The main thread: input, simulation tick, compositing and terminal output.
//! Never blocks on rendering or generation: frames and events arrive on channels.

use crate::brain::{Brain, Cmd, Event};
use crate::db::{Db, PlayerRow};
use crate::model::LiveRef;
use crate::render::{self, Camera, Frame, FrameRequest, RenderHandle, RenderMsg, SceneParams, sky};
use crate::sim::actions::Action;
use crate::sim::pick::Picked;
use crate::sim::{ActorId, Sim, Target};
use journal::LogLine;
use crate::term::{self, Cell, Screen};
use crate::world::characters::TALK_RANGE;
use crate::world::collide::{Obstacles, PLAYER_RADIUS};
use crate::world::describe::describe;
use crate::world::{WorldSnapshot, cull, region_center, region_of};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};
use glam::Vec3;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

mod creations;
mod journal;
mod loading;
mod ears;
mod hud;
mod menu;
pub mod together;
mod voices;
mod work;

pub const WALK_SPEED: f32 = 5.0;
pub const TURN_SPEED: f32 = 1.9;
/// Radians per second while ↑/↓ is held.
const LOOK_SPEED: f32 = 1.2;
const PITCH_MIN: f32 = -0.95;
const PITCH_MAX: f32 = 0.6;
const FOV_Y: f32 = 1.05;
/// At most this many speech boxes over heads at once (the nearest speakers).
const BUBBLES: usize = 3;
/// A speech box stays at most this long.
const BUBBLE_MAX: Duration = Duration::from_secs(10);
const BUBBLE_W: usize = 39;
const BUBBLE_ROWS: usize = 3;
const DIM: [u8; 3] = [130, 134, 150];
const TEXT: [u8; 3] = [222, 224, 230];
const ACCENT: [u8; 3] = [255, 200, 110];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Walk,
    Talk(i64),
    /// After `/`: anything you do, in words (or a command like /undo).
    Command,
}


#[derive(Default)]
struct Keys {
    /// Keys currently held (pressed and not yet released).
    down: HashSet<char>,
}

/// Movement keys normalised to chars: arrows are ^ v < >.
/// No walking key held.
fn dirn_none(w: bool, s: bool, a: bool, d: bool) -> bool {
    !(w || s || a || d)
}

fn key_char(code: KeyCode) -> Option<char> {
    match code {
        KeyCode::Up => Some('^'),
        KeyCode::Down => Some('v'),
        KeyCode::Left => Some('<'),
        KeyCode::Right => Some('>'),
        KeyCode::Char(c) => Some(c.to_ascii_lowercase()),
        _ => None,
    }
}

/// Window mode: one text cell's size in the window's own pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WinGeom {
    pub cell_w: f32,
    pub cell_h: f32,
}

/// Where the app shows itself and hears its keys: a terminal or a window.
pub trait Frontend {
    /// Hand the app whatever input arrived (never blocks).
    fn input(&mut self, app: &mut App) -> std::io::Result<()>;
    /// Show the screen (and in a window, the world beneath it).
    fn present(&mut self, app: &mut App) -> std::io::Result<()>;
    /// Wait up to `d` for input or the next frame slot.
    fn wait(&mut self, d: Duration);
}

/// No window: the screen goes to a writer as terminal text (tests, scripts, benches).
pub struct Headless<'a> {
    pub out: &'a mut dyn Write,
}

impl Frontend for Headless<'_> {
    fn input(&mut self, _: &mut App) -> std::io::Result<()> {
        Ok(())
    }

    fn present(&mut self, app: &mut App) -> std::io::Result<()> {
        app.screen.flush(self.out)
    }

    fn wait(&mut self, d: Duration) {
        std::thread::sleep(d.min(Duration::from_millis(2)));
    }
}

pub struct Stats {
    frames: VecDeque<Instant>,
    pub max_loop_ms: f32,
    pub worst_recent: VecDeque<(Instant, f32)>,
    pub gpu_ms: f32,
    pub insts: usize,
    pub shown: u64,
    /// Longest time between two displayed frames.
    pub max_gap_ms: f32,
}

impl Stats {
    fn fps(&self) -> f32 {
        if self.frames.len() < 2 {
            return 0.0;
        }
        let dt = self.frames.back().unwrap().duration_since(*self.frames.front().unwrap()).as_secs_f32();
        if dt <= 0.0 { 0.0 } else { (self.frames.len() - 1) as f32 / dt }
    }
    fn worst(&self) -> f32 {
        self.worst_recent.iter().map(|x| x.1).fold(0.0, f32::max)
    }
}

pub struct App {
    pub db: Arc<Db>,
    brain: Brain,
    events: crossbeam_channel::Receiver<Event>,
    render: RenderHandle,
    pub snap: Arc<WorldSnapshot>,
    /// The living world: actors, live things, rules, minds.
    pub sim: Sim,
    live: LiveRef,
    pitch: f32,
    cam_y: f32,
    mode: Mode,
    input: String,
    /// Chars of the input right of the caret (0: at the end, where dictation adds).
    caret_back: usize,
    log: VecDeque<LogLine>,
    keys: Keys,
    debug: bool,
    pub screen: Screen,
    last_frame: Option<Frame>,
    /// When the traveler was last seen hurt (the screen flashes red), and
    /// the wounds then.
    hurt_flash: Option<Instant>,
    seen_wounds: f32,
    /// When the traveler was seen to fall (the view drops and tips over).
    fell_at: Option<Instant>,
    outstanding: u32,
    next_frame_id: u64,
    last_request: Instant,
    building: i32,
    appear: HashMap<i64, Instant>,
    regions_inflight: HashSet<(i32, i32)>,
    regions_failed: HashSet<(i32, i32)>,
    conv: HashMap<i64, Vec<(bool, String)>>,
    replying: Option<i64>,
    /// Replies to the sim's own talk requests, being streamed.
    sim_talk: HashMap<i64, String>,
    pub stats: Stats,
    start: Instant,
    last_save: Instant,
    last_char_save: Instant,
    last_region_check: Instant,
    talk_hint: Option<(i64, String)>,
    quit: bool,
    pub noclip: bool,
    /// Running: Shift with the walking keys, or `r` to keep it on.
    shift_run: bool,
    always_run: bool,
    llm_note: Option<String>,
    budget_paused: bool,
    /// Saved settings (budget, frame rate, shadows, how far the world loads).
    settings: crate::settings::Settings,
    /// The settings screen, when open.
    menu: Option<menu::Menu>,
    /// The controls of the menus over the world (mouse and keys).
    pub ui: crate::ui::Ui,
    /// The journal is open (key 1): 0 shut, 1 open.
    log_view: u8,
    /// Log lines scrolled back from the newest (PgUp / PgDn).
    log_scroll: usize,
    /// Which lines the journal shows (Tab cycles).
    journal_filter: journal::Filter,
    /// Wheel steps not yet a whole line of the journal.
    wheel_rows: f32,
    /// When the next everyday line leaves the small log.
    log_expiry: Option<Instant>,
    dirty: bool,
    llm: Option<Arc<crate::llm::Llm>>,
    /// Region plans wait until the universe's look (and so its terrain) exists.
    genesis_pending: bool,
    /// F2: the raw truth about what you point at.
    inspect: bool,
    /// What the middle of the view points at.
    pub pointed: Option<Picked>,
    /// The pointed thing is loose with no use of its own (e picks it up).
    pointed_loose: bool,
    /// Whether g can take hold of what is pointed at (Some(true): alone).
    pointed_grab: Option<bool>,
    last_pick: Instant,
    /// The loading screen, until the world around you is made.
    loading: Option<loading::Loading>,
    /// Maple Cove behind the loading screen, kept on from the title (see
    /// `panorama`); let go once the world is stepped into.
    pub backdrop: Option<crate::panorama::Panorama>,
    /// The world opened on a quick first picture (land and plants only);
    /// the loading screen holds until the whole of it, buildings and all,
    /// has come.
    pub full_pending: bool,
    /// What this world has seen the player do (achievements).
    achievements: crate::achievements::Tracker,
    /// Achievement popups waiting, the first one showing since when.
    toasts: VecDeque<(&'static crate::achievements::Def, Option<Instant>)>,
    /// The "working on" box: slow work in progress, and just done.
    work: work::WorkBox,
    /// What the traveler is doing now (driving, holding…) and since when,
    /// and whether its key tips show this time.
    state_since: Option<(&'static str, Instant)>,
    state_tips: bool,
    /// Walked this session (until then, how to move shows).
    moved: bool,
    /// The sound device, while sound is on and there is one.
    sound: Option<crate::audio::Output>,
    /// What the world sounds like, read off the sim each frame.
    foley: crate::audio::foley::Foley,
    sound_cmds: Vec<crate::audio::mix::Cmd>,
    /// Characters' spoken voices (with sound, when on).
    voice: Option<crate::voice::Speaker>,
    voice_recipes: HashMap<i64, std::sync::Arc<crate::voice::recipe::Recipe>>,
    /// When each speaker's queued words end.
    voice_busy: HashMap<i64, Instant>,
    /// A streaming reply's words not yet spoken.
    voice_said: HashMap<i64, String>,
    /// Each speaker's lines as they play (start, loudness every 20 ms): their
    /// mouth follows it.
    voice_mouth: HashMap<i64, Vec<(Instant, Vec<f32>)>>,
    /// The voice thread's last state told in the log.
    voice_told: Option<u8>,
    /// Speak to talk: the recogniser (while the setting is on), the go of
    /// listening now, and the last trouble told.
    ears: Option<crate::ears::Ears>,
    hearing: Option<ears::Hearing>,
    /// Words just heard wait in the input line for Enter (or editing).
    heard: bool,
    hear_id: u64,
    ears_told: Option<u8>,
    ears_trouble: Option<String>,
    /// Shown in a window rather than a terminal.
    pub win: Option<WinGeom>,
    /// Left for the list of worlds (not quit).
    pub to_worlds: bool,
    /// The world's file (to host it from the pause menu).
    pub path: Option<std::path::PathBuf>,
    /// Window detail on auto: the lines drawn now, and when they last changed.
    auto_lines: f32,
    auto_at: Instant,
    /// Where auto detail last fell behind: it stays below that a while.
    auto_ceiling: Option<(f32, Instant)>,
    /// Playing together: hosting this world, or a guest in someone's.
    pub together: Option<together::Together>,
    /// Why a game together ended (shown on the worlds list).
    pub left_because: Option<String>,
}

pub struct Setup {
    pub db: Arc<Db>,
    pub brain: Brain,
    pub events: crossbeam_channel::Receiver<Event>,
    pub render: RenderHandle,
    pub snap: Arc<WorldSnapshot>,
    pub live: LiveRef,
    pub truecolor: bool,
    pub size: (u16, u16),
    pub llm: Option<Arc<crate::llm::Llm>>,
    pub genesis: bool,
    /// Hosting, or a guest (whose world this is, held only in memory).
    pub together: Option<together::Together>,
    /// The world's seed for this session (a guest takes the host's, so
    /// people look the same to both).
    pub seed: Option<u64>,
}

impl App {
    pub fn new(s: Setup) -> App {
        let player = s.db.player();
        let (pos, yaw, t_game) = match player {
            Some(p) => (crate::model::afloat(&s.snap.terrain, Vec3::new(p.x, 0.0, p.z)), p.yaw, p.t_game),
            // A first arrival: when and facing where the world says, if it does.
            None => {
                let num = |k: &str| s.db.kv_get(k).and_then(|v| v.parse::<f64>().ok());
                let t = num("start.hour").map_or(0.33, |h| (h / 24.0).rem_euclid(1.0)) * sky::DAY_SECONDS;
                (s.snap.spawn, num("start.yaw").unwrap_or(0.0) as f32, t)
            }
        };
        let seed = s.seed.unwrap_or_else(|| s.db.universe().map(|u| u.seed as u64).unwrap_or(1) ^ (crate::db::now() as u64).rotate_left(17));
        let mut sim = Sim::new(s.db.clone(), s.snap.clone(), pos, yaw, t_game, seed);
        sim.has_llm = s.brain.has_llm;
        let achievements = crate::achievements::Tracker::load(&s.db, &sim);
        let eye = sim.player.dims.eye;
        let mut app = App {
            db: s.db,
            brain: s.brain,
            events: s.events,
            render: s.render,
            snap: s.snap,
            sim,
            live: s.live,
            pitch: -0.06,
            cam_y: pos.y + eye,
            mode: Mode::Walk,
            input: String::new(),
            caret_back: 0,
            log: VecDeque::new(),
            keys: Keys::default(),
            debug: false,
            screen: Screen::new(s.size.0, s.size.1, s.truecolor),
            last_frame: None,
            hurt_flash: None,
            seen_wounds: 0.0,
            fell_at: None,
            outstanding: 0,
            next_frame_id: 1,
            last_request: Instant::now(),
            building: 0,
            appear: HashMap::new(),
            regions_inflight: HashSet::new(),
            regions_failed: HashSet::new(),
            conv: HashMap::new(),
            replying: None,
            sim_talk: HashMap::new(),
            stats: Stats { frames: VecDeque::new(), max_loop_ms: 0.0, worst_recent: VecDeque::new(), gpu_ms: 0.0, insts: 0, shown: 0, max_gap_ms: 0.0 },
            start: Instant::now(),
            last_save: Instant::now(),
            last_char_save: Instant::now(),
            last_region_check: Instant::now() - Duration::from_secs(5),
            talk_hint: None,
            quit: false,
            noclip: false,
            shift_run: false,
            always_run: false,
            llm_note: None,
            budget_paused: false,
            settings: crate::settings::Settings::load(),
            menu: None,
            ui: crate::ui::Ui::new(),
            log_view: 0,
            log_scroll: 0,
            journal_filter: journal::Filter::default(),
            wheel_rows: 0.0,
            log_expiry: None,
            dirty: true,
            llm: s.llm.clone(),
            genesis_pending: false,
            inspect: false,
            pointed: None,
            pointed_loose: false,
            pointed_grab: None,
            last_pick: Instant::now(),
            loading: None,
            backdrop: None,
            full_pending: false,
            achievements,
            toasts: VecDeque::new(),
            work: work::WorkBox::default(),
            state_since: None,
            state_tips: false,
            moved: false,
            sound: None,
            foley: Default::default(),
            sound_cmds: Vec::new(),
            voice: None,
            voice_recipes: HashMap::new(),
            voice_busy: HashMap::new(),
            voice_said: HashMap::new(),
            voice_mouth: HashMap::new(),
            voice_told: None,
            ears: None,
            hearing: None,
            heard: false,
            hear_id: 0,
            ears_told: None,
            ears_trouble: None,
            win: None,
            to_worlds: false,
            path: None,
            auto_lines: 240.0,
            auto_at: Instant::now(),
            auto_ceiling: None,
            together: s.together,
            left_because: None,
        };
        app.sync_sound();
        app.sync_ears();
        app.unstick();
        let name = app.snap.look.name.clone();
        app.say(None, &format!("Welcome{}.", if name.is_empty() { String::new() } else { format!(" to {name}") }), DIM);
        match s.llm.as_ref().map(|l| l.describe()) {
            Some(d) => crate::log::info(format!("LLM: {d}")),
            None if app.is_guest() => {}
            None => {
                app.llm_note = Some("no AI key: Esc → Settings → AI".into());
                app.say(None, "No AI key yet: the land is here, but its people don't talk. Add one in Esc → Settings → AI.", DIM);
            }
        }
        if app.render.cpu_fallback {
            app.say(None, "No GPU adapter found: using the slower CPU renderer at reduced resolution.", DIM);
        }
        if s.genesis && app.brain.has_llm {
            app.genesis_pending = true;
            app.say(None, "The world is taking shape around you…", DIM);
            app.brain.send(Cmd::Genesis);
        }
        app
    }

    /// Nobody starts a frame inside a solid: the player or a character caught
    /// inside one (an old save, a building appearing) moves to the nearest free spot.
    fn unstick(&mut self) {
        use crate::world::collide::{NPC_RADIUS, free_spot};
        let snap = self.snap.clone();
        let p = self.sim.player.pos;
        let solids = self.sim.solids_near(p, 6.0);
        if !solids.is_empty() {
            let obs = Obstacles { solids: &solids, bodies: &[] };
            // Stuck where its feet are (a floor, a roof), not at the ground below.
            let cap = self.sim.capsule(ActorId::Player);
            if obs.dist_body(p.x, p.z, p.y, cap) < PLAYER_RADIUS * 0.5 && !obs.clear(p.x, p.z, p.y, cap) {
                self.sim.player.pos = free_spot(&snap.terrain, &obs, p, PLAYER_RADIUS, 40.0, true);
                self.cam_y = self.sim.player.pos.y + self.sim.player.dims.eye;
            }
        }
        for i in 0..self.sim.cast.npcs.len() {
            let p = self.sim.cast.npcs[i].a.pos;
            let solids = self.sim.solids_near(p, 6.0);
            if solids.is_empty() {
                continue;
            }
            let obs = Obstacles { solids: &solids, bodies: &[] };
            let cap = self.sim.capsule(ActorId::Npc(self.sim.cast.npcs[i].def.id));
            if obs.dist_body(p.x, p.z, p.y, cap) < NPC_RADIUS * 0.5 && !obs.clear(p.x, p.z, p.y, cap) {
                self.sim.cast.npcs[i].a.pos = free_spot(&snap.terrain, &obs, p, NPC_RADIUS, 40.0, cap.swims);
            }
        }
    }

    pub fn pos(&self) -> Vec3 {
        self.sim.player.pos
    }

    pub fn yaw(&self) -> f32 {
        self.sim.player.yaw
    }

    fn held(&self, c: char) -> bool {
        self.keys.down.contains(&c)
    }

    fn camera(&self) -> Camera {
        let p = self.sim.player.pos;
        let cam = Camera { pos: Vec3::new(p.x, self.cam_y, p.z), yaw: self.sim.player.yaw, pitch: self.pitch, fov_y: FOV_Y, roll: 0.0 };
        if !self.sim.fallen() {
            return cam;
        }
        // Fallen: the eyes drop to the ground and the view tips onto its side.
        let s = self.fell_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(10.0);
        let k = (s / FALL_SECS).clamp(0.0, 1.0);
        let k = k * k * (3.0 - 2.0 * k);
        // A small bounce as the head meets the ground.
        let bump = if s > FALL_SECS { (-(s - FALL_SECS) * 9.0).exp() * ((s - FALL_SECS) * 18.0).sin() * 0.04 } else { 0.0 };
        let ground = self.snap.terrain.height(p.x, p.z).max(self.snap.terrain.water(p.x, p.z) - 0.2) + 0.22;
        Camera {
            pos: Vec3::new(p.x, cam.pos.y + (ground - cam.pos.y) * k + bump, p.z),
            pitch: cam.pitch + (0.04 - cam.pitch) * k,
            roll: 1.4 * k,
            ..cam
        }
    }

    /// Viewport size in cells: the whole window (what floats over it
    /// takes no rows of its own).
    fn layout(&self) -> (u16, u16, u16) {
        (self.screen.w, self.screen.h, 0)
    }

    fn pixel_size(&self) -> (u32, u32, f32) {
        let (w, vh, _) = self.layout();
        if let Some(g) = self.win {
            // A window: square pixels, as many lines as the detail setting allows.
            let (fw, fh) = (w as f32 * g.cell_w, vh as f32 * g.cell_h);
            let lines = match self.settings.window_detail {
                0 => fh,
                crate::settings::AUTO => self.auto_lines.min(fh),
                l => (l as f32).min(fh),
            };
            let k = lines / fh.max(1.0);
            let (mut pw, mut ph) = ((fw * k).round() as u32, (fh * k).round() as u32);
            if self.render.cpu_fallback {
                pw = pw.div_ceil(4);
                ph = ph.div_ceil(4);
            }
            return (pw.max(8), ph.max(8), 1.0);
        }
        let (mut pw, mut ph, aspect) = (w as u32 * 2, vh as u32 * 2, 0.5);
        if self.render.cpu_fallback {
            pw = pw.div_ceil(2);
            ph = ph.div_ceil(2);
        }
        (pw.max(8), ph.max(8), aspect)
    }

    // ------------------------------------------------------------------ input

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && matches!(k.code, KeyCode::Char('c') | KeyCode::Char('C')) {
            if k.kind != KeyEventKind::Release {
                self.quit = true;
            }
            return;
        }
        // Shift alone (the window reports it): held runs, let go walks.
        if let KeyCode::Modifier(ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift) = k.code {
            self.shift_run = k.kind != KeyEventKind::Release;
            return;
        }
        if k.kind == KeyEventKind::Release {
            if let Some(c) = key_char(k.code) {
                self.keys.down.remove(&c);
            }
            return;
        }
        if self.loading.is_some() {
            return self.loading_key(k);
        }
        // Fallen: only waking (and the menu) answer.
        if self.sim.fallen() && !matches!(k.code, KeyCode::Esc | KeyCode::F(10)) {
            if k.code == KeyCode::Enter && k.kind == KeyEventKind::Press && self.fell_at.is_some_and(|t| t.elapsed().as_secs_f32() > FALL_SECS) {
                self.wake();
            }
            return;
        }
        if k.code == KeyCode::F(10) {
            if k.kind == KeyEventKind::Press {
                self.toggle_menu();
            }
            return;
        }
        if k.code == KeyCode::F(1) {
            if k.kind == KeyEventKind::Press {
                self.debug = !self.debug;
                self.dirty = true;
            }
            return;
        }
        if k.code == KeyCode::F(5) && self.together.is_some() {
            if k.kind == KeyEventKind::Press {
                self.copy_invite();
            }
            return;
        }
        if k.code == KeyCode::F(2) {
            if k.kind == KeyEventKind::Press {
                self.inspect = !self.inspect;
                self.dirty = true;
            }
            return;
        }
        if k.code == KeyCode::F(3) && self.menu.is_none() {
            if k.kind == KeyEventKind::Press {
                self.open_menu(true);
            }
            return;
        }
        if self.menu.is_some() {
            if k.kind == KeyEventKind::Press {
                self.menu_key(k);
            }
            return;
        }
        if matches!(k.code, KeyCode::PageUp | KeyCode::PageDown) {
            let page = self.journal_rows().saturating_sub(1).max(1);
            self.log_scroll = if k.code == KeyCode::PageUp { self.log_scroll + page } else { self.log_scroll.saturating_sub(page) };
            self.dirty = true;
            return;
        }
        match self.mode {
            Mode::Walk => self.walk_key(k),
            Mode::Talk(_) | Mode::Command => self.text_key(k),
        }
    }

    fn walk_key(&mut self, k: KeyEvent) {
        // Cmd comes through only for editing the line.
        if k.modifiers.contains(KeyModifiers::SUPER) && k.kind != KeyEventKind::Release {
            return;
        }
        if let Some(c) = key_char(k.code) {
            // Aboard or swimming, Space and c are held too: up and down for
            // a flyer or a swimmer.
            let flying_key = matches!(c, ' ' | 'c') && (self.sim.player.aboard.is_some() || self.sim.player.swimming);
            if matches!(c, '^' | 'v' | '<' | '>' | 'a' | 'd' | 'w' | 's') || flying_key {
                // Shift with a walking key runs (`r` keeps running on); a
                // repeat says again whether Shift is still down.
                if matches!(c, 'a' | 'd' | 'w' | 's') {
                    let shift = k.modifiers.contains(KeyModifiers::SHIFT) || matches!(k.code, KeyCode::Char(ch) if ch.is_ascii_uppercase());
                    self.shift_run = shift;
                    // Walked: how to move has been learnt, this time.
                    if !self.moved {
                        self.moved = true;
                        if self.tip_ok("move") {
                            self.tip_shown("move");
                        }
                    }
                }
                self.keys.down.insert(c);
                return;
            }
        }
        if k.kind == KeyEventKind::Repeat {
            return;
        }
        match k.code {
            KeyCode::Esc if self.log_view > 0 => self.set_log_view(0),
            KeyCode::Esc => self.toggle_menu(),
            KeyCode::Tab if self.log_view > 0 => {
                self.journal_filter = self.journal_filter.next();
                self.log_scroll = 0;
                self.dirty = true;
            }
            KeyCode::Char('/') => {
                self.set_mode(Mode::Command);
                self.input = "/".into();
                self.start_hearing();
            }
            KeyCode::Char(' ') => {
                if !self.sim.player.seated() {
                    self.sim.jump(ActorId::Player);
                }
            }
            KeyCode::Char('c') | KeyCode::Char('C') => {
                let down = self.sim.crouch(ActorId::Player, None);
                self.say(None, if down { "You crouch down (c to stand)." } else { "You stand up." }, DIM);
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.always_run = !self.always_run;
                self.say(None, if self.always_run { "Running (r to walk)." } else { "Walking." }, DIM);
            }
            KeyCode::Char('e') | KeyCode::Char('E') => self.use_pointed(),
            KeyCode::Char('g') | KeyCode::Char('G') => self.grab_or_drop(),
            KeyCode::Char('f') | KeyCode::Char('F') => self.throw_held(9.0),
            KeyCode::Char('y') | KeyCode::Char('Y') => self.player_act(Action::Answer { yes: true, to: None }),
            KeyCode::Char('n') | KeyCode::Char('N') => self.player_act(Action::Answer { yes: false, to: None }),
            KeyCode::Enter => {
                if let Some((id, name)) = self.talk_hint.clone() {
                    // Animals answer with a noise and their body, no words needed.
                    if !self.sim.speaks(ActorId::Npc(id)) {
                        self.say(Some("you"), &format!("(you call {name})"), DIM);
                        self.sim.animal_answers(id);
                        return;
                    }
                    if !self.brain.has_llm && !self.is_guest() {
                        self.say(None, &format!("{name} looks at you, but words need a Claude API key. Add one here, then open this world again."), DIM);
                        self.open_ai_settings();
                        return;
                    }
                    self.set_mode(Mode::Talk(id));
                    self.sim.talking_to = Some(id);
                    self.say(None, &format!("You approach {name}. (Esc to leave)"), DIM);
                    self.start_hearing();
                }
            }
            KeyCode::Char('1') => self.set_log_view(if self.log_view > 0 { 0 } else { 1 }),
            _ => {}
        }
    }

    /// Act as the player; say how it went.
    fn player_act(&mut self, a: Action) {
        let verb = a.verb();
        self.sim.look_pitch = self.pitch;
        // A guest's deeds are done in the host's world.
        if let Some(together::Together::Guest(g)) = &self.together {
            g.act(together::action_json(&a));
            self.dirty = true;
            return;
        }
        match self.sim.act(ActorId::Player, a) {
            Ok(o) => {
                // Things picked up show in the hand; a being in the arms is told.
                let told = matches!(verb, "use" | "do" | "eat" | "answer" | "give" | "gesture" | "propose" | "pet" | "ride" | "dismount") || (matches!(verb, "hold" | "drop") && o.thing.is_none());
                if !o.ok || told {
                    self.say(None, &you(&o.msg), if o.ok { TEXT } else { DIM });
                }
            }
            Err(e) => self.say(None, &format!("Can't: {}.", you(&e.to_string())), DIM),
        }
        self.dirty = true;
    }

    /// The target the player points at (the middle of the view).
    fn pointed_target(&self) -> Option<Target> {
        self.pointed.as_ref().map(|p| p.target.clone())
    }

    /// The pointed target, if it is a thing (not ground or a far point).
    fn pointed_thing(&self) -> Option<Target> {
        match self.pointed_target() {
            Some(Target::Point(_)) | None => None,
            t => t,
        }
    }

    /// Where on the pointed thing the view lands (world), if it is a thing.
    fn pointed_at(&self) -> Option<[f32; 3]> {
        self.pointed.as_ref().filter(|p| !matches!(p.target, Target::Point(_))).map(|p| p.pos.to_array())
    }

    /// The being the use key is meant for: the one pointed at, else the
    /// one in front close by (small animals are hard to point at), unless
    /// a thing within reach is pointed at.
    fn being_in_front(&self) -> Option<ActorId> {
        if let Some(Target::Actor(a)) = self.pointed_thing() {
            return Some(a);
        }
        let near_thing = self.pointed.as_ref().is_some_and(|p| p.dist < 3.2 && !matches!(p.target, Target::Point(_)));
        if near_thing {
            return None;
        }
        let (id, _) = self.talk_hint.as_ref()?;
        let me = self.pos();
        let a = self.sim.cast.get(*id)?;
        (Vec3::new(a.a.pos.x - me.x, 0.0, a.a.pos.z - me.z).length() < 4.0).then_some(ActorId::Npc(*id))
    }

    fn use_pointed(&mut self) {
        // Riding or driving: e with nothing else in view (or at the mount
        // or vehicle itself) gets down.
        let mount = self.sim.player.riding.map(Target::Actor).or(self.sim.player.aboard.map(Target::Thing));
        if let Some(m) = mount {
            let pointed = self.pointed_thing().filter(|_| self.pointed.as_ref().is_some_and(|p| p.dist < 3.2));
            if pointed.is_none() || pointed == Some(m) {
                self.player_act(Action::Dismount);
                return;
            }
        }
        // A being in front: pet, ride, greet, feed or give, by what it is.
        if let Some(other) = self.being_in_front() {
            if let Some(carried) = self.sim.carried_of(ActorId::Player).filter(|c| *c != other) {
                // Arms full: the one in them is the one you mean.
                self.player_act(Action::Pet { target: Target::Actor(carried) });
                return;
            }
            let at = self.pointed_at();
            match self.sim.approach_action(ActorId::Player, other, at) {
                Some(a) => self.player_act(a),
                None => {
                    if let ActorId::Npc(id) = other {
                        let name = self.sim.actor_name(other);
                        self.say(Some("you"), &format!("(you call {name})"), DIM);
                        self.sim.animal_answers(id);
                    }
                }
            }
            return;
        }
        if let Some(c) = self.sim.carried_of(ActorId::Player) {
            self.player_act(Action::Pet { target: Target::Actor(c) });
            return;
        }
        let target = self.pointed_thing();
        let held = self.sim.player.held;
        let at = self.pointed_at();
        // Something that goes off, in hand: e lights it, whatever is in view.
        if let Some(h) = held.filter(|h| self.sim.burst_of(*h).is_some() && self.sim.things.get(*h).is_some_and(|t| t.shape.fuse_at.is_none())) {
            self.player_act(Action::Use { target: Some(Target::Thing(h)), on: None, at: None });
            return;
        }
        let a = match (held, target) {
            (Some(_), Some(t)) => Action::Use { target: None, on: Some(t), at },
            (Some(_), None) => {
                // A tool at the ground in front: a spade digs there.
                let ground = self.pointed.as_ref().filter(|p| matches!(p.target, Target::Point(_)) && p.dist < 7.0).map(|p| (p.target.clone(), p.pos.to_array()));
                match ground {
                    Some((t, p)) if self.sim.held_tool(ActorId::Player).is_some() => Action::Use { target: None, on: Some(t), at: Some(p) },
                    _ => Action::Use { target: None, on: None, at: None },
                }
            }
            // Empty hands at a loose thing with no use of its own: pick it up.
            (None, Some(t)) if self.pointed_loose => Action::Hold { target: t },
            (None, Some(t)) => Action::Use { target: Some(t), on: None, at },
            (None, None) => {
                self.say(None, "Nothing to use there. (Point at something; e or g picks things up.)", DIM);
                return;
            }
        };
        self.player_act(a);
    }

    fn grab_or_drop(&mut self) {
        if self.sim.player.held.is_some() || self.sim.carried_of(ActorId::Player).is_some() {
            self.player_act(Action::Drop);
            return;
        }
        let pointed = self.pointed_thing().filter(|_| self.pointed.as_ref().is_some_and(|p| p.dist < crate::sim::actor::REACH + 1.0));
        let animal = |a: &ActorId| self.sim.mind_of(*a) != crate::world::species::Mind::Sapient && self.sim.within_touch(ActorId::Player, *a);
        let pointed = pointed.or_else(|| self.being_in_front().filter(animal).map(Target::Actor));
        match pointed.or_else(|| self.sim.nearest_in_front(ActorId::Player)) {
            Some(t) => self.player_act(Action::Hold { target: t }),
            None => self.say(None, "Nothing to pick up within reach.", DIM),
        }
    }

    fn throw_held(&mut self, force: f32) {
        if let Some(c) = self.sim.carried_of(ActorId::Player) {
            let name = self.sim.actor_name(c);
            self.say(None, &format!("You won't throw {name}. (g sets them down)"), DIM);
            return;
        }
        if self.sim.player.held.is_none() {
            self.say(None, "You aren't holding anything to throw.", DIM);
            return;
        }
        let at = match self.pointed_thing() {
            Some(t) => Some(t),
            None => None,
        };
        let ground = self.pointed.as_ref().filter(|p| p.dist < 30.0 && matches!(p.target, Target::Point(_))).map(|p| p.pos);
        let a = match (at, ground) {
            (Some(t), _) if self.pointed.as_ref().is_some_and(|p| p.dist < 30.0) => Action::Throw { at: Some(t), dir: None, force: Some(force) },
            // Pointing at the ground: lob it there.
            (None, Some(p)) => Action::Throw { at: Some(Target::Point(p.to_array())), dir: None, force: Some(force) },
            _ => {
                let cam = self.camera();
                let d = cam.forward() + Vec3::Y * 0.25;
                Action::Throw { at: None, dir: Some(d.to_array()), force: Some(force) }
            }
        };
        self.player_act(a);
    }

    fn set_mode(&mut self, m: Mode) {
        self.stop_hearing();
        if !matches!(m, Mode::Talk(_)) {
            self.sim.talking_to = None;
        }
        self.mode = m;
        self.keys.down.clear();
        self.input.clear();
        self.caret_back = 0;
        self.dirty = true;
    }

    /// The caret as a char index into the input; never before a command's '/'.
    fn caret(&self) -> usize {
        let n = self.input.chars().count();
        let min = (self.mode == Mode::Command && self.input.starts_with('/')) as usize;
        n.saturating_sub(self.caret_back).max(min)
    }

    fn set_caret(&mut self, at: usize) {
        let n = self.input.chars().count();
        let min = (self.mode == Mode::Command && self.input.starts_with('/')) as usize;
        self.caret_back = n - at.clamp(min.min(n), n);
    }

    fn byte_at(&self, at: usize) -> usize {
        self.input.char_indices().nth(at).map_or(self.input.len(), |(i, _)| i)
    }

    /// The start of the word before the caret, or the end of the one after.
    fn word_edge(&self, back: bool) -> usize {
        let cs: Vec<char> = self.input.chars().collect();
        let mut i = self.caret();
        if back {
            while i > 0 && cs[i - 1] == ' ' {
                i -= 1;
            }
            while i > 0 && cs[i - 1] != ' ' {
                i -= 1;
            }
        } else {
            while i < cs.len() && cs[i] == ' ' {
                i += 1;
            }
            while i < cs.len() && cs[i] != ' ' {
                i += 1;
            }
        }
        i
    }

    fn insert_input(&mut self, text: &str) {
        let room = 400usize.saturating_sub(self.input.chars().count());
        let text: String = text.chars().take(room).collect();
        let at = self.byte_at(self.caret());
        self.input.insert_str(at, &text);
    }

    fn text_key(&mut self, k: KeyEvent) {
        if self.hearing.is_some() && k.kind == KeyEventKind::Repeat {
            return;
        }
        if self.hearing_key(k.code) {
            self.dirty = true;
            return;
        }
        self.heard = false;
        match k.code {
            KeyCode::Esc => self.set_mode(Mode::Walk),
            // Enter with nothing to say, in a conversation: speak instead.
            KeyCode::Enter if matches!(self.mode, Mode::Talk(_)) && self.input.trim().is_empty() && self.ears.is_some() => self.start_hearing(),
            KeyCode::Enter => self.submit_input(),
            KeyCode::Backspace if self.mode == Mode::Command && self.input == "/" => self.set_mode(Mode::Walk),
            KeyCode::Backspace => {
                let to = self.caret();
                let m = k.modifiers;
                let from = if m.contains(KeyModifiers::SUPER) {
                    0
                } else if m.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) {
                    self.word_edge(true)
                } else {
                    to.saturating_sub(1)
                };
                let from = from.max((self.mode == Mode::Command && self.input.starts_with('/')) as usize);
                if from < to {
                    let (a, b) = (self.byte_at(from), self.byte_at(to));
                    self.input.replace_range(a..b, "");
                }
            }
            KeyCode::Delete => {
                let at = self.caret();
                if self.caret_back > 0 {
                    let (a, b) = (self.byte_at(at), self.byte_at(at + 1));
                    self.input.replace_range(a..b, "");
                    self.caret_back -= 1;
                }
            }
            KeyCode::Left if k.modifiers.contains(KeyModifiers::SUPER) => self.set_caret(0),
            KeyCode::Right if k.modifiers.contains(KeyModifiers::SUPER) => self.caret_back = 0,
            KeyCode::Left if k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => self.set_caret(self.word_edge(true)),
            KeyCode::Right if k.modifiers.intersects(KeyModifiers::ALT | KeyModifiers::CONTROL) => self.set_caret(self.word_edge(false)),
            KeyCode::Left => self.set_caret(self.caret().saturating_sub(1)),
            KeyCode::Right => self.caret_back = self.caret_back.saturating_sub(1),
            KeyCode::Home | KeyCode::Up => self.set_caret(0),
            KeyCode::End | KeyCode::Down => self.caret_back = 0,
            KeyCode::Char('a') if k.modifiers.contains(KeyModifiers::CONTROL) => self.set_caret(0),
            KeyCode::Char('e') if k.modifiers.contains(KeyModifiers::CONTROL) => self.caret_back = 0,
            KeyCode::Char('u') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input = if self.mode == Mode::Command { "/".into() } else { String::new() };
                self.caret_back = 0;
            }
            KeyCode::Char(c) => {
                let c = if k.modifiers.contains(KeyModifiers::SHIFT) { c.to_uppercase().next().unwrap_or(c) } else { c };
                self.insert_input(&c.to_string());
            }
            _ => {}
        }
        self.dirty = true;
    }

    /// Send the input line (typed, or heard and paused on).
    fn submit_input(&mut self) {
        // Mid-reply: it waits, still in the line, for the next Enter.
        if matches!(self.mode, Mode::Talk(_)) && self.replying.is_some() {
            return;
        }
        let text = std::mem::take(&mut self.input);
        match self.mode {
            Mode::Talk(cid) => self.send_talk(cid, text),
            Mode::Command => {
                self.set_mode(Mode::Walk);
                self.command(text.trim());
            }
            Mode::Walk => {}
        }
        self.dirty = true;
    }

    /// Anything you do or make, in words: the world decides what it means
    /// from the words, what the middle of the view points at, and what you hold.
    fn intent(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        // A guest's deeds in words are the host's LLM's work.
        if !self.brain.has_llm && !self.is_guest() {
            self.say(None, "Doing things in words needs an AI key (Esc → Settings → AI).", DIM);
            return;
        }
        self.say(Some("you"), &format!("*{text}*"), ACCENT);
        if self.over_budget() {
            return;
        }
        // A thing, or else the ground or far point the view rests on.
        let on = self.pointed_thing().or_else(|| self.pointed.as_ref().filter(|p| p.dist < 40.0).map(|p| p.target.clone()));
        let at = self.pointed_at();
        self.player_act(Action::Do { text: text.to_string(), on, at });
    }

    /// The nearest character whose name starts with `who`, or the one pointed at.
    /// Something to ride or drive: a being, or a vehicle (pointed at, or by name).
    fn ride_target(&mut self, what: &str) -> Option<Target> {
        if let Some(t) = self.person(what) {
            return Some(t);
        }
        let t = if what.trim().is_empty() {
            self.pointed_thing()
        } else {
            let p = self.sim.player.pos;
            self.sim.find_named(what, p, ActorId::Player)
        }?;
        (self.sim.target_drive(&t).is_some() || self.sim.target_seats(&t)).then_some(t)
    }

    fn person(&mut self, who: &str) -> Option<Target> {
        if who.trim().is_empty() {
            // The crosshair wins: on a thing, nobody nearby is meant.
            return match self.pointed_thing() {
                Some(t @ Target::Actor(_)) => Some(t),
                Some(_) => None,
                None => self.talk_hint.as_ref().map(|(id, _)| Target::Actor(ActorId::Npc(*id))),
            };
        }
        let p = self.sim.player.pos;
        self.sim.find_named(who, p, ActorId::Player).filter(|t| matches!(t, Target::Actor(_)))
    }

    fn command(&mut self, line: &str) {
        let body = line.trim_start_matches('/').trim();
        let (cmd, rest) = body.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((body, ""));
        let lc = cmd.to_lowercase();
        // Shortcuts are taken only in their exact form ("/wave", "/hug Mara");
        // anything else ("/wave the flag") is something you do, in words.
        if let Some(k) = crate::sim::actor::GestureKind::parse(&lc) {
            let to = self.person(rest);
            // With a thing in the crosshair, made-up gestures act on it, in words.
            let on_thing = rest.is_empty() && self.pointed_thing().is_some_and(|t| !matches!(t, Target::Actor(_)));
            let custom = matches!(k, crate::sim::actor::GestureKind::Custom(_));
            if (rest.is_empty() && !k.contact() && !(on_thing && custom)) || to.is_some() {
                self.player_act(Action::Gesture { kind: lc.clone(), to });
                return;
            }
        }
        match lc.as_str() {
            "" => {}
            // Playing together: words for everyone here.
            "invite" if self.together.is_some() => self.copy_invite(),
            "kick" if self.together.is_some() => self.send_away(rest),
            "name" if !rest.is_empty() => {
                let name: String = rest.chars().filter(|c| !c.is_control()).take(24).collect();
                self.settings.name = name.trim().to_string();
                self.settings.save();
                self.say(None, &format!("You'll be {} when playing together (from the next game).", self.settings.name), DIM);
            }
            "help" | "?" => {
                for l in [
                    "/ <anything> — do or make anything, at what the middle of the view points at:",
                    "  /a lighthouse on that hill · /punch a hole here · /add the stick to this wall (stick in hand) · /rub the stone on the lantern",
                    "/undo — undo the last thing you created   /history — list world versions",
                    "e use (opens doors; swings, chops or digs with a held tool; shoots a bow or a gun where you look, and loads it with ammunition you point at; lights something that goes off; pets, rides, greets or feeds who is in front; gets into a cart or car) · e also picks up a loose thing when your hands are empty · g pick up / put down (small animals too, if they let you; pulls out what is stuck fast) · f throw (keen things fly point first) · y/n answer",
                    "/wave /bow /nod /cheer /dance /sit /hug NAME /kiss NAME /handshake NAME /highfive NAME · /gesture ANY [NAME]",
                    "/give NAME · /say TEXT · /propose NAME catch|carry|dance|walk|… · /drop",
                    "/ride NAME · /drive (the vehicle in view) · /dismount · /pet NAME · /wear (what you hold) · /takeoff — on a flyer, look up or down to climb or dive",
                    "/day, /night, /time <hour 0–23> — jump the clock forward to that time",
                    "Walk: W/S move, A/D strafe, ←→ turn, ↑↓ look, Space jump, c crouch, Shift+move or r run, Tab ascii/blocks, F1 stats, F2 inspect, F3 achievements, Esc pause menu",
                    "Log: 1 bigger (half, full, back to small), PgUp/PgDn scroll back",
                    "Talk: walk up to someone and press Enter; Esc to leave.",
                    "Playing together: /say TEXT is heard by everyone playing · /invite (or F5) copies the address and code · /name NAME · host: /kick NAME sends a guest away",
                ] {
                    self.say(None, l, DIM);
                }
            }
            "undo" if self.is_guest() => self.say(None, "Only the host can undo in their world.", DIM),
            "undo" => {
                self.brain.send(Cmd::Undo);
                let got = self.achievements.grant("fresh_start");
                self.announce(got);
            }
            "history" => self.brain.send(Cmd::History),
            "day" | "noon" => self.set_hour(12.0),
            "night" | "midnight" => self.set_hour(0.0),
            "morning" | "dawn" => self.set_hour(7.0),
            "dusk" | "evening" => self.set_hour(18.5),
            "time" if rest.parse::<f64>().is_ok_and(|h| (0.0..24.0).contains(&h)) => self.set_hour(rest.parse().unwrap_or(12.0)),
            "gesture" | "g" if !rest.is_empty() => {
                // Any gesture; ones nobody knows yet are written once by the LLM.
                let (kind, who) = rest.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((rest, ""));
                if kind.is_empty() {
                    self.say(None, "Usage: /gesture salute [name]", DIM);
                    return;
                }
                let to = if who.is_empty() { None } else { self.person(who) };
                self.player_act(Action::Gesture { kind: kind.to_string(), to });
            }
            "drop" if rest.is_empty() => self.player_act(Action::Drop),
            "ride" | "mount" | "drive" | "board" | "sit" if self.ride_target(rest).is_some() => {
                if let Some(t) = self.ride_target(rest) {
                    self.player_act(Action::Ride { target: t });
                }
            }
            "pet" | "stroke" | "pat" if self.person(rest).is_some() => {
                if let Some(t) = self.person(rest) {
                    self.player_act(Action::Pet { target: t });
                }
            }
            "dismount" | "getdown" | "getout" | "exit" if rest.is_empty() => self.player_act(Action::Dismount),
            "wear" | "puton" if rest.is_empty() && self.sim.player.held.is_some() => self.player_act(Action::Wear { target: None, on: None }),
            "takeoff" if rest.is_empty() => self.player_act(Action::TakeOff { target: None, from: None }),
            "inspect" if rest.is_empty() => {
                self.inspect = !self.inspect;
            }
            "yes" if rest.is_empty() => self.player_act(Action::Answer { yes: true, to: None }),
            "no" if rest.is_empty() => self.player_act(Action::Answer { yes: false, to: None }),
            "say" | "shout" if !rest.is_empty() => {
                // Out loud: the people here hear it, and so does everyone playing.
                if self.together.is_some() {
                    self.say_to_all(rest.to_string());
                } else {
                    self.say(Some("you"), rest, ACCENT);
                }
                let to = self.person("");
                self.player_act(Action::Say { text: rest.to_string(), to });
            }
            "give" if self.sim.player.held.is_some() && self.person(rest).is_some() => {
                if let Some(t) = self.person(rest) {
                    self.player_act(Action::Give { to: t });
                }
            }
            "propose" if rest.split_once(' ').is_some_and(|(who, what)| !what.trim().is_empty() && self.person(who).is_some()) => {
                let (who, what) = rest.split_once(' ').unwrap_or((rest, ""));
                if let Some(t) = self.person(who) {
                    let with = self.pointed_thing().filter(|x| !matches!(x, Target::Actor(_)));
                    let with = with.or(self.sim.player.held.map(Target::Thing));
                    self.player_act(Action::Propose { to: t, activity: what.trim().to_string(), with });
                }
            }
            _ => self.intent(body),
        }
    }

    /// Jump the clock forward to the next occurrence of `hour` (never backwards).
    fn set_hour(&mut self, hour: f64) {
        let day = sky::DAY_SECONDS;
        let mut t = (self.sim.t / day).floor() * day + hour / 24.0 * day;
        if t <= self.sim.t {
            t += day;
        }
        self.sim.t = t;
        self.say(None, &format!("Time jumps to {:02}:{:02} ({}).", hour as u32, ((hour.fract()) * 60.0).round() as u32, sky::time_label(t)), DIM);
    }

    fn talk_context(&mut self) -> String {
        let cam = self.camera();
        let npcs = self.sim.npc_views();
        let view = describe(&self.snap, &mut self.sim.cache, &npcs, &cam, 1.6, self.sim.t);
        let seen: Vec<String> = view.visible.iter().take(10).map(|s| format!("{} ({:.0} m {})", s.name, s.distance, s.third)).collect();
        let place = view.region.clone().unwrap_or_else(|| view.biome.clone());
        let held = self.sim.player.held.map(|h| format!(" The traveler is holding {}.", crate::sim::actions::the(&self.sim.thing_name(h)))).unwrap_or_default();
        let dark = self.sim.talking_to.and_then(|cid| self.sim.drive_line(cid)).map(|l| format!(" {l}")).unwrap_or_default();
        let dark = format!("{dark}{}", self.sim.trouble_line(cam.pos).map(|l| format!(" {l}")).unwrap_or_default());
        let dark = format!("{dark}{}", self.sim.talking_to.and_then(|cid| self.sim.fear_line(cid, ActorId::Player)).map(|l| format!(" {l}")).unwrap_or_default());
        format!(
            "It is {} in {}. Visible around you: {}. Recently the traveler {}.{held}{dark}",
            view.time,
            place,
            if seen.is_empty() { "open land".into() } else { seen.join(", ") },
            if self.sim.recent_player.is_empty() { "just arrived".into() } else { self.sim.recent_player.iter().map(|(t, w)| format!("{w} ({})", sky::ago(self.sim.t - t))).collect::<Vec<_>>().join("; ") }
        )
    }

    fn send_talk(&mut self, cid: i64, text: String) {
        let text = text.trim().to_string();
        if text.is_empty() || self.replying.is_some() {
            return;
        }
        // A guest's words go to the host's world (whose LLM answers).
        if let Some(together::Together::Guest(g)) = &self.together {
            g.talk(cid, text.clone());
            self.say(Some("you"), &text, ACCENT);
            self.conv.entry(cid).or_default().push((true, text));
            self.replying = Some(cid);
            return;
        }
        if self.over_budget() {
            return;
        }
        self.say(Some("you"), &text, ACCENT);
        let context = self.talk_context();
        let history = self.conv.get(&cid).cloned().unwrap_or_default();
        self.conv.entry(cid).or_default().push((true, text.clone()));
        self.replying = Some(cid);
        let name = self.sim.actor_name(ActorId::Npc(cid));
        self.sim.event("said", Some(ActorId::Player), Some(ActorId::Npc(cid).key()), format!("the traveler said to {name}: \"{text}\""), Some(self.pos()), serde_json::json!({ "text": text }));
        self.brain.send(Cmd::Talk { cid, text, context, history });
    }

    // ------------------------------------------------------------------ events

    fn on_event(&mut self, ev: Event) {
        if self.loading_event(&ev) {
            return;
        }
        // Guests hear what is said, and a reply to one of them is theirs.
        if self.share_event(&ev) {
            return;
        }
        match ev {
            Event::Flip { snap, region } => self.flip(snap, region),
            Event::Log(s) => {
                if s.starts_with("Budget reached") {
                    if self.budget_paused {
                        return;
                    }
                    self.budget_paused = true;
                }
                self.say(None, &s, TEXT)
            }
            Event::Building(d) => {
                self.building = (self.building + d).max(0);
                self.dirty = true;
            }
            Event::RegionStarted(r) => {
                self.regions_inflight.insert(r);
            }
            Event::RegionFinished(r, ok) => {
                self.regions_inflight.remove(&r);
                if !ok {
                    self.regions_failed.insert(r);
                }
            }
            Event::Token { cid, text } => {
                let name = self.sim.actor_name(ActorId::Npc(cid));
                match self.log.back_mut() {
                    Some(l) if l.streaming == Some(cid) => {
                        l.text.push_str(&text);
                        l.at = Instant::now();
                    }
                    _ => self.log.push_back(LogLine { streaming: Some(cid), who: Some(cid), ..LogLine::new(journal::Kind::Talk, Some(name), text.trim_start().to_string(), TEXT) }),
                }
                self.sim_talk.entry(cid).or_default().push_str(&text);
                self.voice_token(cid, &text);
                self.dirty = true;
            }
            Event::ReplyDone { cid, ok } => {
                self.voice_reply_done(cid, ok);
                if let Some(l) = self.log.iter_mut().rev().find(|l| l.streaming == Some(cid)) {
                    l.streaming = None;
                    if ok {
                        let reply = l.text.clone();
                        let h = self.conv.entry(cid).or_default();
                        let said = h.last().filter(|x| x.0).map(|x| x.1.clone());
                        h.push((false, reply.clone()));
                        // What was asked may now be done, not only talked about
                        // (in the host's world, by the host's game).
                        if let Some(said) = said.filter(|_| self.replying == Some(cid) && !self.is_guest()) {
                            self.sim.asked(cid, &said, reply.trim());
                        }
                    }
                }
                if let Some(line) = self.sim_talk.remove(&cid) {
                    if ok && !self.is_guest() {
                        self.sim.npc_said(cid, line.trim());
                    }
                }
                if !ok {
                    // Keep the conversation history alternating.
                    if let Some(h) = self.conv.get_mut(&cid) {
                        h.pop();
                    }
                }
                self.replying = None;
                self.dirty = true;
            }
            Event::GenesisDone => {
                self.genesis_pending = false;
                // Nobody has moved yet: begin where the new land says, by the start region's settlement.
                if self.loading.is_some() {
                    self.sim.player.pos = self.snap.spawn;
                    self.cam_y = self.sim.player.pos.y + self.sim.player.dims.eye;
                }
            }
            Event::History(lines) => {
                self.say(None, "World history (newest first):", DIM);
                for l in lines.iter().take(12) {
                    self.say(None, l, DIM);
                }
            }
            other => {
                let mut scratch = HashMap::new();
                crate::sim::headless::apply(&mut self.sim, other, &mut scratch);
                self.dirty = true;
            }
        }
    }

    fn flip(&mut self, snap: Arc<WorldSnapshot>, region: Option<((i32, i32), String)>) {
        self.full_pending = false;
        let old: HashSet<i64> = self.snap.instances.iter().map(|p| p.id).collect();
        let now = Instant::now();
        for p in &snap.instances {
            if !old.contains(&p.id) {
                self.appear.insert(p.id, now);
            }
        }
        if let Some((r, name)) = region {
            let pr = region_of(self.pos().x, self.pos().z);
            if (r.0 - pr.0).abs() <= 1 && (r.1 - pr.1).abs() <= 1 && !name.is_empty() {
                self.say(None, &format!("The fog lifts over {name}."), [170, 200, 255]);
            }
        }
        let terrain_changed = render::terrain_epoch(&snap.terrain) != render::terrain_epoch(&self.snap.terrain);
        self.snap = snap.clone();
        self.sim.flip(snap);
        if self.is_guest() {
            // The made ground on the land made anew; and what waited for its kind.
            if terrain_changed {
                crate::sim::persist::apply_made_ground(&mut self.sim);
            }
            self.sim.mirror_pending();
        }
        self.unstick();
        if terrain_changed {
            // The land reshaped itself; don't leave the player under water.
            let p = self.pos();
            let h = self.snap.terrain.height(p.x, p.z);
            if h < self.snap.terrain.water(p.x, p.z) + 0.3 {
                self.sim.player.pos = self.snap.spawn;
            }
            let p = self.pos();
            self.sim.player.pos.y = self.snap.terrain.height(p.x, p.z);
            self.cam_y = self.sim.player.pos.y + self.sim.player.dims.eye;
        }
        self.dirty = true;
    }

    // ------------------------------------------------------------------ sim

    fn tick(&mut self, dt: f32) {
        if self.sim.fallen() {
            if self.fell_at.is_none() {
                self.fell_at = Some(Instant::now());
                self.keys.down.clear();
                self.mode = Mode::Walk;
                self.input.clear();
            }
            self.dirty = true;
        }
        if (self.mode == Mode::Walk || self.noclip) && !self.sim.fallen() {
            let fwd = self.held('w') as i32 - self.held('s') as i32;
            let turn = self.held('>') as i32 - self.held('<') as i32;
            let strafe = self.held('d') as i32 - self.held('a') as i32;
            let look = self.held('^') as i32 - self.held('v') as i32;
            let aboard = self.sim.player.aboard.is_some() && !self.noclip;
            // In deep water: Space swims up, c down.
            self.sim.player.swim = if self.sim.player.swimming { (self.held(' ') as i32 - self.held('c') as i32) as f32 } else { 0.0 };
            if !aboard {
                self.sim.player.yaw += turn as f32 * TURN_SPEED * dt;
            }
            if look != 0 {
                self.pitch = (self.pitch + look as f32 * LOOK_SPEED * dt).clamp(PITCH_MIN, PITCH_MAX);
                self.dirty = true;
            }
            let yaw = self.yaw();
            let f = Vec3::new(yaw.sin(), 0.0, yaw.cos());
            let r = Vec3::new(yaw.cos(), 0.0, -yaw.sin());
            let mut dir = f * fwd as f32 + r * strafe as f32;
            if dir.length() > 1.0 {
                dir = dir.normalize();
            }
            if aboard {
                // Driving: W/S throttle and brake, A/D and ←→ steer.
                let steer = (strafe + turn).clamp(-1, 1) as f32;
                // Flyers and submarines: Space up, c down; or, moving, the way you look.
                let keys = self.held(' ') as i32 - self.held('c') as i32;
                let climb = if keys != 0 { keys as f32 } else if fwd != 0 { (self.pitch * 2.0).clamp(-1.0, 1.0) } else { 0.0 };
                if !self.guest_steers(crate::net::wire::Up::Steer { throttle: fwd as f32, steer, climb }) {
                    self.sim.steer(ActorId::Player, fwd as f32, steer, climb, dt);
                }
                self.dirty = true;
            } else if self.sim.player.riding.is_some() && !self.noclip {
                // Riding: the walk keys steer the mount (a guest's, in the host's world).
                if !self.guest_steers(crate::net::wire::Up::Ride { dir: dir.to_array(), pitch: self.pitch }) {
                    self.sim.drive(dir, self.pitch, dt);
                }
                if dir != Vec3::ZERO {
                    self.sim.player.task = None;
                }
            } else if dir != Vec3::ZERO {
                self.sim.player.running = self.always_run || self.shift_run;
                let delta = dir * WALK_SPEED * self.sim.gait_factor(ActorId::Player) * dt;
                if self.noclip {
                    let p = self.pos() + delta;
                    self.sim.player.pos = Vec3::new(p.x, self.snap.terrain.height(p.x, p.z), p.z);
                } else {
                    self.sim.walk(ActorId::Player, delta);
                    self.sim.kick_things(ActorId::Player, delta);
                }
                // Walking away cancels an agent-style task.
                self.sim.player.task = None;
            }
        }
        if self.noclip && !self.sim.player.seated() {
            let p = self.pos();
            self.sim.player.pos.y = self.snap.terrain.height(p.x, p.z);
        }
        if dirn_none(self.held('w'), self.held('s'), self.held('a'), self.held('d')) {
            self.shift_run = false;
        }
        // Wading, the view stays over the water; swimming, it goes where the body does.
        let feet = if self.sim.player.swimming { self.sim.player.pos.y } else { self.sim.player.pos.y.max(self.snap.terrain.water(self.sim.player.pos.x, self.sim.player.pos.z) - 0.4) };
        let eye = feet + self.sim.player.eye_height();
        // In a closed cab, the view is from just over it.
        let eye = eye + self.sim.eye_lift(Vec3::new(self.sim.player.pos.x, eye, self.sim.player.pos.z));
        // Smooth over stairs; follow a jump or a fall at once.
        let k = if self.sim.player.grounded { (dt * 10.0).min(1.0) } else { 1.0 };
        self.cam_y += (eye - self.cam_y) * k;

        // The living world (a guest's is the host's: only their own body
        // moves here).
        let t0 = Instant::now();
        if self.is_guest() {
            self.sim.step_guest(dt);
        } else {
            self.sim.step(dt);
        }
        self.together_tick();
        slow("sim step", t0, || format!("{} active cells, {} things, {} characters", self.sim.field.active(), self.sim.things.map.len(), self.sim.cast.npcs.len()));
        for r in self.sim.drain_requests() {
            if self.budget_paused && !matches!(r, crate::sim::Request::Witness { .. }) {
                continue;
            }
            crate::sim::headless::forward(&self.sim, &self.brain, r);
        }
        let (placed, skip) = self.sim.drain_placed();
        let notes = self.sim.drain_notes();
        self.share_notes(&notes, &placed, &skip);
        for n in notes {
            self.tell(n);
        }
        self.share_sounds();
        self.hear(dt);
        self.mouths();
        self.ears_frame();
        if self.log_expiry.is_some_and(|t| Instant::now() >= t) {
            self.log_expiry = None;
            self.dirty = true;
        }
        self.update_work();
        // A guest's achievements wait (the world isn't theirs).
        if !self.is_guest() {
            let got = self.achievements.update(&self.sim);
            self.announce(got);
        }
        if !self.toasts.is_empty() {
            self.dirty = true;
        }
        // Leave talk mode if they walked off.
        if let Mode::Talk(id) = self.mode {
            let me = self.pos();
            if self.sim.cast.get(id).is_none_or(|n| (n.a.pos - me).length() > TALK_RANGE * 3.0) {
                self.set_mode(Mode::Walk);
            }
        }
        // Who could we talk to?
        let me = self.pos();
        let yaw = self.yaw();
        let f = Vec3::new(yaw.sin(), 0.0, yaw.cos());
        let mut best: Option<(f32, i64, String)> = None;
        for n in self.sim.cast.npcs.iter().filter(|n| n.here()) {
            let d = n.a.pos - me;
            let dist = Vec3::new(d.x, 0.0, d.z).length();
            if dist > TALK_RANGE || n.a.asleep && dist > 2.0 {
                continue;
            }
            let ang = (Vec3::new(d.x, 0.0, d.z).normalize_or_zero().dot(f)).clamp(-1.0, 1.0).acos();
            if ang < 0.75 && best.as_ref().is_none_or(|b| dist < b.0) {
                best = Some((dist, n.def.id, n.name().to_string()));
            }
        }
        let hint = best.map(|b| (b.1, b.2));
        if hint != self.talk_hint {
            self.talk_hint = hint;
            self.dirty = true;
        }
        // What are we pointing at?
        // While typing a command, keep what was pointed at when / was pressed.
        if self.mode != Mode::Command && self.last_pick.elapsed() > Duration::from_millis(120) {
            self.repick();
        }
        {
            let mut l = self.live.lock();
            l.player = self.pos();
            l.characters = self.sim.cast.npcs.iter().map(|n| n.a.pos).collect();
        }
        if self.last_save.elapsed() > Duration::from_secs(3) {
            self.save_player();
        }
        if self.last_char_save.elapsed() > Duration::from_secs(10) {
            self.save_characters();
        }
        if self.last_region_check.elapsed() > Duration::from_millis(500) {
            self.last_region_check = Instant::now();
            self.live.lock().types = self.sim.types_in_use();
            self.schedule_regions();
            let p = self.pos();
            self.sim.cache.trim(p, render::VIEW_DIST + 80.0);
        }
    }

    /// What the world sounds like this frame, to the sound device.
    fn hear(&mut self, dt: f32) {
        let Some(out) = self.sound.as_ref() else {
            self.sim.sounds.clear();
            return;
        };
        self.foley.frame(&mut self.sim, dt, &mut self.sound_cmds);
        for c in self.sound_cmds.drain(..) {
            out.send(c);
        }
        self.voice_frame();
    }

    /// Sound on or off and its volume, as the settings say.
    pub(super) fn sync_sound(&mut self) {
        match (&self.sound, self.settings.sound) {
            (None, true) => self.sound = crate::audio::start(self.settings.volume),
            (Some(_), false) => {
                if let Some(o) = self.sound.take() {
                    o.send(crate::audio::mix::Cmd::Clear);
                }
            }
            (Some(o), true) => o.send(crate::audio::mix::Cmd::Volume(self.settings.volume)),
            (None, false) => {}
        }
        self.sync_voice();
    }

    /// Point at whatever is in the middle of the view.
    fn repick(&mut self) {
        self.last_pick = Instant::now();
        let cam = self.camera();
        let (w, vh, _) = self.layout();
        let (pw, ph, pa) = self.pixel_size();
        let aspect = pw as f32 / ph as f32 * pa;
        let (f, r, u) = cam.basis();
        let tan = (cam.fov_y * 0.5).tan();
        let (cx, cy) = (w as f32 * 0.5, vh as f32 * 0.5);
        let uu = 2.0 * cx / w as f32 - 1.0;
        let vv = 1.0 - 2.0 * cy / vh as f32;
        let rd = (f + r * uu * tan * aspect + u * vv * tan).normalize();
        // Half a reticle cell, as a slope off the ray.
        let cone = (tan / vh as f32).max(tan * aspect / w as f32);
        let picked = self.sim.pick(cam.pos, rd, 40.0, cone, Some(ActorId::Player));
        if picked.as_ref().map(|p| (&p.target, p.name.as_str())) != self.pointed.as_ref().map(|p| (&p.target, p.name.as_str())) {
            self.dirty = true;
        }
        let loose = picked.as_ref().is_some_and(|p| p.dist < crate::sim::actor::REACH + 1.0 && self.sim.just_to_pick_up(&p.target));
        if loose != self.pointed_loose {
            self.dirty = true;
        }
        self.pointed_loose = loose;
        let grab = picked.as_ref().filter(|p| p.dist < crate::sim::actor::REACH + 1.0).and_then(|p| self.sim.grabbable(&p.target));
        if grab != self.pointed_grab {
            self.dirty = true;
        }
        self.pointed_grab = grab;
        self.pointed = picked;
    }

    pub fn save_player(&mut self) {
        self.last_save = Instant::now();
        self.achievements.touch();
        let p = self.pos();
        let _ = self.db.save_player(PlayerRow { x: p.x, z: p.z, yaw: self.yaw(), t_game: self.sim.t });
        self.save_guests();
    }

    pub fn save_characters(&mut self) {
        self.last_char_save = Instant::now();
        let t0 = Instant::now();
        crate::sim::persist::save(&mut self.sim);
        slow("save", t0, || format!("{} cells", self.sim.field.cells.len()));
    }

    /// Queue story plans for unplanned regions within 2 regions, nearest and
    /// most in the direction of travel first.
    fn schedule_regions(&mut self) {
        if !self.brain.has_llm || self.budget_paused || self.genesis_pending || self.regions_inflight.len() >= 2 {
            return;
        }
        let me = self.pos();
        let pr = region_of(me.x, me.z);
        let f = Vec3::new(self.yaw().sin(), 0.0, self.yaw().cos());
        let mut best: Option<(f32, (i32, i32))> = None;
        let rr = self.settings.region_radius;
        for dz in -rr..=rr {
            for dx in -rr..=rr {
                let r = (pr.0 + dx, pr.1 + dz);
                if self.snap.regions.contains_key(&r) || self.regions_inflight.contains(&r) || self.regions_failed.contains(&r) {
                    continue;
                }
                let c = region_center(r);
                let to = Vec3::new(c.x - me.x, 0.0, c.z - me.z);
                let d = to.length();
                let score = d - to.normalize_or_zero().dot(f) * crate::world::REGION * 0.6;
                if best.is_none_or(|b| score < b.0) {
                    best = Some((score, r));
                }
            }
        }
        if let Some((_, r)) = best {
            self.regions_inflight.insert(r);
            self.brain.send(Cmd::Region(r));
        }
    }

    // ------------------------------------------------------------------ render

    fn request_frame(&mut self) {
        let (pw, ph, pa) = self.pixel_size();
        let cam = self.camera();
        let aspect = pw as f32 / ph as f32 * pa;
        self.sim.night.view_slope = (cam.fov_y * 0.5).tan() * aspect;
        let now = Instant::now();
        let mut fade = HashMap::new();
        self.appear.retain(|id, t| {
            let f = now.duration_since(*t).as_secs_f32() / 1.0;
            if f < 1.0 {
                fade.insert(*id, f.max(0.001));
                true
            } else {
                false
            }
        });
        let drawn = self.sim.draw_first_person(&cam, render::VIEW_DIST);
        let culled = cull::cull(&self.snap, &mut self.sim.cache, &cam, aspect, &drawn.insts, &fade);
        self.stats.insts = culled.insts.len();
        let light = sky::lighting(self.sim.t, &self.snap.look.palette);
        let sp = SceneParams {
            terrain: &self.snap.terrain,
            palette: &self.snap.look.palette,
            camera: cam,
            width: pw,
            height: ph,
            pixel_aspect: pa,
            light,
            time: self.start.elapsed().as_secs_f32(),
            frame: self.next_frame_id as u32,
            shadows: !self.render.cpu_fallback && self.settings.shadows,
            lights: &drawn.lights,
            weather: render::WeatherView::of(&self.sim.wx, self.sim.weather.flash(self.sim.t, self.sim.wx.storm), !self.sim.open_sky(cam.pos)),
        };
        let mut globals = render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
        // In a window, each frame's rays shift a little: blended, they add up to more detail.
        if self.blending() {
            let j = crate::window::jitter(self.next_frame_id);
            // z = 1: blend this frame into the window's history.
            globals.jitter = [j[0], j[1], 1.0, 0.0];
        }
        let req = FrameRequest {
            id: self.next_frame_id,
            width: pw,
            height: ph,
            globals,
            instances: culled.insts,
            holes: drawn.holes,
            grid: culled.grid,
            scene: self.snap.scene.clone(),
            terrain: self.snap.terrain.clone(),
            look: self.snap.look.clone(),
        };
        self.next_frame_id += 1;
        if self.render.tx.send(RenderMsg::Frame(Box::new(req))).is_ok() {
            self.outstanding += 1;
            self.last_request = now;
        }
    }

    fn compose(&mut self) {
        let (w, vh, _) = self.layout();
        self.screen.clear_decor();
        // The view: the whole window.
        if self.win.is_some() {
            self.screen.fill_world(vh);
        } else if let Some(f) = &self.last_frame {
            frame_to_cells(f, &mut self.screen, w, vh, false);
        } else {
            for y in 0..vh {
                self.screen.fill_row(y, Cell { bg: [0, 0, 0], ..Cell::BLANK });
            }
        }
        let journal = self.log_view > 0;
        if self.menu.is_none() && !journal {
            self.draw_work_marks(w, vh);
            self.draw_labels(w, vh);
            self.draw_pointer(w, vh);
        }
        if self.debug {
            self.draw_debug();
        }
        if self.inspect {
            self.draw_inspect(w, vh);
        }
        self.draw_hurt(w, vh);
        self.draw_fallen(w, vh);
        if self.menu.is_some() {
            self.draw_menu(w, vh, true);
        } else if journal {
            self.draw_journal(w, vh);
        } else {
            self.draw_work(w, vh);
            self.draw_hud(w, vh);
        }
        self.draw_toast(w, vh);
    }

    /// Wake from the fall: at the world's beginning, in the morning.
    fn wake(&mut self) {
        self.sim.wake_traveler();
        self.fell_at = None;
        self.hurt_flash = None;
        self.seen_wounds = 0.0;
        self.pitch = 0.0;
        self.keys.down.clear();
        self.cam_y = self.sim.player.pos.y + self.sim.player.dims.eye;
        self.dirty = true;
    }

    /// Fallen: the view goes dark red (the world still showing through),
    /// and over it, YOU DIED, who did it, and how to wake.
    fn draw_fallen(&mut self, w: u16, vh: u16) {
        if !self.sim.fallen() {
            return;
        }
        let s = self.fell_at.map(|t| t.elapsed().as_secs_f32()).unwrap_or(10.0);
        let k = (s / 1.4).clamp(0.0, 1.0) * 0.55;
        const BLOOD: [f32; 3] = [96.0, 6.0, 4.0];
        let tint = |v: [u8; 3]| [0, 1, 2].map(|i| (v[i] as f32 + (BLOOD[i] - v[i] as f32) * k) as u8);
        for y in 0..vh {
            for x in 0..w {
                self.screen.over(x, y, |mut c| {
                    c.bg = tint(c.bg);
                    c.fg = tint(c.fg);
                    c
                });
            }
        }
        if s < FALL_SECS * 0.7 {
            return;
        }
        let red = [236, 62, 50];
        let shadow = [40, 0, 0];
        let title = "YOU DIED";
        if self.win.is_some() {
            // In a window: the words themselves, large.
            let mut y = (vh as f32 / 2.0 - 3.0).max(2.0);
            self.screen.labels.push(crate::term::Label { x: w as f32 / 2.0 + 0.15, y: y + 0.2, text: "You died".into(), size: 5.0, color: shadow, face: crate::term::Face::Title, center: true });
            self.screen.title(w as f32 / 2.0, y, "You died", 5.0, red);
            y += 4.0;
            let by = self.sim.night.fallen.clone().unwrap_or_default();
            let line = format!("{}.", crate::sim::physics::cap(&by));
            self.text_over(w.saturating_sub(line.chars().count() as u16) / 2, y as u16, &line, TEXT, false);
            if s > FALL_SECS {
                let pulse = 0.6 + 0.4 * (s * 2.5).sin().abs();
                let c = [(255.0 * pulse) as u8, (220.0 * pulse) as u8, (200.0 * pulse) as u8];
                let hint = "Press Enter to wake where you began, in the morning";
                self.text_over(w.saturating_sub(hint.chars().count() as u16) / 2, y as u16 + 2, hint, c, false);
            }
            return;
        }
        let big: Vec<String> = (0..5).map(|row| title.chars().map(|ch| glyph(ch)[row]).collect::<Vec<_>>().join(" ")).collect();
        let bw = big[0].chars().count() as u16;
        let mut y = (vh as i32 / 2 - 5).max(1) as u16;
        if bw + 4 <= w && vh >= 14 {
            let x0 = (w - bw) / 2;
            for (r, line) in big.iter().enumerate() {
                for (i, ch) in line.chars().enumerate() {
                    if ch == ' ' {
                        continue;
                    }
                    let (x, yy) = (x0 + i as u16, y + r as u16);
                    // A drop shadow, down and to the right.
                    self.screen.over(x + 1, yy + 1, |c| Cell { ch: ' ', bg: shadow, ..c });
                    self.screen.over(x, yy, |c| Cell { ch: '█', fg: red, ..c });
                }
            }
            y += 7;
        } else {
            let x = w.saturating_sub(title.len() as u16) / 2;
            self.text_over(x, y, title, red, true);
            y += 2;
        }
        let by = self.sim.night.fallen.clone().unwrap_or_default();
        let line = format!("{}.", crate::sim::physics::cap(&by));
        self.text_over(w.saturating_sub(line.chars().count() as u16) / 2, y, &line, TEXT, false);
        if s > FALL_SECS {
            let hint = "Press Enter to wake where you began, in the morning";
            let pulse = 0.6 + 0.4 * (s * 2.5).sin().abs();
            let c = [(255.0 * pulse) as u8, (220.0 * pulse) as u8, (200.0 * pulse) as u8];
            self.text_over(w.saturating_sub(hint.chars().count() as u16) / 2, y + 2, hint, c, false);
        }
    }

    /// Text over the view, keeping what is behind it as its background.
    fn text_over(&mut self, x: u16, y: u16, s: &str, fg: [u8; 3], bold: bool) {
        for (i, ch) in s.chars().enumerate() {
            self.screen.over(x + i as u16, y, |c| Cell { ch: crate::term::narrow(ch), fg, bold, bg: c.bg.map(|v| (v as f32 * 0.55) as u8) });
        }
    }

    /// Hurt: the edges of the view flash red, fading.
    fn draw_hurt(&mut self, w: u16, vh: u16) {
        let wounds = self.sim.night.wounds;
        if wounds > self.seen_wounds + 0.001 {
            self.hurt_flash = Some(Instant::now());
        }
        self.seen_wounds = wounds;
        let Some(s) = self.hurt_flash.map(|t| t.elapsed().as_secs_f32()) else { return };
        if s > 0.6 {
            self.hurt_flash = None;
            return;
        }
        self.dirty = true;
        let fade = 1.0 - s / 0.6;
        let depth = (w.min(vh * 2) / 8).max(2) as f32;
        for y in 0..vh {
            for x in 0..w {
                // Distance from the nearest edge, in cells (rows count double: they're tall).
                let d = (x as f32).min((w - 1 - x) as f32).min(y as f32 * 2.0).min((vh - 1 - y) as f32 * 2.0);
                if d >= depth {
                    continue;
                }
                let k = (1.0 - d / depth).powi(2) * fade * 0.75;
                let red = |v: [u8; 3]| [(v[0] as f32 + (200.0 - v[0] as f32) * k) as u8, (v[1] as f32 * (1.0 - k)) as u8, (v[2] as f32 * (1.0 - k)) as u8];
                self.screen.over(x, y, |c| Cell { bg: red(c.bg), fg: red(c.fg), ..c });
            }
        }
    }

    /// True (and says so) when POCKET_BUDGET_USD has been reached.
    fn over_budget(&mut self) -> bool {
        if self.llm.as_ref().is_some_and(|l| l.over_budget()) {
            self.budget_paused = true;
            self.say(None, "Budget reached: generation and dialogue are paused; walking still works. Esc opens settings to raise it.", ACCENT);
            return true;
        }
        false
    }

    fn spent(&self) -> f64 {
        self.llm.as_ref().map(|l| *l.spent.lock()).unwrap_or(0.0)
    }

    /// Tell of achievements just earned: a line in the log and a popup.
    fn announce(&mut self, got: Vec<&'static crate::achievements::Def>) {
        for d in got {
            self.say(None, &format!("{} Achievement: {}. {}", menu::icon(d.tier), d.title, d.text), menu::tier_color(d.tier));
            self.toasts.push_back((d, None));
        }
    }

    /// The achievement popup: a card that slides down at the top middle
    /// (clear of where you are, your health and what you look at), stays a
    /// while and goes back up. Its medal is the one in the list.
    fn draw_toast(&mut self, w: u16, _vh: u16) {
        use crate::term::{Deco, Face};
        const IN: f32 = 0.4;
        const HOLD: f32 = 5.0;
        const OUT: f32 = 0.4;
        let Some((d, since)) = self.toasts.front_mut() else { return };
        let since = *since.get_or_insert_with(Instant::now);
        let d: &'static crate::achievements::Def = d;
        let age = since.elapsed().as_secs_f32();
        if age > IN + HOLD + OUT {
            self.toasts.pop_front();
            self.dirty = true;
            return;
        }
        self.dirty = true;
        let color = menu::tier_color(d.tier);
        let cw = (w as f32 - 8.0).min(54.0);
        let text_x = 8.6;
        let body = hud::wrap(d.text, cw - text_x - 1.6, cw - text_x - 1.6, Face::Body, 0.9);
        let body: Vec<String> = body.into_iter().take(2).collect();
        let ch = 4.4 + body.len() as f32 * 1.1;
        let ease = |p: f32| 1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(3);
        let shown = if age < IN { ease(age / IN) } else if age > IN + HOLD { 1.0 - ease((age - IN - HOLD) / OUT) } else { 1.0 };
        let x = (w as f32 - cw) * 0.5;
        let y = -ch + (ch + 0.8) * shown;
        // Diamond: the edge brightens and fades, slowly.
        let t = self.start.elapsed().as_secs_f32();
        let edge = if d.tier == crate::achievements::Tier::Diamond {
            let k = 0.5 + 0.5 * (t * 2.5).sin();
            color.map(|c| (c as f32 + (255.0 - c as f32) * k * 0.6) as u8)
        } else {
            color
        };
        let s = &mut self.screen;
        // Over everything else (a menu's words too): its shapes and words go on top.
        let (d0, l0) = (s.decor.len(), s.labels.len());
        s.deco(Deco { x, y, w: cw, h: ch, fill: crate::ui::PANEL, alpha: 0.95, radius: 0.8, shadow: true, border: Some(edge) });
        menu::medal(s, x + 1.4, y + (ch - 2.8) * 0.5 - 0.2, 2.8, d.tier, true);
        let tx = x + text_x;
        hud::words(s, tx, y + 1.3, &format!("Achievement · {}", menu::cap(menu::tier_name(d.tier))), 0.8, [150, 155, 172], Face::Body);
        hud::words(s, tx, y + 2.6, d.title, 1.3, color, Face::Title);
        for (i, l) in body.iter().enumerate() {
            hud::words(s, tx, y + 3.95 + i as f32 * 1.1, l, 0.9, TEXT, Face::Body);
        }
        let decor: Vec<_> = s.decor.drain(d0..).collect();
        let labels: Vec<_> = s.labels.drain(l0..).collect();
        s.over_decor.extend(decor);
        s.over_labels.extend(labels);
    }

    fn draw_labels(&mut self, w: u16, vh: u16) {
        let cam = self.camera();
        let (f, r, u) = cam.basis();
        let tan = (cam.fov_y * 0.5).tan();
        let (pw, ph, pa) = self.pixel_size();
        let aspect = pw as f32 / ph as f32 * pa;
        let mut labels = Vec::new();
        for n in self.sim.cast.npcs.iter().filter(|n| n.here()) {
            let head = n.a.pos + Vec3::Y * (n.a.dims.height + 0.35);
            let v = head - cam.pos;
            let d = v.length();
            if d > 10.0 {
                continue;
            }
            let z = v.dot(f);
            if z < 0.3 {
                continue;
            }
            let sx = v.dot(r) / z / (tan * aspect);
            let sy = v.dot(u) / z / tan;
            if sx.abs() > 1.0 || sy.abs() > 1.0 {
                continue;
            }
            let col = ((sx + 1.0) * 0.5 * w as f32) as i32;
            let row = ((1.0 - sy) * 0.5 * vh as f32) as i32 - 1;
            let name = if n.a.asleep { format!("{} (asleep)", n.name()) } else { n.name().to_string() };
            labels.push((d, col, row, n.def.id, name));
        }
        // What each one said a moment ago goes in a box between name and head;
        // only the nearest few speakers get one. The log keeps it all.
        labels.sort_by(|a, b| a.0.total_cmp(&b.0));
        let now = Instant::now();
        let mut boxes = 0;
        let mut taken: Vec<(i32, i32, i32, i32)> = Vec::new();
        let mut draw = Vec::new();
        use crate::term::Face;
        const SAY: f32 = 0.92;
        for (_, col, row, id, name) in labels {
            let said = if boxes < BUBBLES { self.said_lately(id, now) } else { None };
            let lines = said.map(|t| bubble_lines(&t)).unwrap_or_default();
            boxes += !lines.is_empty() as usize;
            let text_w = lines.iter().map(|l| hud::width(l, Face::Body, SAY)).fold(0.0, f32::max);
            let bw = (text_w + 2.0).max(hud::width(&name, Face::Bold, SAY) + 1.6).ceil() as i32;
            let x0 = (col - bw / 2).clamp(0, (w as i32 - bw).max(0));
            // Nearer ones are placed first; a farther one that would cover them moves up.
            let mut bottom = row;
            let mut top = bottom - lines.len() as i32 - (!lines.is_empty()) as i32;
            while let Some(t) = taken.iter().find(|t| x0 < t.2 && t.0 < x0 + bw && top <= t.3 && t.1 <= bottom) {
                bottom = t.1 - 1;
                top = bottom - lines.len() as i32 - (!lines.is_empty()) as i32;
            }
            taken.push((x0, top, x0 + bw, bottom));
            draw.push((col, top, name, lines, x0, bw));
        }
        for (col, top, name, lines, x0, bw) in draw.into_iter().rev() {
            if top < -1 || top >= vh as i32 {
                continue;
            }
            let s = &mut self.screen;
            if !lines.is_empty() {
                // The words in a soft box under the name.
                let (by, bh) = (top as f32 + 1.0, lines.len() as f32 * 1.1 + 0.6);
                s.deco(crate::term::Deco { x: x0 as f32, y: by, w: bw as f32, h: bh, fill: [24, 26, 36], alpha: 0.86, radius: 0.55, shadow: false, border: None });
                for (i, l) in lines.iter().enumerate() {
                    s.labels.push(crate::ui::left(x0 as f32 + 1.0, by + 0.3 + 0.55 + i as f32 * 1.1, l, SAY, [238, 238, 242], Face::Body));
                }
            }
            let nw = hud::width(&name, Face::Bold, SAY) + 1.4;
            let cx = (col as f32 + 0.5).clamp(nw * 0.5, w as f32 - nw * 0.5);
            hud::shade(s, cx - nw * 0.5, top as f32 + 0.1, nw, 1.3, 0.65);
            s.labels.push(crate::term::Label { x: cx, y: top as f32 + 0.75, text: name, size: SAY, color: [255, 255, 255], face: Face::Bold, center: true });
        }
    }

    /// The last thing this character said, while it is still fresh.
    fn said_lately(&self, id: i64, now: Instant) -> Option<String> {
        for l in self.log.iter().rev() {
            let age = now.duration_since(l.at);
            if age > BUBBLE_MAX {
                return None;
            }
            if l.kind == journal::Kind::Talk && l.who == Some(id) {
                let n = l.text.chars().count() as u64;
                let keep = Duration::from_millis((3000 + n * 70).min(BUBBLE_MAX.as_millis() as u64));
                return (l.streaming.is_some() || age < keep).then(|| l.text.clone());
            }
        }
        None
    }

    /// A small reticle in the middle of the view: what you point at (its
    /// name and keys are under it, see `hud`).
    fn draw_pointer(&mut self, w: u16, vh: u16) {
        if self.mode == Mode::Walk && !self.sim.fallen() {
            self.screen.over(w / 2, vh / 2, |c| Cell { ch: '+', fg: [255, 255, 255], bold: true, ..c });
        }
    }

    /// F2: the raw truth about what you point at (or yourself).
    fn draw_inspect(&mut self, w: u16, vh: u16) {
        let target = self.pointed.as_ref().filter(|p| p.dist < 40.0).map(|p| p.target.clone()).unwrap_or(Target::Actor(ActorId::Player));
        let lines = self.sim.inspect_lines(&target);
        let pw = (w as usize / 2).clamp(30, 72);
        let x0 = w.saturating_sub(pw as u16);
        let mut y = 0u16;
        for l in lines {
            for wl in term::wrap(&l, pw - 2) {
                if y >= vh {
                    return;
                }
                let s = format!(" {wl:<width$}", width = pw - 1);
                self.screen.text(x0, y, &s, [210, 230, 255], [10, 14, 28], false);
                y += 1;
            }
        }
    }

    fn draw_debug(&mut self) {
        let lines = vec![
            format!("fps {:5.1}   worst loop {:4.1} ms   worst gap {:4.1} ms", self.stats.fps(), self.stats.worst(), self.stats.max_gap_ms),
            format!("gpu {:5.2} ms   {}   world {}x{}", self.stats.gpu_ms, self.render.backend, self.pixel_size().0, self.pixel_size().1),
            format!("instances {}   out {} KB/frame", self.stats.insts, self.screen.bytes_last / 1024),
            format!("pos {:.0}, {:.0}  yaw {:.0}°  v{}", self.pos().x, self.pos().z, self.yaw().to_degrees().rem_euclid(360.0), self.snap.version),
            format!("live {} things ({} moving)  {} burning cells  {} people", self.sim.things.len(), self.sim.things.live().filter(|t| !t.asleep).count(), self.sim.field.cells.values().filter(|c| c.props[crate::sim::props::P_FIRE] > 0.0).count(), self.sim.cast.npcs.len()),
            format!("events {}  llm queued {}  rules {} ({} the universe's own)", self.sim.log.total, self.sim.queued(), self.sim.rules.len(), self.sim.rules.iter().filter(|r| !r.builtin).count()),
            (if self.screen.truecolor { "truecolor" } else { "256 colours" }).to_string(),
        ];
        for (i, l) in lines.iter().enumerate() {
            let s = format!(" {l:<44}");
            self.screen.text(0, i as u16, &s, [180, 255, 180], [0, 0, 0], false);
        }
    }

    // ------------------------------------------------------------------ loop

    /// Run without a window, the screen written to `out`.
    pub fn run(&mut self, out: &mut dyn Write, stop: &AtomicBool, script: Option<&mut dyn FnMut(&mut App, f32) -> bool>) -> std::io::Result<()> {
        self.run_on(&mut Headless { out }, stop, script)
    }

    pub fn run_on(&mut self, front: &mut dyn Frontend, stop: &AtomicBool, mut script: Option<&mut dyn FnMut(&mut App, f32) -> bool>) -> std::io::Result<()> {
        let mut last = Instant::now();
        // Test hook: prove the terminal survives a panic.
        let panic_at = std::env::var("POCKET_TEST_PANIC_MS").ok().and_then(|v| v.parse::<u64>().ok()).map(|ms| Instant::now() + Duration::from_millis(ms));
        loop {
            let loop_start = Instant::now();
            let frame_interval = Duration::from_secs_f32(1.0 / self.settings.fps);
            if panic_at.is_some_and(|t| loop_start > t) {
                panic!("test panic (POCKET_TEST_PANIC_MS)");
            }
            if stop.load(Ordering::Relaxed) {
                self.quit = true;
            }
            // Input (non-blocking).
            front.input(self)?;
            if self.quit {
                break;
            }
            while let Ok(ev) = self.events.try_recv() {
                self.on_event(ev);
            }
            let now = Instant::now();
            let dt = now.duration_since(last).as_secs_f32().min(0.1);
            last = now;
            if let Some(s) = script.as_mut() {
                if !s(self, dt) {
                    break;
                }
            }
            if self.loading.is_some() {
                self.tick_loading(dt);
            } else {
                self.tick(dt);
            }

            // Frames in flight: at most two; keep only the newest result.
            let mut got = false;
            if self.render.rx.is_empty() && self.render.thread.as_ref().is_some_and(|t| t.is_finished()) {
                // The renderer died (e.g. a GPU fault): keep going on the CPU.
                crate::log::error("renderer thread stopped; switching to the CPU renderer");
                self.render = crate::render::cpu::spawn();
                self.outstanding = 0;
                self.say(None, "The GPU renderer stopped; continuing with the CPU renderer.", ACCENT);
            }
            while let Ok(f) = self.render.rx.try_recv() {
                self.outstanding = self.outstanding.saturating_sub(1);
                if f.width == 0 {
                    continue;
                }
                if let Some(g) = f.gpu_ms {
                    self.stats.gpu_ms = self.stats.gpu_ms * 0.8 + g * 0.2;
                }
                self.last_frame = Some(f);
                got = true;
            }
            if got {
                self.auto_detail();
            }
            // While loading, a slow trickle of frames keeps the renderer warm for the world flipping in.
            let interval = if self.loading.is_some() { Duration::from_millis(500) } else { frame_interval.mul_f32(0.9) };
            if self.outstanding < 2 && now.duration_since(self.last_request) >= interval {
                self.request_frame();
            }
            if self.loading.is_some() {
                if self.dirty {
                    self.compose_loading();
                    front.present(self)?;
                    self.dirty = false;
                }
            } else if got || self.dirty {
                self.compose();
                front.present(self)?;
                self.dirty = false;
                if got {
                    self.stats.shown += 1;
                    if let Some(prev) = self.stats.frames.back() {
                        self.stats.max_gap_ms = self.stats.max_gap_ms.max(prev.elapsed().as_secs_f32() * 1000.0);
                    }
                    self.stats.frames.push_back(Instant::now());
                    while self.stats.frames.len() > 90 {
                        self.stats.frames.pop_front();
                    }
                }
            }
            let ms = loop_start.elapsed().as_secs_f32() * 1000.0;
            self.stats.max_loop_ms = self.stats.max_loop_ms.max(ms);
            self.stats.worst_recent.push_back((Instant::now(), ms));
            while self.stats.worst_recent.front().is_some_and(|x| x.0.elapsed() > Duration::from_secs(2)) {
                self.stats.worst_recent.pop_front();
            }
            // Wait for input or the next frame slot (whichever first).
            let spent = loop_start.elapsed();
            let budget = Duration::from_millis(4).max(frame_interval.saturating_sub(spent) / 2);
            front.wait(budget);
        }
        Ok(())
    }

    /// Window detail on auto: more lines while the GPU keeps up with the frame
    /// rate cap, fewer when it falls behind. Steps rarely, so it holds still.
    fn auto_detail(&mut self) {
        const MIN: f32 = 144.0;
        if self.win.is_none() || self.settings.window_detail != crate::settings::AUTO || self.loading.is_some() || self.stats.gpu_ms <= 0.0 {
            return;
        }
        if self.auto_at.elapsed() < Duration::from_millis(1500) {
            return;
        }
        // Judged by the frames that arrive, not the GPU's own time: a GPU
        // with room to spare clocks itself down, so its time alone says
        // "busy" when it isn't.
        let cap = self.settings.fps;
        let fps = self.stats.fps();
        let interval = 1000.0 / cap;
        let (_, vh, _) = self.layout();
        let full = self.win.map(|g| vh as f32 * g.cell_h).unwrap_or(2160.0);
        let now = Instant::now();
        let lines = self.auto_lines;
        // Behind with the GPU near full (not the world loading, say, holding frames up).
        let next = if fps < cap * 0.93 && self.stats.gpu_ms > interval * 0.75 {
            // Behind: down, by about what it lacks (time grows with the pixels,
            // so with the square of the lines), and don't come back up here soon.
            self.auto_ceiling = Some((lines * 0.95, now + Duration::from_secs(30)));
            lines * ((fps / cap).sqrt() * 0.97).max(0.7)
        } else if fps >= cap * 0.97 && self.stats.gpu_ms < interval * 0.8 {
            let ceiling = self.auto_ceiling.filter(|c| c.1 > now).map(|c| c.0).unwrap_or(f32::MAX);
            (lines * 1.1).min(ceiling)
        } else {
            lines
        };
        let next = next.clamp(MIN, 2160.0).min(full.max(MIN));
        if std::env::var_os("POCKET_AUTO_DEBUG").is_some() {
            eprintln!("auto: fps {fps:.1} gpu {:.2} lines {lines:.0} -> {next:.0} loop {:.1} gap {:.1}", self.stats.gpu_ms, self.stats.worst(), self.stats.max_gap_ms);
        }
        // Only clear changes: each one starts the blended picture over.
        if (next / lines - 1.0).abs() > 0.04 {
            self.auto_lines = next;
            self.auto_at = now;
        }
    }

    pub fn resize(&mut self, w: u16, h: u16) {
        self.screen.resize(w, h);
        self.last_frame = None;
        self.dirty = true;
    }

    pub fn paste(&mut self, s: &str) {
        // Into a field of the menu (an API key, a name), when it is open.
        if self.menu.is_some() {
            self.ui.feed(&crate::window::Ev::Paste(s.to_string()));
            self.dirty = true;
            return;
        }
        if self.mode != Mode::Walk {
            self.stop_hearing();
            self.insert_input(&s.replace(['\n', '\r'], " "));
            self.dirty = true;
        }
    }

    pub fn focus_lost(&mut self) {
        self.keys.down.clear();
    }

    /// Mouse look (window): `dx`, `dy` in the window's pixels.
    pub fn mouse_look(&mut self, dx: f32, dy: f32) {
        if self.mode != Mode::Walk || self.menu.is_some() || self.loading.is_some() || self.sim.fallen() {
            return;
        }
        const RAD_PER_PX: f32 = 0.0022;
        if self.sim.player.aboard.is_none() || self.noclip {
            self.sim.player.yaw += dx * RAD_PER_PX;
        }
        self.pitch = (self.pitch - dy * RAD_PER_PX).clamp(PITCH_MIN, PITCH_MAX);
        self.dirty = true;
    }

    /// Whether mouse look applies now (walking, nothing open).
    pub fn wants_mouse(&self) -> bool {
        self.mode == Mode::Walk && self.menu.is_none() && self.loading.is_none()
    }

    pub fn last_frame(&self) -> Option<&Frame> {
        match self.loading_backdrop() {
            Some(f) => Some(f),
            None => self.last_frame.as_ref(),
        }
    }

    pub fn view_rows(&self) -> u16 {
        if self.loading_backdrop().is_some() {
            return self.screen.h;
        }
        self.layout().1
    }

    /// The view behind the loading screen, while there is one.
    fn loading_backdrop(&self) -> Option<&Frame> {
        self.loading.as_ref()?;
        self.backdrop.as_ref()?.frame()
    }

    /// Frames are drawn apart and blended (a window, the GPU, the setting on).
    fn blending(&self) -> bool {
        self.win.is_some() && !self.render.cpu_fallback && self.settings.blend_frames
    }

    /// The detail drawn now, for the settings screen: "321p", "321p, blended to 642p".
    fn detail_now(&self) -> String {
        let (_, h, _) = self.pixel_size();
        let (_, vh, _) = self.layout();
        let view = self.win.map(|g| (vh as f32 * g.cell_h) as u32).unwrap_or(h);
        if self.blending() && h * 2 > h.min(view) { format!("{h}p, blended to {}p", (h * 2).min(view)) } else { format!("{h}p") }
    }

    pub fn set_window(&mut self, g: WinGeom) {
        self.win = Some(g);
        self.last_frame = None;
        self.dirty = true;
    }

    /// One line on how fast it ran: frames shown, render time, frame size.
    pub fn stats_line(&self) -> String {
        let (pw, ph, _) = self.pixel_size();
        format!("{} frames, fps {:.1}, gpu {:.2} ms, world {pw}x{ph}, worst gap {:.0} ms", self.stats.shown, self.stats.fps(), self.stats.gpu_ms, self.stats.max_gap_ms)
    }

    pub fn quit_now(&mut self) {
        self.quit = true;
    }

    pub fn shutdown(&mut self) {
        self.together_end();
        if let Some(o) = self.sound.take() {
            o.send(crate::audio::mix::Cmd::Clear);
        }
        self.save_player();
        self.save_characters();
        self.render.shutdown();
    }
}


/// Words for a speech box: wrapped narrow, a few rows, the rest cut with "…".
fn bubble_lines(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = term::wrap(text.trim(), BUBBLE_W).into_iter().filter(|l| !l.is_empty()).collect();
    if lines.len() > BUBBLE_ROWS {
        lines.truncate(BUBBLE_ROWS);
        let last = &mut lines[BUBBLE_ROWS - 1];
        while last.chars().count() > BUBBLE_W - 1 {
            last.pop();
        }
        last.push('…');
    }
    lines
}

fn speaker_color(name: &str) -> [u8; 3] {
    let h = crate::noise::pcg(name.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)));
    let hue = crate::noise::u2f(h);
    let c = crate::lang::api::Api::Hsv;
    let mut out = [0.0; 3];
    crate::lang::api::eval(c, &[hue, 0.45, 1.0], &mut out);
    [(out[0] * 255.0) as u8, (out[1] * 255.0) as u8, (out[2] * 255.0) as u8]
}

const RAMP: &[u8] = b" .:-=+*#%@";

/// A message about the traveler, said to them: "the traveler pets Mog" →
/// "You pet Mog", "Mog nips the traveler's hand" → "Mog nips your hand".
fn you(msg: &str) -> String {
    you_as(msg, "the traveler")
}

/// A message about `name` said to them: "Bea picks up the stone" → "You
/// pick up the stone".
fn you_as(msg: &str, name: &str) -> String {
    let s = msg.replace(&format!("{name}'s"), "your");
    let s = match s.strip_prefix(&format!("{name} ")) {
        Some(rest) => {
            let (verb, tail) = rest.split_once(' ').unwrap_or((rest, ""));
            let verb = match verb {
                "is" => "are".to_string(),
                "has" => "have".to_string(),
                "won't" | "can't" | "doesn't" => verb.to_string(),
                // tries → try, carries → carry; but dies, lies, ties just lose the s.
                v if v.ends_with("ies") && v.len() > 4 => format!("{}y", &v[..v.len() - 3]),
                v if ["shes", "ches", "xes", "sses"].iter().any(|e| v.ends_with(e)) => v[..v.len() - 2].to_string(),
                v if v.ends_with('s') && !v.ends_with("ss") => v[..v.len() - 1].to_string(),
                v => v.to_string(),
            };
            let verb = if verb == "doesn't" { "don't".to_string() } else { verb };
            if tail.is_empty() { format!("You {verb}") } else { format!("You {verb} {tail}") }
        }
        None => s,
    };
    let s = s.replace(&format!(" {name}"), " you");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().chain(c).collect(),
        None => s,
    }
}

/// Monochrome text with per-frame auto-contrast (no colour to carry the image).
fn frame_to_mono(f: &Frame, ascii: bool, w: u16, vh: u16) -> String {
    let lums: Vec<f32> = f.pixels.iter().map(|p| lum(render::unpack(*p))).collect();
    let mut sorted = lums.clone();
    sorted.sort_by(|a, b| a.total_cmp(b));
    let lo = sorted[sorted.len() / 50];
    let hi = sorted[sorted.len() * 49 / 50].max(lo + 0.05);
    let norm = |l: f32| ((l - lo) / (hi - lo)).clamp(0.0, 1.0);
    let at = |x: u32, y: u32| lums[(y.min(f.height - 1) * f.width + x.min(f.width - 1)) as usize];
    let shades = [' ', '░', '▒', '▓', '█'];
    let mut out = String::new();
    for y in 0..vh as u32 {
        for x in 0..w as u32 {
            if ascii {
                let l = norm(at(x, y));
                out.push(RAMP[((l * (RAMP.len() - 1) as f32).round() as usize).min(RAMP.len() - 1)] as char);
            } else {
                let (x, y) = (x * 2, y * 2);
                let l = (norm(at(x, y)) + norm(at(x + 1, y)) + norm(at(x, y + 1)) + norm(at(x + 1, y + 1))) * 0.25;
                out.push(shades[((l * 4.0).round() as usize).min(4)]);
            }
        }
        out.push('\n');
    }
    out
}

fn lum(c: [u8; 3]) -> f32 {
    (0.299 * c[0] as f32 + 0.587 * c[1] as f32 + 0.114 * c[2] as f32) / 255.0
}

/// Quadrant glyphs by mask: bit 0 top-left, 1 top-right, 2 bottom-left, 3 bottom-right.
const QUADS: [char; 16] = [' ', '▘', '▝', '▀', '▖', '▌', '▞', '▛', '▗', '▚', '▐', '▜', '▄', '▙', '▟', '█'];

/// Splits to try, each with the top-left in the foreground so every split appears
/// once. Whole and half-block first so they win ties (steadier, cheaper to send).
const SPLITS: [usize; 8] = [15, 3, 5, 9, 1, 7, 11, 13];

/// A split whose two colours are this close (per channel) is drawn flat.
const FLAT: i32 = 6;

/// The quadrant cell that best fits a 2×2 block (tl, tr, bl, br): the split into
/// two colours with the least squared error, each colour its group's mean.
fn quad_cell(p: [[u8; 3]; 4]) -> Cell {
    let mut best = (u32::MAX, 15, [0u8; 3], [0u8; 3]);
    for mask in SPLITS {
        let mut sum = [[0u32; 3]; 2];
        let mut n = [0u32; 2];
        for (i, c) in p.iter().enumerate() {
            let g = (mask >> i & 1) as usize;
            n[g] += 1;
            for k in 0..3 {
                sum[g][k] += c[k] as u32;
            }
        }
        let mean = |g: usize| -> [u8; 3] { std::array::from_fn(|k| if n[g] == 0 { 0 } else { ((sum[g][k] + n[g] / 2) / n[g]) as u8 }) };
        let (fg, bg) = (mean(1), mean(0));
        let err: u32 = p
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let m = if mask >> i & 1 == 1 { fg } else { bg };
                (0..3).map(|k| (c[k] as i32 - m[k] as i32).pow(2) as u32).sum::<u32>()
            })
            .sum();
        if err < best.0 {
            best = (err, mask, fg, bg);
        }
    }
    let (_, mask, fg, bg) = best;
    if mask == 15 {
        return Cell { ch: ' ', fg, bg: fg, bold: false };
    }
    if (0..3).all(|k| (fg[k] as i32 - bg[k] as i32).abs() <= FLAT) {
        let ones = mask.count_ones();
        let flat = std::array::from_fn(|k| ((fg[k] as u32 * ones + bg[k] as u32 * (4 - ones) + 2) / 4) as u8);
        return Cell { ch: ' ', fg: flat, bg: flat, bold: false };
    }
    Cell { ch: QUADS[mask], fg, bg, bold: false }
}

/// Convert a rendered frame to cells (quadrants or coloured ASCII), scaling
/// with nearest-neighbour if the frame size differs (CPU fallback).
pub fn frame_to_cells(f: &Frame, screen: &mut Screen, w: u16, vh: u16, ascii: bool) {
    let (cols_px, rows_px) = if ascii { (w as u32, vh as u32) } else { (w as u32 * 2, vh as u32 * 2) };
    let px = |x: u32, y: u32| -> [u8; 3] {
        let sx = (x * f.width / cols_px).min(f.width - 1);
        let sy = (y * f.height / rows_px).min(f.height - 1);
        render::unpack(f.pixels[(sy * f.width + sx) as usize])
    };
    for y in 0..vh {
        for x in 0..w {
            let cell = if ascii {
                let c = px(x as u32, y as u32);
                let l = lum(c);
                let ch = RAMP[((l * (RAMP.len() - 1) as f32 * 1.15).round() as usize).min(RAMP.len() - 1)] as char;
                let m = c[0].max(c[1]).max(c[2]).max(1) as f32;
                let k = (255.0 / m).min(3.0);
                let fg = [(c[0] as f32 * k).min(255.0) as u8, (c[1] as f32 * k).min(255.0) as u8, (c[2] as f32 * k).min(255.0) as u8];
                Cell { ch, fg, bg: [0, 0, 0], bold: false }
            } else {
                let (x, y) = (x as u32 * 2, y as u32 * 2);
                quad_cell([px(x, y), px(x + 1, y), px(x, y + 1), px(x + 1, y + 1)])
            };
            screen.set(x, y, cell);
        }
    }
}

/// Plain-text rendering for `pocket snapshot`.
pub fn frame_to_text(f: &Frame, ascii: bool, mono: bool, truecolor: bool) -> String {
    let (w, vh) = if ascii { (f.width as u16, f.height as u16) } else { ((f.width / 2) as u16, (f.height / 2) as u16) };
    if mono {
        return frame_to_mono(f, ascii, w, vh);
    }
    let mut s = Screen::new(w, vh, truecolor);
    frame_to_cells(f, &mut s, w, vh, ascii);
    let mut out = String::new();
    for y in 0..vh {
        let mut pen: Option<([u8; 3], [u8; 3])> = None;
        for x in 0..w {
            let c = s.get(x, y).unwrap_or(Cell::BLANK);
            if mono {
                let ch = if ascii {
                    c.ch
                } else {
                    let l = (lum(c.fg) + lum(c.bg)) * 0.5;
                    [' ', '░', '▒', '▓', '█'][((l * 5.0) as usize).min(4)]
                };
                out.push(ch);
            } else {
                if pen != Some((c.fg, c.bg)) {
                    if truecolor {
                        out.push_str(&format!("\x1b[38;2;{};{};{};48;2;{};{};{}m", c.fg[0], c.fg[1], c.fg[2], c.bg[0], c.bg[1], c.bg[2]));
                    } else {
                        out.push_str(&format!("\x1b[38;5;{};48;5;{}m", term::ansi256(c.fg), term::ansi256(c.bg)));
                    }
                    pen = Some((c.fg, c.bg));
                }
                out.push(c.ch);
            }
        }
        if !mono {
            out.push_str("\x1b[0m");
        }
        out.push('\n');
    }
    out
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{Llm, Msg};
    use crate::model::{Live, WorldModel};
    use crossterm::event::{KeyEventState, KeyModifiers};
    use parking_lot::Mutex;

    fn key(code: KeyCode, kind: KeyEventKind) -> KeyEvent {
        KeyEvent { code, modifiers: KeyModifiers::NONE, kind, state: KeyEventState::NONE }
    }

    #[test]
    fn quad_cells_fit_the_block() {
        let (k, s, r) = ([0, 0, 0], [120, 180, 255], [200, 40, 40]);
        // Top / bottom: the half-block, colours exact.
        let c = quad_cell([s, s, k, k]);
        assert_eq!((c.ch, c.fg, c.bg), ('▀', s, k));
        // A vertical edge, a corner and a diagonal.
        assert_eq!(quad_cell([s, k, s, k]).ch, '▌');
        let c = quad_cell([k, k, k, r]);
        assert_eq!((c.ch, c.fg, c.bg), ('▛', k, r));
        assert_eq!(quad_cell([s, k, k, s]).ch, '▚');
        // Flat and nearly flat blocks are plain cells.
        assert_eq!(quad_cell([s; 4]).ch, ' ');
        let c = quad_cell([[100, 100, 100], [103, 100, 100], [100, 100, 100], [100, 100, 100]]);
        assert_eq!((c.ch, c.bg), (' ', [101, 100, 100]));
        // Three colours: the odd pair is averaged, the rest stays exact.
        let c = quad_cell([s, s, k, r]);
        assert_eq!((c.ch, c.fg), ('▀', s));
    }

    fn reply(sys: &str, msgs: &[Msg]) -> String {
        let user = msgs.last().map(|m| m.text.as_str()).unwrap_or("");
        let lh = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/good/lighthouse.js")).unwrap();
        if sys.contains("You are a character") {
            return "You look like you fell from the sky.".into();
        }
        if user.contains("is making something in the world") && user.contains("basement") {
            // Set into the ground, and lifted by mistake: it still goes into the ground.
            let house = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/physics/basement_house.js")).unwrap();
            return format!("```json\n{{\"summary\": \"a house with a basement\", \"reuse\": null, \"placements\": [{{\"right\": 0, \"forward\": 0, \"lift\": 1.0}}]}}\n```\n```js\n{house}\n```");
        }
        if user.contains("is making something in the world") {
            return format!("```json\n{{\"summary\": \"a lighthouse\", \"reuse\": null, \"placements\": [{{\"right\": 0, \"forward\": 0}}]}}\n```\n```js\n{lh}\n```");
        }
        if sys.contains("physics and common sense") && user.contains("basement") {
            return r#"{"narration": "", "make": [{"text": "a house with a basement over there"}]}"#.into();
        }
        if sys.contains("physics and common sense") && user.contains("lighthouse") {
            return r#"{"narration": "", "make": [{"text": "a lighthouse on that hill"}]}"#.into();
        }
        if sys.contains("You decide what a character") {
            return r#"{"action": "ignore", "line": null}"#.into();
        }
        "ok".into()
    }

    fn make_app() -> (App, Arc<Db>) {
        let (a, d, _) = make_app_gpu();
        (a, d)
    }

    fn make_app_gpu() -> (App, Arc<Db>, Option<Arc<crate::render::gpu::Gpu>>) {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("pocket-app-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.pocket");
        let db = Db::create(&path, 4242, "a quiet test valley", &serde_json::to_string(&crate::world::Look::default()).unwrap()).unwrap();
        db.kv_set("genesis", "done").unwrap();
        // One character, standing right next to the spawn.
        let gpu = crate::render::gpu::Gpu::new().ok();
        let live = Arc::new(Mutex::new(Live::default()));
        let mut model = WorldModel::load(db.clone(), gpu.clone(), live.clone()).unwrap();
        let spawn = model.snapshot().unwrap().spawn;
        let persona = crate::world::Persona { name: "Mara".into(), ..Default::default() };
        db.tx(|tx| {
            let v = crate::db::add_version(tx, None, "region", "test region", None)?;
            crate::db::add_character(tx, crate::world::region_of(spawn.x, spawn.z), &serde_json::to_string(&persona)?, spawn.x, spawn.z + 2.5, v)?;
            crate::db::set_region(tx, 0, 0, "done", "{\"name\": \"Test Vale\"}", Some(v))?;
            Ok(())
        })
        .unwrap();
        let mut model = WorldModel::load(db.clone(), gpu.clone(), live.clone()).unwrap();
        let snap = model.snapshot().unwrap();
        let llm = Llm::scripted(db.clone(), Arc::new(|s: &str, m: &[Msg]| reply(s, m)));
        let (tx, rx) = crossbeam_channel::unbounded();
        let brain = Brain::start(model, Some(llm.clone()), tx, false);
        let render = match &gpu {
            Some(g) => crate::render::gpu::spawn(g.clone()),
            None => crate::render::cpu::spawn(),
        };
        let app = App::new(Setup { db: db.clone(), brain, events: rx, render, snap, live, truecolor: true, size: (100, 34), llm: Some(llm), genesis: false, together: None, seed: None });
        (app, db, gpu)
    }

    /// Look at the creations page of a real world (a copy of POCKET_WORLD):
    /// `POCKET_WORLD=~/.pocket/universes/x.pocket cargo test --release real_world_creations -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_world_creations() {
        let Ok(src) = std::env::var("POCKET_WORLD") else { return };
        let dir = std::env::temp_dir().join(format!("pocket-real-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.pocket");
        std::fs::copy(&src, &path).unwrap();
        let db = Db::open(&path).unwrap();
        let live = Arc::new(Mutex::new(Live::default()));
        let mut model = WorldModel::load(db.clone(), None, live.clone()).unwrap();
        let snap = model.snapshot().unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        let brain = Brain::start(model, None, tx, false);
        let mut app = App::new(Setup { db: db.clone(), brain, events: rx, render: crate::render::cpu::spawn(), snap, live, truecolor: true, size: (110, 44), llm: None, genesis: false, together: None, seed: None });
        app.open_menu(false);
        app.menu_page(menu::Page::Creations);
        for _ in 0..3 {
            app.compose();
            println!("{}", screen_text(&app));
            app.on_key(key(KeyCode::Tab, KeyEventKind::Press));
        }
        app.shutdown();
    }

    /// Run the loop with `steps(app, elapsed_seconds) -> keep going`.
    fn drive(app: &mut App, secs: f32, mut steps: impl FnMut(&mut App, f32) -> bool) {
        let stop = AtomicBool::new(false);
        let mut t = 0.0f32;
        let mut sink = Vec::new();
        let mut f = |a: &mut App, dt: f32| {
            t += dt;
            t < secs && steps(a, t)
        };
        app.run(&mut sink, &stop, Some(&mut f)).unwrap();
        assert!(!sink.is_empty(), "something was drawn");
    }

    fn log_has(app: &App, s: &str) -> bool {
        app.log.iter().any(|l| l.text.contains(s) || l.speaker.as_deref().is_some_and(|sp| format!("{sp}: {}", l.text).contains(s)))
    }

    #[test]
    fn messages_about_the_traveler_are_said_to_them() {
        assert_eq!(you("the traveler pets Mog"), "You pet Mog");
        assert_eq!(you("the traveler climbs into the hand cart (W/S speed)"), "You climb into the hand cart (W/S speed)");
        assert_eq!(you("Mog nips the traveler's hand and wriggles away"), "Mog nips your hand and wriggles away");
        assert_eq!(you("the traveler is already riding"), "You are already riding");
        assert_eq!(you("the traveler tries to use the juniper"), "You try to use the juniper");
        assert_eq!(you("the traveler carries Mog"), "You carry Mog");
        assert_eq!(you("the traveler ties the rope"), "You tie the rope");
        assert_eq!(you("the traveler won't throw Mog"), "You won't throw Mog");
        assert_eq!(you("Mog darts out of the traveler's reach"), "Mog darts out of your reach");
        assert_eq!(you("too far away (3.0 m)"), "Too far away (3.0 m)");
    }

    #[test]
    fn arrows_move_the_caret_in_the_line() {
        let (mut app, _db) = make_app();
        let k = |c| key(c, KeyEventKind::Press);
        app.on_key(k(KeyCode::Char('/')));
        app.stop_hearing();
        for c in "dig hole".chars() {
            app.on_key(k(KeyCode::Char(c)));
        }
        for _ in 0..4 {
            app.on_key(k(KeyCode::Left));
        }
        for c in "a ".chars() {
            app.on_key(k(KeyCode::Char(c)));
        }
        assert_eq!(app.input, "/dig a hole");
        app.on_key(k(KeyCode::Home));
        app.on_key(k(KeyCode::Backspace));
        assert_eq!(app.input, "/dig a hole", "the / stays");
        app.on_key(k(KeyCode::Delete));
        app.on_key(k(KeyCode::End));
        app.on_key(k(KeyCode::Char('!')));
        assert_eq!(app.input, "/ig a hole!");
    }

    #[test]
    fn e_greets_the_person_in_front() {
        let (mut app, _db) = make_app();
        app.sim.player.yaw = 0.0;
        let mut done = false;
        drive(&mut app, 10.0, |a, _t| {
            if let Some(n) = a.sim.cast.npcs.first_mut() {
                n.a.task = Some(crate::sim::actor::Task::Face { target: Target::Actor(ActorId::Player), until: f64::MAX });
                n.think_at = f64::MAX;
                n.plan.clear();
            }
            if a.talk_hint.is_some() {
                a.compose();
                let shown = screen_text(a);
                assert!(shown.contains("E nod") && shown.contains("Enter talk to Mara"), "the keys say what they do:\n{shown}");
                a.on_key(key(KeyCode::Char('e'), KeyEventKind::Press));
                done = true;
                return false;
            }
            true
        });
        assert!(done, "Mara was in front");
        assert!(log_has(&app, "You nod at Mara"), "log: {:?}", app.log.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        app.shutdown();
    }

    /// Conjuring a house with a basement (set into the ground): its ground
    /// floor at the ground, the earth cleared round its basement, the
    /// traveler walking down its stairs into it; undone, the ground is whole.
    #[test]
    fn a_conjured_house_with_a_basement() {
        let (mut app, _db) = make_app();
        app.sim.player.yaw = std::f32::consts::PI;
        let mut phase = 0;
        let mut cellar = Vec3::ZERO;
        drive(&mut app, 40.0, |a, _t| {
            match phase {
                0 => {
                    for c in "/a house with a basement over there".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    phase = 1;
                }
                1 if log_has(a, "Built the basement house") => {
                    let snap = a.snap.clone();
                    let t = &snap.terrain;
                    let p = snap.instances.iter().find(|p| snap.type_of(p.type_id).is_some_and(|t| t.name() == "basement house")).expect("placed").clone();
                    let e = snap.type_of(p.type_id).unwrap();
                    let g = p.gpu(e, 1.0);
                    let front = g.from_local(Vec3::new(0.0, 0.0, 4.2));
                    assert!((p.pos.y - t.height(front.x, front.z)).abs() < 0.2, "its ground floor at the ground, not lifted: {} vs {}", p.pos.y, t.height(front.x, front.z));
                    cellar = g.from_local(Vec3::new(1.0, -1.5, 0.0));
                    assert!(!t.solid(cellar), "air in the basement");
                    assert!(t.solid(g.from_local(Vec3::new(3.1, -1.5, 0.0))), "earth round it");
                    // Down the stairs from their top.
                    a.sim.player.pos = g.from_local(Vec3::new(-1.9, 0.02, -1.8));
                    let down = g.from_local(Vec3::new(0.0, 0.0, 1.0)) - g.from_local(Vec3::ZERO);
                    for _ in 0..240 {
                        a.sim.walk(ActorId::Player, down * (1.4 / 60.0));
                        a.sim.step_footing(ActorId::Player, 1.0 / 60.0);
                    }
                    let me = a.sim.player.pos;
                    assert!((me.y - (p.pos.y - 2.6)).abs() < 0.3, "walked down into the basement: {} (its floor {})", me.y, p.pos.y - 2.6);
                    for c in "/undo".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    phase = 2;
                }
                2 if log_has(a, "Undone") => {
                    assert!(a.snap.terrain.solid(cellar), "the ground is whole again");
                    phase = 3;
                    return false;
                }
                _ => {}
            }
            true
        });
        assert_eq!(phase, 3, "log: {:?}", app.log.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        app.shutdown();
    }

    #[test]
    fn talk_create_undo_via_keys() {
        let (mut app, _db) = make_app();
        // Keep Mara where she is, in front of us.
        app.sim.player.yaw = 0.0;
        let mut phase = 0;
        drive(&mut app, 40.0, |a, _t| {
            if let Some(n) = a.sim.cast.npcs.first_mut() {
                if phase < 3 {
                    // Keep Mara standing still, facing us.
                    n.a.task = Some(crate::sim::actor::Task::Face { target: Target::Actor(ActorId::Player), until: f64::MAX });
                    n.think_at = f64::MAX;
                    n.plan.clear();
                }
            }
            match phase {
                0 if a.talk_hint.is_some() => {
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    assert!(matches!(a.mode, Mode::Talk(_)));
                    for c in "hello there".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    phase = 1;
                }
                1 if a.replying.is_none() && log_has(a, "fell from the sky") => {
                    a.on_key(key(KeyCode::Esc, KeyEventKind::Press));
                    assert_eq!(a.mode, Mode::Walk);
                    a.on_key(key(KeyCode::Char('/'), KeyEventKind::Press));
                    assert_eq!(a.mode, Mode::Command);
                    for c in "a lighthouse on that hill".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    phase = 2;
                }
                2 if log_has(a, "Built the lighthouse") => {
                    assert!(a.snap.instances.iter().any(|p| a.snap.type_of(p.type_id).is_some_and(|t| t.name() == "lighthouse")));
                    for c in "/undo".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    phase = 3;
                }
                3 if log_has(a, "Undone") => {
                    assert!(a.snap.instances.is_empty());
                    phase = 4;
                    return false;
                }
                _ => {}
            }
            true
        });
        assert_eq!(phase, 4, "log: {:?}", app.log.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        assert!(log_has(&app, "Mara: You look like you fell from the sky."));
        app.shutdown();
    }

    fn region_reply(sys: &str, msgs: &[Msg]) -> String {
        let user = msgs.last().map(|m| m.text.as_str()).unwrap_or("");
        if user.contains("Plan the story layer") {
            let r = user.split("Region (").nth(1).and_then(|s| s.split(')').next()).unwrap_or("0, 0").replace(", ", "_").replace('-', "m");
            return format!(
                "```json\n{{\"name\": \"Vale {r}\", \"mood\": \"quiet\", \"facts\": [], \"new_types\": [{{\"name\": \"hut {r}\", \"description\": \"hut\", \"size_m\": [6, 6, 5], \"tags\": [\"building\"]}}],
                 \"landmarks\": [{{\"type\": \"hut {r}\", \"x\": 128, \"z\": 128, \"rot\": 0}}], \"settlement\": null, \"characters\": [{{\"name\": \"Ola {r}\", \"home_x\": 120, \"home_z\": 120}}]}}\n```"
            );
        }
        if user.contains("Object type to write") {
            let name = user.split('"').nth(1).unwrap_or("hut");
            let cottage = std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/mock/cottage.js")).unwrap();
            return format!("```js\n{}\n```", cottage.replace("keeper's cottage", name));
        }
        reply(sys, msgs)
    }

    /// Walk in a straight line while regions generate and flip in; no frame may
    /// take more than 50 ms. (Slow: run with --ignored.)
    #[test]
    #[ignore]
    fn long_walk_with_generation_has_no_hitches() {
        let (mut app, db, gpu) = make_app_gpu();
        let llm = Llm::scripted(db.clone(), Arc::new(|s: &str, m: &[Msg]| region_reply(s, m)));
        // Swap in a brain that plans regions (same GPU device as the renderer).
        let live = app.live.clone();
        let mut model = WorldModel::load(db.clone(), gpu, live).unwrap();
        app.snap = model.snapshot().unwrap();
        let (tx, rx) = crossbeam_channel::unbounded();
        app.brain = Brain::start(model, Some(llm.clone()), tx, false);
        app.events = rx;
        app.noclip = true;
        let distance: f32 = std::env::var("POCKET_WALK_M").ok().and_then(|v| v.parse().ok()).unwrap_or(1500.0);
        let start = app.pos();
        let mut warm = 0.0;
        drive(&mut app, 900.0, |a, t| {
            if t < 2.0 {
                return true;
            }
            if warm == 0.0 {
                a.stats.max_loop_ms = 0.0;
                a.stats.max_gap_ms = 0.0;
                warm = t;
            }
            a.keys.down.insert('w');
            let d = Vec3::new(a.pos().x - start.x, 0.0, a.pos().z - start.z).length();
            d < distance
        });
        let regions = app.snap.regions.len();
        eprintln!("walked {distance} m: worst loop {:.1} ms, worst gap between frames {:.1} ms, {} frames, {regions} regions generated", app.stats.max_loop_ms, app.stats.max_gap_ms, app.stats.shown);
        assert!(regions >= 4, "regions kept appearing");
        assert!(app.stats.max_loop_ms < 50.0, "worst loop {:.1} ms", app.stats.max_loop_ms);
        assert!(app.stats.max_gap_ms < 50.0, "worst frame gap {:.1} ms", app.stats.max_gap_ms);
        app.shutdown();
    }

    #[test]
    fn held_key_moves_at_constant_speed_and_stops_on_release() {
        let (mut app, _db) = make_app();
        app.noclip = true; // isolate key handling from collisions
        let start = app.pos();
        let mut pressed_at = None;
        let mut released_pos = None;
        drive(&mut app, 3.0, |a, t| {
            if pressed_at.is_none() && t > 0.3 {
                a.on_key(key(KeyCode::Char('w'), KeyEventKind::Press));
                pressed_at = Some(t);
            }
            if let Some(p0) = pressed_at {
                if t - p0 >= 1.0 && released_pos.is_none() {
                    a.on_key(key(KeyCode::Char('w'), KeyEventKind::Release));
                    released_pos = Some(a.pos());
                }
            }
            true
        });
        let rp = released_pos.unwrap();
        let moved = Vec3::new(rp.x - start.x, 0.0, rp.z - start.z).length();
        assert!((moved - WALK_SPEED).abs() < WALK_SPEED * 0.2, "1 s held moved {moved} m");
        let after = Vec3::new(app.pos().x - rp.x, 0.0, app.pos().z - rp.z).length();
        assert!(after < 0.15, "kept moving {after} m after release");
        app.shutdown();
    }

    /// Shift pressed or let go while already walking runs or walks.
    #[test]
    fn shift_mid_walk_runs_and_letting_go_walks() {
        let (mut app, _db) = make_app();
        app.noclip = true;
        let shift = |kind| KeyEvent::new_with_kind(KeyCode::Modifier(ModifierKeyCode::LeftShift), KeyModifiers::SHIFT, kind);
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Press));
        drive(&mut app, 0.1, |_, _| true);
        assert!(!app.sim.player.running, "plain W walks");
        app.on_key(shift(KeyEventKind::Press));
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.sim.player.running, "Shift mid-walk runs");
        app.on_key(shift(KeyEventKind::Release));
        drive(&mut app, 0.1, |_, _| true);
        assert!(!app.sim.player.running, "letting go of Shift walks");
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Release));
        app.shutdown();
    }

    #[test]
    fn arrows_look_up_and_down_without_moving() {
        let (mut app, _db) = make_app();
        app.noclip = true;
        let start = app.pos();
        let p0 = app.pitch;
        app.on_key(key(KeyCode::Up, KeyEventKind::Press));
        drive(&mut app, 2.0, |_, _| true);
        app.on_key(key(KeyCode::Up, KeyEventKind::Release));
        assert!((app.pitch - PITCH_MAX).abs() < 1e-4, "held ↑ looks up to the limit, pitch {} from {p0}", app.pitch);
        let moved = Vec3::new(app.pos().x - start.x, 0.0, app.pos().z - start.z).length();
        assert!(moved < 0.01, "looking doesn't walk, moved {moved}");
        app.on_key(key(KeyCode::Down, KeyEventKind::Press));
        drive(&mut app, 0.3, |_, _| true);
        app.on_key(key(KeyCode::Down, KeyEventKind::Release));
        assert!(app.pitch < PITCH_MAX - 0.2, "↓ looks back down, pitch {}", app.pitch);
        app.shutdown();
    }
    /// Struck down: the view drops and tips, goes dark red with the world
    /// showing through, YOU DIED over it with who did it; keys do nothing
    /// but Enter, and only once the fall is over; Enter wakes the traveler
    /// at the world's beginning.
    #[test]
    fn falling_shows_you_died_and_enter_wakes_you() {
        let (mut app, _db) = make_app();
        let mara = app.sim.cast.npcs[0].def.id;
        app.sim.player.pos += Vec3::new(5.0, 0.0, 5.0);
        let fell_at = app.sim.player.pos;
        app.sim.wound(ActorId::Player, 1.0, ActorId::Npc(mara), "struck by Mara with the stick");
        assert!(app.sim.fallen());
        app.on_key(key(KeyCode::Enter, KeyEventKind::Press));
        drive(&mut app, 0.3, |_, _| true);
        assert!(app.sim.fallen(), "Enter does nothing mid-fall");
        assert!(app.camera().roll > 0.1, "the view tips over");
        drive(&mut app, 1.2, |_, _| true);
        let cam = app.camera();
        assert!(cam.roll > 1.3 && cam.pos.y < app.sim.player.pos.y + 0.5, "lying on the ground: roll {:.2}, eye {:.2} over the ground", cam.roll, cam.pos.y - app.sim.player.pos.y);
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Press));
        drive(&mut app, 0.4, |_, _| true);
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Release));
        let moved = Vec3::new(app.sim.player.pos.x - fell_at.x, 0.0, app.sim.player.pos.z - fell_at.z).length();
        assert!(moved < 0.05, "the fallen don't walk: {moved:.2} m");
        app.compose();
        let text = screen_text(&app);
        if std::env::var("POCKET_SHOW").is_ok() {
            eprintln!("{text}");
        }
        assert!(text.contains("Struck by Mara with the stick.") && text.contains("Press Enter") && text.contains("█   █"), "{text}");
        app.on_key(key(KeyCode::Enter, KeyEventKind::Press));
        assert!(!app.sim.fallen() && app.sim.health() == 1.0);
        let spawn = app.snap.spawn;
        assert!(Vec3::new(app.sim.player.pos.x - spawn.x, 0.0, app.sim.player.pos.z - spawn.z).length() < 0.01);
        assert_eq!(app.camera().roll, 0.0);
        app.shutdown();
    }

    fn screen_text(app: &App) -> String {
        app.screen.plain_text()
    }

    #[test]
    fn feed_shows_the_last_lines_and_the_journal_all() {
        let (mut app, _db) = make_app();
        for i in 0..80 {
            app.say(None, &format!("line number {i}"), TEXT);
        }
        app.compose();
        let small = screen_text(&app);
        assert!(small.contains("line number 79") && !small.contains("line number 60"), "{small}");
        // Lines said a while ago leave the feed.
        for l in app.log.iter_mut() {
            l.at -= Duration::from_secs(40);
        }
        app.compose();
        assert!(!screen_text(&app).contains("line number 79"));
        app.on_key(key(KeyCode::Char('1'), KeyEventKind::Press));
        assert_eq!(app.log_view, 1);
        app.compose();
        let all = screen_text(&app);
        assert!(all.contains("Journal") && all.contains("line number 79") && all.contains("line number 60"), "{all}");
        app.on_key(key(KeyCode::PageUp, KeyEventKind::Press));
        app.compose();
        let back = screen_text(&app);
        assert!(back.contains("lines back") && !back.contains("line number 79"), "{back}");
        app.on_key(key(KeyCode::Esc, KeyEventKind::Press));
        assert_eq!(app.log_view, 0);
        assert!(app.menu.is_none(), "Esc closes the journal first");
        app.on_key(key(KeyCode::Char('1'), KeyEventKind::Press));
        app.on_key(key(KeyCode::Char('1'), KeyEventKind::Press));
        assert_eq!(app.log_view, 0, "1 opens and closes it");
    }

    /// The small log keeps talk and what changes the world; everyday life
    /// shows a while, said-again lines count up, far news waits in the
    /// journal, whose filters pick what to see.
    #[test]
    fn log_keeps_what_matters_and_the_journal_keeps_all() {
        use crate::sim::Note;
        let (mut app, _db) = make_app();
        for _ in 0..3 {
            app.tell(Note::Ambient("The horse gives a snort.".into()));
        }
        app.tell(Note::Notable("Ganzorig made a felt saddle.".into()));
        app.tell(Note::Far { text: "Oyunaa made a drying rack.".into(), at: [400.0, 0.0, 0.0], made: true });
        app.tell(Note::Line { id: None, who: "Ganzorig".into(), text: format!("{}\nAny word from the stone grandfather?", "Hoy! ".repeat(30)) });
        app.compose();
        let small = screen_text(&app);
        assert!(small.contains("The horse gives a snort. ×3"), "{small}");
        assert!(small.contains("✦ Ganzorig made a felt saddle."), "{small}");
        assert!(!small.contains("drying rack"), "far news waits in the journal: {small}");
        assert!(small.contains("Ganzorig: Hoy!"), "{small}");
        // Everyday life gives way after a while.
        for l in app.log.iter_mut().filter(|l| l.kind == journal::Kind::Life) {
            l.at -= Duration::from_secs(30);
        }
        app.compose();
        let later = screen_text(&app);
        assert!(!later.contains("snort") && later.contains("felt saddle"), "{later}");
        // The journal has it all; Tab picks what to see.
        app.on_key(key(KeyCode::Char('1'), KeyEventKind::Press));
        app.compose();
        let all = screen_text(&app);
        assert!(all.contains("snort") && all.contains("✧ Oyunaa made a drying rack.") && !all.contains("stirring nearby"), "{all}");
        assert!(all.contains("\n    Any word from the stone grandfather?") || all.contains("   Any word from the stone grandfather?"), "wrapped lines hang under the speaker: {all}");
        for _ in 0..4 {
            app.on_key(key(KeyCode::Tab, KeyEventKind::Press));
        }
        assert_eq!(app.journal_filter, journal::Filter::Made);
        app.compose();
        let made = screen_text(&app);
        assert!(made.contains("drying rack") && !made.contains("felt saddle") && !made.contains("snort"), "makings, near and far: {made}");
        app.on_key(key(KeyCode::Tab, KeyEventKind::Press));
        assert_eq!(app.journal_filter, journal::Filter::Elsewhere);
        app.compose();
        let far = screen_text(&app);
        assert!(far.contains("drying rack") && !far.contains("felt saddle") && !far.contains("snort"), "{far}");
        // An incident is one line, rewritten as it goes, however much happens.
        for k in 1..=30 {
            app.tell(Note::Incident { id: 7, text: format!("Wildfire from the root kiln: {k} burnt."), at: [0.0, 0.0, 0.0], near: true });
        }
        assert_eq!(app.log.iter().filter(|l| l.incident == Some(7)).count(), 1);
        assert!(app.log.iter().any(|l| l.text == "Wildfire from the root kiln: 30 burnt."));
    }

    #[test]
    fn achievements_pop_up_are_kept_and_listed() {
        let (mut app, db) = make_app();
        let p = app.pos();
        app.sim.event("made", Some(ActorId::Player), Some("instance:1".into()), "the traveler made a lamp", Some(p), serde_json::json!({}));
        drive(&mut app, 0.2, |_, _| true);
        assert!(app.achievements.earned("word_made_real").is_some());
        assert!(app.achievements.earned("first_spark").is_none());
        app.compose();
        let s = screen_text(&app);
        assert!(s.contains("Word Made Real") && s.contains("Achievement"), "{s}");
        // Kept for you by this world's id, not in the world's file.
        let t = crate::achievements::Tracker::load(&db, &app.sim);
        assert!(t.earned("word_made_real").is_some());
        assert_eq!(t.count(), 1);
        assert!(db.kv_get("world_id").is_some());
        let (other, other_db) = make_app();
        assert_ne!(other_db.world_id(), db.world_id());
        assert_eq!(other.achievements.count(), 0, "another world starts with none");
        // Each is earned once.
        app.toasts.clear();
        app.sim.event("made", Some(ActorId::Player), Some("instance:2".into()), "the traveler made a cup", Some(p), serde_json::json!({}));
        drive(&mut app, 0.2, |_, _| true);
        assert!(app.toasts.is_empty());
        // F3 opens the list; Esc goes back to the pause menu, and again to the world.
        app.on_key(key(KeyCode::F(3), KeyEventKind::Press));
        app.compose();
        let s = screen_text(&app);
        assert!(s.contains("1 of 40 earned") && s.contains("Second Draft") && s.contains("just now"), "{s}");
        // Two fingers (or the wheel) scroll the list, and it stays scrolled.
        app.scroll(-2.0);
        app.compose();
        app.compose();
        assert!(app.menu.as_ref().is_some_and(|m| m.ach_scroll > 0.0), "the list scrolls");
        press(&mut app, KeyCode::Esc);
        assert!(screen_text(&app).contains("Paused"));
        press(&mut app, KeyCode::Esc);
        assert!(app.menu.is_none());
        // /undo is noticed by the app itself.
        app.command("/undo");
        assert!(app.achievements.earned("fresh_start").is_some());
    }

    #[test]
    fn work_in_progress_shows_until_done() {
        let (mut app, _db) = make_app();
        app.sim.has_llm = true;
        app.sim.act(ActorId::Player, Action::Do { text: "whittle a flute".into(), on: None, at: None }).unwrap();
        app.update_work();
        app.compose();
        let s = screen_text(&app);
        assert!(s.contains("doing") && s.contains("whittle a flute"), "{s}");
        // A long deed is written out in full, wrapped, not cut short.
        app.sim.act(ActorId::Player, Action::Do { text: "carve a long wooden stick into a sunwheel and set it turning on the mill by the river bend".into(), on: None, at: None }).unwrap();
        app.update_work();
        app.compose();
        let s = screen_text(&app);
        assert!(s.contains("river bend") && !s.lines().any(|l| l.contains("sunwheel") && l.contains('…')), "{s}");
        // No LLM here, so the world answers "nothing happens" at once.
        drive(&mut app, 0.5, |a, _| !a.sim.work().is_empty());
        app.update_work();
        app.compose();
        let s = screen_text(&app);
        assert!(s.contains("✓ done") && s.contains("whittle a flute"), "{s}");
        // Off: none shown.
        app.settings.show_work = crate::settings::ShowWork::Off;
        app.update_work();
        app.compose();
        assert!(!screen_text(&app).contains("whittle"));
    }

    #[test]
    fn a_poke_is_no_makeover() {
        let (mut app, _db) = make_app();
        let p = app.pos();
        let deed = |app: &mut App, data: serde_json::Value| app.sim.event("deed_on", Some(ActorId::Player), Some("npc:7".into()), "the traveler did something to the moth", Some(p), data);
        deed(&mut app, serde_json::json!({ "mood": true, "looks": false, "trust": -0.2 }));
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("trust_issues").is_some());
        assert!(app.achievements.earned("makeover").is_none(), "a change of feeling is no makeover");
        deed(&mut app, serde_json::json!({ "mood": false, "looks": true, "trust": 0.0 }));
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("makeover").is_some());
    }

    #[test]
    fn achievements_follow_the_fire() {
        let (mut app, _db) = make_app();
        let p = app.pos();
        let t = app.sim.t;
        let ev = |app: &mut App, kind: &str, actor: Option<ActorId>, subject: &str, at: Vec3| app.sim.event(kind, actor, Some(subject.into()), kind, Some(at), serde_json::json!({}));
        // Fire far away, nobody near: nothing.
        ev(&mut app, "ignited", None, "thing:90", p + Vec3::new(200.0, 0.0, 0.0));
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("first_spark").is_none());
        // The player uses something, and fire starts beside them, then spreads.
        ev(&mut app, "used", Some(ActorId::Player), "thing:1", p);
        ev(&mut app, "ignited", None, "thing:2", p + Vec3::new(2.0, 0.0, 0.0));
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("first_spark").is_some());
        assert!(app.achievements.earned("tinkerer").is_some());
        assert!(app.achievements.earned("chain_reaction").is_none());
        for i in 3..6 {
            ev(&mut app, "ignited", None, &format!("thing:{i}"), p + Vec3::new(i as f32 * 3.0, 0.0, 0.0));
        }
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("chain_reaction").is_some());
        assert!(app.sim.t - t < 120.0);
        // Thrown by a mount.
        ev(&mut app, "thrown", Some(ActorId::Npc(7)), "player", p);
        drive(&mut app, 0.1, |_, _| true);
        assert!(app.achievements.earned("thrown").is_some());
        assert_eq!(app.toasts.len(), 4, "one popup each, queued");
    }

    /// A key in a menu: it acts when the menu is next drawn.
    fn press(app: &mut App, code: KeyCode) {
        app.on_key(key(code, KeyEventKind::Press));
        app.compose();
    }

    #[test]
    fn quit_is_in_the_esc_menu_not_walk() {
        let (mut app, _db) = make_app();
        app.on_key(key(KeyCode::Char('q'), KeyEventKind::Press));
        assert!(!app.quit, "q while walking does nothing");
        press(&mut app, KeyCode::Esc);
        assert!(screen_text(&app).contains("Quit game"));
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit, "q in the menu quits");
    }

    #[test]
    fn pause_menu_settings_and_world_options() {
        let (mut app, _db) = make_app();
        press(&mut app, KeyCode::Esc);
        assert!(app.menu.is_some(), "Esc in walk opens the pause menu");
        assert!(screen_text(&app).contains("Paused") && screen_text(&app).contains("Back to game"));
        // Down to Settings (after Back, Achievements, Creations, World options).
        for _ in 0..4 {
            press(&mut app, KeyCode::Down);
        }
        press(&mut app, KeyCode::Enter);
        let shown = screen_text(&app);
        assert!(shown.contains("Settings") && shown.contains("Video") && shown.contains("Quality"), "{shown}");
        // Walking keys do nothing while it is open.
        let before = app.pos();
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Press));
        drive(&mut app, 0.3, |_, _| true);
        assert_eq!(app.pos(), before);
        // The AI tab: the budget, no limit by default; ← steps down to $50.
        app.ui.focus = Some(2);
        press(&mut app, KeyCode::Enter);
        assert!(screen_text(&app).contains("Who answers"), "{}", screen_text(&app));
        // After who answers and the key.
        app.ui.focus = Some(8);
        app.compose();
        press(&mut app, KeyCode::Left);
        assert_eq!(app.settings.budget(), Some(50.0));
        // Sound and its volume (no device in tests: it stays quiet).
        app.ui.focus = Some(1);
        press(&mut app, KeyCode::Enter);
        app.ui.focus = Some(6);
        press(&mut app, KeyCode::Enter);
        assert!(!app.settings.sound, "Enter turns sound off");
        press(&mut app, KeyCode::Enter);
        assert!(app.settings.sound);
        press(&mut app, KeyCode::Down);
        let vol = app.settings.volume;
        press(&mut app, KeyCode::Left);
        assert!(app.settings.volume < vol);
        // Voices and speak to talk: off, and not downloaded (tests never
        // fetch). Each is a Download button, then its page as a link.
        assert!(!app.settings.voices, "voices are off by default");
        assert!(screen_text(&app).contains("Download · 354 MB"), "{}", screen_text(&app));
        assert!(screen_text(&app).contains("github.com/thewh1teagle/kokoro-onnx"), "{}", screen_text(&app));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.settings.voices && app.voice.is_none(), "asked for: on once they're here");
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        assert!(app.settings.speak_to_talk && app.ears.is_none(), "asked for: on once it's here");
        // Esc: back to the pause menu, then World options.
        press(&mut app, KeyCode::Esc);
        assert!(screen_text(&app).contains("Paused"));
        app.ui.focus = Some(3);
        press(&mut app, KeyCode::Enter);
        assert!(screen_text(&app).contains("World options"));
        // A world setting changes the running sim and is kept with the world.
        assert_eq!(app.sim.cfg.difficulty, 0, "peaceful by default");
        press(&mut app, KeyCode::Right);
        assert_eq!(app.sim.level().name, "easy");
        assert_eq!(app.db.kv_get("sim.difficulty").as_deref(), Some("1"));
        press(&mut app, KeyCode::Down);
        let was = app.sim.cfg.max_creatures;
        press(&mut app, KeyCode::Right);
        assert!(app.sim.cfg.max_creatures > was);
        assert_eq!(app.db.kv_get("sim.max_creatures"), Some(app.sim.cfg.max_creatures.to_string()));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Esc);
        assert!(app.menu.is_none());
    }

    /// Tips that teach keys show at first, then stop: how to move until you
    /// walk, what to do with what you hold the first few times.
    #[test]
    fn key_tips_show_a_few_times_then_stop() {
        let (mut app, _db) = make_app();
        app.compose();
        assert!(screen_text(&app).contains("W/A/S/D move"), "{}", screen_text(&app));
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Press));
        app.on_key(key(KeyCode::Char('w'), KeyEventKind::Release));
        app.compose();
        assert!(!screen_text(&app).contains("W/A/S/D move"), "walked: learnt for this time");
        let stick = app.sim.type_by_name("stick").unwrap().id;
        let id = app.sim.spawn_thing(stick, app.pos(), 0.0, 1.0, Default::default(), true).unwrap();
        for i in 0..4 {
            app.sim.player.held = Some(id);
            app.compose();
            assert_eq!(screen_text(&app).contains("F throw"), i < 3, "time {i}: {}", screen_text(&app));
            app.sim.player.held = None;
            app.compose();
        }
    }

    #[test]
    fn grab_throw_inspect_and_do_with_keys() {
        let (mut app, _db) = make_app();
        let stick = app.sim.type_by_name("stick").unwrap().id;
        let at = app.pos() + Vec3::new(1.2, 0.0, 1.0);
        let id = app.sim.spawn_thing(stick, at, 0.0, 1.0, Default::default(), true).unwrap();
        app.sim.player.yaw = 1.2f32.atan2(1.0);
        app.pitch = -0.78;
        let mut phase = 0;
        drive(&mut app, 12.0, |a, _t| {
            if let Some(n) = a.sim.cast.npcs.first_mut() {
                n.think_at = f64::MAX;
            }
            match phase {
                0 if a.pointed.as_ref().is_some_and(|p| p.target == Target::Thing(id)) => {
                    a.on_key(key(KeyCode::Char('g'), KeyEventKind::Press));
                    assert_eq!(a.sim.player.held, Some(id), "picked up with g");
                    phase = 1;
                }
                1 => {
                    a.on_key(key(KeyCode::F(2), KeyEventKind::Press));
                    a.compose();
                    let txt = screen_text(a);
                    assert!(txt.contains("stick") || txt.contains("traveler"), "inspect panel shows something:\n{txt}");
                    a.on_key(key(KeyCode::Char('f'), KeyEventKind::Press));
                    assert_eq!(a.sim.player.held, None, "thrown with f");
                    assert!(!a.sim.things.get(id).unwrap().asleep, "flying");
                    phase = 2;
                }
                2 if a.sim.things.get(id).is_some_and(|t| t.asleep) => {
                    // Point at it with the middle of the view and pick it up again.
                    let me = a.pos();
                    let tp = a.sim.things.get(id).unwrap().pos;
                    a.sim.player.yaw = (tp.x - me.x).atan2(tp.z - me.z);
                    a.repick();
                    if (tp - me).length() < 3.0 && a.pointed.as_ref().is_some_and(|p| p.target == Target::Thing(id)) {
                        a.on_key(key(KeyCode::Char('g'), KeyEventKind::Press));
                    }
                    phase = 3;
                }
                3 => {
                    a.on_key(key(KeyCode::Char('/'), KeyEventKind::Press));
                    assert_eq!(a.mode, Mode::Command);
                    for c in "whistle a tune".chars() {
                        a.on_key(key(KeyCode::Char(c), KeyEventKind::Press));
                    }
                    a.on_key(key(KeyCode::Enter, KeyEventKind::Press));
                    assert_eq!(a.mode, Mode::Walk);
                    phase = 4;
                }
                4 if log_has(a, "whistle a tune") => {
                    phase = 5;
                    return false;
                }
                _ => {}
            }
            true
        });
        assert!(phase >= 4, "stuck in phase {phase}; log: {:?}", app.log.iter().map(|l| l.text.clone()).collect::<Vec<_>>());
        assert!(log_has(&app, "whistle a tune"), "the do line was shown");
        let kinds: Vec<String> = app.sim.log.recent.iter().map(|e| e.kind.clone()).collect();
        assert!(kinds.contains(&"picked_up".to_string()) && kinds.contains(&"threw".to_string()), "{kinds:?}");
        app.shutdown();
    }

}

/// Log a main-thread stall (what made the game choppy), at most every few seconds.
fn slow(what: &str, t0: Instant, detail: impl FnOnce() -> String) {
    static LAST: parking_lot::Mutex<Option<Instant>> = parking_lot::Mutex::new(None);
    let ms = t0.elapsed().as_secs_f32() * 1000.0;
    if ms < 50.0 {
        return;
    }
    let mut last = LAST.lock();
    if last.is_some_and(|l| l.elapsed() < Duration::from_secs(5)) {
        return;
    }
    *last = Some(Instant::now());
    crate::log::info(format!("slow {what}: {ms:.0} ms ({})", detail()));
}

/// The health bar's colour: green when whole, amber at half, red near none.
fn health_color(h: f32) -> [u8; 3] {
    const GREEN: [f32; 3] = [96.0, 214.0, 120.0];
    const AMBER: [f32; 3] = [240.0, 186.0, 64.0];
    const RED: [f32; 3] = [226.0, 64.0, 58.0];
    let (a, b, k) = if h >= 0.5 { (AMBER, GREEN, (h - 0.5) * 2.0) } else { (RED, AMBER, h * 2.0) };
    let k = k.clamp(0.0, 1.0);
    [0, 1, 2].map(|i| (a[i] + (b[i] - a[i]) * k) as u8)
}

/// How long the fall takes (s).
const FALL_SECS: f32 = 0.9;

/// Big letters for the death screen: five rows each.
fn glyph(c: char) -> [&'static str; 5] {
    match c {
        'Y' => ["█   █", " █ █ ", "  █  ", "  █  ", "  █  "],
        'O' => [" ███ ", "█   █", "█   █", "█   █", " ███ "],
        'U' => ["█   █", "█   █", "█   █", "█   █", " ███ "],
        'D' => ["████ ", "█   █", "█   █", "█   █", "████ "],
        'I' => ["███", " █ ", " █ ", " █ ", "███"],
        'E' => ["████", "█   ", "███ ", "█   ", "████"],
        _ => ["  ", "  ", "  ", "  ", "  "],
    }
}

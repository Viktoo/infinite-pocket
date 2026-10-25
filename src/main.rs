//! Infinite Pocket: an infinite, colour 3D world in your terminal.

mod achievements;
mod audio;
mod author;
mod app;
mod brain;
mod db;
mod decider;
#[cfg(test)]
mod e2e_tests;
mod lang;
mod llm;
mod log;
mod model;
mod net;
mod noise;
mod own_app;
mod pace;
mod panorama;
mod picker;
mod png;
mod prompts;
mod random_world;
mod record;
mod render;
mod settings;
mod share;
mod starter;
mod sim;
mod term;
mod terrain;
mod ui;
mod ground;
mod landforms;
mod inground;
mod settle;
mod rivers;
mod voice;
mod ears;
mod window;
mod world;

use anyhow::{Context, Result, bail};
use glam::Vec3;
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const USAGE: &str = "Infinite Pocket — living worlds, written as code

  pocket                                 open the game (the title: continue, worlds, settings…)
  pocket FILE                            open the game straight into a world
  pocket list                            start on your worlds (open one, or start a new one)
  pocket new [\"<prompt>\"] [--out FILE] [--difficulty peaceful|easy|normal(default)|hard]
                                         start on the new-world screen: check or edit the
                                         prompt (a random one if none given), then step in
  pocket prompts [--sample N]            print N random prompts (default 20)
  pocket version                         this build: its release (0.1.0) or dev and its commit
  pocket host [FILE] [--port 7777] [--code CODE] [--ngrok]
                                         open a world (or your last) for others to join
                                         (--ngrok: from anywhere, through an ngrok tunnel)
  pocket join ADDRESS CODE               join someone's world: their address (192.168.1.20:7777,
                                         or an ngrok https URL) and the code they gave you

  Any of the above with --shot OUT.png [--size 1600x1000] [--scale 2] [--after SECS] [--keys esc,down] [--keys-at SECS]
  runs unseen for a while, presses the keys a second before the end, and saves the window as a PNG.

  pocket snapshot FILE --at x,z,yawDeg [--size 120x40] [--ascii|--blocks] [--mono] [--time HOUR] [--weather NAME] [--pitch DEG] [--hands POSE]
                       [--lift M] [--phase ID=RAD,…] [--face [ID] [--dist M] [--turn DEG] [--look name=v,…] [--talk 0-1] [--blink 0-1] [--clock S] [--open]]  (a portrait of a character)
  pocket record FILE --at x,z,yawDeg --out DIR [--lift M] [--pitch DEG] [--to x,z,yawDeg [--to-lift M] [--to-pitch DEG]]
                     [--follow ID [--dist M]] [--seconds S | --frames N] [--fps 15] [--speed 1] [--time HOUR] [--size 800x450] [--loop N]
                     [--mp4] [--webp] [--gif] [--frames-only]   the world run unseen, saved as frames and a clip (ffmpeg)
  pocket describe FILE --at x,z,yawDeg [--size 120x40]   JSON of what is visible
  pocket bench FILE [--distance 2000] [--size 120x40]
  pocket gpubench FILE --at x,z,yawDeg [--px 640x360] [--breakdown]   GPU time for one view, by part
  pocket sim FILE [--hours H] [--seed S] [--events out.jsonl] [--save] [--no-llm] [--verify]
                                         run the world headless, fast, and report what emerged
  pocket sim --replay out.jsonl [--from T] [--kinds ignited,caught]
  pocket act FILE --as player|ID|NAME '{\"do\": \"hold\", \"target\": {\"name\": \"stick\"}}' [--run SECS] [--dry]
  pocket act FILE --stdin                one JSON command per line (actions, inspect, look, run)
  pocket inspect FILE player|npc:ID|thing:ID|instance:ID|cell:X,Z|NAME   (or --look [--as WHO])
  pocket listen --play | --demo | FILE [--species NAME | --at x,z,yawDeg --walk --calls]   hear it (WAVs)
  pocket check TYPE.js                   validate an object type module (steps 1-4)
  pocket author DIR [--out FILE]         make a world written by hand (see src/author.rs)
  pocket author heights FILE x0,z0,x1,z1 STEP   the ground's heights on a grid
  pocket selftest                        GPU/CPU parity and validator checks

Environment: ANTHROPIC_API_KEY, or POCKET_LLM_BASE_URL (+ POCKET_LLM_API_KEY) for any
OpenAI-compatible endpoint; POCKET_MODEL_{BUILDER,CHARACTER,DECIDER,SUMMARIZER};
POCKET_BUDGET_USD; POCKET_REGION_RADIUS (default 2); POCKET_DECIDER_URL; POCKET_FPS;
POCKET_NO_GPU=1; POCKET_HOME (default ~/.pocket); POCKET_SIM_NEAR, POCKET_SIM_MEDIUM,
POCKET_SIM_FAR (frozen | catchup:HOURS), POCKET_LLM_PER_MIN (how alive the world is),
POCKET_NO_SOUND=1.";

fn main() {
    log::init();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = run(args);
    if let Err(e) = r {
        eprintln!("pocket: {e:#}");
        std::process::exit(1);
    }
}

pub(crate) fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1).cloned())
}

pub(crate) fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn run(args: Vec<String>) -> Result<()> {
    // Any screen of the game can be run unseen and saved as a PNG.
    let shot = window::Shot::from_args(&args)?;
    match args.first().map(String::as_str) {
        Some("-h") | Some("--help") | Some("help") => {
            println!("{USAGE}");
            Ok(())
        }
        Some("version") | Some("--version") | Some("-V") => {
            println!("{}", db::build_name());
            Ok(())
        }
        Some("new") => {
            // Every new world begins on the new-world screen: the prompt given,
            // to check, or a random one to keep or change.
            let given = args.get(1).filter(|a| !a.starts_with("--")).cloned();
            let difficulty = flag(&args, "--difficulty").unwrap_or_else(|| NEW_DIFFICULTY.into());
            let n = sim::night::LEVELS.iter().position(|l| l.name == difficulty.trim().to_lowercase()).with_context(|| format!("difficulty is one of peaceful, easy, normal, hard (not {difficulty})"))?;
            session(Start::New(given), Some(n), flag(&args, "--out").map(PathBuf::from), shot)
        }
        Some("list") | Some("ls") => session(Start::List, None, None, shot),
        Some("continue") | Some("last") => session(Start::Last, None, None, shot),
        Some("host") => {
            let port = flag(&args, "--port").map(|p| p.parse::<u16>().context("--port takes a number")).transpose()?.unwrap_or(net::PORT);
            let path = match args.get(1).filter(|a| !a.starts_with("--")) {
                Some(p) => PathBuf::from(p),
                None => picker::last().or_else(|| picker::worlds().into_iter().next().map(|w| w.path)).context("no world to host: make one first")?,
            };
            let code = flag(&args, "--code").or_else(|| std::env::var("POCKET_HOST_CODE").ok()).unwrap_or_else(net::new_code);
            session(Start::Host(path, port, code, has(&args, "--ngrok")), None, None, shot)
        }
        Some("join") => {
            let addr = args.get(1).filter(|a| !a.starts_with("--")).context("usage: pocket join ADDRESS CODE")?.clone();
            let code = args.get(2).filter(|a| !a.starts_with("--")).context("usage: pocket join ADDRESS CODE (the host's code)")?.clone();
            session(Start::Join(addr, code), None, None, shot)
        }
        Some("prompts") => {
            let n = flag(&args, "--sample").and_then(|n| n.parse().ok()).unwrap_or(20);
            for p in random_world::sample(n) {
                println!("{p}\n");
            }
            Ok(())
        }
        Some("open") => session(Start::Play(PathBuf::from(args.get(1).context("usage: pocket open FILE")?), false), None, None, shot),
        Some("window") => match args.get(1).filter(|a| !a.starts_with("--")) {
            Some(p) => session(Start::Play(PathBuf::from(p), false), None, None, shot),
            None => session(Start::Title, None, None, shot),
        },
        Some("snapshot") => snapshot(&args[1..]),
        Some("record") => record::cli(&args[1..]),
        Some("describe") => describe(&args[1..]),
        Some("bench") => bench(&args[1..]),
        Some("selftest") => selftest(),
        Some("gpubench") => gpubench(&args[1..]),
        Some("check") => check_file(&args[1..]),
        Some("author") => author::cli(&args[1..]),
        Some("sim") => sim::headless::run_sim_cli(&args[1..]),
        Some("act") => sim::headless::act_cli(&args[1..]),
        Some("inspect") => sim::headless::inspect_cli(&args[1..]),
        Some("listen") => audio::listen::cli(&args[1..]),
        Some(p) if !p.starts_with('-') => session(Start::Play(PathBuf::from(p), false), None, None, shot),
        None | Some("--shot") => session(Start::Title, None, None, shot),
        Some(other) => bail!("unknown option {other}\n\n{USAGE}"),
    }
}

fn default_path(prompt: &str) -> PathBuf {
    let slug: String = prompt.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    let slug: String = slug.split('-').filter(|s| !s.is_empty()).take(5).collect::<Vec<_>>().join("-");
    let dir = log::dir().join("universes");
    let mut p = dir.join(format!("{}.pocket", if slug.is_empty() { "universe" } else { &slug }));
    let mut n = 2;
    while p.exists() {
        p = dir.join(format!("{slug}-{n}.pocket"));
        n += 1;
    }
    p
}

/// New worlds start here (older worlds, made before difficulty, stay peaceful).
pub(crate) const NEW_DIFFICULTY: &str = "normal";

fn create_universe(path: &Path, prompt: &str) -> Result<()> {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_nanos();
    let seed = noise::pcg((t as u32) ^ ((t >> 32) as u32) ^ std::process::id());
    let look = serde_json::to_string(&world::Look::default())?;
    let db = db::Db::create(path, seed, prompt.trim(), &look)?;
    let n = sim::night::LEVELS.iter().position(|l| l.name == NEW_DIFFICULTY).unwrap_or(0);
    db.kv_set("sim.difficulty", &n.to_string())?;
    Ok(())
}

struct Loaded {
    db: Arc<db::Db>,
    gpu: Option<Arc<render::gpu::Gpu>>,
    model: model::WorldModel,
    snap: Arc<world::WorldSnapshot>,
    live: model::LiveRef,
    /// `snap` is the quick base snapshot; the full world flips in later.
    partial: bool,
}

/// Above this many types, start from a base snapshot so the player can walk at once.
const QUICK_START_TYPES: usize = 16;

fn load(path: &Path) -> Result<Loaded> {
    load_with(path, false)
}

fn load_with(path: &Path, quick: bool) -> Result<Loaded> {
    let t0 = Instant::now();
    let l = load_db(db::Db::open(path)?, quick)?;
    log::info(format!("loaded {} (v{}, {} types, {} instances) in {:.0} ms", path.display(), l.snap.version, l.snap.scene.types.len(), l.snap.instances.len(), t0.elapsed().as_secs_f64() * 1000.0));
    Ok(l)
}

/// A world (on disk, or held in memory) made ready to play or look at.
fn load_db(db: Arc<db::Db>, quick: bool) -> Result<Loaded> {
    let gpu = match render::gpu::Gpu::new() {
        Ok(g) => Some(g),
        Err(e) => {
            log::error(format!("GPU unavailable: {e:#}"));
            None
        }
    };
    let live = Arc::new(Mutex::new(model::Live::default()));
    let mut model = model::WorldModel::load(db.clone(), gpu.clone(), live.clone())?;
    let t0 = Instant::now();
    let partial = quick && model.active_type_count() > QUICK_START_TYPES;
    let snap = if partial { model.snapshot_base() } else { model.snapshot() };
    let snap = snap.map_err(|d| anyhow::anyhow!("building the world shader failed: {}", lang::format_diags(&d)))?;
    crate::log::info(format!("world built in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0));
    Ok(Loaded { db, gpu, model, snap, live, partial })
}

/// Where a session starts.
enum Start {
    /// The title screen.
    Title,
    /// The last world played, or the new-world screen the first time.
    Last,
    List,
    New(Option<String>),
    /// A new world: its prompt and difficulty (an index into the levels).
    Create(String, Option<usize>),
    Play(PathBuf, bool),
    /// Open a world for others to join, on a port, with a code; over the
    /// internet too (ngrok) when asked.
    Host(PathBuf, u16, String, bool),
    /// Join someone's world: their address and code.
    Join(String, String),
}

/// The game: one window, moving between your worlds, a new world, and play,
/// until it is closed. With `shot`, the same unseen, saved as a PNG at the end.
fn session(mut next: Start, difficulty: Option<usize>, out: Option<PathBuf>, shot: Option<window::Shot>) -> Result<()> {
    // An unseen run (a check, a screenshot) leaves the player's last world be.
    let remember = shot.is_none();
    if remember {
        own_app::become_own_app();
    }
    let mut win = match shot {
        Some(s) => window::Win::offscreen(s)?,
        None => window::Win::open()?,
    };
    let stop = win.stop.clone();
    // A line for the worlds list (why a game together ended).
    let mut note: Option<String> = None;
    // Maple Cove behind the title, kept on behind the next world's loading.
    let mut pano = None;
    loop {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        next = match next {
            // The last world played, else the one most lately changed.
            Start::Last => match picker::last().or_else(|| picker::worlds().into_iter().next().map(|w| w.path)) {
                Some(p) => Start::Play(p, false),
                None => Start::New(None),
            },
            Start::Title | Start::List | Start::New(_) => {
                let entry = match next {
                    Start::List => ui::screens::Entry::Worlds,
                    Start::New(given) => ui::screens::Entry::New(given),
                    _ => ui::screens::Entry::Title,
                };
                match ui::screens::menus(&mut win, entry, note.take(), &mut pano) {
                    ui::screens::Choice::Open(p) => Start::Play(p, false),
                    ui::screens::Choice::New(prompt, d) => Start::Create(prompt, Some(d)),
                    ui::screens::Choice::Host(p, internet) => Start::Host(p, net::PORT, net::new_code(), internet),
                    ui::screens::Choice::Join(addr, code) => Start::Join(addr, code),
                    ui::screens::Choice::Quit => break,
                }
            }
            Start::Create(prompt, d) => {
                let path = out.clone().unwrap_or_else(|| default_path(&prompt));
                create_universe(&path, &prompt)?;
                if let Some(d) = d.or(difficulty) {
                    db::Db::open(&path)?.kv_set("sim.difficulty", &d.to_string())?;
                }
                Start::Play(path, true)
            }
            Start::Play(path, fresh) => match play(&mut win, &path, fresh, remember, None, pano.take(), &stop)? {
                (true, why) => {
                    note = why;
                    Start::Title
                }
                (false, _) => break,
            },
            Start::Host(path, port, code, internet) => match play(&mut win, &path, false, remember, Some((port, code, internet)), pano.take(), &stop)? {
                (true, why) => {
                    note = why;
                    Start::Title
                }
                (false, _) => break,
            },
            Start::Join(addr, code) => match {
                pano = None;
                join(&mut win, &addr, &code, &stop)
            } {
                Ok((true, why)) => {
                    note = why;
                    Start::Title
                }
                Ok((false, _)) => break,
                Err(e) => {
                    log::info(format!("joining {addr}: {e:#}"));
                    note = Some(format!("Couldn't join: {e}"));
                    Start::Title
                }
            },
        };
    }
    win.finish()
}

/// Play one world in the window (hosting it on `host`, a port, if given);
/// true when the player went back to their worlds, and why if not by choice.
fn play(win: &mut window::Win, path: &Path, fresh: bool, remember: bool, host: Option<(u16, String, bool)>, backdrop: Option<panorama::Panorama>, stop: &AtomicBool) -> Result<(bool, Option<String>)> {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    if remember {
        let _ = std::fs::write(log::dir().join("last"), path.display().to_string());
    }
    let name = picker::worlds().into_iter().find(|w| w.path == path).map(|w| w.name).unwrap_or_else(|| "a new world".into());
    win.set_title(&format!("{name} · Infinite Pocket"));
    let mut backdrop = backdrop;
    let l = opening(win, &name, &path, &mut backdrop)?;
    let llm = llm::Llm::from_env(l.db.clone());
    let genesis = fresh || l.db.kv_get("genesis").is_none();
    let partial = l.partial;
    let (etx, erx) = crossbeam_channel::unbounded();
    let brain = brain::Brain::start(l.model, llm.clone(), etx, l.partial);
    let render = match &l.gpu {
        Some(g) => render::gpu::spawn(g.clone()),
        None => render::cpu::spawn(),
    };
    let (cols, rows, _) = win.grid();
    let together = match host {
        Some((port, code, internet)) => {
            let (me, _) = settings::Settings::load().player();
            let h = net::host::Host::start(port, code)?;
            Some(app::together::Together::Host(app::together::Hosting::new(h, path.clone(), name.clone(), me, internet)))
        }
        None => None,
    };
    let mut app = app::App::new(app::Setup {
        db: l.db.clone(),
        brain,
        events: erx,
        render,
        snap: l.snap,
        live: l.live,
        truecolor: true,
        size: (cols, rows),
        llm,
        genesis,
        together,
        seed: None,
    });
    app.path = Some(path.clone());
    app.backdrop = backdrop;
    app.full_pending = partial;
    app.start_loading();
    let r = window::play(win, &mut app, stop);
    app.shutdown();
    win.set_title("Infinite Pocket");
    if win.is_shot() {
        println!("{}", app.stats_line());
    }
    r?;
    log::info(format!("saved {}", path.display()));
    Ok((app.to_worlds, app.left_because.take()))
}

/// Load a world, showing Maple Cove (or the drawn cove, before it has
/// come) with the world's name until it is read.
fn opening(win: &mut window::Win, name: &str, path: &Path, pano: &mut Option<panorama::Panorama>) -> Result<Loaded> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    let p = path.to_path_buf();
    std::thread::Builder::new().name("opening".into()).spawn(move || {
        let _ = tx.send(load_with(&p, true));
    })?;
    let mut back = ui::title::Backdrop::new();
    let start = Instant::now();
    loop {
        let _ = win.pump(Duration::from_millis(33));
        if let Ok(l) = rx.try_recv() {
            return l;
        }
        let (w, h, g) = win.grid();
        if let Some(p) = pano.as_mut() {
            p.tick((w as f32 * g.cell_w, h as f32 * g.cell_h));
        }
        let mut scr = term::Screen::new(w, h, true);
        let t = start.elapsed().as_secs_f32();
        let frame = pano.as_ref().and_then(|p| p.frame().map(|f| (f, p.shown())));
        match frame {
            Some((_, shown)) => back.draw_world(&mut scr, t, 0.5, shown),
            None => back.draw(&mut scr, t, 0.5),
        }
        let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
        scr.title(cx + 0.12, cy - 1.4, name, 2.4, [20, 14, 8]);
        scr.title(cx, cy - 1.5, name, 2.4, ui::ACCENT);
        let dots = ".".repeat(1 + (t * 2.0) as usize % 3);
        ui::note(&mut scr, cx, cy + 1.0, &format!("opening{dots:<3}"), ui::DIM);
        match frame {
            Some((f, _)) => win.present(&scr, h, Some(f)),
            None => win.present(&scr, h, Some(back.frame((w as f32 * g.cell_w, h as f32 * g.cell_h), t))),
        }
    }
}

/// Join someone's world and play in it until it ends; as `play`.
fn join(win: &mut window::Win, addr: &str, code: &str, stop: &AtomicBool) -> Result<(bool, Option<String>)> {
    let (me, token) = settings::Settings::load().player();
    picker::notice(win, "Joining", "connecting…");
    let mut last = Instant::now() - Duration::from_secs(1);
    let joined = net::guest::join(addr, code, &me, &token, |_, what| {
        if last.elapsed() > Duration::from_millis(100) {
            last = Instant::now();
            picker::notice(win, "Joining", what);
        }
    })?;
    let world = joined.welcome.world.clone();
    picker::notice(win, &world, "opening…");
    win.set_title(&format!("{world} · Infinite Pocket"));
    let db = db::Db::from_bytes(&joined.world)?;
    net::guest::as_guest(&db, &joined.welcome)?;
    let l = load_db(db, true)?;
    let (etx, erx) = crossbeam_channel::unbounded();
    // The host's world: no words of one's own here (the host's LLM answers).
    let brain = brain::Brain::start(l.model, None, etx, l.partial);
    let render = match &l.gpu {
        Some(g) => render::gpu::spawn(g.clone()),
        None => render::cpu::spawn(),
    };
    let (cols, rows, _) = win.grid();
    let guesting = app::together::Guesting::new(joined.link, &joined.welcome, joined.roster.clone());
    let mut app = app::App::new(app::Setup {
        db: l.db.clone(),
        brain,
        events: erx,
        render,
        snap: l.snap,
        live: l.live,
        truecolor: true,
        size: (cols, rows),
        llm: None,
        genesis: false,
        together: Some(app::together::Together::Guest(guesting)),
        seed: Some(joined.welcome.seed),
    });
    app.arrive_as_guest(&joined.welcome, joined.roster);
    app.start_loading();
    let r = window::play(win, &mut app, stop);
    app.shutdown();
    win.set_title("Infinite Pocket");
    if win.is_shot() {
        println!("{}", app.stats_line());
    }
    r?;
    Ok((app.to_worlds, app.left_because.take()))
}

fn parse_at(args: &[String]) -> Result<(f32, f32, f32)> {
    let at = flag(args, "--at").context("--at x,z,yawDeg is required")?;
    let v: Vec<f32> = at.split(',').map(|s| s.trim().parse::<f32>()).collect::<Result<_, _>>().context("--at expects x,z,yawDeg")?;
    if v.len() != 3 {
        bail!("--at expects x,z,yawDeg");
    }
    Ok((v[0], v[1], v[2].to_radians()))
}

fn parse_size(args: &[String]) -> Result<(u16, u16)> {
    let s = flag(args, "--size").unwrap_or_else(|| "120x40".into());
    let (w, h) = s.split_once('x').context("--size expects COLSxROWS")?;
    Ok((w.parse::<u16>()?.clamp(8, 1000), h.parse::<u16>()?.clamp(4, 500)))
}

/// A person's half-width, to size a close-up of anything wider by.
const NPC_SPAN: f32 = world::collide::NPC_RADIUS;

/// One frame, rendered synchronously (shared by snapshot and tests).
/// A portrait: one character stood before the camera, posed and dressed as asked.
#[derive(Default)]
struct Portrait {
    id: Option<i64>,
    dist: f32,
    turn: f32,
    look: Vec<(String, f32)>,
    talk: f32,
    blink: f32,
    clock: Option<f64>,
    /// Out in the open near where they are, their own place behind them.
    open: bool,
}

impl Portrait {
    fn from_args(args: &[String]) -> Result<Option<Portrait>> {
        let Some(i) = args.iter().position(|a| a == "--face") else { return Ok(None) };
        let id = args.get(i + 1).and_then(|s| s.parse::<i64>().ok());
        let num = |f: &str, d: f32| flag(args, f).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        let mut look = Vec::new();
        for kv in flag(args, "--look").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
            let (k, v) = kv.split_once('=').context("--look expects name=value,…")?;
            look.push((k.trim().to_string(), v.trim().parse::<f32>().context("--look value")?));
        }
        Ok(Some(Portrait { id, dist: num("--dist", 1.2), turn: num("--turn", 0.0).to_radians(), look, talk: num("--talk", 0.0), blink: num("--blink", 0.0), clock: flag(args, "--clock").and_then(|v| v.parse().ok()), open: has(args, "--open") }))
    }

    /// Put the character before `cam` and look at their face.
    fn stage(&self, live: &mut sim::Sim, cam: &mut render::Camera) -> Result<()> {
        let id = match self.id {
            Some(id) => id,
            None => live.cast.npcs.iter().filter(|n| n.here()).min_by(|a, b| (a.a.pos - cam.pos).length().total_cmp(&(b.a.pos - cam.pos).length())).map(|n| n.def.id).context("no character here to portray")?,
        };
        if self.open {
            if let Some((at, yaw)) = self.open_spot(live, id) {
                cam.pos = at;
                cam.yaw = yaw;
            }
        }
        let fwd = Vec3::new(cam.yaw.sin(), 0.0, cam.yaw.cos());
        let snap = live.snap.clone();
        let n = live.cast.get_mut(id).with_context(|| format!("no character {id}"))?;
        let body = n.body_ty;
        if let Some(b) = snap.type_of(body).and_then(|t| t.ct.meta.body.clone()) {
            for (name, v) in &self.look {
                let i = b.slider(name).with_context(|| format!("no look slider {name}: {}", b.look.iter().map(|l| l.0.as_str()).collect::<Vec<_>>().join(", ")))?;
                n.sliders[i] = *v;
            }
        }
        // A close-up keeps a body bigger than a person (a horse, a stone elder)
        // as far off as its size, so its head fills the frame as a face would.
        let big = if self.dist < 2.0 { (n.a.dims.height / 1.75).max(n.a.dims.radius / NPC_SPAN).max(1.0) } else { 1.0 };
        let flat = cam.pos + fwd * self.dist * big;
        let ground = snap.terrain.height(flat.x, flat.z);
        n.a.pos = Vec3::new(flat.x, ground, flat.z);
        n.a.yaw = cam.yaw + std::f32::consts::PI + self.turn;
        n.a.pose = Default::default();
        n.a.asleep = false;
        // Whatever their hours (a night walker by day), they sit for it.
        n.away = false;
        // Near: the face; farther: all of them.
        if self.dist < 2.0 {
            cam.pos.y = n.a.pos.y + n.a.dims.eye - 0.05;
            cam.fov_y = 0.55;
        } else {
            cam.pos.y = n.a.pos.y + n.a.dims.height * 0.55;
            cam.fov_y = 2.0 * (n.a.dims.height * 0.62 / self.dist).atan();
        }
        cam.pitch = 0.0;
        live.face_hold = Some((self.talk, self.blink));
        Ok(())
    }

    /// Where to stand the camera so the character is out in the open: the
    /// nearest spot round where they are with sky over it, dry ground, and
    /// nothing solid between them and the camera, looking back toward where
    /// they were (their home, their street) behind them.
    fn open_spot(&self, live: &mut sim::Sim, id: i64) -> Option<(Vec3, f32)> {
        use world::collide::{Capsule, Obstacles};
        let c = live.cast.get(id)?.a.pos;
        let t = live.snap.terrain.clone();
        let dry = |p: Vec3| t.water(p.x, p.z) < t.height(p.x, p.z) + 0.05;
        for ring in 0..20 {
            let r = 3.0 + ring as f32 * 2.0;
            for k in 0..16 {
                let a = (k as f32 + 0.5 * (ring % 2) as f32) / 16.0 * std::f32::consts::TAU;
                let s = c + Vec3::new(a.sin(), 0.0, a.cos()) * r;
                let fwd = (c - s).with_y(0.0).normalize_or_zero();
                let cam = s - fwd * self.dist;
                let (s, cam) = (s.with_y(t.height(s.x, s.z)), cam.with_y(t.height(cam.x, cam.z)));
                if !dry(s) || !dry(cam) || (s.y - cam.y).abs() > 1.0 || !live.open_sky(s + Vec3::Y) || !live.open_sky(cam + Vec3::Y) {
                    continue;
                }
                let solids = live.solids_near((s + cam) * 0.5, self.dist * 0.5 + 3.0);
                let obs = Obstacles { solids: &solids, bodies: &[] };
                let body = Capsule::of(2.0, 0.45);
                let clear = (0..=8).all(|i| {
                    let p = cam.lerp(s, i as f32 / 8.0);
                    obs.clear(p.x, p.z, t.height(p.x, p.z), body)
                });
                if clear {
                    return Some((cam, fwd.x.atan2(fwd.z)));
                }
            }
        }
        None
    }
}

#[allow(clippy::too_many_arguments)]
fn render_once(l: &Loaded, mut cam: render::Camera, w: u32, h: u32, pixel_aspect: f32, t_game: f64, weather: Option<&str>, hands: Option<&str>, face: Option<&Portrait>, strides: &[(i64, f32)]) -> Result<render::Frame> {
    let snap = &l.snap;
    let mut live = sim::Sim::new(l.db.clone(), snap.clone(), cam.pos, cam.yaw, t_game, 1);
    // Caught mid-stride: a walk phase (radians) for each character asked.
    for (id, phase) in strides {
        live.cast.get_mut(*id).with_context(|| format!("no character {id}"))?.a.phase = *phase;
    }
    if let Some(p) = face {
        p.stage(&mut live, &mut cam)?;
        if let Some(c) = p.clock {
            live.t = c;
        }
    }
    if let Some(name) = weather {
        let kind = snap.look.climate.kinds.iter().find(|k| k.name == name).cloned().or_else(|| world::weather::sample(name)).with_context(|| format!("no weather called {name} (the world's own, or clear, fair, cloudy, overcast, fog, rain, storm, snow, ash, motes)"))?;
        live.weather_st.spell = Some(world::weather::Spell { kind, from: t_game - 3.0 * world::weather::HOUR, until: t_game + 3.0 * world::weather::HOUR, by: String::new() });
        live.step_weather();
    }
    // Lamps and lit windows shine after dark, as in play.
    live.cache.overlay.lamps = render::sky::lighting(t_game, &snap.look.palette).night;
    let aspect = w as f32 / h as f32 * pixel_aspect;
    // Seen through the traveler's eyes, their arms posed: a gesture's
    // height ("punch", "grab", "wave"…), "hold", or "conjure".
    let drawn = match hands {
        Some(pose) => {
            live.player.pos = cam.pos - Vec3::Y * live.player.dims.eye;
            live.player.yaw = cam.yaw;
            live.player.conjuring = pose == "conjure";
            let kind = sim::actor::GestureKind::named(pose);
            if pose != "conjure" && pose != "hold" && kind.is_none() {
                bail!("--hands takes a gesture (punch, grab, throw, touch, wave, point…), hold or conjure");
            }
            if let Some(k) = kind {
                let at = if matches!(k, sim::actor::GestureKind::Punch) { 0.35 } else { 0.5 };
                live.player.gesture = Some(sim::actor::GestureRun { kind: k, with: None, t0: t_game - (k.duration() * at) as f64, dur: k.duration() });
            }
            let held = live.held_size(sim::ActorId::Player).or((pose == "hold").then_some(false));
            for _ in 0..60 {
                live.player.update_pose(t_game, 0.05, held);
            }
            live.draw_first_person(&cam, render::VIEW_DIST)
        }
        None => live.draw(cam.pos, render::VIEW_DIST),
    };
    let culled = world::cull::cull(snap, &mut live.cache, &cam, aspect, &drawn.insts, &Default::default());
    let light = render::sky::lighting(t_game, &snap.look.palette);
    let sp = render::SceneParams { terrain: &snap.terrain, palette: &snap.look.palette, camera: cam, width: w, height: h, pixel_aspect, light, time: 0.0, frame: 0, shadows: l.gpu.is_some() && std::env::var("POCKET_NO_SHADOWS").is_err(), lights: &drawn.lights, weather: render::WeatherView::of(&live.wx, 0.0, !live.open_sky(cam.pos)) };
    let globals = render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
    let req = render::FrameRequest { id: 1, width: w, height: h, globals, instances: culled.insts, holes: drawn.holes.clone(), grid: culled.grid, scene: snap.scene.clone(), terrain: snap.terrain.clone(), look: snap.look.clone() };
    let mut handle = match &l.gpu {
        Some(g) => render::gpu::spawn(g.clone()),
        None => render::cpu::spawn(),
    };
    handle.tx.send(render::RenderMsg::Frame(Box::new(req)))?;
    let f = handle.rx.recv_timeout(Duration::from_secs(60)).context("render timed out")?;
    handle.shutdown();
    if f.width == 0 {
        bail!("render failed");
    }
    Ok(f)
}

fn camera_at(snap: &world::WorldSnapshot, x: f32, z: f32, yaw: f32) -> render::Camera {
    let y = snap.terrain.height(x, z).max(snap.terrain.water(x, z) - 0.4) + 1.65;
    render::Camera { pos: Vec3::new(x, y, z), yaw, pitch: -0.06, fov_y: 1.05, roll: 0.0 }
}

fn snapshot(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().context("usage: pocket snapshot FILE --at x,z,yawDeg")?);
    let (x, z, yaw) = parse_at(args)?;
    let (w, h) = parse_size(args)?;
    let ascii = has(args, "--ascii");
    let mono = has(args, "--mono");
    let l = load(&path)?;
    let mut t_game = l.db.player().map(|p| p.t_game).unwrap_or(0.33 * render::sky::DAY_SECONDS);
    if let Some(h) = flag(args, "--time").and_then(|h| h.parse::<f64>().ok()) {
        t_game = (h / 24.0).rem_euclid(1.0) * render::sky::DAY_SECONDS;
    }
    let mut cam = camera_at(&l.snap, x, z, yaw);
    if let Some(p) = flag(args, "--pitch").and_then(|p| p.parse::<f32>().ok()) {
        cam.pitch = p.to_radians();
    }
    if let Some(m) = flag(args, "--lift").and_then(|m| m.parse::<f32>().ok()) {
        cam.pos.y += m;
    }
    let png_out = flag(args, "--png");
    // Quadrant cells: 2×2 pixels each. A PNG gets square pixels at the same detail.
    let (pw, ph, pa) = match (ascii, png_out.is_some()) {
        (true, _) => (w as u32, h as u32, 0.5),
        (false, true) => (w as u32 * 2, h as u32 * 4, 1.0),
        (false, false) => (w as u32 * 2, h as u32 * 2, 0.5),
    };
    let face = Portrait::from_args(args)?;
    let mut strides = Vec::new();
    for kv in flag(args, "--phase").unwrap_or_default().split(',').filter(|s| !s.is_empty()) {
        let (id, p) = kv.split_once('=').context("--phase expects ID=radians,…")?;
        strides.push((id.trim().parse::<i64>().context("--phase id")?, p.trim().parse::<f32>().context("--phase radians")?));
    }
    let f = render_once(&l, cam, pw, ph, pa, t_game, flag(args, "--weather").as_deref(), flag(args, "--hands").as_deref(), face.as_ref(), &strides)?;
    if let Some(png) = png_out {
        let s = if ascii { 4u32 } else { 2 };
        let mut rgb = Vec::with_capacity((f.width * s * f.height * s * 3) as usize);
        for y in 0..f.height * s {
            for x in 0..f.width * s {
                rgb.extend_from_slice(&render::unpack(f.pixels[((y / s) * f.width + x / s) as usize]));
            }
        }
        std::fs::write(&png, png::encode(f.width * s, f.height * s, &rgb))?;
        return Ok(());
    }
    print!("{}", app::frame_to_text(&f, ascii, mono, term::truecolor() || std::env::var("COLORTERM").is_err()));
    Ok(())
}

fn describe(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().context("usage: pocket describe FILE --at x,z,yawDeg")?);
    let (x, z, yaw) = parse_at(args)?;
    let l = load(&path)?;
    let t_game = l.db.player().map(|p| p.t_game).unwrap_or(0.33 * render::sky::DAY_SECONDS);
    let cam = camera_at(&l.snap, x, z, yaw);
    let mut live = sim::Sim::new(l.db.clone(), l.snap.clone(), cam.pos, cam.yaw, t_game, 1);
    let npcs = live.npc_views();
    let (w, h) = parse_size(args)?;
    let aspect = w as f32 / (h as f32 * 2.0);
    let v = world::describe::describe(&l.snap, &mut live.cache, &npcs, &cam, aspect, t_game);
    println!("{}", serde_json::to_string_pretty(&v)?);
    Ok(())
}

fn bench(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().context("usage: pocket bench FILE")?);
    let distance: f32 = flag(args, "--distance").and_then(|d| d.parse().ok()).unwrap_or(2000.0);
    let (w, h) = parse_size(args)?;
    let l = load(&path)?;
    let llm = if has(args, "--with-llm") { llm::Llm::from_env(l.db.clone()) } else { None };
    let (etx, erx) = crossbeam_channel::unbounded();
    let brain = brain::Brain::start(l.model, llm.clone(), etx, l.partial);
    let render = match &l.gpu {
        Some(g) => render::gpu::spawn(g.clone()),
        None => render::cpu::spawn(),
    };
    let backend = render.backend.clone();
    let mut app = app::App::new(app::Setup { db: l.db.clone(), brain, events: erx, render, snap: l.snap, live: l.live, truecolor: true, size: (w, h), llm, genesis: false, together: None, seed: None });
    app.noclip = true;
    let start = app.pos();
    let stop = AtomicBool::new(false);
    let t0 = Instant::now();
    let mut warm = 0.0f32;
    let mut walked = 0.0f32;
    let mut script = |a: &mut app::App, dt: f32| -> bool {
        warm += dt;
        if warm < 1.0 {
            return true; // let the first frames settle
        }
        let f = Vec3::new(a.yaw().sin(), 0.0, a.yaw().cos());
        let p = a.pos() + f * app::WALK_SPEED * 2.0 * dt;
        a.sim.player.pos = Vec3::new(p.x, a.snap.terrain.height(p.x, p.z), p.z);
        walked = Vec3::new(a.pos().x - start.x, 0.0, a.pos().z - start.z).length();
        if warm > 1.0 && warm < 1.05 {
            a.stats.max_loop_ms = 0.0;
            a.stats.max_gap_ms = 0.0;
        }
        walked < distance && t0.elapsed() < Duration::from_secs(600)
    };
    let mut sink = CountingSink::default();
    app.run(&mut sink, &stop, Some(&mut script))?;
    let secs = t0.elapsed().as_secs_f32() - 1.0;
    let frames = app.stats.shown;
    println!("backend        {backend}");
    println!("viewport       {w}x{h} cells");
    println!("walked         {walked:.0} m in {secs:.1} s");
    println!("frames shown   {} ({:.1} fps)", frames, frames as f32 / secs.max(0.01));
    println!("worst loop     {:.1} ms", app.stats.max_loop_ms);
    println!("worst gap      {:.1} ms between displayed frames", app.stats.max_gap_ms);
    println!("gpu (avg)      {:.2} ms", app.stats.gpu_ms);
    println!("terminal bytes {:.1} KB/frame", sink.0 as f32 / frames.max(1) as f32 / 1024.0);
    app.shutdown();
    Ok(())
}

#[derive(Default)]
struct CountingSink(usize);
impl std::io::Write for CountingSink {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0 += b.len();
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

thread_local! {
    static SETTLED: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
}

/// `pocket gpubench FILE --at x,z,yawDeg [--px 640x360 | --size COLSxROWS] [--breakdown]`:
/// GPU time for one view; with `--breakdown`, what each part of a frame costs.
fn gpubench(args: &[String]) -> Result<()> {
    let path = PathBuf::from(args.first().context("usage: pocket gpubench FILE --at x,z,yawDeg")?);
    let (x, z, yaw) = parse_at(args)?;
    let (pw, ph) = match flag(args, "--px") {
        Some(p) => {
            let (w, h) = p.split_once('x').context("--px expects WxH")?;
            (w.parse::<u32>()?, h.parse::<u32>()?)
        }
        None => {
            let (w, h) = parse_size(args)?;
            (w as u32, h as u32 * 2)
        }
    };
    let l = load(&path)?;
    let gpu = l.gpu.clone().context("no GPU")?;
    let snap = &l.snap;
    let cam = camera_at(snap, x, z, yaw);
    let mut cache = world::scatter::ScatterCache::default();
    let culled = world::cull::cull(snap, &mut cache, &cam, pw as f32 / ph as f32, &[], &Default::default());
    let light = render::sky::lighting(400.0, &snap.look.palette);
    let sp = render::SceneParams { terrain: &snap.terrain, palette: &snap.look.palette, camera: cam, width: pw, height: ph, pixel_aspect: 1.0, light, time: 0.0, frame: 0, shadows: std::env::var("POCKET_NO_SHADOWS").is_err(), lights: &[], weather: Default::default() };
    let globals = render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
    let mut handle = render::gpu::spawn(gpu);
    let mut id = 0u64;
    // Median pass time over `n` frames with these flags (and the last frame).
    let mut run = |flags: u32, n: usize| -> Result<(f32, f64, render::Frame)> {
        let mut g = globals;
        g.dims[3] = flags;
        let mut times = Vec::new();
        let t0 = Instant::now();
        let mut last = None;
        // Two frames in flight, as in the game: the GPU never waits on the
        // CPU, so its clock stays where play would put it.
        let send = |id: &mut u64| -> Result<()> {
            *id += 1;
            let req = render::FrameRequest { id: *id, width: pw, height: ph, globals: g, instances: culled.insts.clone(), holes: Vec::new(), grid: culled.grid.clone(), scene: snap.scene.clone(), terrain: snap.terrain.clone(), look: snap.look.clone() };
            handle.tx.send(render::RenderMsg::Frame(Box::new(req)))?;
            Ok(())
        };
        send(&mut id)?;
        std::thread::sleep(Duration::from_millis(2));
        send(&mut id)?;
        let mut sent = 2;
        for i in 0..n + 20 {
            if i == 20 {
                times.clear();
            }
            let f = handle.rx.recv_timeout(Duration::from_secs(10))?;
            if sent < n + 20 {
                send(&mut id)?;
                sent += 1;
            }
            times.push(f.gpu_ms.unwrap_or(0.0));
            last = Some(f);
        }
        if std::env::var_os("POCKET_BENCH_DUMP").is_some() {
            eprintln!("{:?}", times.iter().map(|t| (t * 10.0).round() / 10.0).collect::<Vec<_>>());
        }
        // The GPU clocks itself down after a second or so of this: the fastest
        // frames measure the work itself (compare changes by these), the
        // later ones what it settles to.
        let settled = {
            let mut late = times[times.len() / 2..].to_vec();
            late.sort_by(|a, b| a.total_cmp(b));
            late[late.len() / 2]
        };
        times.sort_by(|a, b| a.total_cmp(b));
        SETTLED.with(|c| c.set(settled));
        Ok((times[times.len() / 20], t0.elapsed().as_secs_f64() * 1000.0 / (n + 20) as f64, last.expect("a frame")))
    };
    let base = globals.dims[3];
    // A/B: frames alternate between the usual flags and these added, so both
    // see the same GPU clock; the ratio is what the change is worth.
    if let Some(ab) = flag(args, "--ab").and_then(|v| v.parse::<u32>().ok()) {
        let (mut ta, mut tb) = (Vec::new(), Vec::new());
        let (mut fa, mut fb) = (None, None);
        for i in 0..600 {
            let (f, out) = if i % 2 == 0 { (base, &mut ta) } else { (base | ab, &mut tb) };
            let mut g = globals;
            g.dims[3] = f;
            let req = render::FrameRequest { id: 10_000 + i, width: pw, height: ph, globals: g, instances: culled.insts.clone(), holes: Vec::new(), grid: culled.grid.clone(), scene: snap.scene.clone(), terrain: snap.terrain.clone(), look: snap.look.clone() };
            handle.tx.send(render::RenderMsg::Frame(Box::new(req)))?;
            let f = handle.rx.recv_timeout(Duration::from_secs(10))?;
            if i >= 40 {
                out.push(f.gpu_ms.unwrap_or(0.0));
            }
            if i % 2 == 0 { fa = Some(f) } else { fb = Some(f) }
        }
        // How far the pictures differ (0-255 per channel).
        if let (Some(a), Some(b)) = (&fa, &fb) {
            let (mut sum, mut max, mut over) = (0u64, 0u8, 0usize);
            for (p, q) in a.pixels.iter().zip(&b.pixels) {
                let (p, q) = (render::unpack(*p), render::unpack(*q));
                let d = (0..3).map(|k| p[k].abs_diff(q[k])).max().unwrap_or(0);
                sum += d as u64;
                max = max.max(d);
                over += (d > 24) as usize;
            }
            println!("  picture: mean difference {:.2}, max {max}, {:.2}% of pixels differ by more than 24", sum as f64 / a.pixels.len() as f64, over as f64 * 100.0 / a.pixels.len() as f64);
            if let Some(dir) = flag(args, "--shots") {
                for (name, f) in [("a", a), ("b", b)] {
                    let rgb: Vec<u8> = f.pixels.iter().flat_map(|p| render::unpack(*p)).collect();
                    std::fs::write(PathBuf::from(&dir).join(format!("{name}.png")), png::encode(f.width, f.height, &rgb))?;
                    std::fs::write(PathBuf::from(&dir).join(format!("{name}.rgb")), &rgb)?;
                }
            }
        }
        let med = |v: &mut Vec<f32>| {
            v.sort_by(|a, b| a.total_cmp(b));
            v[v.len() / 2]
        };
        let (a, b) = (med(&mut ta), med(&mut tb));
        println!("{pw}x{ph} px: A (as is) {a:.2} ms, B (+{ab}) {b:.2} ms: B/A = {:.2}", b / a);
        handle.shutdown();
        return Ok(());
    }
    let (full, wall, _) = run(base, 300)?;
    println!("{pw}x{ph} px, {} instances: pass {full:.2} ms at full clock, {:.2} ms settled; throughput {wall:.2} ms/frame", culled.insts.len(), SETTLED.with(|c| c.get()));
    if has(args, "--breakdown") {
        use render::{FLAG_HEATMAP, FLAG_MARCH_ONLY, FLAG_NO_AO, FLAG_NO_INSTANCES, FLAG_SHADOWS};
        let rows = [
            ("no shadows", base & !FLAG_SHADOWS),
            ("no shadows, AO, open sky", (base & !FLAG_SHADOWS) | FLAG_NO_AO),
            ("view ray march only", base | FLAG_MARCH_ONLY),
            ("march only, no instances", base | FLAG_MARCH_ONLY | FLAG_NO_INSTANCES),
        ];
        for (name, f) in rows {
            let (ms, _, _) = run(f, 200)?;
            println!("  {name:<28} {ms:6.2} ms  ({:+.0}%)", (ms / full - 1.0) * 100.0);
        }
        let (_, _, heat) = run(base | FLAG_HEATMAP, 3)?;
        let n = heat.pixels.len() as f64;
        let (mut steps, mut cand, mut capped) = (0.0, 0.0, 0usize);
        for p in &heat.pixels {
            let c = render::unpack(*p);
            steps += c[0] as f64 / 255.0 * 200.0;
            cand += c[1] as f64 / 255.0 * 24.0;
            capped += (c[0] as f64 / 255.0 * 200.0 >= 159.0) as usize;
        }
        println!("  view ray: {:.1} steps and {:.1} candidates per pixel on average; {:.1}% of pixels use all 160 steps", steps / n, cand / n, capped as f64 / n * 100.0);
    }
    handle.shutdown();
    Ok(())
}

fn check_file(args: &[String]) -> Result<()> {
    let p = args.first().context("usage: pocket check TYPE.js")?;
    let src = std::fs::read_to_string(p)?;
    let ct = match lang::compile(&src) {
        Ok(ct) => ct,
        Err(d) => bail!("{}", lang::format_diags(&d)),
    };
    let rep = lang::probe::probe(&ct).map_err(|d| anyhow::anyhow!(lang::format_diags(&d)))?;
    println!("ok: {} bounds {:?} tags {:?}; solid from y={:.2} to y={:.2}; max {} ops per sdf call", ct.meta.name, ct.meta.bounds, ct.meta.tags, rep.bottom, rep.top, rep.max_fuel);
    Ok(())
}

fn selftest() -> Result<()> {
    use lang::{compile, probe::probe};
    println!("built-in types:");
    let mut types = Vec::new();
    for (i, src) in model::BUILTIN_SOURCES.iter().enumerate() {
        let ct = compile(src).map_err(|d| anyhow::anyhow!(lang::format_diags(&d)))?;
        let rep = probe(&ct).map_err(|d| anyhow::anyhow!(lang::format_diags(&d)))?;
        println!("  {:<12} ok (probe max fuel {})", ct.meta.name, rep.max_fuel);
        types.push((i as u32 + 1, ct));
    }
    let gpu = render::gpu::Gpu::new()?;
    println!("GPU: {}", gpu.name);
    let refs: Vec<(u32, &lang::CompiledType)> = types.iter().map(|(i, t)| (*i, t)).collect();
    let t0 = Instant::now();
    let pipe = gpu.build_pipeline(&render::shader::assemble(&refs), 1).map_err(|e| anyhow::anyhow!(e))?;
    println!("pipeline built in {:.0} ms", t0.elapsed().as_secs_f64() * 1000.0);
    let terrain = terrain::Terrain::new(4242, terrain::default_biomes());
    for (id, t) in &types {
        model::gpu_parity(&gpu, &pipe, &terrain, *id, t).map_err(|d| anyhow::anyhow!(lang::format_diags(&d)))?;
        println!("  parity {:<12} ok", t.meta.name);
    }
    let pts: Vec<[f32; 4]> = (0..2000).map(|i| {
        let a = i as f32 * 2.399;
        let r = (i as f32).sqrt() * 120.0;
        [a.cos() * r, 0.0, a.sin() * r, 0.0]
    }).collect();
    let out = gpu.probe(&pipe, &model::probe_globals(&terrain), 1, 0, &render::GpuInst::default(), &pts)?;
    let worst = pts.iter().zip(&out).map(|(p, g)| (g[0] - terrain.height(p[0], p[2])).abs()).fold(0.0f32, f32::max);
    println!("  parity terrain      worst |Δh| = {worst:.2e} m ({})", if worst <= 1e-3 { "ok" } else { "FAIL" });
    if worst > 1e-3 {
        bail!("terrain parity failed");
    }
    println!("all good");
    Ok(())
}

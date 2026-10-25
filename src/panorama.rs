//! Behind the title and the loading screen: Maple Cove (see `starter`),
//! cutting between a few views of it while its people go about their day.
//! The same for everyone, so a first-time player sees it too. It loads in
//! the background; until the first frame the land of words is shown, and
//! the world fades in over it. Only on a GPU: the CPU renderer is too slow.
//!
//! It lives from the title until a world is stepped into (it is kept behind
//! that world's loading screen), and starts again on the way back to the
//! title. Nothing in it is saved, and it asks no AI for anything.

use crate::render::{self, Camera, Frame, FrameRequest, RenderHandle, RenderMsg};
use crate::sim::Sim;
use crate::world::WorldSnapshot;
use glam::Vec3;
use serde::Deserialize;
use std::sync::Arc;
use std::time::Instant;

/// Seconds each view is held, and how long the fade through dark between
/// two takes (half out, half in).
const HOLD: f32 = 12.0;
const FADE: f32 = 1.2;
/// Lines drawn (the window scales them up, softly: it is a backdrop).
const LINES: f32 = 300.0;

/// One view: where to stand and look, as `pocket snapshot --at x,z,yawDeg
/// --lift M --pitch D --time H` takes them, and a slow turn (degrees a second).
#[derive(Deserialize, Clone, Debug)]
pub struct Shot {
    #[allow(dead_code)]
    pub name: String,
    pub at: [f32; 3],
    #[serde(default)]
    pub lift: f32,
    #[serde(default)]
    pub pitch: f32,
    pub hour: f64,
    #[serde(default)]
    pub turn: f32,
}

pub fn shots() -> Vec<Shot> {
    serde_json::from_str(crate::starter::SHOTS).unwrap_or_default()
}

struct Ready {
    snap: Arc<WorldSnapshot>,
    sim: Sim,
    render: RenderHandle,
}

type Loaded = (Arc<crate::db::Db>, Arc<render::gpu::Gpu>, Arc<WorldSnapshot>);

pub struct Panorama {
    loading: Option<crossbeam_channel::Receiver<Option<Loaded>>>,
    ready: Option<Ready>,
    frame: Option<Frame>,
    waiting: bool,
    next_id: u64,
    shots: Vec<Shot>,
    /// The view now (an index into `shots`), and since when.
    shot: usize,
    since: Instant,
    last: Instant,
    /// When the first frame came (the world fades in from then).
    first: Option<Instant>,
}

impl Panorama {
    /// Begin loading Maple Cove in the background.
    pub fn start() -> Panorama {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let _ = std::thread::Builder::new().name("panorama".into()).spawn(move || {
            // POCKET_NO_PANORAMA=1: only the drawn cove (as with no GPU), to look at it.
            if std::env::var_os("POCKET_NO_PANORAMA").is_some() {
                let _ = tx.send(None);
                return;
            }
            let got = crate::starter::backdrop()
                .map_err(|e| crate::log::info(format!("panorama: {e:#}")))
                .ok()
                .and_then(|p| crate::load_with(&p, false).ok())
                .and_then(|l| l.gpu.map(|g| (l.db, g, l.snap)));
            if got.is_none() {
                crate::log::info("panorama: no view (no GPU, or Maple Cove didn't load)");
            }
            let _ = tx.send(got);
        });
        let now = Instant::now();
        Panorama { loading: Some(rx), ready: None, frame: None, waiting: false, next_id: 1, shots: shots(), shot: 0, since: now, last: now, first: None }
    }

    /// Take what came back and ask for the next frame, `size` pixels of window.
    pub fn tick(&mut self, size: (f32, f32)) {
        if let Some(rx) = &self.loading {
            if let Ok(got) = rx.try_recv() {
                self.loading = None;
                if let Some((db, gpu, snap)) = got {
                    if let Some(s) = self.shots.first() {
                        let stand = Vec3::new(s.at[0], snap.terrain.height(s.at[0], s.at[1]), s.at[1]);
                        let sim = Sim::new(db, snap.clone(), stand, s.at[2].to_radians(), hour(s.hour), 1);
                        self.ready = Some(Ready { snap, sim, render: render::gpu::spawn(gpu) });
                        self.cut(0);
                    }
                }
            }
        }
        let now = Instant::now();
        let dt = now.duration_since(self.last).as_secs_f32();
        self.last = now;
        if self.ready.is_none() {
            return;
        }
        // On to the next view once this one has been held (cut while dark).
        if self.since.elapsed().as_secs_f32() >= HOLD + FADE * 0.5 {
            self.cut((self.shot + 1) % self.shots.len());
        }
        let Some(r) = self.ready.as_mut() else { return };
        // Its people go about their day; the light moves with the clock.
        r.sim.step(dt);
        while let Ok(f) = r.render.rx.try_recv() {
            self.waiting = false;
            if f.width > 0 {
                self.first.get_or_insert(now);
                self.frame = Some(f);
            }
        }
        if self.waiting {
            return;
        }
        let k = (LINES / size.1.max(1.0)).min(1.0);
        let (w, h) = (((size.0 * k).round() as u32).max(8), ((size.1 * k).round() as u32).max(8));
        let s = &self.shots[self.shot];
        let t = self.since.elapsed().as_secs_f32();
        let cam = camera(&r.snap, s, t);
        let aspect = w as f32 / h as f32;
        let drawn = r.sim.draw(cam.pos, render::VIEW_DIST);
        let culled = crate::world::cull::cull(&r.snap, &mut r.sim.cache, &cam, aspect, &drawn.insts, &Default::default());
        let light = render::sky::lighting(r.sim.t, &r.snap.look.palette);
        let sp = render::SceneParams {
            terrain: &r.snap.terrain,
            palette: &r.snap.look.palette,
            camera: cam,
            width: w,
            height: h,
            pixel_aspect: 1.0,
            light,
            time: self.first.map_or(0.0, |f| f.elapsed().as_secs_f32()),
            frame: self.next_id as u32,
            shadows: true,
            lights: &drawn.lights,
            weather: render::WeatherView::of(&r.sim.wx, 0.0, !r.sim.open_sky(cam.pos)),
        };
        let globals = render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
        let req = FrameRequest { id: self.next_id, width: w, height: h, globals, instances: culled.insts, holes: drawn.holes.clone(), grid: culled.grid, scene: r.snap.scene.clone(), terrain: r.snap.terrain.clone(), look: r.snap.look.clone() };
        self.next_id += 1;
        self.waiting = r.render.tx.send(RenderMsg::Frame(Box::new(req))).is_ok();
    }

    /// Go to view `i`: stand there at its hour.
    fn cut(&mut self, i: usize) {
        self.shot = i;
        self.since = Instant::now();
        let (Some(r), Some(s)) = (self.ready.as_mut(), self.shots.get(i)) else { return };
        r.sim.player.pos = Vec3::new(s.at[0], r.snap.terrain.height(s.at[0], s.at[1]), s.at[1]);
        r.sim.t = hour(s.hour);
    }

    /// The latest view, once there is one.
    pub fn frame(&self) -> Option<&Frame> {
        self.frame.as_ref()
    }

    /// How much of the view shows: 0 → 1 over the first second and a half,
    /// and down to dark and back up again across each cut.
    pub fn shown(&self) -> f32 {
        let Some(first) = self.first else { return 0.0 };
        let fade_in = (first.elapsed().as_secs_f32() / 1.5).min(1.0);
        let t = self.since.elapsed().as_secs_f32();
        let half = FADE * 0.5;
        let cut = if t < half && first.elapsed().as_secs_f32() > t { t / half } else if t > HOLD { 1.0 - ((t - HOLD) / half).min(1.0) } else { 1.0 };
        fade_in.min(cut)
    }
}

impl Drop for Panorama {
    fn drop(&mut self) {
        if let Some(r) = self.ready.as_mut() {
            r.render.shutdown();
        }
    }
}

fn hour(h: f64) -> f64 {
    (h / 24.0).rem_euclid(1.0) * render::sky::DAY_SECONDS
}

/// Where the view looks from `t` seconds into a shot (as `pocket snapshot`
/// places its camera, turning slowly).
fn camera(snap: &WorldSnapshot, s: &Shot, t: f32) -> Camera {
    let (x, z) = (s.at[0], s.at[1]);
    let y = snap.terrain.height(x, z).max(snap.terrain.water(x, z) - 0.4) + 1.65 + s.lift;
    Camera { pos: Vec3::new(x, y, z), yaw: (s.at[2] + s.turn * t).to_radians(), pitch: s.pitch.to_radians(), fov_y: 1.05, roll: 0.0 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn every_shot_reads() {
        let shots = super::shots();
        assert!(shots.len() >= 3, "worlds/starter/shots.json");
        assert!(shots.iter().all(|s| (0.0..24.0).contains(&s.hour)));
    }
}

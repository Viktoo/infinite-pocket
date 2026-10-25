//! `pocket record`: a world run unseen, saved frame by frame as PNGs and
//! made into a clip with ffmpeg (when it is installed). The world goes on
//! as in play (people about their day, weather, fire, water), so motion can
//! be looked at, not only still views; and short clips come of it.
//!
//! The camera stands still, moves in a straight line from `--at` to `--to`,
//! or follows a character. A private copy of the world is run, so nothing
//! of the recording is saved into it.

use crate::render::{self, Camera, FrameRequest, RenderMsg};
use crate::sim::Sim;
use crate::world::WorldSnapshot;
use crate::{flag, has};
use anyhow::{bail, Context, Result};
use glam::Vec3;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the camera stands and looks: as `pocket snapshot` takes it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub x: f32,
    pub z: f32,
    pub yaw: f32,
    pub lift: f32,
    pub pitch: f32,
}

impl Pose {
    fn lerp(&self, o: &Pose, k: f32) -> Pose {
        let m = |a: f32, b: f32| a + (b - a) * k;
        Pose { x: m(self.x, o.x), z: m(self.z, o.z), yaw: m(self.yaw, o.yaw), lift: m(self.lift, o.lift), pitch: m(self.pitch, o.pitch) }
    }

    fn camera(&self, snap: &WorldSnapshot) -> Camera {
        let y = snap.terrain.height(self.x, self.z).max(snap.terrain.water(self.x, self.z) - 0.4) + 1.65 + self.lift;
        Camera { pos: Vec3::new(self.x, y, self.z), yaw: self.yaw.to_radians(), pitch: self.pitch.to_radians(), fov_y: 1.05, roll: 0.0 }
    }
}

pub enum Move {
    Still(Pose),
    Line(Pose, Pose),
    /// A character, seen from `dist` metres off, from the side `--at`'s yaw faces.
    Follow { id: i64, from: Pose, dist: f32 },
}

pub struct Plan {
    pub frames: u32,
    pub fps: f32,
    /// Game seconds that pass for each second of clip.
    pub speed: f32,
    pub hour: Option<f64>,
    pub width: u32,
    pub height: u32,
    pub path: Move,
    /// Frames rendered past the end and faded into the start, so the clip
    /// loops without a jump (clouds and people don't come back to where they began).
    pub fade: u32,
}

impl Plan {
    /// Where the camera is at frame `i` of the clip (smoothed in and out).
    fn camera(&self, sim: &Sim, i: u32, last: Option<Camera>) -> Result<Camera> {
        let k = if self.frames > 1 { (i as f32 / (self.frames - 1) as f32).min(1.0) } else { 0.0 };
        let ease = k * k * (3.0 - 2.0 * k);
        Ok(match &self.path {
            Move::Still(p) => p.camera(&sim.snap),
            Move::Line(a, b) => a.lerp(b, ease).camera(&sim.snap),
            Move::Follow { id, from, dist } => {
                let n = sim.cast.get(*id).with_context(|| format!("no character {id}"))?;
                let target = n.a.pos + Vec3::Y * n.a.dims.height * 0.6;
                let back = Vec3::new(from.yaw.to_radians().sin(), 0.0, from.yaw.to_radians().cos());
                let ground = n.a.pos - back * *dist;
                let y = sim.snap.terrain.height(ground.x, ground.z).max(n.a.pos.y) + 1.65 + from.lift;
                let want = Vec3::new(ground.x, y, ground.z);
                // A camera on a dolly, not on a string: it eases after them.
                let pos = match last {
                    Some(c) => c.pos + (want - c.pos) * 0.08,
                    None => want,
                };
                let d = target - pos;
                Camera { pos, yaw: d.x.atan2(d.z), pitch: d.y.atan2(Vec3::new(d.x, 0.0, d.z).length()) + from.pitch.to_radians(), fov_y: 0.9, roll: 0.0 }
            }
        })
    }

    pub fn from_args(args: &[String]) -> Result<Plan> {
        let num = |f: &str| flag(args, f).map(|v| v.parse::<f32>().with_context(|| format!("{f} takes a number"))).transpose();
        let pose = |at: &str, lift: &str, pitch: &str, base: Option<Pose>| -> Result<Option<Pose>> {
            let Some(s) = flag(args, at) else { return Ok(None) };
            let v: Vec<f32> = s.split(',').map(|x| x.trim().parse::<f32>()).collect::<Result<_, _>>().with_context(|| format!("{at} x,z,yawDeg"))?;
            if v.len() != 3 {
                bail!("{at} takes x,z,yawDeg");
            }
            Ok(Some(Pose { x: v[0], z: v[1], yaw: v[2], lift: num(lift)?.or(base.map(|b| b.lift)).unwrap_or(0.0), pitch: num(pitch)?.or(base.map(|b| b.pitch)).unwrap_or(-4.0) }))
        };
        let from = pose("--at", "--lift", "--pitch", None)?.context("--at x,z,yawDeg is required")?;
        let path = if let Some(id) = flag(args, "--follow") {
            Move::Follow { id: id.parse().context("--follow takes a character id")?, from, dist: num("--dist")?.unwrap_or(6.0) }
        } else if let Some(to) = pose("--to", "--to-lift", "--to-pitch", Some(from))? {
            Move::Line(from, to)
        } else {
            Move::Still(from)
        };
        let fps = num("--fps")?.unwrap_or(15.0).clamp(1.0, 60.0);
        let frames = match num("--seconds")? {
            Some(s) => (s * fps).round().max(1.0) as u32,
            None => flag(args, "--frames").map(|v| v.parse::<u32>()).transpose().context("--frames takes a count")?.unwrap_or((6.0 * fps) as u32),
        };
        let (width, height) = match flag(args, "--size") {
            Some(s) => {
                let (w, h) = s.split_once('x').context("--size WxH, in pixels")?;
                (w.parse().context("--size width")?, h.parse().context("--size height")?)
            }
            None => (800, 450),
        };
        Ok(Plan { frames, fps, speed: num("--speed")?.unwrap_or(1.0).max(0.0), hour: flag(args, "--time").map(|h| h.parse::<f64>()).transpose().context("--time takes an hour")?, width, height, path, fade: flag(args, "--loop").map(|v| v.parse::<u32>()).transpose().context("--loop takes a count of frames")?.unwrap_or(0).min(frames) })
    }
}

pub fn cli(args: &[String]) -> Result<()> {
    let file = PathBuf::from(args.first().filter(|a| !a.starts_with("--")).context(USAGE)?);
    let plan = Plan::from_args(args)?;
    let out = PathBuf::from(flag(args, "--out").context("--out DIR is required (the frames go there)")?);
    let n = record(&file, &plan, &out)?;
    eprintln!("{n} frames in {}", out.display());
    let clips: Vec<&str> = ["--mp4", "--webp", "--gif"].into_iter().filter(|f| has(args, f)).collect();
    // A clip unless only frames were asked for.
    let clips = if clips.is_empty() && !has(args, "--frames-only") { vec!["--mp4"] } else { clips };
    for c in clips {
        let kind = &c[2..];
        match encode(&out, plan.fps, kind) {
            Ok(p) => eprintln!("{}", p.display()),
            Err(e) => eprintln!("no {kind}: {e:#} (the frames are there; install ffmpeg to make clips)"),
        }
    }
    Ok(())
}

pub const USAGE: &str = "usage: pocket record FILE --at x,z,yawDeg --out DIR [--lift M] [--pitch DEG] [--to x,z,yawDeg [--to-lift M] [--to-pitch DEG]] [--follow ID [--dist M]] [--seconds S | --frames N] [--fps 15] [--speed 1] [--time HOUR] [--size 800x450] [--loop N] [--mp4] [--webp] [--gif] [--frames-only]";

/// Run the world and save each frame as `DIR/frame00000.png`; how many were saved.
pub fn record(file: &Path, plan: &Plan, out: &Path) -> Result<u32> {
    let copy = scratch_copy(file)?;
    let l = crate::load(&copy)?;
    std::fs::create_dir_all(out)?;
    for e in std::fs::read_dir(out)?.flatten() {
        let name = e.file_name();
        if name.to_string_lossy().starts_with("frame") {
            let _ = std::fs::remove_file(e.path());
        }
    }
    let t0 = match plan.hour {
        Some(h) => (h / 24.0).rem_euclid(1.0) * render::sky::DAY_SECONDS,
        None => l.db.player().map(|p| p.t_game).unwrap_or(0.33 * render::sky::DAY_SECONDS),
    };
    let start = match &plan.path {
        Move::Still(p) | Move::Line(p, _) | Move::Follow { from: p, .. } => *p,
    };
    let stand = Vec3::new(start.x, l.snap.terrain.height(start.x, start.z), start.z);
    let mut sim = Sim::new(l.db.clone(), l.snap.clone(), stand, start.yaw.to_radians(), t0, 1);
    // A first step settles who is about at this hour.
    sim.step(0.05);
    let mut handle = match &l.gpu {
        Some(g) => render::gpu::spawn(g.clone()),
        None => render::cpu::spawn(),
    };
    let per_frame = plan.speed / plan.fps;
    let mut last = None;
    let result = (|| {
        let mut head = Vec::new();
        for i in 0..plan.frames + plan.fade {
            let cam = plan.camera(&sim, i, last)?;
            last = Some(cam);
            // The world is lived in around the traveler, but people come to
            // see one: keep them out of sight behind the camera, past where
            // anyone seeks company (35 m), well within where all is lived (220 m).
            let behind = cam.pos - Vec3::new(cam.yaw.sin(), 0.0, cam.yaw.cos()) * 45.0;
            sim.player.pos = Vec3::new(behind.x, sim.snap.terrain.height(behind.x, behind.z), behind.z);
            let f = frame(&mut sim, &mut handle, cam, plan, i, l.gpu.is_some())?;
            let mut rgb = Vec::with_capacity((f.width * f.height * 3) as usize);
            for px in &f.pixels {
                rgb.extend_from_slice(&render::unpack(*px));
            }
            if i < plan.fade {
                head.push(rgb.clone());
            }
            // Past the end: the clip goes on from here into its start again.
            let (i, rgb) = match i.checked_sub(plan.frames) {
                Some(k) => {
                    let w = (k + 1) as f32 / (plan.fade + 1) as f32;
                    (k, rgb.iter().zip(&head[k as usize]).map(|(a, b)| (*a as f32 * (1.0 - w) + *b as f32 * w).round() as u8).collect())
                }
                None => (i, rgb),
            };
            std::fs::write(out.join(format!("frame{i:05}.png")), crate::png::encode(f.width, f.height, &rgb))?;
            // On to the next frame's moment (the sim steps a quarter second at most).
            let mut left = per_frame;
            while left > 0.0 {
                let d = left.min(0.25);
                sim.step(d);
                left -= d;
            }
        }
        Ok(plan.frames)
    })();
    handle.shutdown();
    let _ = std::fs::remove_dir_all(copy.parent().unwrap_or(&copy));
    result
}

fn frame(sim: &mut Sim, handle: &mut render::RenderHandle, cam: Camera, plan: &Plan, i: u32, gpu: bool) -> Result<render::Frame> {
    let (w, h) = (plan.width, plan.height);
    let aspect = w as f32 / h as f32;
    let drawn = sim.draw(cam.pos, render::VIEW_DIST);
    let culled = crate::world::cull::cull(&sim.snap, &mut sim.cache, &cam, aspect, &drawn.insts, &Default::default());
    let light = render::sky::lighting(sim.t, &sim.snap.look.palette);
    let sp = render::SceneParams {
        terrain: &sim.snap.terrain,
        palette: &sim.snap.look.palette,
        camera: cam,
        width: w,
        height: h,
        pixel_aspect: 1.0,
        light,
        time: i as f32 / plan.fps,
        frame: i,
        shadows: gpu && std::env::var("POCKET_NO_SHADOWS").is_err(),
        lights: &drawn.lights,
        weather: render::WeatherView::of(&sim.wx, 0.0, !sim.open_sky(cam.pos)),
    };
    let globals = render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
    let req = FrameRequest { id: i as u64 + 1, width: w, height: h, globals, instances: culled.insts, holes: drawn.holes.clone(), grid: culled.grid, scene: sim.snap.scene.clone(), terrain: sim.snap.terrain.clone(), look: sim.snap.look.clone() };
    handle.tx.send(RenderMsg::Frame(Box::new(req)))?;
    let f = handle.rx.recv_timeout(Duration::from_secs(120)).context("render timed out")?;
    if f.width == 0 {
        bail!("render failed");
    }
    Ok(f)
}

/// A private copy of a world file, so the recording leaves it as it was.
fn scratch_copy(path: &Path) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join(format!("pocket-record-{}-{}", std::process::id(), crate::db::now() as u64));
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(path.file_name().unwrap_or_default());
    std::fs::copy(path, &p).with_context(|| format!("copying {}", path.display()))?;
    Ok(p)
}

/// The frames in `dir` made into `dir/clip.KIND` (mp4, webp or gif) by ffmpeg.
pub fn encode(dir: &Path, fps: f32, kind: &str) -> Result<PathBuf> {
    let out = dir.join(format!("clip.{kind}"));
    let fps = format!("{fps}");
    let input = dir.join("frame%05d.png");
    let mut cmd = std::process::Command::new("ffmpeg");
    cmd.args(["-y", "-loglevel", "error", "-framerate", &fps, "-i"]).arg(&input);
    match kind {
        "mp4" => cmd.args(["-c:v", "libx264", "-pix_fmt", "yuv420p", "-crf", "18", "-movflags", "+faststart"]),
        // Looping, for a README; small enough to embed.
        "webp" => cmd.args(["-c:v", "libwebp_anim", "-loop", "0", "-quality", "75", "-compression_level", "6"]),
        // One palette for the whole clip, so colours hold still between frames.
        "gif" => cmd.args(["-vf", "split[a][b];[a]palettegen=stats_mode=diff[p];[b][p]paletteuse=dither=bayer:bayer_scale=4", "-loop", "0"]),
        _ => bail!("no clip kind {kind}"),
    };
    let st = cmd.arg(&out).status().context("running ffmpeg")?;
    if !st.success() {
        bail!("ffmpeg failed ({st})");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn plan_reads_a_line_and_its_length() {
        let p = Plan::from_args(&args("w.pocket --at 1,2,90 --lift 10 --to 5,6,180 --seconds 2 --fps 10 --speed 20 --time 17.5")).unwrap();
        assert_eq!(p.frames, 20);
        assert_eq!(p.hour, Some(17.5));
        let Move::Line(a, b) = p.path else { panic!("a line") };
        // The end keeps the start's lift and pitch unless given its own.
        assert_eq!((a.lift, b.lift, b.pitch), (10.0, 10.0, a.pitch));
        assert_eq!(a.lerp(&b, 0.5).x, 3.0);
        assert!(matches!(Plan::from_args(&args("w.pocket --at 0,0,0 --follow 4")).unwrap().path, Move::Follow { id: 4, .. }));
        assert!(Plan::from_args(&args("w.pocket --to 0,0,0")).is_err());
    }
}

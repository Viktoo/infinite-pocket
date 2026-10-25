//! Headless wgpu compute renderer. No window or surface: each frame is
//! raymarched into a storage buffer and read back through a ring of three
//! mappable buffers, so the GPU never waits on the CPU.

use super::{Frame, FrameRequest, GpuInst, Globals, RenderHandle, RenderMsg};
use anyhow::{Context, anyhow};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub bgl: wgpu::BindGroupLayout,
    pub layout: wgpu::PipelineLayout,
    pub name: String,
    pub timestamps: bool,
}

/// A compiled scene shader: the render and probe entry points.
pub struct ScenePipeline {
    pub key: u64,
    pub render: wgpu::ComputePipeline,
    pub probe: wgpu::ComputePipeline,
    pub heightmap: wgpu::ComputePipeline,
    pub heightmap_fine: wgpu::ComputePipeline,
}

impl std::fmt::Debug for ScenePipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ScenePipeline({:x})", self.key)
    }
}

fn storage(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only }, has_dynamic_offset: false, min_binding_size: None },
        count: None,
    }
}

impl Gpu {
    pub fn new() -> anyhow::Result<Arc<Gpu>> {
        if std::env::var("POCKET_NO_GPU").is_ok_and(|v| v == "1") {
            return Err(anyhow!("GPU disabled by POCKET_NO_GPU=1"));
        }
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            ..Default::default()
        }))
        .context("no GPU adapter")?;
        let info = adapter.get_info();
        let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let mut features = wgpu::Features::empty();
        if timestamps {
            features |= wgpu::Features::TIMESTAMP_QUERY;
        }
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("pocket"),
            required_features: features,
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        }))
        .context("requesting GPU device")?;
        device.on_uncaptured_error(Arc::new(|e: wgpu::Error| {
            crate::log::error(format!("uncaptured GPU error: {e}"));
        }));
        let bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Uniform, has_dynamic_offset: false, min_binding_size: None },
                    count: None,
                },
                storage(1, true),
                storage(2, true),
                storage(3, true),
                storage(4, false),
                storage(5, false),
                storage(6, false),
                storage(7, true),
                storage(8, true),
            ],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: Some("scene"), bind_group_layouts: &[Some(&bgl)], immediate_size: 0 });
        let name = format!("{} ({:?}{})", info.name, info.backend, if timestamps { ", timestamps" } else { "" });
        Ok(Arc::new(Gpu { device, queue, bgl, layout, name, timestamps }))
    }

    /// Validate and compile a full scene shader. Blocking: call from a worker thread.
    pub fn build_pipeline(&self, wgsl: &str, key: u64) -> Result<Arc<ScenePipeline>, String> {
        super::shader::validate(wgsl)?;
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = self.device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("scene"), source: wgpu::ShaderSource::Wgsl(wgsl.into()) });
        let mk = |entry: &str| {
            self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&self.layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        };
        // The three entry points compile independently: do it in parallel.
        let (render, probe, heightmap, heightmap_fine) = std::thread::scope(|s| {
            let a = s.spawn(|| mk("render_main"));
            let b = s.spawn(|| mk("probe_main"));
            let d = s.spawn(|| mk("heightmap_fine_main"));
            let c = mk("heightmap_main");
            (a.join().expect("compile thread"), b.join().expect("compile thread"), c, d.join().expect("compile thread"))
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            return Err(format!("pipeline creation failed: {e}"));
        }
        Ok(Arc::new(ScenePipeline { key, render, probe, heightmap, heightmap_fine }))
    }

    fn buf(&self, label: &str, size: u64, usage: wgpu::BufferUsages) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size: size.max(16), usage, mapped_at_creation: false })
    }

    /// Evaluate points on the GPU. mode 0: type `tid` with params of `inst`
    /// → (sdf, r, g, b). mode 1: terrain → (height, ground rgb).
    pub fn probe(&self, pipe: &ScenePipeline, globals: &Globals, mode: u32, tid: u32, inst: &GpuInst, points: &[[f32; 4]]) -> anyhow::Result<Vec<[f32; 4]>> {
        let mut g = *globals;
        g.ground = [0.0; 4];
        g.lands = [0.0; 4];
        self.probe_ground(pipe, &g, &[0], &[0], mode, tid, inst, points)
    }

    /// `probe` of the terrain with its landform map (`LandMap::words`, matching `globals.lands`).
    pub fn probe_lands(&self, pipe: &ScenePipeline, globals: &Globals, lands: &[u32], mode: u32, points: &[[f32; 4]]) -> anyhow::Result<Vec<[f32; 4]>> {
        let mut g = *globals;
        g.ground = [0.0; 4];
        self.probe_ground(pipe, &g, &[0], lands, mode, 0, &super::GpuInst::default(), points)
    }

    /// `probe` with the ground edits' tiles (`ground::Pack::words`, matching `globals.ground`).
    #[allow(clippy::too_many_arguments)]
    pub fn probe_ground(&self, pipe: &ScenePipeline, globals: &Globals, ground: &[u32], lands: &[u32], mode: u32, tid: u32, inst: &GpuInst, points: &[[f32; 4]]) -> anyhow::Result<Vec<[f32; 4]>> {
        self.probe_holed(pipe, globals, ground, lands, mode, tid, inst, &[], points)
    }

    /// `probe_ground` with the frame's holes the instance's `more` points into.
    #[allow(clippy::too_many_arguments)]
    pub fn probe_holed(&self, pipe: &ScenePipeline, globals: &Globals, ground: &[u32], lands: &[u32], mode: u32, tid: u32, inst: &GpuInst, holes: &[[f32; 4]], points: &[[f32; 4]]) -> anyhow::Result<Vec<[f32; 4]>> {
        use wgpu::BufferUsages as U;
        let n = points.len();
        let mut g = *globals;
        g.probe = [n as u32, mode, tid, 0];
        g.extra[2] = 0;
        let ub = self.buf("probe-globals", std::mem::size_of::<Globals>() as u64, U::UNIFORM | U::COPY_DST);
        self.queue.write_buffer(&ub, 0, bytemuck::bytes_of(&g));
        let ib = self.buf("probe-inst", INST_BYTES, U::STORAGE | U::COPY_DST);
        self.queue.write_buffer(&ib, 0, bytemuck::bytes_of(inst));
        let cb = self.buf("probe-cells", 16, U::STORAGE);
        let words: Vec<u32> = holes.iter().flat_map(|h| h.map(f32::to_bits)).chain([0; 4]).collect();
        let tb = self.buf("probe-items", (words.len() * 4) as u64, U::STORAGE | U::COPY_DST);
        self.queue.write_buffer(&tb, 0, bytemuck::cast_slice(&words));
        let ob = self.buf("probe-out", 16, U::STORAGE);
        let size = (n * 16) as u64;
        let pb = self.buf("probe-io", size, U::STORAGE | U::COPY_DST | U::COPY_SRC);
        self.queue.write_buffer(&pb, 0, bytemuck::cast_slice(points));
        let rb = self.buf("probe-read", size, U::MAP_READ | U::COPY_DST);
        let hb = self.buf("probe-hmap", 16, U::STORAGE);
        let gb = self.buf("probe-ground", (ground.len() * 4) as u64, U::STORAGE | U::COPY_DST);
        self.queue.write_buffer(&gb, 0, bytemuck::cast_slice(ground));
        let lb = self.buf("probe-lands", (lands.len() * 4) as u64, U::STORAGE | U::COPY_DST);
        self.queue.write_buffer(&lb, 0, bytemuck::cast_slice(lands));
        let bg = self.bind_group(&ub, &ib, &cb, &tb, &ob, &pb, &hb, &gb, &lb);
        let mut enc = self.device.create_command_encoder(&Default::default());
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("probe"), timestamp_writes: None });
            pass.set_pipeline(&pipe.probe);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups((n as u32).div_ceil(64), 1, 1);
        }
        enc.copy_buffer_to_buffer(&pb, 0, &rb, 0, Some(size));
        let idx = self.queue.submit([enc.finish()]);
        let done = Arc::new(AtomicU8::new(0));
        let d2 = done.clone();
        rb.slice(..size).map_async(wgpu::MapMode::Read, move |r| d2.store(if r.is_ok() { 1 } else { 2 }, Ordering::SeqCst));
        let _ = self.device.poll(wgpu::PollType::Wait { submission_index: Some(idx), timeout: Some(Duration::from_secs(10)) });
        for _ in 0..1000 {
            if done.load(Ordering::SeqCst) != 0 {
                break;
            }
            let _ = self.device.poll(wgpu::PollType::Poll);
            std::thread::sleep(Duration::from_millis(1));
        }
        if done.load(Ordering::SeqCst) != 1 {
            return Err(anyhow!("probe readback failed"));
        }
        let out = {
            let view = rb.slice(..size).get_mapped_range().map_err(|e| anyhow!("{e:?}"))?;
            bytemuck::cast_slice::<u8, [f32; 4]>(&view).to_vec()
        };
        rb.unmap();
        Ok(out)
    }

    #[allow(clippy::too_many_arguments)]
    fn bind_group(&self, g: &wgpu::Buffer, i: &wgpu::Buffer, c: &wgpu::Buffer, t: &wgpu::Buffer, o: &wgpu::Buffer, p: &wgpu::Buffer, h: &wgpu::Buffer, gr: &wgpu::Buffer, l: &wgpu::Buffer) -> wgpu::BindGroup {
        fn e(b: u32, buf: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding: b, resource: buf.as_entire_binding() }
        }
        self.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("scene"),
            layout: &self.bgl,
            entries: &[e(0, g), e(1, i), e(2, c), e(3, t), e(4, o), e(5, p), e(6, h), e(7, gr), e(8, l)],
        })
    }
}

struct Slot {
    buf: Option<wgpu::Buffer>,
    cap: u64,
    /// 0 = idle, 1 = mapping, 2 = mapped, 3 = failed
    state: Arc<AtomicU8>,
    busy: bool,
    id: u64,
    w: u32,
    h: u32,
    ts_offset: u64,
    view: [[f32; 4]; 5],
}

struct Renderer {
    gpu: Arc<Gpu>,
    globals: wgpu::Buffer,
    inst: wgpu::Buffer,
    inst_cap: u64,
    cells: wgpu::Buffer,
    cells_cap: u64,
    items: wgpu::Buffer,
    items_cap: u64,
    out: wgpu::Buffer,
    out_cap: u64,
    probe_dummy: wgpu::Buffer,
    hmap: wgpu::Buffer,
    /// The ground edits' tiles round the camera, and which (origin, version) they hold.
    ground: wgpu::Buffer,
    ground_cap: u64,
    ground_state: Option<([i32; 2], u32)>,
    /// The landform map, and which one it holds (its address).
    lands: wgpu::Buffer,
    lands_cap: u64,
    lands_state: usize,
    /// (origin x, origin z, terrain epoch) of the heightmap contents.
    hmap_state: Option<(f32, f32, u32)>,
    /// The same for the fine heightmap.
    fine_state: Option<(f32, f32, u32)>,
    bind: Option<wgpu::BindGroup>,
    slots: Vec<Slot>,
    order: std::collections::VecDeque<usize>,
    queries: Option<(wgpu::QuerySet, wgpu::Buffer)>,
    ts_period: f32,
}

const SLOTS: usize = 3;
const INST_BYTES: u64 = std::mem::size_of::<GpuInst>() as u64;
/// Heightmap: 512² cells of 0.75 m (±192 m), recentred every 24 m.
const HMAP_N: usize = 512;
const HMAP_CELL: f32 = 0.75;
const HMAP_RECENTER: f32 = 24.0;
/// Fine heightmap: 384² cells of 12.5 cm (±24 m), recentred every 6 m (the
/// shader reads it within 16 m, so it always covers what it serves).
const FINE_N: usize = 384;
const FINE_CELL: f32 = 0.125;
const FINE_RECENTER: f32 = 6.0;

impl Renderer {
    fn new(gpu: Arc<Gpu>) -> Renderer {
        use wgpu::BufferUsages as U;
        let globals = gpu.buf("globals", std::mem::size_of::<Globals>() as u64, U::UNIFORM | U::COPY_DST);
        let inst = gpu.buf("inst", INST_BYTES * 256, U::STORAGE | U::COPY_DST);
        let cells = gpu.buf("cells", 8 * 1024, U::STORAGE | U::COPY_DST);
        let items = gpu.buf("items", 4 * 4096, U::STORAGE | U::COPY_DST);
        let out = gpu.buf("out", 4 * 64 * 64, U::STORAGE | U::COPY_SRC);
        let probe_dummy = gpu.buf("probe-dummy", 16, U::STORAGE);
        let hmap = gpu.buf("heightmap", ((HMAP_N * HMAP_N + FINE_N * FINE_N) * 4) as u64, U::STORAGE);
        let ground = gpu.buf("ground", 1 << 16, U::STORAGE | U::COPY_DST);
        let lands = gpu.buf("lands", 16, U::STORAGE | U::COPY_DST);
        let queries = gpu.timestamps.then(|| {
            let qs = gpu.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("ts"), ty: wgpu::QueryType::Timestamp, count: (SLOTS * 2) as u32 });
            let rb = gpu.buf("ts-resolve", 256 * SLOTS as u64, U::QUERY_RESOLVE | U::COPY_SRC);
            (qs, rb)
        });
        let ts_period = gpu.queue.get_timestamp_period();
        let slots = (0..SLOTS)
            .map(|_| Slot { buf: None, cap: 0, state: Arc::new(AtomicU8::new(0)), busy: false, id: 0, w: 0, h: 0, ts_offset: 0, view: [[0.0; 4]; 5] })
            .collect();
        Renderer {
            gpu,
            globals,
            inst,
            inst_cap: INST_BYTES * 256,
            cells,
            cells_cap: 8 * 1024,
            items,
            items_cap: 4 * 4096,
            out,
            out_cap: 4 * 64 * 64,
            probe_dummy,
            hmap,
            ground,
            ground_cap: 1 << 16,
            ground_state: None,
            lands,
            lands_cap: 16,
            lands_state: 0,
            hmap_state: None,
            fine_state: None,
            bind: None,
            slots,
            order: Default::default(),
            queries,
            ts_period,
        }
    }

    fn ensure(gpu: &Gpu, buf: &mut wgpu::Buffer, cap: &mut u64, need: u64, label: &str, usage: wgpu::BufferUsages) -> bool {
        if need <= *cap {
            return false;
        }
        let mut c = (*cap).max(256);
        while c < need {
            c *= 2;
        }
        *buf = gpu.buf(label, c, usage);
        *cap = c;
        true
    }

    fn submit(&mut self, req: &FrameRequest) -> bool {
        use wgpu::BufferUsages as U;
        let Some(pipe) = req.scene.pipeline.as_ref() else { return false };
        let Some(si) = self.slots.iter().position(|s| !s.busy) else { return false };
        let gpu = self.gpu.clone();
        let npx = (req.width * req.height) as u64;
        let insts: &[GpuInst] = if req.instances.is_empty() { &[GpuInst { info: [u32::MAX, 0, 0, 0], ..Default::default() }] } else { &req.instances };
        let (cells, mut items): (Vec<[u32; 2]>, Vec<u32>) = match &req.grid {
            Some(g) if !g.cells.is_empty() => (g.cells.clone(), if g.items.is_empty() { vec![0] } else { g.items.clone() }),
            _ => (vec![[0, 0]], vec![0]),
        };
        // The frame's holes ride after the items (no binding to spare).
        let holes_at = items.len() as u32;
        items.extend(req.holes.iter().flat_map(|h| h.map(f32::to_bits)));
        let mut dirty = self.bind.is_none();
        dirty |= Self::ensure(&gpu, &mut self.inst, &mut self.inst_cap, insts.len() as u64 * INST_BYTES, "inst", U::STORAGE | U::COPY_DST);
        dirty |= Self::ensure(&gpu, &mut self.cells, &mut self.cells_cap, (cells.len() * 8) as u64, "cells", U::STORAGE | U::COPY_DST);
        dirty |= Self::ensure(&gpu, &mut self.items, &mut self.items_cap, (items.len() * 4) as u64, "items", U::STORAGE | U::COPY_DST);
        // Colours, then distances.
        dirty |= Self::ensure(&gpu, &mut self.out, &mut self.out_cap, npx * 8, "out", U::STORAGE | U::COPY_SRC);
        // The ground edits round the camera, when they or the window changed.
        let pack = (req.globals.ground[2] > 0.0).then(|| req.terrain.carve.read().ground.pack(req.globals.cam_pos[0], req.globals.cam_pos[2]));
        let ground_new = pack.as_ref().map(|p| (p.origin, p.version));
        if let Some(p) = pack.as_ref().filter(|_| ground_new != self.ground_state) {
            if Self::ensure(&gpu, &mut self.ground, &mut self.ground_cap, (p.words.len() * 4) as u64, "ground", U::STORAGE | U::COPY_DST) {
                dirty = true;
            }
            gpu.queue.write_buffer(&self.ground, 0, bytemuck::cast_slice(&p.words));
            self.ground_state = ground_new;
        }
        // The landform map, once per world.
        let lands_new = req.terrain.lands.as_ref().map(|m| std::sync::Arc::as_ptr(m) as usize).unwrap_or(0);
        if lands_new != self.lands_state {
            if let Some(m) = &req.terrain.lands {
                if Self::ensure(&gpu, &mut self.lands, &mut self.lands_cap, (m.words.len() * 4) as u64, "lands", U::STORAGE | U::COPY_DST) {
                    dirty = true;
                }
                gpu.queue.write_buffer(&self.lands, 0, bytemuck::cast_slice(&m.words));
            }
            self.lands_state = lands_new;
        }
        if dirty {
            self.bind = Some(gpu.bind_group(&self.globals, &self.inst, &self.cells, &self.items, &self.out, &self.probe_dummy, &self.hmap, &self.ground, &self.lands));
        }
        let ts_offset = (npx * 8).div_ceil(256) * 256;
        let need = ts_offset + 16;
        let slot = &mut self.slots[si];
        if slot.cap < need || slot.buf.is_none() {
            slot.buf = Some(gpu.buf("readback", need, U::MAP_READ | U::COPY_DST));
            slot.cap = need;
        }
        slot.ts_offset = ts_offset;
        // Rebuild the camera-centred heightmap when the camera has moved far
        // enough from its centre or the terrain function changed.
        let mut globals = req.globals;
        globals.extra[2] = holes_at;
        let (cx, cz) = (globals.cam_pos[0], globals.cam_pos[2]);
        let epoch = globals.seed[3];
        let half = HMAP_CELL * HMAP_N as f32 * 0.5;
        let rebuild = match self.hmap_state {
            None => true,
            Some((ox, oz, ep)) => ep != epoch || (cx - (ox + half)).abs() > HMAP_RECENTER || (cz - (oz + half)).abs() > HMAP_RECENTER,
        };
        if rebuild {
            let ox = ((cx - half) / HMAP_CELL).floor() * HMAP_CELL;
            let oz = ((cz - half) / HMAP_CELL).floor() * HMAP_CELL;
            self.hmap_state = Some((ox, oz, epoch));
        }
        let (ox, oz, _) = self.hmap_state.expect("heightmap state");
        globals.hmap = [ox, oz, HMAP_CELL, HMAP_N as f32];
        let fhalf = FINE_CELL * FINE_N as f32 * 0.5;
        let fine_rebuild = match self.fine_state {
            None => true,
            Some((ox, oz, ep)) => ep != epoch || (cx - (ox + fhalf)).abs() > FINE_RECENTER || (cz - (oz + fhalf)).abs() > FINE_RECENTER,
        };
        if fine_rebuild {
            let ox = ((cx - fhalf) / FINE_CELL).floor() * FINE_CELL;
            let oz = ((cz - fhalf) / FINE_CELL).floor() * FINE_CELL;
            self.fine_state = Some((ox, oz, epoch));
        }
        let (fx, fz, _) = self.fine_state.expect("fine heightmap state");
        globals.hmap2 = [fx, fz, FINE_CELL, FINE_N as f32];
        let q = &gpu.queue;
        q.write_buffer(&self.globals, 0, bytemuck::bytes_of(&globals));
        q.write_buffer(&self.inst, 0, bytemuck::cast_slice(insts));
        q.write_buffer(&self.cells, 0, bytemuck::cast_slice(&cells));
        q.write_buffer(&self.items, 0, bytemuck::cast_slice(&items));
        let mut enc = gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("frame") });
        if rebuild {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("heightmap"), timestamp_writes: None });
            pass.set_pipeline(&pipe.heightmap);
            pass.set_bind_group(0, self.bind.as_ref(), &[]);
            pass.dispatch_workgroups((HMAP_N as u32).div_ceil(8), (HMAP_N as u32).div_ceil(8), 1);
        }
        if fine_rebuild {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("heightmap fine"), timestamp_writes: None });
            pass.set_pipeline(&pipe.heightmap_fine);
            pass.set_bind_group(0, self.bind.as_ref(), &[]);
            pass.dispatch_workgroups((FINE_N as u32).div_ceil(8), (FINE_N as u32).div_ceil(8), 1);
        }
        {
            let tw = self.queries.as_ref().map(|(qs, _)| wgpu::ComputePassTimestampWrites {
                query_set: qs,
                beginning_of_pass_write_index: Some((si * 2) as u32),
                end_of_pass_write_index: Some((si * 2 + 1) as u32),
            });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("raymarch"), timestamp_writes: tw });
            pass.set_pipeline(&pipe.render);
            pass.set_bind_group(0, self.bind.as_ref(), &[]);
            pass.dispatch_workgroups(req.width.div_ceil(8), req.height.div_ceil(8), 1);
        }
        let rb = slot.buf.as_ref().expect("readback");
        enc.copy_buffer_to_buffer(&self.out, 0, rb, 0, Some(npx * 8));
        if let Some((qs, resolve)) = &self.queries {
            let base = (si * 2) as u32;
            enc.resolve_query_set(qs, base..base + 2, resolve, (si * 256) as u64);
            enc.copy_buffer_to_buffer(resolve, (si * 256) as u64, rb, ts_offset, Some(16));
        }
        q.submit([enc.finish()]);
        slot.state.store(1, Ordering::SeqCst);
        let st = slot.state.clone();
        rb.slice(..need).map_async(wgpu::MapMode::Read, move |r| st.store(if r.is_ok() { 2 } else { 3 }, Ordering::SeqCst));
        slot.busy = true;
        slot.id = req.id;
        slot.w = req.width;
        slot.h = req.height;
        let gl = &req.globals;
        slot.view = [gl.cam_pos, gl.cam_fwd, gl.cam_right, gl.cam_up, gl.jitter];
        self.order.push_back(si);
        true
    }

    /// Collect the oldest finished frame, if any.
    fn collect(&mut self, wait: bool) -> Option<Frame> {
        let &si = self.order.front()?;
        let _ = self.gpu.device.poll(if wait {
            wgpu::PollType::Wait { submission_index: None, timeout: Some(Duration::from_millis(100)) }
        } else {
            wgpu::PollType::Poll
        });
        let slot = &mut self.slots[si];
        let st = slot.state.load(Ordering::SeqCst);
        if st < 2 {
            return None;
        }
        self.order.pop_front();
        slot.busy = false;
        slot.state.store(0, Ordering::SeqCst);
        let rb = slot.buf.as_ref()?;
        if st == 3 {
            return None;
        }
        let npx = (slot.w * slot.h) as usize;
        let (pixels, depth, gpu_ms) = {
            let view = rb.slice(..slot.ts_offset + 16).get_mapped_range().ok()?;
            let px: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&view[..npx * 4]).to_vec();
            let depth: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&view[npx * 4..npx * 8]).to_vec();
            let ts = if self.queries.is_some() {
                let t: &[u64] = bytemuck::cast_slice(&view[slot.ts_offset as usize..slot.ts_offset as usize + 16]);
                let dt = t[1].wrapping_sub(t[0]) as f64 * self.ts_period as f64 / 1e6;
                (dt > 0.0 && dt < 1000.0).then_some(dt as f32)
            } else {
                None
            };
            (px, depth, ts)
        };
        rb.unmap();
        Some(Frame { id: slot.id, width: slot.w, height: slot.h, pixels, gpu_ms, depth, view: slot.view })
    }
}

pub fn spawn(gpu: Arc<Gpu>) -> RenderHandle {
    let (tx, rx) = crossbeam_channel::unbounded::<RenderMsg>();
    let (ftx, frx) = crossbeam_channel::unbounded::<Frame>();
    let backend = gpu.name.clone();
    let thread = std::thread::Builder::new()
        .name("render".into())
        .spawn(move || {
            let mut r = Renderer::new(gpu);
            let mut pending: std::collections::VecDeque<Box<FrameRequest>> = Default::default();
            let mut submitted_at: std::collections::HashMap<u64, std::time::Instant> = Default::default();
            loop {
                let idle = r.order.is_empty() && pending.is_empty();
                let msg = if idle {
                    match rx.recv() {
                        Ok(m) => Some(m),
                        Err(_) => break,
                    }
                } else {
                    match rx.try_recv() {
                        Ok(m) => Some(m),
                        Err(crossbeam_channel::TryRecvError::Empty) => None,
                        Err(crossbeam_channel::TryRecvError::Disconnected) => break,
                    }
                };
                let got_msg = msg.is_some();
                match msg {
                    Some(RenderMsg::Quit) => break,
                    Some(RenderMsg::Frame(f)) => pending.push_back(f),
                    None => {}
                }
                // Only the newest pending request matters.
                while pending.len() > 1 {
                    pending.pop_front();
                }
                if !pending.is_empty() && r.order.len() < 2 && r.slots.iter().any(|s| !s.busy) {
                    let f = pending.pop_front().expect("pending");
                    if r.submit(&f) {
                        submitted_at.insert(f.id, std::time::Instant::now());
                    } else if ftx.send(Frame { id: f.id, ..Default::default() }).is_err() {
                        break;
                    }
                    continue;
                }
                if let Some(mut frame) = r.collect(!got_msg) {
                    if frame.gpu_ms.is_none() {
                        frame.gpu_ms = submitted_at.get(&frame.id).map(|t| t.elapsed().as_secs_f32() * 1000.0);
                    }
                    submitted_at.remove(&frame.id);
                    if ftx.send(frame).is_err() {
                        break;
                    }
                }
            }
            // Drain in-flight work before the device is dropped.
            while !r.order.is_empty() {
                if r.collect(true).is_none() {
                    break;
                }
            }
        })
        .expect("spawn render thread");
    RenderHandle { tx, rx: frx, backend, cpu_fallback: false, thread: Some(thread) }
}

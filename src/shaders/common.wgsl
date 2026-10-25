// Shared declarations, noise and terrain. Twins: src/noise.rs, src/terrain.rs.

struct Globals {
  cam_pos: vec4f,     // xyz, time (s)
  cam_fwd: vec4f,     // xyz, max view distance
  cam_right: vec4f,   // xyz scaled by tan(fov/2) * aspect
  cam_up: vec4f,      // xyz scaled by tan(fov/2)
  sun_dir: vec4f,     // xyz (sun, or moon at night), daylight 0..1
  sun_col: vec4f,     // rgb, night 0..1
  sky_zen: vec4f,     // rgb, fog density
  sky_hor: vec4f,     // rgb, ambient strength
  water: vec4f,       // rgb, level
  rock: vec4f,        // rgb, unused
  sand: vec4f,
  snow: vec4f,
  toff0: vec4f,       // terrain offsets 0..3
  toff1: vec4f,       // terrain offsets 4..5, -, -
  dims: vec4u,        // width, height, instance count, flags (1 = grid, 2 = shadows)
  seed: vec4u,        // seed, biome count, frame, -
  grid0: vec4f,       // origin x, origin z, cell size, -
  grid1: vec4u,       // cells x, cells z, point lights, -
  probe: vec4u,       // count, mode (0 = type, 1 = terrain), type id, -
  hmap: vec4f,        // heightmap origin x, origin z, cell size, cells per side (0 = none)
  biomes: array<vec4f, 18>, // per biome: (cx, cy, base, amp), (rough, g1), (g2, cliffs)
  lights: array<vec4f, 8>,     // point lights: position, intensity
  light_cols: array<vec4f, 8>, // colour, reach (m)
  extra: vec4u,       // hollows, cave parts, where the holes start in items, -
  hollows: array<vec4f, 64>,   // per hollow: (cx, cz, cos, sin), (hx, hz, floor, ±edge: + round, - box)
  // weather: 0 (cover, drift x, drift z, cloud height), 1 (cloud rgb, flash),
  // 2 (falls rgb, amount), 3 (wind dir x, z, wind, falls look), 4 (soak, dusk,
  // sheltered, gloom), 5 (the sun itself xyz, its height), 6 (sunset glow rgb,
  // daylight), 7 (dusk sky rgb, -)
  wx: array<vec4f, 8>,
  hmap2: vec4f,       // fine heightmap near the camera: origin x, origin z, cell size, cells per side (0 = none)
  jitter: vec4f,      // sub-pixel offset of every ray (x, y pixels), for blending frames
  ground: vec4f,      // ground tiles: first tile x, z, tiles across (0 = none), any water
  voids: array<vec4f, 80>,     // caves: per capsule (a.xyz, r), (b.xyz, -); per box (c.xyz, -1), (half.xyz, yaw)
  craters: array<vec4f, 2>,    // how cratered each biome is, four to a row
  lands: vec4f,       // landform map: origin x, origin z, cell size, samples a side (0 = none)
  grain: array<vec4f, 12>,     // per biome: (fleck rgb, blades), (fleck2 rgb, flecks)
}

struct Inst {
  pos_scale: vec4f,   // world position, scale
  rot: vec4f,         // cos, sin, bounding radius, fade 0..1
  tilt: vec4f,        // xyz of a unit quaternion (w >= 0 implied) applied after the yaw; 0 = upright; w: the holes, as bits (see `GpuInst::set_holes`)
  k0: vec4f,          // seed, scale, a, b
  k1: vec4f,          // c, d, e, f
  s0: vec4f,          // live state s0..s3
  s1: vec4f,          // live state s4..s7
  s2: vec4f,          // s8 grip, s9 mouth, s10 eyes shut, s11 clock (bodies)
  fx: vec4f,          // charred, wet, glow, highlight
  info: vec4u,        // type id, sphere centre y, box half-extents x/z
  cuts: array<vec4f, 4>, // local centre, size (>0 sphere radius, <0 cube half-size, 0 none)
  look: array<vec4f, 5>, // look sliders g..z
}

struct Params { seed: f32, scale: f32, a: f32, b: f32, c: f32, d: f32, e: f32, f: f32,
  s0: f32, s1: f32, s2: f32, s3: f32, s4: f32, s5: f32, s6: f32, s7: f32, s8: f32, s9: f32, s10: f32, s11: f32,
  g: f32, h: f32, i: f32, j: f32, k: f32, l: f32, m: f32, n: f32, o: f32, p: f32, q: f32, r: f32, s: f32, t: f32, u: f32, v: f32, w: f32, x: f32, y: f32, z: f32, }

@group(0) @binding(0) var<uniform> G: Globals;
@group(0) @binding(1) var<storage, read> insts: array<Inst>;
@group(0) @binding(2) var<storage, read> cells: array<vec2u>;
@group(0) @binding(3) var<storage, read> items: array<u32>;
@group(0) @binding(4) var<storage, read_write> outp: array<u32>;
@group(0) @binding(5) var<storage, read_write> probe_io: array<vec4f>;
@group(0) @binding(6) var<storage, read_write> hmap: array<f32>;
@group(0) @binding(7) var<storage, read> gtiles: array<u32>;
@group(0) @binding(8) var<storage, read> lands: array<u32>;

fn pcg(v: u32) -> u32 {
  let state = v * 747796405u + 2891336453u;
  let word = ((state >> ((state >> 28u) + 4u)) ^ state) * 277803737u;
  return (word >> 22u) ^ word;
}
fn u2f(h: u32) -> f32 { return f32(h >> 8u) * (1.0 / 16777216.0); }
fn h2(ix: i32, iy: i32, s: u32) -> u32 { return pcg(pcg(bitcast<u32>(ix) + s) + bitcast<u32>(iy)); }
fn h3(ix: i32, iy: i32, iz: i32, s: u32) -> u32 { return pcg(pcg(pcg(bitcast<u32>(ix) + s) + bitcast<u32>(iy)) + bitcast<u32>(iz)); }
fn nlerp(a: f32, b: f32, t: f32) -> f32 { return a + (b - a) * t; }
fn nfade(t: f32) -> f32 { return t * t * (3.0 - 2.0 * t); }

fn vnoise2(x: f32, y: f32, s: u32) -> f32 {
  let fx = floor(x); let fy = floor(y);
  let ix = i32(fx); let iy = i32(fy);
  let ux = nfade(x - fx); let uy = nfade(y - fy);
  let a = u2f(h2(ix, iy, s));
  let b = u2f(h2(ix + 1, iy, s));
  let c = u2f(h2(ix, iy + 1, s));
  let d = u2f(h2(ix + 1, iy + 1, s));
  return nlerp(nlerp(a, b, ux), nlerp(c, d, ux), uy);
}

fn vnoise3(x: f32, y: f32, z: f32, s: u32) -> f32 {
  let fx = floor(x); let fy = floor(y); let fz = floor(z);
  let ix = i32(fx); let iy = i32(fy); let iz = i32(fz);
  let ux = nfade(x - fx); let uy = nfade(y - fy); let uz = nfade(z - fz);
  let a = u2f(h3(ix, iy, iz, s));
  let b = u2f(h3(ix + 1, iy, iz, s));
  let c = u2f(h3(ix, iy + 1, iz, s));
  let d = u2f(h3(ix + 1, iy + 1, iz, s));
  let e = u2f(h3(ix, iy, iz + 1, s));
  let f = u2f(h3(ix + 1, iy, iz + 1, s));
  let g = u2f(h3(ix, iy + 1, iz + 1, s));
  let h = u2f(h3(ix + 1, iy + 1, iz + 1, s));
  let lo = nlerp(nlerp(a, b, ux), nlerp(c, d, ux), uy);
  let hi = nlerp(nlerp(e, f, ux), nlerp(g, h, ux), uy);
  return nlerp(lo, hi, uz);
}

fn fbm2(x: f32, y: f32, s: u32, octaves: u32) -> f32 {
  var sum = 0.0; var amp = 0.5; var norm = 0.0;
  for (var i = 0u; i < min(octaves, 5u); i = i + 1u) {
    let m = FBM_M[i];
    sum = sum + amp * vnoise2(m.x * x + m.y * y, m.z * x + m.w * y, s + i * 1013u);
    norm = norm + amp;
    amp = amp * 0.5;
  }
  return (sum / norm) * 2.0 - 1.0;
}

fn ridged2(x: f32, y: f32, s: u32, octaves: u32) -> f32 {
  var sum = 0.0; var amp = 0.5; var norm = 0.0;
  for (var i = 0u; i < min(octaves, 5u); i = i + 1u) {
    let m = RIDGE_M[i];
    let n = 1.0 - abs(vnoise2(m.x * x + m.y * y, m.z * x + m.w * y, s + i * 7919u) * 2.0 - 1.0);
    sum = sum + amp * n * n;
    norm = norm + amp;
    amp = amp * 0.5;
  }
  return sum / norm;
}

fn fbits(x: f32) -> u32 { if (x == 0.0) { return 0u; } return bitcast<u32>(x); }
fn hash3f(a: f32, b: f32, c: f32) -> f32 { return u2f(pcg(fbits(a) + pcg(fbits(b) + pcg(fbits(c))))); }

// ---- terrain ----

const BIOME_SHARPNESS: f32 = 26.0;

struct Weights { w: array<f32, 6>, rise: f32, }

// One landform sample: six biome shares (bytes) and the rise. Twin of
// LandMap::sample in landforms.rs.
fn land_sample(i: u32, j: u32) -> f32 {
  let a = lands[i * 2u + j / 4u];
  if (j == 6u) { return f32(a >> 16u) * 0.1 - 2000.0; }
  return f32((a >> ((j % 4u) * 8u)) & 255u) / 255.0;
}

// What the landforms make of the land at (x, z) laid over the climate's
// weights. Twin of Terrain::weights_rise in terrain.rs.
fn lands_at(x: f32, z: f32, out: ptr<function, Weights>) {
  let n = G.lands.w;
  if (n < 2.0) { return; }
  let gx = (x - G.lands.x) / G.lands.z;
  let gz = (z - G.lands.y) / G.lands.z;
  if (!(gx >= 0.0 && gz >= 0.0 && gx < n - 1.0 && gz < n - 1.0)) { return; }
  let ix = u32(gx); let iz = u32(gz);
  let fx = gx - f32(ix); let fz = gz - f32(iz);
  let ni = u32(n);
  let i0 = iz * ni + ix;
  var t: array<f32, 6>;
  var s = 0.0;
  for (var j = 0u; j < 6u; j = j + 1u) {
    t[j] = nlerp(nlerp(land_sample(i0, j), land_sample(i0 + 1u, j), fx), nlerp(land_sample(i0 + ni, j), land_sample(i0 + ni + 1u, j), fx), fz);
    s = s + t[j];
  }
  for (var j = 0u; j < 6u; j = j + 1u) { (*out).w[j] = (*out).w[j] * (1.0 - s) + t[j]; }
  (*out).rise = nlerp(nlerp(land_sample(i0, 6u), land_sample(i0 + 1u, 6u), fx), nlerp(land_sample(i0 + ni, 6u), land_sample(i0 + ni + 1u, 6u), fx), fz);
}

fn terrain_weights(x: f32, z: f32) -> Weights {
  let s = G.seed.x;
  var t = vnoise2(x * 0.0011 + G.toff0.x, z * 0.0011 + G.toff0.y, s);
  var m = vnoise2(x * 0.0011 + G.toff0.z, z * 0.0011 + G.toff0.w, s + 17u);
  t = clamp((t - 0.5) * 2.4 + 0.5, 0.0, 1.0);
  m = clamp((m - 0.5) * 2.4 + 0.5, 0.0, 1.0);
  var out: Weights;
  var sum = 1e-6;
  let nb = G.seed.y;
  for (var i = 0u; i < 6u; i = i + 1u) {
    if (i < nb) {
      let b = G.biomes[i * 3u];
      let dt = t - b.x; let dm = m - b.y;
      let w = exp(-(dt * dt + dm * dm) * BIOME_SHARPNESS);
      out.w[i] = w;
      sum = sum + w;
    } else {
      out.w[i] = 0.0;
    }
  }
  for (var i = 0u; i < 6u; i = i + 1u) { out.w[i] = out.w[i] / sum; }
  out.rise = 0.0;
  lands_at(x, z, &out);
  return out;
}

fn terrain_height_w(x: f32, z: f32, wt: Weights) -> f32 {
  var base = wt.rise; var amp = 0.0; var rough = 0.0; var cl = 0.0; var cr = 0.0;
  for (var i = 0u; i < 6u; i = i + 1u) {
    if (i < G.seed.y) {
      let b0 = G.biomes[i * 3u];
      let b1 = G.biomes[i * 3u + 1u];
      base = base + wt.w[i] * b0.z;
      amp = amp + wt.w[i] * b0.w;
      rough = rough + wt.w[i] * b1.x;
      cl = cl + wt.w[i] * G.biomes[i * 3u + 2u].w;
      cr = cr + wt.w[i] * G.craters[i / 4u][i % 4u];
    }
  }
  let s = G.seed.x;
  let n = fbm2(x * 0.0045 + G.toff1.x, z * 0.0045 + G.toff1.y, s + 101u, 5u);
  let r = ridged2(x * 0.0032 + G.toff1.y, z * 0.0032 + G.toff1.x, s + 202u, 3u);
  return craters_h(x, z, cliff_h(x, z, base + amp * nlerp(n, r * 2.0 - 0.6, rough), cl, amp), cr);
}

const CLIFF_FACE: f32 = 0.12;
const CLIFF_TILT: f32 = 0.2;

// Ledges and rock faces where the land has cliffs. Twin of Terrain::cliff
// in terrain.rs (see there for how it works); keep the operation order.
fn cliff_h(x: f32, z: f32, h: f32, cl: f32, amp: f32) -> f32 {
  if (cl < 0.01) { return h; }
  let s = G.seed.x;
  let k = sstep(1.0 - cl, 1.3 - cl, vnoise2(x * 0.004, z * 0.004, s + 404u)) * sstep(3.0, 12.0, amp);
  if (k <= 0.0) { return h; }
  let st = 5.0 + 7.0 * vnoise2(x * 0.0023, z * 0.0023, s + 405u);
  let j = (0.3 * vnoise2(x * 0.02, z * 0.02, s + 406u) + 0.08 * vnoise2(x * 0.12, z * 0.12, s + 408u)) * sstep(4.0, 20.0, amp);
  let w = nlerp(CLIFF_FACE, 1.0, sstep(0.6, 0.75, vnoise2(x * 0.03, z * 0.03, s + 407u)));
  let wf = min(w * (0.55 + 0.9 * vnoise2(x * 0.2, z * 0.2, s + 409u)), 1.0);
  let q = h / st + j;
  let t = q - floor(q);
  let g = CLIFF_TILT * t + (1.0 - CLIFF_TILT) * sstep(1.0 - wf, 1.0, t);
  return h + k * (g - t) * st;
}

const CRATER_CELLS = array<f32, 2>(140.0, 30.0);

// Craters where the land has them. Twin of Terrain::craters in terrain.rs
// (see there for how it works); keep the operation order.
fn craters_h(x: f32, z: f32, h: f32, cr: f32) -> f32 {
  if (cr < 0.01) { return h; }
  var d = 0.0;
  for (var k = 0u; k < 2u; k = k + 1u) {
    let cell = CRATER_CELLS[k];
    let gx = i32(floor(x / cell)); let gz = i32(floor(z / cell));
    for (var oz = -1; oz <= 1; oz = oz + 1) {
      for (var ox = -1; ox <= 1; ox = ox + 1) {
        let ix = gx + ox; let iz = gz + oz;
        let hs = h2(ix, iz, G.seed.x + 510u + k);
        let a = clamp((cr - u2f(hs)) * 8.0, 0.0, 1.0);
        if (a <= 0.0) { continue; }
        let cx = (f32(ix) + 0.15 + 0.7 * u2f(pcg(hs + 1u))) * cell;
        let cz = (f32(iz) + 0.15 + 0.7 * u2f(pcg(hs + 2u))) * cell;
        let r = cell * (0.18 + 0.27 * u2f(pcg(hs + 3u)));
        let dx = x - cx; let dz = z - cz;
        let q = sqrt(dx * dx + dz * dz) / r;
        if (q >= 1.6) { continue; }
        let depth = r * 0.22 * a;
        if (q < 1.0) { d = d - depth * (1.0 - q * q); }
        let u = (q - 1.05) / 0.55;
        if (abs(u) < 1.0) {
          let b = 1.0 - u * u;
          d = d + depth * 0.3 * b * b;
        }
      }
    }
  }
  return h + d;
}

// Ground taken away (cellars, houses set into hills, dug pits): the height
// with the hollows taken out (x) and how far into one the point is (y).
// Twin of Carve::apply in terrain.rs.
fn carve(x: f32, z: f32, h: f32) -> vec2f {
  var out = h;
  var wmax = 0.0;
  let n = min(G.extra.x, 32u);
  for (var i = 0u; i < n; i = i + 1u) {
    let a = G.hollows[i * 2u];
    let b = G.hollows[i * 2u + 1u];
    let dx = x - a.x; let dz = z - a.y;
    var e: f32;
    // w: the rim's width, negative for a box.
    let edge = abs(b.w);
    if (b.w > 0.0) {
      e = sqrt(dx * dx + dz * dz) - b.x;
    } else {
      let lx = a.z * dx - a.w * dz;
      let lz = a.w * dx + a.z * dz;
      e = max(abs(lx) - b.x, abs(lz) - b.y);
    }
    if (e >= edge) { continue; }
    let w = sstep(edge, 0.0, e);
    if (h > b.z) { out = min(out, h - w * (h - b.z)); }
    wmax = max(wmax, w);
  }
  return vec2f(out, wmax);
}

// ---- ground edits (twin of ground.rs: Ground::at) ----

const GCELLS: u32 = 33u;           // samples per tile row
const GSAMPLES: u32 = 1089u;       // samples per tile
const NO_WATER: f32 = -1.0e4;

// The tile holding (x, z): the first word of its samples (0 = none), the
// sample at the cell's corner, and the fractions across the cell.
struct GCell { base: u32, i0: u32, ux: f32, uz: f32, }

fn ground_cell(x: f32, z: f32) -> GCell {
  var out = GCell(0u, 0u, 0.0, 0.0);
  let n = i32(G.ground.z);
  if (n == 0) { return out; }
  let fx = x * 2.0;
  let fz = z * 2.0;
  let cx = floor(fx);
  let cz = floor(fz);
  let ix = i32(cx); let iz = i32(cz);
  let tx = ix >> 5u; let tz = iz >> 5u;
  let dx = tx - i32(G.ground.x); let dz = tz - i32(G.ground.y);
  if (dx < 0 || dz < 0 || dx >= n || dz >= n) { return out; }
  let e = gtiles[u32(dz * n + dx)];
  if (e == 0u) { return out; }
  out.base = u32(n * n) + (e - 1u) * GSAMPLES * 4u;
  out.i0 = u32(iz - (tz << 5u)) * GCELLS + u32(ix - (tx << 5u));
  out.ux = fx - cx;
  out.uz = fz - cz;
  return out;
}

fn gbl(c: GCell, off: u32) -> f32 {
  let i = c.base + off + c.i0;
  let a = bitcast<f32>(gtiles[i]); let b = bitcast<f32>(gtiles[i + 1u]);
  let d = bitcast<f32>(gtiles[i + GCELLS]); let e = bitcast<f32>(gtiles[i + GCELLS + 1u]);
  return nlerp(nlerp(a, b, c.ux), nlerp(d, e, c.ux), c.uz);
}

// How far edits moved the ground at (x, z).
fn ground_delta(x: f32, z: f32) -> f32 {
  let c = ground_cell(x, z);
  if (c.base == 0u) { return 0.0; }
  return gbl(c, 0u);
}

// The water standing over (x, z) above the sea (NO_WATER: none).
fn ground_water(x: f32, z: f32) -> f32 {
  if (G.ground.w == 0.0) { return NO_WATER; }
  let c = ground_cell(x, z);
  if (c.base == 0u) { return NO_WATER; }
  return gbl(c, GSAMPLES * 2u);
}

// What covers the ground at (x, z): rgb premultiplied by cover, cover.
fn ground_paint(x: f32, z: f32) -> vec4f {
  let c = ground_cell(x, z);
  if (c.base == 0u) { return vec4f(0.0); }
  let i = c.base + GSAMPLES + c.i0;
  let a = unpack4x8unorm(gtiles[i]); let b = unpack4x8unorm(gtiles[i + 1u]);
  let d = unpack4x8unorm(gtiles[i + GCELLS]); let e = unpack4x8unorm(gtiles[i + GCELLS + 1u]);
  return mix(mix(a, b, c.ux), mix(d, e, c.ux), c.uz);
}

// The laid pattern's code at (x, z), from the nearest sample (0: none).
fn ground_pattern(x: f32, z: f32) -> u32 {
  let c = ground_cell(x, z);
  if (c.base == 0u) { return 0u; }
  var i = c.base + GSAMPLES * 3u + c.i0;
  if (c.uz >= 0.5) { i = i + GCELLS; }
  if (c.ux >= 0.5) { i = i + 1u; }
  return gtiles[i];
}

fn pedge(f: f32) -> f32 { return min(f, 1.0 - f); }

// How a laid pattern shades the ground at (x, z) seen from `dist`: twin of
// ground.rs: pattern_shade.
fn pattern_shade(code: u32, x: f32, z: f32, dist: f32, seed: u32) -> f32 {
  return laid_shade(code, x, z, dist, seed) * grit(x, z, dist, seed, select(0.5, 1.0, (code & 255u) == 0u));
}

// Every laid surface's faint grit: a mottle, small stones close up.
fn grit(x: f32, z: f32, dist: f32, seed: u32, k: f32) -> f32 {
  let mid = sstep(120.0, 25.0, dist);
  if (mid <= 0.0) { return 1.0; }
  var f = 1.0 + (vnoise2(x * 1.3, z * 1.3, seed + 320u) - 0.5) * 0.12 * k * mid;
  let near = sstep(30.0, 8.0, dist);
  if (near > 0.0) {
    let gx = floor(x * 6.0);
    let gz = floor(z * 6.0);
    let h = h2(i32(gx), i32(gz), seed + 321u);
    let px = gx + 0.25 + 0.5 * u2f(pcg(h));
    let pz = gz + 0.25 + 0.5 * u2f(pcg(h ^ 0x51edu));
    let d = length(vec2f(x * 6.0 - px, z * 6.0 - pz));
    let r = 0.18 + 0.2 * u2f(pcg(h ^ 0x2b3cu));
    if (d < r) {
      f = f * (1.0 + (u2f(h) - 0.5) * 0.15 * k * near * sstep(r, r * 0.6, d));
    }
  }
  return f;
}

fn laid_shade(code: u32, x: f32, z: f32, dist: f32, seed: u32) -> f32 {
  let kind = code & 255u;
  if (kind == 0u) { return 1.0; }
  let s = f32((code >> 8u) & 255u) / 20.0;
  let fade = sstep(0.35, 0.12, dist * 0.004 / s);
  if (fade <= 0.0) { return 1.0; }
  var e = 0.0; var cx = 0; var cz = 0;
  if (kind == 2u) {
    let gx = i32(floor(x / s)); let gz = i32(floor(z / s));
    var f1 = 9.0; var f2 = 9.0;
    for (var j = -1; j <= 1; j = j + 1) {
      for (var i = -1; i <= 1; i = i + 1) {
        let ix = gx + i; let iz = gz + j;
        let px = f32(ix) + 0.15 + 0.7 * u2f(h2(ix, iz, seed));
        let pz = f32(iz) + 0.15 + 0.7 * u2f(h2(iz, ix, seed ^ 0x9e37u));
        let d = length(vec2f(x / s - px, z / s - pz));
        if (d < f1) { f2 = f1; f1 = d; cx = ix; cz = iz; } else if (d < f2) { f2 = d; }
      }
    }
    e = (f2 - f1) * 0.5 * s;
  } else if (kind == 4u) {
    let v = z / s;
    let row = i32(floor(v));
    let u = x / (s * 7.0) + u2f(h2(row, 7, seed));
    e = min(pedge(abs(fract(v))), pedge(u - floor(u)) * 7.0) * s;
    cx = i32(floor(u)); cz = row;
  } else if (kind == 5u) {
    let bx = i32(floor(x / (2.0 * s))); let bz = i32(floor(z / (2.0 * s)));
    let turn = ((bx + bz) & 1) == 1;
    var a = x; var b = z;
    if (turn) { a = z; b = x; }
    let u = a / (2.0 * s); let v = b / s;
    e = min(pedge(u - floor(u)), pedge(v - floor(v)) * 0.5) * 2.0 * s;
    cx = bx * 2 + (i32(floor(v)) & 1); cz = bz;
  } else {
    var long = 1.0;
    if (kind == 3u) { long = 1.5; }
    let v = z / s;
    let row = i32(floor(v));
    var shift = 0.5 * f32(row & 1);
    if (kind == 3u) { shift = u2f(h2(row, 3, seed)); }
    let u = x / (s * long) + shift;
    e = min(pedge(v - floor(v)), pedge(u - floor(u)) * long) * s;
    cx = i32(floor(u)); cz = row;
  }
  var w = s * 0.06;
  if (kind == 2u) { w = s * 0.11; }
  let joint = sstep(w * 1.6, w, e);
  let piece = 0.86 + 0.26 * u2f(h2(cx, cz, seed ^ 0x51u));
  let shade = piece + (0.5 - piece) * joint;
  return 1.0 + (shade - 1.0) * fade;
}

// Distance to the nearest cave hollow (negative inside; 1e4 with none).
// Twin of Ground::void in ground.rs.
fn void_dist(p: vec3f) -> f32 {
  var d = 1e4;
  let n = min(G.extra.y, 40u);
  for (var i = 0u; i < n; i = i + 1u) {
    let a = G.voids[i * 2u];
    let b = G.voids[i * 2u + 1u];
    let pa = p - a.xyz;
    if (a.w < 0.0) {
      // A room set into the ground: a box turned about the vertical.
      let c = cos(b.w); let s = sin(b.w);
      let l = vec3f(c * pa.x - s * pa.z, pa.y, s * pa.x + c * pa.z);
      let q = abs(l) - b.xyz;
      d = min(d, length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0));
      continue;
    }
    let ba = b.xyz - a.xyz;
    let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-6), 0.0, 1.0);
    d = min(d, length(pa - ba * h) - a.w);
  }
  return d;
}

// How far into the open air a point is from the solid ground (negative
// inside it): above the surface, or inside a cave.
fn ground_air(p: vec3f, h: f32) -> f32 {
  let d = p.y - h;
  if (G.extra.y == 0u) { return d; }
  return max(d, -void_dist(p));
}

fn terrain_height(x: f32, z: f32) -> f32 {
  let h = terrain_height_w(x, z, terrain_weights(x, z)) + ground_delta(x, z);
  if (G.extra.x == 0u) { return h; }
  return carve(x, z, h).x;
}

// Rendering-only level of detail: beyond ~60 m the two finest octaves fade
// out (blended, so nothing pops). Collision, placement and parity use the
// full terrain_height.
// Bilinear lookup in the camera-centred heightmap (rendering only).
fn hmap_height(x: f32, z: f32, ok: ptr<function, bool>, steep: ptr<function, bool>) -> f32 {
  let n = G.hmap.w;
  let gx = (x - G.hmap.x) / G.hmap.z;
  let gz = (z - G.hmap.y) / G.hmap.z;
  if (n < 2.0 || gx < 0.0 || gz < 0.0 || gx >= n - 1.0 || gz >= n - 1.0) { *ok = false; return 0.0; }
  let ix = u32(gx); let iz = u32(gz);
  let fx = gx - f32(ix); let fz = gz - f32(iz);
  let ni = u32(n);
  let i0 = iz * ni + ix;
  let a = hmap[i0]; let b = hmap[i0 + 1u];
  let c = hmap[i0 + ni]; let d = hmap[i0 + ni + 1u];
  *ok = true;
  // A cell this steep (a rock face) is too coarse to draw from up close.
  *steep = max(max(a, b), max(c, d)) - min(min(a, b), min(c, d)) > HMAP_STEEP;
  return mix(mix(a, b, fx), mix(c, d, fx), fz);
}

// Rise across one heightmap cell beyond which the exact height is used
// instead, at any distance (a bilinear cell turns a sheer face's edge into
// teeth, plain to see from a hundred metres and more).
const HMAP_STEEP: f32 = 1.2;

// The fine heightmap, stored after the coarse one: 12.5 cm cells round the
// camera, so steps near it read a height instead of working one out.
fn hmap_fine(x: f32, z: f32, ok: ptr<function, bool>) -> f32 {
  let n = G.hmap2.w;
  let gx = (x - G.hmap2.x) / G.hmap2.z;
  let gz = (z - G.hmap2.y) / G.hmap2.z;
  if (n < 2.0 || gx < 0.0 || gz < 0.0 || gx >= n - 1.0 || gz >= n - 1.0) { *ok = false; return 0.0; }
  let ix = u32(gx); let iz = u32(gz);
  let fx = gx - f32(ix); let fz = gz - f32(iz);
  let ni = u32(n);
  let i0 = u32(G.hmap.w) * u32(G.hmap.w) + iz * ni + ix;
  let a = hmap[i0]; let b = hmap[i0 + 1u];
  let c = hmap[i0 + ni]; let d = hmap[i0 + ni + 1u];
  // A cell this steep (a rock face) is drawn from the exact height instead.
  *ok = max(max(a, b), max(c, d)) - min(min(a, b), min(c, d)) <= HMAP_FINE_STEEP;
  return mix(mix(a, b, fx), mix(c, d, fx), fz);
}

// Rise across one fine cell beyond which the exact height is used.
const HMAP_FINE_STEEP: f32 = 0.2;
// The fine map serves steps this close (it reaches further, recentred as the camera moves).
const HMAP_FINE_NEAR: f32 = 16.0;

fn terrain_height_lod(x: f32, z: f32, t: f32) -> f32 {
  if (t < HMAP_FINE_NEAR) {
    var ok = false;
    let h = hmap_fine(x, z, &ok);
    if (ok) { return h; }
  }
  return terrain_height_lod_exact(x, z, t);
}

// As terrain_height_lod, but never from the fine map (normals: a bilinear
// map would light each cell flat).
fn terrain_height_lod_exact(x: f32, z: f32, t: f32) -> f32 {
  let h = terrain_height_lod_raw(x, z, t);
  if (G.extra.x == 0u) { return h; }
  return carve(x, z, h).x;
}

fn terrain_height_lod_raw(x: f32, z: f32, t: f32) -> f32 {
  if (t > 12.0) {
    var ok = false;
    var steep = false;
    let h = hmap_height(x, z, &ok, &steep);
    if (ok && !steep) { return h; }
  }
  let fine = 1.0 - clamp((t - 35.0) / 40.0, 0.0, 1.0);
  if (fine >= 1.0) { return terrain_height_w(x, z, terrain_weights(x, z)) + ground_delta(x, z); }
  let wt = terrain_weights(x, z);
  var base = wt.rise; var amp = 0.0; var rough = 0.0; var cl = 0.0; var cr = 0.0;
  for (var i = 0u; i < 6u; i = i + 1u) {
    if (i < G.seed.y) {
      let b0 = G.biomes[i * 3u];
      let b1 = G.biomes[i * 3u + 1u];
      base = base + wt.w[i] * b0.z;
      amp = amp + wt.w[i] * b0.w;
      rough = rough + wt.w[i] * b1.x;
      cl = cl + wt.w[i] * G.biomes[i * 3u + 2u].w;
      cr = cr + wt.w[i] * G.craters[i / 4u][i % 4u];
    }
  }
  let s = G.seed.x;
  let fx = x * 0.0045; let fz = z * 0.0045;
  var sum = 0.0; var a = 0.5;
  for (var i = 0u; i < 5u; i = i + 1u) {
    var w = 1.0;
    if (i >= 3u) { w = fine; }
    if (w > 0.0) {
      let m = FBM_M[i];
      sum = sum + a * w * vnoise2(m.x * fx + m.y * fz, m.z * fx + m.w * fz, s + 101u + i * 1013u);
    }
    if (i >= 3u) { sum = sum + a * (1.0 - w) * 0.5; }
    a = a * 0.5;
  }
  let n = (sum / 0.96875) * 2.0 - 1.0;
  var r = 0.5;
  if (rough > 0.02) {
    r = ridged2(x * 0.0032, z * 0.0032, s + 202u, select(3u, 2u, fine <= 0.0));
  }
  return craters_h(x, z, cliff_h(x, z, base + amp * nlerp(n, r * 2.0 - 0.6, rough), cl, amp), cr) + ground_delta(x, z);
}

fn sstep(e0: f32, e1: f32, x: f32) -> f32 {
  let t = clamp((x - e0) / (e1 - e0), 0.0, 1.0);
  return t * t * (3.0 - 2.0 * t);
}

// The ground's fine grain: blades combed one way, flecks in clusters, a
// mottle further out. Twin: `terrain::ground_grain`.
fn ground_grain(w6: array<f32, 6>, c0: vec3f, x: f32, z: f32, dist: f32, s: u32) -> vec3f {
  var wt = w6;
  var blades = 0.0;
  var flecks = 0.0;
  var f1 = vec3f(0.0);
  var f2 = vec3f(0.0);
  for (var i = 0u; i < 6u; i = i + 1u) {
    if (i < G.seed.y) {
      let a = G.grain[i * 2u];
      let b = G.grain[i * 2u + 1u];
      blades = blades + wt[i] * a.w;
      flecks = flecks + wt[i] * b.w;
      f1 = f1 + wt[i] * a.xyz;
      f2 = f2 + wt[i] * b.xyz;
    }
  }
  let mid = sstep(220.0, 40.0, dist);
  var c = c0 * (1.0 + (vnoise2(x * 0.7, z * 0.7, s + 306u) - 0.5) * 0.22 * blades * mid);
  let near = sstep(45.0, 12.0, dist);
  if (near > 0.0) {
    let a = vnoise2(x * 0.04, z * 0.04, s + 307u) * 6.2832;
    let sa = sin(a);
    let ca = cos(a);
    let u = x * ca + z * sa;
    let v = z * ca - x * sa;
    let row = floor(v * 22.0);
    let cell = floor(u * 3.0 + u2f(h2(i32(row), 0, s + 308u)) * 3.0);
    let hb = u2f(h2(i32(row), i32(cell), s + 309u));
    c = c * (1.0 + (hb - 0.5) * 0.34 * blades * near);
  }
  let speck = sstep(70.0, 18.0, dist);
  if (speck > 0.0 && flecks > 0.0) {
    let gx = floor(x * 3.0);
    let gz = floor(z * 3.0);
    let hc = h2(i32(gx), i32(gz), s + 310u);
    let clump = vnoise2(x * 0.3, z * 0.3, s + 311u);
    if (u2f(hc) < flecks * clump * 0.8) {
      let px = gx + 0.2 + 0.6 * u2f(pcg(hc ^ 0x5bd1u));
      let pz = gz + 0.2 + 0.6 * u2f(pcg(hc ^ 0x2c1bu));
      let an = u2f(pcg(hc ^ 0x7f4au)) * 3.1416;
      let sa = sin(an);
      let ca = cos(an);
      let dx = x * 3.0 - px;
      let dz = z * 3.0 - pz;
      let du = dx * ca + dz * sa;
      let dv = (dz * ca - dx * sa) / 0.55;
      let r = 0.12 + 0.18 * u2f(pcg(hc ^ 0x1e35u));
      let d = sqrt(du * du + dv * dv) + (vnoise2(x * 14.0, z * 14.0, s + 312u) - 0.5) * 0.12;
      if (d < r) {
        let col = select(f2, f1, u2f(pcg(hc ^ 0x68e3u)) < 0.5);
        c = mix(c, col * (0.8 + 0.4 * u2f(pcg(hc ^ 0x3a9cu))), sstep(r, r * 0.55, d) * 0.7 * speck);
      }
    }
  }
  return c;
}

// `dist`: how far from the eye (the fine grain fades with it).
fn ground_color(x: f32, z: f32, h: f32, ny: f32, dist: f32) -> vec3f {
  let wt = terrain_weights(x, z);
  let s = G.seed.x;
  let v = vnoise2(x * 0.11, z * 0.11, s + 303u);
  let v2 = vnoise2(x * 0.023, z * 0.023, s + 304u);
  var c = vec3f(0.0);
  for (var i = 0u; i < 6u; i = i + 1u) {
    if (i < G.seed.y) {
      let b1 = G.biomes[i * 3u + 1u];
      let b2 = G.biomes[i * 3u + 2u];
      c = c + wt.w[i] * mix(b1.yzw, b2.xyz, clamp(v * 0.5 + v2 * 0.8, 0.0, 1.0));
    }
  }
  c = ground_grain(wt.w, c, x, z, dist, s);
  let steep = sstep(0.82, 0.68, ny);
  let strata = sstep(0.3, 0.7, vnoise2(h * 0.9, (x + z) * 0.015, s + 305u));
  let bed = G.rock.xyz * (0.55 + 0.25 * v + 0.45 * strata);
  c = mix(c, bed * vec3f(1.0 + 0.1 * strata, 1.0, 1.0 - 0.1 * strata), steep);
  let beach = sstep(G.water.w + 1.4, G.water.w + 0.3, h);
  c = mix(c, G.sand.xyz, beach);
  let snowy = sstep(46.0, 56.0, h + v * 6.0) * sstep(0.6, 0.8, ny);
  c = mix(c, G.snow.xyz, snowy);
  let gp = ground_paint(x, z);
  if (gp.w > 0.0) {
    let lay = pattern_shade(ground_pattern(x, z), x, z, dist, s);
    c = mix(c, gp.xyz / max(gp.w, 1.0 / 255.0) * (0.9 + 0.2 * v) * lay, gp.w);
  }
  if (G.extra.x > 0u) {
    let dug = carve(x, z, h).y;
    c = mix(c, vec3f(0.29, 0.22, 0.16) * (0.8 + 0.3 * v), dug * 0.85);
  }
  return c;
}

// Raymarcher: terrain heightfield + instanced SDF objects, sun/sky/fog/water.
// Appended after common.wgsl, the API functions, the generated types and the
// dispatch functions `type_sdf` / `type_color`.

const MAX_CAND: u32 = 24u;
const NEAR: f32 = 0.06;

var<private> cand: array<u32, 24>;
var<private> cand_t: array<vec2f, 24>;
// bounding sphere (centre, radius) of each candidate, cached to avoid storage reads per step
var<private> cand_s: array<vec4f, 24>;
var<private> ncand: u32;
var<private> dither: f32;

fn inst_params(i: Inst) -> Params {
  return Params(i.k0.x, i.k0.y, i.k0.z, i.k0.w, i.k1.x, i.k1.y, i.k1.z, i.k1.w,
                i.s0.x, i.s0.y, i.s0.z, i.s0.w, i.s1.x, i.s1.y, i.s1.z, i.s1.w, i.s2.x, i.s2.y, i.s2.z, i.s2.w,
                i.look[0].x, i.look[0].y, i.look[0].z, i.look[0].w, i.look[1].x, i.look[1].y, i.look[1].z, i.look[1].w,
                i.look[2].x, i.look[2].y, i.look[2].z, i.look[2].w, i.look[3].x, i.look[3].y, i.look[3].z, i.look[3].w,
                i.look[4].x, i.look[4].y, i.look[4].z, i.look[4].w);
}

// Rotate v by the unit quaternion (q, w) with w = sqrt(1 - |q|^2).
fn tilt_rot(q: vec3f, v: vec3f) -> vec3f {
  let w = sqrt(max(1.0 - dot(q, q), 0.0));
  let t = 2.0 * cross(q, v);
  return v + w * t + cross(q, t);
}

fn to_local(i: Inst, p: vec3f) -> vec3f {
  let d = p - i.pos_scale.xyz;
  let c = i.rot.x; let s = i.rot.y;
  var l = vec3f(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
  if (any(i.tilt.xyz != vec3f(0.0))) { l = tilt_rot(-i.tilt.xyz, l); }
  return l / i.pos_scale.w;
}

fn inst_center(i: Inst) -> vec3f {
  var off = vec3f(0.0, bitcast<f32>(i.info.y) * i.pos_scale.w, 0.0);
  if (any(i.tilt.xyz != vec3f(0.0))) {
    off = tilt_rot(i.tilt.xyz, off);
    let c = i.rot.x; let s = i.rot.y;
    off = vec3f(c * off.x + s * off.z, off.y, -s * off.x + c * off.z);
  }
  return i.pos_scale.xyz + off;
}

fn cut_sdf(c: vec4f, p: vec3f) -> f32 {
  let d = p - c.xyz;
  if (c.w > 0.0) { return length(d) - c.w; }
  let q = abs(d) - vec3f(-c.w);
  return length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0);
}

// A tube from a.xyz to b.xyz, radius a.w.
fn bore_sdf(a: vec4f, b: vec4f, p: vec3f) -> f32 {
  let pa = p - a.xyz;
  let ba = b.xyz - a.xyz;
  let h = clamp(dot(pa, ba) / max(dot(ba, ba), 1e-8), 0.0, 1.0);
  return length(pa - ba * h) - a.w;
}

// One of the frame's holes: four words after the grid's items, from
// G.extra.z on (see `FrameRequest::holes`).
fn hole(k: u32) -> vec4f {
  let b = G.extra.z + k * 4u;
  return vec4f(bitcast<f32>(items[b]), bitcast<f32>(items[b + 1u]), bitcast<f32>(items[b + 2u]), bitcast<f32>(items[b + 3u]));
}

// A shape's local distance with the instance's cuts taken out.
fn shape_sdf(i: Inst, lp: vec3f) -> f32 {
  var d = type_sdf(i.info.x, lp, inst_params(i));
  for (var c = 0u; c < 4u; c = c + 1u) {
    let cut = i.cuts[c];
    if (cut.w != 0.0) { d = max(d, -cut_sdf(cut, lp)); }
  }
  // Its many small holes only matter right at its surface: further out the
  // distance without them is still a safe step. Where they are rides in
  // the tilt's spare w: start << 13 | cuts << 7 | bores.
  let hb = bitcast<u32>(i.tilt.w);
  if (hb != 0u && d < 0.05) {
    let at = hb >> 13u;
    let cuts = (hb >> 7u) & 63u;
    for (var c = 0u; c < cuts; c = c + 1u) {
      d = max(d, -cut_sdf(hole(at + c), lp));
    }
    for (var c = 0u; c < (hb & 127u); c = c + 1u) {
      d = max(d, -bore_sdf(hole(at + cuts + c * 2u), hole(at + cuts + c * 2u + 1u), lp));
    }
  }
  return d;
}

fn inst_sdf(idx: u32, p: vec3f) -> f32 {
  return inst_sdf_of(insts[idx], p);
}

fn inst_sdf_of(i: Inst, p: vec3f) -> f32 {
  return shape_sdf(i, to_local(i, p)) * i.pos_scale.w;
}

fn consider(idx: u32, ro: vec3f, rd: vec3f, tmax: f32, dedupe: bool) {
  let i = insts[idx];
  if (i.rot.w < 1.0 && i.rot.w <= dither) { return; }
  let ctr = inst_center(i);
  let oc = ro - ctr;
  let r = i.rot.z;
  let b = dot(oc, rd);
  let c = dot(oc, oc) - r * r;
  let disc = b * b - c;
  if (disc < 0.0) { return; }
  let sq = sqrt(disc);
  let t0 = -b - sq;
  let t1 = -b + sq;
  if (t1 < 0.0 || t0 > tmax) { return; }
  if (dedupe) {
    for (var j = 0u; j < ncand; j = j + 1u) { if (cand[j] == idx) { return; } }
  }
  // Keep the list sorted by entry distance; when full, drop the farthest.
  if (ncand == MAX_CAND && t0 >= cand_t[MAX_CAND - 1u].x) { return; }
  var k = min(ncand, MAX_CAND - 1u);
  loop {
    if (k == 0u || cand_t[k - 1u].x <= t0) { break; }
    cand[k] = cand[k - 1u];
    cand_t[k] = cand_t[k - 1u];
    cand_s[k] = cand_s[k - 1u];
    k = k - 1u;
  }
  cand[k] = idx;
  cand_t[k] = vec2f(t0, t1);
  cand_s[k] = vec4f(ctr, r);
  if (ncand < MAX_CAND) { ncand = ncand + 1u; }
}

fn gather(ro: vec3f, rd: vec3f, tmax: f32) {
  ncand = 0u;
  if ((G.dims.w & 4u) != 0u) { return; }
  let n = G.dims.z;
  if ((G.dims.w & 1u) == 0u) {
    for (var i = 0u; i < n; i = i + 1u) { consider(i, ro, rd, tmax, false); }
    return;
  }
  // Coarse XZ grid: walk the cells the ray crosses, nearest first.
  let cs = G.grid0.z;
  let org = G.grid0.xy;
  let gw = i32(G.grid1.x); let gh = i32(G.grid1.y);
  var cx = i32(floor((ro.x - org.x) / cs));
  var cz = i32(floor((ro.z - org.y) / cs));
  let sx = select(-1, 1, rd.x >= 0.0);
  let sz = select(-1, 1, rd.z >= 0.0);
  let ix = select(1e30, abs(1.0 / rd.x), abs(rd.x) > 1e-6);
  let iz = select(1e30, abs(1.0 / rd.z), abs(rd.z) > 1e-6);
  let nbx = org.x + f32(cx + select(0, 1, sx > 0)) * cs;
  let nbz = org.y + f32(cz + select(0, 1, sz > 0)) * cs;
  var tx = abs(nbx - ro.x) * ix;
  var tz = abs(nbz - ro.z) * iz;
  let dx = cs * ix; let dz = cs * iz;
  var t = 0.0;
  for (var step = 0; step < 96; step = step + 1) {
    if (cx >= 0 && cz >= 0 && cx < gw && cz < gh) {
      let cell = cells[u32(cz * gw + cx)];
      for (var k = 0u; k < cell.y; k = k + 1u) {
        consider(items[cell.x + k], ro, rd, tmax, true);
      }
    }
    if (ncand >= MAX_CAND || t > tmax) { break; }
    if (tx < tz) { t = tx; tx = tx + dx; cx = cx + sx; } else { t = tz; tz = tz + dz; cz = cz + sz; }
  }
}

// x = safe step distance, y = candidate slot (-1 = terrain), z = distance to
// real surfaces only (bounding spheres excluded; used for penumbra and AO)
// `t` selects terrain detail; `ray_t` >= 0 lets candidates the ray has already
// left be skipped (their distance can only grow from there).
fn map_scene_lod(p: vec3f, t: f32, ray_t: f32) -> vec3f {
  var dt = (p.y - terrain_height_lod(p.x, p.z, t)) * 0.6;
  var d = dt;
  var id = -1.0;
  for (var j = 0u; j < ncand; j = j + 1u) {
    if (ray_t > cand_t[j].y + 0.5) { continue; }
    let sph = cand_s[j];
    let ds = length(p - sph.xyz) - sph.w;
    if (ds >= d) { continue; }
    if (ds < 0.25) {
      let i = insts[cand[j]];
      let lp = to_local(i, p);
      // Local bounding box (much tighter than the sphere for tall or flat shapes).
      let bx = bitcast<f32>(i.info.z);
      let bz = bitcast<f32>(i.info.w);
      if (bx > 0.0) {
        let rs = sph.w / i.pos_scale.w;
        let hy = sqrt(max(rs * rs - bx * bx - bz * bz, 0.0)) + 0.02;
        let q = abs(lp - vec3f(0.0, bitcast<f32>(i.info.y), 0.0)) - vec3f(bx, hy, bz);
        let db = (length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0)) * i.pos_scale.w;
        if (db > 0.2) {
          if (db < d) { d = db; }
          continue;
        }
      }
      let dj = shape_sdf(i, lp) * i.pos_scale.w;
      dt = min(dt, dj);
      if (dj < d) { d = dj; id = f32(j); }
    } else {
      d = ds;
    }
  }
  return vec3f(d, id, dt);
}

fn map_scene(p: vec3f) -> vec3f { return map_scene_lod(p, 0.0, -1.0); }

// Distance to the gathered instances only (no terrain): straight up from a
// point the land can't cover it, being a height field. x = safe step (to
// bounding spheres too), y = distance to real surfaces only (for penumbra
// and AO: a bounding sphere counted as solid shades rings round each thing).
fn map_insts(p: vec3f) -> vec2f {
  var d = 1e9;
  var real = 1e9;
  for (var j = 0u; j < ncand; j = j + 1u) {
    let sph = cand_s[j];
    let ds = length(p - sph.xyz) - sph.w;
    if (ds >= d) { continue; }
    if (ds < 0.25) {
      let i = insts[cand[j]];
      let dj = shape_sdf(i, to_local(i, p)) * i.pos_scale.w;
      d = min(d, dj);
      real = min(real, dj);
    } else {
      d = ds;
    }
  }
  return vec2f(d, real);
}

// Distance for ambient occlusion: the exact ground (the fine heightmap is
// centimetres off, which AO turns into contour lines across the land) and
// the gathered things.
fn ao_dist(p: vec3f, t: f32) -> f32 {
  return min(ground_air(p, terrain_height_lod_exact(p.x, p.z, t)) * 0.6, map_insts(p).y);
}

// Along a ray: only spheres the ray is inside right now are evaluated, and the
// step never jumps past the next sphere's entry. x = step, y = slot, z = true distance.
fn map_ray(p: vec3f, t: f32, lod_t: f32, relax: f32) -> vec3f {
  let dterr = ground_air(p, terrain_height_lod(p.x, p.z, lod_t)) * 0.6;
  var dtrue = dterr;
  var id = -1.0;
  var step = dterr * relax;
  for (var j = 0u; j < ncand; j = j + 1u) {
    let tr = cand_t[j];
    if (t < tr.x - 0.02) { step = min(step, max(tr.x - t, 0.0)); break; }
    if (t > tr.y + 0.02) { continue; }
    let i = insts[cand[j]];
    let lp = to_local(i, p);
    let bx = bitcast<f32>(i.info.z);
    let bz = bitcast<f32>(i.info.w);
    if (bx > 0.0) {
      let rs = i.rot.z / i.pos_scale.w;
      let hy = sqrt(max(rs * rs - bx * bx - bz * bz, 0.0)) + 0.02;
      let q = abs(lp - vec3f(0.0, bitcast<f32>(i.info.y), 0.0)) - vec3f(bx, hy, bz);
      let db = (length(max(q, vec3f(0.0))) + min(max(q.x, max(q.y, q.z)), 0.0)) * i.pos_scale.w;
      if (db > 0.2) { step = min(step, db); continue; }
    }
    let dj = shape_sdf(i, lp) * i.pos_scale.w;
    if (dj < dtrue) { dtrue = dj; id = f32(j); }
    step = min(step, dj);
  }
  return vec3f(step, id, dtrue);
}

struct Hit { t: f32, id: f32, steps: f32, }

fn march(ro: vec3f, rd: vec3f, tmax: f32) -> Hit {
  var t = NEAR;
  var tp = NEAR;
  var h = Hit(1e9, -2.0, 0.0);
  for (var i = 0; i < 160; i = i + 1) {
    let p = ro + rd * t;
    // Far away the height bound is loosened (bisection repairs overshoots).
    let m = map_ray(p, t, t, select(1.0, 1.3, t > 40.0));
    let eps = 0.0012 * t + 0.002;
    if (m.z < eps) {
      h = Hit(t, m.y, f32(i));
      break;
    }
    tp = t;
    t = t + max(m.x, 0.01 + 0.002 * t);
    if (t > tmax) { break; }
  }
  if (h.id == -1.0) {
    // Refine terrain hits by bisection (the height bound can overshoot),
    // onto the exact ground: the fine heightmap only speeds the steps, and
    // a hit left on its cells (a few cm off) shades in contour lines.
    var a = tp;
    var b = h.t + 0.05 + 0.002 * h.t;
    for (var k = 0; k < 8; k = k + 1) {
      let m = (a + b) * 0.5;
      let p = ro + rd * m;
      if (ground_air(p, terrain_height_lod_exact(p.x, p.z, m)) > 0.0) { a = m; } else { b = m; }
    }
    h.t = b;
  } else if (h.id >= 0.0) {
    // A hit is a few millimetres off its surface: in front (it stops
    // within eps) or inside (a step is never shorter than a centimetre).
    // Bring it onto the surface, or fine detail (an eye in a face) is
    // coloured from off it.
    let d0 = map_ray(ro + rd * h.t, h.t, h.t, 1.0);
    if (d0.z < 0.0) {
      var a = tp;
      var b = h.t;
      for (var k = 0; k < 6; k = k + 1) {
        let m = (a + b) * 0.5;
        let d = map_ray(ro + rd * m, m, m, 1.0);
        if (d.z > 0.0) { a = m; } else { b = m; if (d.y >= 0.0) { h.id = d.y; } }
      }
      h.t = b;
    } else {
      var d = d0;
      for (var k = 0; k < 4; k = k + 1) {
        if (d.z < 0.0001) { break; }
        h.t = h.t + d.z;
        d = map_ray(ro + rd * h.t, h.t, h.t, 1.0);
        if (d.y >= 0.0) { h.id = d.y; }
      }
    }
  }
  return h;
}

fn terrain_normal(p: vec3f, t: f32) -> vec3f {
  // A cave's wall, floor or roof: the hollow's own slope.
  if (G.extra.y > 0u) {
    let dv = void_dist(p);
    if (dv > -0.3 && dv < 0.3 && p.y < terrain_height_lod_exact(p.x, p.z, t) - 0.05) {
      let e = 0.02;
      let g = vec3f(void_dist(p + vec3f(e, 0.0, 0.0)) - void_dist(p - vec3f(e, 0.0, 0.0)),
                    void_dist(p + vec3f(0.0, e, 0.0)) - void_dist(p - vec3f(0.0, e, 0.0)),
                    void_dist(p + vec3f(0.0, 0.0, e)) - void_dist(p - vec3f(0.0, 0.0, e)));
      return -normalize(g + vec3f(0.0, 1e-6, 0.0));
    }
  }
  let e = 0.04 + 0.0025 * t;
  let hx = terrain_height_lod_exact(p.x + e, p.z, t) - terrain_height_lod_exact(p.x - e, p.z, t);
  let hz = terrain_height_lod_exact(p.x, p.z + e, t) - terrain_height_lod_exact(p.x, p.z - e, t);
  return normalize(vec3f(-hx, 2.0 * e, -hz));
}

fn inst_normal(idx: u32, p: vec3f, t: f32) -> vec3f {
  let e = 0.002 + 0.0006 * t;
  let k1 = vec3f(1.0, -1.0, -1.0); let k2 = vec3f(-1.0, -1.0, 1.0);
  let k3 = vec3f(-1.0, 1.0, -1.0); let k4 = vec3f(1.0, 1.0, 1.0);
  return normalize(k1 * inst_sdf(idx, p + k1 * e) + k2 * inst_sdf(idx, p + k2 * e)
                 + k3 * inst_sdf(idx, p + k3 * e) + k4 * inst_sdf(idx, p + k4 * e));
}

// Shadow rays read the heightmap, but exact heights on steep cells.
const SHADOW_LOD: f32 = 40.0;

fn soft_shadow(ro: vec3f, rd: vec3f) -> f32 {
  let tmax = 24.0;
  gather(ro, rd, tmax);
  var res = 1.0;
  var t = 0.08;
  for (var i = 0; i < 28; i = i + 1) {
    let p = ro + rd * t;
    // Coarse terrain is plenty for a short soft shadow, except on a rock
    // face: there a heightmap cell spans the wall and would put the start
    // of the ray inside it (black blots along the foot of every cliff).
    let m = map_ray(p, t, SHADOW_LOD, 1.0);
    res = min(res, 40.0 * m.z / t);
    if (res < 0.02) { return 0.0; }
    t = t + clamp(m.x, 0.08 + 0.04 * t, 3.0);
    if (t > tmax) { break; }
  }
  return clamp(res, 0.0, 1.0);
}

// ---- sky and weather ----

// How a direction lies toward the sun, round the horizon: 1 facing it, 0 away.
fn toward_sun(rd: vec3f) -> f32 {
  let sun = G.wx[5].xyz;
  let a = normalize(vec2f(rd.x, rd.z) + vec2f(1e-4, 0.0));
  let b = normalize(vec2f(sun.x, sun.z) + vec2f(1e-4, 0.0));
  return dot(a, b) * 0.5 + 0.5;
}

// Cloud over a point of the cloud layer (world xz), 0..1. Twin of
// `cloud_density` in render/cpu.rs.
fn cloud_density(xz: vec2f) -> f32 {
  let cover = G.wx[0].x;
  if (cover < 0.01) { return 0.0; }
  let q = (xz - G.wx[0].yz) * 0.0021;
  let n = fbm2(q.x, q.y, G.seed.x + 911u, 4u) * 0.5 + 0.5;
  let thr = 0.62 - 0.27 * cover;
  return max(sstep(thr, thr + 0.13, n), sstep(0.8, 1.0, cover) * 0.9);
}

// The clouds' own colour seen along rd, `d` thick.
fn cloud_shade(rd: vec3f, d: f32) -> vec3f {
  let sun = G.wx[5].xyz;
  let day = G.wx[6].w;
  let dusk = G.wx[4].y;
  let night = G.sun_col.w;
  // Thick cloud is darker underneath.
  let body = 1.0 - 0.45 * d * G.wx[0].x - 0.35 * G.wx[4].w;
  let amb = mix(G.sky_hor.xyz, G.sky_zen.xyz, 0.4) * 0.55 + vec3f(0.33) * day;
  let fwd = pow(max(dot(rd, sun), 0.0), 6.0);
  var lit = G.sun_col.xyz * day * (0.5 * body + 0.7 * fwd * (1.0 - d * 0.5));
  // Sunrise and sunset light them from below: a blaze toward the sun, rose
  // and violet away from it, lingering a while after the sun has gone.
  let glowc = G.wx[6].xyz;
  let pink = mix(glowc, G.wx[7].xyz * 1.6 + vec3f(0.3, 0.06, 0.22), 0.5);
  let under = mix(pink, glowc * 1.3, pow(toward_sun(rd), 2.0));
  lit = lit + under * dusk * (0.65 + 0.6 * fwd) * (1.0 - 0.3 * d);
  // At night: dark shapes, faintly moonlit.
  lit = lit + vec3f(0.045, 0.055, 0.085) * night * (0.4 + 0.6 * pow(max(dot(rd, G.sun_dir.xyz), 0.0), 4.0));
  return G.wx[1].xyz * (amb * body * (1.0 - night * 0.88) + lit);
}

fn sky_base(rd: vec3f) -> vec3f {
  let up = max(rd.y, 0.0);
  var col = mix(G.sky_hor.xyz, G.sky_zen.xyz, sqrt(up));
  if (rd.y < 0.0) { col = G.sky_hor.xyz * (1.0 + rd.y * 0.3); }
  // Sunrise and sunset: a blaze low toward the sun, a rose band higher up
  // and a violet one away from it (a heavy overcast hides most of it).
  let cover = G.wx[0].x;
  let dusk = G.wx[4].y * (1.0 - 0.75 * cover * cover);
  if (dusk > 0.001) {
    let toward = toward_sun(rd);
    let low = exp(-up * 4.5);
    let glowc = G.wx[6].xyz;
    col = col + glowc * dusk * low * (0.12 + 0.88 * pow(toward, 4.0)) * 0.9;
    let band = exp(-abs(rd.y - 0.2) * 6.0);
    col = mix(col, G.wx[7].xyz * 1.3 + vec3f(0.2, 0.05, 0.14), dusk * band * (1.0 - toward) * 0.4);
    col = col + glowc * pow(max(dot(rd, G.wx[5].xyz), 0.0), 3.0) * dusk * 0.4;
  }
  let s = max(dot(rd, G.sun_dir.xyz), 0.0);
  let day = G.sun_dir.w;
  let night = G.sun_col.w;
  // The sun is wider low down.
  let lowsun = 1.0 - smoothstep(0.0, 0.3, G.wx[5].w);
  col = col + G.sun_col.xyz * (pow(s, mix(900.0, 350.0, lowsun)) * 6.0 * day + pow(s, 10.0) * 0.22 * day);
  // moon disc (sun_dir points at the moon at night)
  col = col + vec3f(0.85, 0.9, 1.0) * smoothstep(0.9993, 0.9996, s) * night;
  if (night > 0.0 && rd.y > 0.02) {
    let q = rd * 260.0;
    let hsh = hash3f(floor(q.x), floor(q.y), floor(q.z));
    if (hsh > 0.9965) {
      let tw = 0.6 + 0.4 * sin(G.cam_pos.w * 1.7 + hsh * 900.0);
      col = col + vec3f(0.9, 0.92, 1.0) * night * tw * smoothstep(0.02, 0.2, rd.y) * (1.0 - G.wx[4].y);
    }
  }
  return col;
}

fn sky(rd: vec3f) -> vec3f {
  var col = sky_base(rd);
  let cover = G.wx[0].x;
  if (cover > 0.01 && rd.y > 0.0) {
    let t = max(G.wx[0].w - G.cam_pos.y, 50.0) / max(rd.y, 0.015);
    let d = cloud_density(G.cam_pos.xz + rd.xz * t);
    if (d > 0.002) {
      // Far clouds melt into the haze at the horizon.
      let far = sstep(0.01, 0.18, rd.y);
      let c = mix(fog_color(rd), cloud_shade(rd, d), 0.3 + 0.7 * far);
      col = mix(col, c, d * (0.3 + 0.7 * far));
    }
  }
  // Lightning lights the sky from inside the clouds.
  return col + vec3f(0.6, 0.65, 0.85) * G.wx[1].w * (0.25 + 0.6 * cover);
}

fn fog_color(rd: vec3f) -> vec3f {
  let s = max(dot(rd, G.sun_dir.xyz), 0.0);
  let glow = G.wx[6].xyz * G.wx[4].y * pow(toward_sun(rd), 4.0) * 0.45 * (1.0 - 0.6 * G.wx[0].x);
  return G.sky_hor.xyz + G.sun_col.xyz * pow(s, 6.0) * 0.25 * G.sun_dir.w + glow;
}

// What falls (rain, snow, ash, motes). Drops live in the world, on a grid of
// unseen upright sheets (one set facing x, one facing z) that the view ray
// crosses near the eye. Each sheet carries its own falling drops, so near
// ones sweep past as you walk and far ones barely move; anything nearer
// than a drop hides it.
fn precip(col: vec3f, rd: vec3f, hit: f32) -> vec3f {
  let amount = G.wx[2].w;
  if (amount < 0.01) { return col; }
  let look = u32(G.wx[3].w + 0.5);
  let ro = G.cam_pos.xyz;
  let tm = G.cam_pos.w;
  let tw = tm % 600.0;
  // How big one pixel is, a metre away.
  let px = 2.0 * atan(length(G.cam_up.xyz)) / f32(G.dims.y);
  var speed = 8.0; var carry = 0.35; var cw = 0.5; var ch = 2.4; var hw = 0.004; var hl = 0.2; var alpha = 0.5; var dens = 0.45;
  if (look == 1u) { speed = 1.1; carry = 0.9; cw = 0.35; ch = 0.35; hw = 0.005; hl = 0.005; alpha = 0.7; dens = 0.16; }
  if (look == 2u) { speed = 0.15; carry = 0.3; cw = 0.5; ch = 0.5; hw = 0.006; hl = 0.006; alpha = 0.85; dens = 0.18; }
  // Everything falls with the wind; drops drawn as it streaks.
  let wind = G.wx[3].xy * G.wx[3].z * speed * carry;
  let vel = vec3f(wind.x, -speed, wind.y);
  let light = clamp(dot(G.sky_hor.xyz, vec3f(0.33)) * 1.1 + 0.1 + G.wx[1].w, 0.12, 1.4);
  let gap = 1.5;
  let reach = 18.0;
  let tmax = min(hit, reach);
  // Under a roof nothing falls right before the eyes; flakes and motes
  // keep a little further off so none fills the view.
  let tmin = select(select(1.4, 0.8, look == 0u), 6.0, G.wx[4].z > 0.5);
  var out = col;
  for (var f = 0u; f < 2u; f = f + 1u) {
    let d = select(rd.x, rd.z, f == 1u);
    let o = select(ro.x, ro.z, f == 1u);
    let ad = abs(d);
    if (ad < 0.04) { continue; }
    let dir = sign(d);
    var k = select(ceil(o / gap) - 1.0, floor(o / gap) + 1.0, d > 0.0);
    for (var j = 0u; j < 14u; j = j + 1u) {
      let t = (k * gap - o) / d;
      let ki = i32(k);
      k = k + dir;
      if (t > tmax) { break; }
      if (t < tmin) { continue; }
      // Where on this sheet, in the drops' own falling frame.
      let q = ro + rd * t - vel * tw;
      let vu = select(vel.z, vel.x, f == 1u);
      let u = select(q.z, q.x, f == 1u) + vu / speed * q.y;
      let v = q.y;
      let col_i = i32(floor(u / cw));
      let hc = u2f(h2(col_i, ki, 5113u + f));
      let vv = v + hc * 97.0;
      let cell = vec2i(col_i, i32(floor(vv / ch)));
      let h = h3(cell.x, cell.y, ki, 977u + f * 31u);
      if (u2f(h) > amount * dens) { continue; }
      var cx = 0.2 + 0.6 * u2f(pcg(h));
      let cy = 0.2 + 0.6 * u2f(pcg(h ^ 7u));
      if (look != 0u) { cx = cx + 0.2 * sin(tm * (0.7 + u2f(pcg(h ^ 3u))) + f32(h & 63u)); }
      // Never thinner than about a pixel, seen edge-on or far away; a flake
      // smaller than its pixel shows only as faint as the share it covers.
      let p = px * t;
      let w = max(hw, 0.5 * p / ad);
      let l = max(hl, 0.5 * p);
      let ax = 1.0 - sstep(w, w + 0.5 * p / ad, abs((fract(u / cw) - cx) * cw));
      let ay = 1.0 - sstep(l, l + 0.5 * p, abs((fract(vv / ch) - cy) * ch));
      var a = ax * ay * alpha;
      if (look != 0u) { a = a * min(sqrt(hw * hl / (w * l)) * 1.6, 1.0); } else { a = a * min(sqrt(hw / w) * 1.2, 1.0); }
      a = a * sstep(tmin, tmin + 1.5, t) * (1.0 - sstep(reach * 0.55, reach, t)) * sstep(0.04, 0.2, ad);
      if (look == 2u) { a = a * (0.55 + 0.45 * sin(tm * 3.0 + f32(h & 255u))); }
      if (a > 0.0) { out = mix(out, G.wx[2].xyz * light, a); }
    }
  }
  return out;
}

fn to_lin(c: vec3f) -> vec3f { return c * c; }

// Lanterns, fires and other glowing things: no shadows, soft falloff.
fn point_light(p: vec3f, n: vec3f) -> vec3f {
  var sum = vec3f(0.0);
  let count = min(G.grid1.z, 8u);
  for (var i = 0u; i < count; i = i + 1u) {
    let l = G.lights[i];
    let c = G.light_cols[i];
    let d = l.xyz - p;
    let dist = length(d);
    if (dist > c.w) { continue; }
    let fall = 1.0 - smoothstep(c.w * 0.4, c.w, dist);
    let ndl = max(dot(n, d / max(dist, 1e-3)), 0.0) * 0.8 + 0.2;
    sum = sum + to_lin(c.xyz) * l.w * ndl * fall / (1.0 + dist * dist * 0.3);
  }
  return sum;
}

// Generic looks every object can have: charred, wet, highlighted (fx.w).
fn apply_fx(albedo: vec3f, fx: vec4f) -> vec3f {
  var a = mix(albedo, vec3f(0.07, 0.06, 0.055), clamp(fx.x, 0.0, 1.0));
  a = a * (1.0 - 0.35 * clamp(fx.y, 0.0, 1.0));
  return mix(a, vec3f(1.0, 0.97, 0.8), clamp(fx.w, 0.0, 1.0) * 0.28);
}

// How open the sky is straight above a point: 1 in the open, low under a
// roof, a canopy, an overhang. It gathers what stands above the point: the
// view ray's candidates were only those it passed near, so a canopy shaded
// the ground in streaks fanning out from the eye, shifting as it moved.
// (The candidates are the shadow ray's next anyway.)
fn sky_open(p: vec3f) -> f32 {
  var res = 1.0;
  // Down in a cave the sky is far: the deeper, the less of it.
  if (G.extra.y > 0u) {
    let under = terrain_height(p.x, p.z) - p.y;
    if (under > 0.3 && void_dist(p) < 0.5) { res = clamp(1.0 - under * 0.35, 0.03, 1.0); }
  }
  gather(p, vec3f(0.0, 1.0, 0.0), 8.0);
  var t = 0.15;
  for (var i = 0; i < 20; i = i + 1) {
    let m = map_insts(p + vec3f(0.0, t, 0.0));
    res = min(res, 8.0 * m.y / t);
    if (res < 0.05) { break; }
    t = t + clamp(m.x, 0.1, 1.5);
    if (t > 8.0) { break; }
  }
  return clamp(res, 0.0, 1.0);
}

fn shade(p: vec3f, n: vec3f, albedo: vec3f, rd: vec3f, ao: f32) -> vec3f {
  let sun = G.sun_dir.xyz;
  // Indoors the sky's blue doesn't reach; daylight spills in warm instead.
  var open = 1.0;
  if ((G.dims.w & 32u) == 0u) { open = sky_open(p + n * 0.25); }
  let ndl = dot(n, sun);
  var shadow = 1.0;
  if (ndl > 0.0 && (G.dims.w & 2u) != 0u && sun.y > 0.0) {
    shadow = soft_shadow(p + n * 0.05, sun);
  }
  // Clouds' shadows slide over the land.
  if (ndl > 0.0 && G.wx[0].x > 0.01 && sun.y > 0.05) {
    shadow = shadow * (1.0 - 0.7 * cloud_density(p.xz + sun.xz * ((G.wx[0].w - p.y) / sun.y)));
  }
  // Out in the open, what fell darkens things and makes them shine.
  let wet = G.wx[4].x * open;
  let a = to_lin(albedo) * (1.0 - 0.4 * wet);
  let shut = 1.0 - open;
  let sky_amb = to_lin(mix(G.sky_hor.xyz, G.sky_zen.xyz, 0.5 + 0.5 * n.y)) * G.sky_hor.w * (0.25 + 0.75 * open)
    + vec3f(0.11, 0.09, 0.07) * shut * G.sky_hor.w * (0.3 + 0.7 * G.sun_dir.w);
  let bounce = to_lin(G.sky_hor.xyz) * 0.15 * clamp(-n.y * 0.5 + 0.5, 0.0, 1.0) * G.sky_hor.w * (0.4 + 0.6 * open);
  // Moonlight stays useful at night: the land must remain walkable.
  let strength = max(0.15 + 0.85 * G.sun_dir.w, 0.75 * G.sun_col.w);
  let direct = to_lin(G.sun_col.xyz) * max(ndl, 0.0) * shadow * strength * 1.6;
  let night_floor = vec3f(0.018, 0.022, 0.035) * G.sun_col.w;
  var lin = a * (direct + (sky_amb + bounce + night_floor) * ao + point_light(p, n));
  let h = normalize(sun - rd);
  lin = lin + to_lin(G.sun_col.xyz) * pow(max(dot(n, h), 0.0), 40.0) * (0.08 + 0.4 * wet) * shadow * G.sun_dir.w;
  lin = lin + to_lin(G.sky_hor.xyz) * pow(max(dot(reflect(rd, n), vec3f(0.0, 1.0, 0.0)), 0.0), 4.0) * 0.12 * wet;
  return sqrt(max(lin, vec3f(0.0)));
}

fn apply_fog(col: vec3f, t: f32, rd: vec3f) -> vec3f {
  let maxd = G.cam_fwd.w;
  var f = 1.0 - exp(-t * 0.0045 * G.sky_zen.w);
  f = max(f, smoothstep(maxd * 0.72, maxd, t));
  return mix(col, fog_color(rd), clamp(f, 0.0, 1.0));
}

fn bayer4(x: u32, y: u32) -> f32 {
  let m = array<f32, 16>(0.0, 8.0, 2.0, 10.0, 12.0, 4.0, 14.0, 6.0, 3.0, 11.0, 1.0, 9.0, 15.0, 7.0, 13.0, 5.0);
  return (m[(y & 3u) * 4u + (x & 3u)] + 0.5) / 16.0;
}

// Colour, and the distance to what is seen (1e9: the sky).
fn render(ro: vec3f, rd: vec3f) -> vec4f {
  let tmax = G.cam_fwd.w;
  gather(ro, rd, tmax);
  let h = march(ro, rd, tmax);
  var col: vec3f;
  var t_hit = h.t;
  if ((G.dims.w & 8u) != 0u) {
    // debug: step heatmap
    return vec4f(h.steps / 200.0, f32(ncand) / 24.0, 0.0, h.t);
  }
  if ((G.dims.w & 16u) != 0u) {
    return vec4f(vec3f(select(clamp(h.t / 200.0, 0.0, 1.0), 1.0, h.id < -1.5)), h.t);
  }
  let no_ao = (G.dims.w & 32u) != 0u;
  if (h.id < -1.5) {
    col = sky(rd);
    t_hit = 1e9;
  } else {
    let p = ro + rd * h.t;
    var n: vec3f;
    var albedo: vec3f;
    var glow = vec3f(0.0);
    var ao = 1.0;
    if (h.id < 0.0) {
      n = terrain_normal(p, h.t);
      albedo = ground_color(p.x, p.z, p.y, n.y, h.t);
      // Inside the ground: its rock and earth.
      if (G.extra.y > 0u && p.y < terrain_height(p.x, p.z) - 0.3) {
        let v = vnoise3(p.x * 1.7, p.y * 1.7, p.z * 1.7, G.seed.x + 77u);
        albedo = mix(G.rock.xyz * 0.7, vec3f(0.29, 0.22, 0.16), 0.35 + 0.3 * v);
      }
      // contact darkening from nearby objects
      if (!no_ao) {
        let o = ao_dist(p + n * 0.6, h.t);
        ao = clamp(0.45 + o * 0.9, 0.0, 1.0);
      }
    } else {
      let idx = cand[u32(h.id)];
      n = inst_normal(idx, p, h.t);
      let i = insts[idx];
      let raw = api_split_glow(type_color(i.info.x, to_local(i, p), inst_params(i)));
      let base = clamp(raw.xyz, vec3f(0.0), vec3f(1.0));
      albedo = apply_fx(base, i.fx);
      // Only the parts marked with glow() shine, unless the type marks none.
      let lit = select(1.0, raw.w, type_marks_glow(i.info.x));
      // The dark's own (charred past 1): whatever glows on it glows red.
      let gc = select(base, vec3f(1.0, 0.07, 0.03), i.fx.x > 1.5);
      glow = gc * clamp(i.fx.z, 0.0, 2.0) * lit;
      if (!no_ao) {
        let o1 = ao_dist(p + n * 0.15, h.t);
        let o2 = ao_dist(p + n * 0.5, h.t);
        ao = clamp(0.35 + (o1 / 0.15) * 0.3 + (o2 / 0.5) * 0.35, 0.0, 1.0);
      }
    }
    col = shade(p, n, albedo, rd, ao) + glow;
  }
  // The eye under the water: what is seen fades into the water's own colour
  // within a few metres, darker the deeper; overhead, the surface shines.
  var we = G.water.w;
  if (G.ground.w > 0.0) { we = max(we, ground_water(ro.x, ro.z)); }
  let eye_ground = terrain_height(ro.x, ro.z);
  let eye_cave = G.extra.y > 0u && ro.y < eye_ground - 0.3 && void_dist(ro) < 0.3;
  if (ro.y < we && eye_ground < we && !eye_cave) {
    let lit = shade(ro, vec3f(0.0, 1.0, 0.0), G.water.xyz, vec3f(0.0, -1.0, 0.0), 1.0);
    let medium = lit * exp(-(we - ro.y) * 0.08);
    if (rd.y > 0.0) {
      let tw = (we - ro.y) / rd.y;
      if (tw < t_hit) {
        // The surface from below: the sky through it straight up, the
        // water's colour mirrored at a slant.
        let up = smoothstep(0.55, 0.95, rd.y);
        col = mix(lit * 1.35, mix(lit, sky(rd), 0.6), up);
        t_hit = tw;
      }
    }
    let f = 1.0 - exp(-min(t_hit, 400.0) * 0.16);
    col = mix(col * mix(vec3f(1.0), G.water.xyz * 1.6, 0.5), medium, clamp(f, 0.0, 1.0));
    return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), t_hit);
  }
  // water plane: the sea, or a river's, a pond's or a canal's where the
  // ground seen lies under one (taken as level where it is crossed)
  var wl = G.water.w;
  if (t_hit < 1e8 && G.ground.w > 0.0) {
    let ph = ro + rd * t_hit;
    let lw = ground_water(ph.x, ph.z);
    if (lw > ph.y) { wl = max(wl, lw); }
  }
  // Inside a cave the sea doesn't reach.
  if (t_hit < 1e8 && G.extra.y > 0u) {
    let ph = ro + rd * t_hit;
    if (void_dist(ph) < 0.3 && ph.y < terrain_height(ph.x, ph.z) - 0.3) { wl = -1e4; }
  }
  if (rd.y < 0.0 && ro.y > wl) {
    let tw = (wl - ro.y) / rd.y;
    if (tw < t_hit && tw < tmax) {
      let pw = ro + rd * tw;
      // How deep what is seen lies: the bed, or something in the water over
      // it (a fish, kelp), which shows through clearer the nearer the top.
      var seen = terrain_height(pw.x, pw.z);
      if (t_hit < 1e8) { seen = max(seen, (ro + rd * t_hit).y); }
      let depth = max(wl - seen, 0.0);
      let tm = G.cam_pos.w;
      let rx = vnoise2(pw.x * 0.7 + tm * 0.35, pw.z * 0.7, 9u) - 0.5;
      let rz = vnoise2(pw.x * 0.7, pw.z * 0.7 - tm * 0.3, 10u) - 0.5;
      let rough = 0.22 * (1.0 + 1.6 * G.wx[3].z + 1.2 * G.wx[2].w);
      let n = normalize(vec3f(rx * rough, 1.0, rz * rough));
      let fres = 0.03 + 0.97 * pow(1.0 - max(dot(-rd, n), 0.0), 5.0);
      let refl = sky(reflect(rd, n));
      let body = shade(pw, vec3f(0.0, 1.0, 0.0), G.water.xyz, rd, 1.0);
      let under = col * vec3f(0.55, 0.75, 0.8);
      var wc = mix(under, body, clamp(1.0 - exp(-depth * 0.7), 0.15, 1.0));
      wc = mix(wc, refl, fres * 0.85);
      let hv = normalize(G.sun_dir.xyz - rd);
      wc = wc + G.sun_col.xyz * pow(max(dot(n, hv), 0.0), mix(120.0, 45.0, G.wx[4].y)) * (0.6 + 0.5 * G.wx[4].y) * G.sun_dir.w * (1.0 - 0.8 * G.wx[4].w);
      col = wc;
      t_hit = tw;
    }
  }
  if (t_hit < 1e8) {
    col = apply_fog(col, t_hit, rd);
    // Lightning lights the land too.
    col = col + vec3f(0.5, 0.55, 0.7) * G.wx[1].w * 0.3;
  }
  col = precip(col, rd, t_hit);
  return vec4f(clamp(col, vec3f(0.0), vec3f(1.0)), t_hit);
}

@compute @workgroup_size(8, 8)
fn render_main(@builtin(global_invocation_id) gid: vec3u) {
  let w = G.dims.x; let hgt = G.dims.y;
  if (gid.x >= w || gid.y >= hgt) { return; }
  dither = bayer4(gid.x, gid.y);
  let u = (2.0 * (f32(gid.x) + 0.5 + G.jitter.x) / f32(w)) - 1.0;
  let v = 1.0 - (2.0 * (f32(gid.y) + 0.5 + G.jitter.y) / f32(hgt));
  let rd = normalize(G.cam_fwd.xyz + G.cam_right.xyz * u + G.cam_up.xyz * v);
  let c = render(G.cam_pos.xyz, rd);
  outp[gid.y * w + gid.x] = pack4x8unorm(vec4f(c.rgb, 1.0));
  // After the colours: how far each pixel's surface is.
  outp[w * hgt + gid.y * w + gid.x] = bitcast<u32>(c.w);
}

@compute @workgroup_size(8, 8)
fn heightmap_fine_main(@builtin(global_invocation_id) gid: vec3u) {
  let n = u32(G.hmap2.w);
  if (gid.x >= n || gid.y >= n) { return; }
  let x = G.hmap2.x + f32(gid.x) * G.hmap2.z;
  let z = G.hmap2.y + f32(gid.y) * G.hmap2.z;
  hmap[u32(G.hmap.w) * u32(G.hmap.w) + gid.y * n + gid.x] = terrain_height(x, z);
}

@compute @workgroup_size(8, 8)
fn heightmap_main(@builtin(global_invocation_id) gid: vec3u) {
  let n = u32(G.hmap.w);
  if (gid.x >= n || gid.y >= n) { return; }
  let x = G.hmap.x + f32(gid.x) * G.hmap.z;
  let z = G.hmap.y + f32(gid.y) * G.hmap.z;
  // Before the pits are cut: they are cut after the lookup, exactly, where a
  // cell holding one would smear its rim into teeth (a shallow pit's cells
  // are not steep enough to be read exactly).
  hmap[gid.y * n + gid.x] = terrain_height_w(x, z, terrain_weights(x, z)) + ground_delta(x, z);
}

// Parity probe: evaluate a type (mode 0) or the terrain (mode 1) at given points.
@compute @workgroup_size(64)
fn probe_main(@builtin(global_invocation_id) gid: vec3u) {
  let i = gid.x;
  if (i >= G.probe.x) { return; }
  let q = probe_io[i];
  if (G.probe.y == 0u) {
    var inst = insts[0];
    inst.info.x = G.probe.z;
    let k = inst_params(inst);
    let d = shape_sdf(inst, q.xyz);
    let c = type_color(G.probe.z, q.xyz, k);
    probe_io[i] = vec4f(d, c);
  } else if (G.probe.y == 3u) {
    // An instance in the world: its distance at a world point, and its middle.
    var inst = insts[0];
    inst.info.x = G.probe.z;
    probe_io[i] = vec4f(inst_sdf_of(inst, q.xyz), inst_center(inst));
  } else if (G.probe.y == 4u) {
    // The ground edits' water and paint.
    let gp = ground_paint(q.x, q.z);
    probe_io[i] = vec4f(ground_water(q.x, q.z), gp.xyz / max(gp.w, 1.0 / 255.0));
  } else if (G.probe.y == 2u) {
    let s = G.seed.x;
    let wt = terrain_weights(q.x, q.z);
    let n = fbm2(q.x * 0.0045 + G.toff1.x, q.z * 0.0045 + G.toff1.y, s + 101u, 5u);
    let r = ridged2(q.x * 0.0032 + G.toff1.y, q.z * 0.0032 + G.toff1.x, s + 202u, 3u);
    probe_io[i] = vec4f(n, r, wt.w[0], wt.w[1]);
  } else {
    let h = terrain_height(q.x, q.z);
    probe_io[i] = vec4f(h, ground_color(q.x, q.z, h, 1.0, 1.0e4));
  }
}

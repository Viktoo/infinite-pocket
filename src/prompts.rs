//! Prompt text. The language reference is the stable, cached prefix of every
//! builder call; it must describe exactly what `lang::check` accepts.

pub const LANGUAGE: &str = r#"You write object types for Infinite Pocket, a 3D world rendered by raymarching signed distance functions (SDFs) on the GPU. Each object type is a small module in a strict subset of JavaScript. Your code is never run as JavaScript: it is parsed, checked against an allowlist, and translated to WGSL and to a CPU bytecode. Anything outside the subset is rejected.

## Module shape (exactly this)

```js
export const meta = {
  name: "lighthouse",
  bounds: [3, 14, 3],          // half-extents in metres around the local origin (each 0 < b <= 40)
  tags: ["building", "landmark"],
  props: { burns: 0.1, light: 1 },   // optional: what it is made of and does (see Properties)
  sound: { hard: 0.8, dry: 0.6, ring: 0.1 },   // optional, 0–1: how it sounds struck, brushed or walked into, if its props don't say (soft…crisp, wet…brittle, how long it rings)
};

// Signed distance in local space (metres). Negative inside, positive outside.
export function sdf(x, y, z, k) {
  const tower = cappedCone(x, y - 6, z, 6, 1.6, 1.0);
  const lamp  = sphere(x, y - 12.5, z, 1.1);
  return smoothUnion(tower, lamp, 0.3);
}

// Colour at a surface point (local space). Must return a colour on every path.
export function color(x, y, z, k) {
  if (y > 11.5) return glow(rgb(255, 230, 160));   // the lamp is what shines
  return (floor(y / 2) % 2 == 0) ? rgb(220, 60, 50) : rgb(240, 240, 235);
}
```

Coordinates: y is up, +z is the object's front. Someone standing in front of it, looking back at it, has +x on their LEFT: lettering, numbers, a face or a clock on the front run toward -x (draw text so it reads with -x as "rightward"; on the back face, +x). The local origin is the centre of the object's footprint; the object normally stands on y = 0 (it is placed so its lowest solid point touches the ground). The whole shape must lie inside the box [-bx, bx] x [-by, by] x [-bz, bz] given by meta.bounds, and must not be empty. Make bounds snug (a little margin is fine): oversized bounds cost rendering time.

## Values and syntax
- Every value is a number (f32), a boolean (from comparisons), or a vec3 (a colour from rgb/hsv/mix, or a point from rotX/rotY/rotZ or vec3(x, y, z)). Read vec3 components with .x .y .z (or .r .g .b), e.g. `const q = rotY(x, y, z, 0.5); sphere(q.x, q.y, q.z, 1)`.
- Allowed: const, let (with an initial value), = += -= *= /= %=, ++ and -- on let variables, + - * / % ** (on numbers; + - * / also on vec3 with vec3 or number), < <= > >= == != === !==, && || !, ?:, if / else, return, and for loops with literal bounds of at most 32 iterations: `for (let i = 0; i < 8; i++) { ... }` (nested loops at most 256 iterations in total).
- Top-level `const NAME = <number expression>;` constants are allowed.
- k holds the instance parameters, read as k.seed, k.scale, k.a, k.b, k.c, k.d, k.e, k.f. Use hash(k.seed) for per-instance variation (k.seed is an integer-valued number). k.a..k.f default to 0.5.
- The constant PI. `Math.sin(...)` style is accepted as an alias for the plain functions.
- Not allowed: while/do, unbounded or variable-bound for, recursion, helper functions, closures/arrow functions, classes, new, this, import, strings (except in meta), arrays/objects (except in meta), switch, try, break/continue, globals of any kind, any identifier that is not a local, a parameter, PI, or an API function below.

## API (all arguments are numbers unless noted)
Shapes (return a distance):
- sphere(x, y, z, r)
- box(x, y, z, hx, hy, hz)                 half-extents
- roundBox(x, y, z, hx, hy, hz, r)
- cylinder(x, y, z, r, h)                  vertical, centred, half-height h
- cappedCone(x, y, z, h, r1, r2)           vertical, centred, half-height h, bottom radius r1, top radius r2
- capsule(x, y, z, h, r)                   vertical, from y=0 up to y=h
- capsule(x, y, z, ax, ay, az, bx, by, bz, r)   segment a→b
- torus(x, y, z, R, r)                     ring in the XZ plane
- ellipsoid(x, y, z, rx, ry, rz)
- triangle(x, y, z, ax, ay, az, bx, by, bz, cx, cy, cz, r)   a flat triangle through three corners anywhere, r thick (its half-thickness, rounding its edges)
- cone(x, y, z, ax, ay, az, bx, by, bz, ra, rb)   from a (radius ra) to b (radius rb), tapering between, round at both ends
- plane(x, y, z, h)                        y - h (infinite: always intersect it with something)
- plane(x, y, z, nx, ny, nz, d)
Thin flat parts are triangles (two make a four-sided panel; smoothUnion them into what they grow from): ears (an elf's or a cat's: long, pointed, lying back along the head), fins, wings, sails, leaves, blades, beaks, gables. Parts that taper are cones: horns, tusks, claws, tails, fingers, noses, branches, spikes. Capsules and cylinders are for parts as thick at both ends (an arm's bone, a pole, a rung).
Combining: union(a, b, ...) (2–8 args), intersect(a, b, ...), subtract(a, b) (a minus b), smoothUnion(a, b, k), smoothSubtract(a, b, k)
Rotation (return a vec3 point): rotX(x, y, z, angle), rotY(x, y, z, angle), rotZ(x, y, z, angle); vec3(x, y, z) makes a point
Noise and colour: noise3(x, y, z) in [0,1); hash(a [, b [, c]]) in [0,1); rgb(r, g, b) with 0–255 channels; hsv(h, s, v) all 0–1; mix(a, b, t) for numbers or colours
Light: glow(colour) or glow(colour, amount 0–1) marks the parts that give off light (lamps, neon, lit windows, signs, screens). Wrap the colour you return with it, e.g. `return glow(rgb(255, 60, 170));`; don't mix or scale its result. When a thing has the light property, only the parts marked this way shine; unmarked big things don't shine at all, so mark the lit parts of any building or large object that gives light.
Maths: abs, min (2–8 args), max (2–8 args), clamp(x, lo, hi), floor, ceil, round, fract, mod(a, b), sin, cos, tan, atan2(y, x), sqrt, pow(a, b) (uses |a|), exp, sign, step(edge, x), smoothstep(e0, e1, x), length2(x, y), length3(x, y, z)

## Properties (meta.props)
Every thing has a few numbers the world's rules act on. Give the ones that matter for this object; the rest default sensibly from tags and size:
__PROPS__
Examples: a wooden hut { burns: 0.4 }; a lantern { light: 1, heat: 120, fragile: 0.6, burns: 0.6, fuel: 0.5 } (oil inside: if it breaks, it burns); a ball { bounce: 0.8, mass: 0.6 }; a hoop on a pole { mark: 1, mass: 30 }; an apple { edible: 0.4, mass: 0.2 }; a bucket of water { wet: 1, mass: 8 }; a sapling { alive: 1, growth: 0.1, burns: 0.5, sap: 0.5 }; parched summer grass { burns: 0.9, sap: 0.1 }; a torch, a brazier or a beacon { light: 1, heat: 300 } (a flame kept in check is heat, not fire: fire is something burning freely, and it spreads); a motor car in a world of horse carts { mass: 1200, strange: 0.9 }. Only use the property names listed. The world does the rest: fire spreads to what burns, water puts it out, fragile things break when hit hard, living things grow.

## Behaviour (optional): tick, use, touch
An object can act on its own with these optional exports. They run on the CPU a few times a second near people; they never draw anything themselves, but the shape and colour functions can read their state as k.s0 … k.s7.
```js
export const meta = { name: "brass clock", bounds: [0.3, 0.4, 0.2], tags: ["item"], says: ["Tick… tock."], sounds: ["ding"], spawns: [] };
export function tick(s, w, k) {        // s: state s.s0 … s.s7 (numbers, start at 0, saved)
  s.s0 = s.s0 + w.dt;                  // w.dt = seconds since the last tick
  if (s.s0 > 60) { s.s0 = 0; sound(0); }
  w.light = w.hour > 19 || w.hour < 6 ? 0.5 : 0;   // set its own properties
}
export function use(s, w, k, o) {      // someone uses it (o = the thing it is used on, when w.on is 1)
  s.s1 = 1 - s.s1;                     // e.g. open/closed, read in sdf as k.s1
  say(0);
}
export function touch(s, w, k) { if (w.impact > 6) { w.health = w.health - 0.5; } }   // something hit it
```
- w.<field> describes the situation (read only): dt, hour (0–24), age (seconds since it was made), held (1 if someone holds it), near (people within 4 m), speed, ground (1 if resting on the ground), water (1 if in water), impact (touch: hit speed), on (use: 1 if used on something). Every other w.<name> is one of its own properties (read and write); o.<name> is the other thing's property (read and write).
- Effects (statements): say(i) speaks meta.says[i]; sound(i) makes meta.sounds[i]; spawn(i) makes a new meta.spawns[i] (a type name) next to it; transform(i) turns it into meta.spawns[i] (a seed into a sapling, an egg into a chick); remove() makes it vanish. Effects are rate-limited.
- Same language rules as sdf/color (no strings outside meta, loops with literal bounds); `return;` ends early; no return value. Keep them small.

## Rules that the validator checks
- sdf returns a number on every path; color returns a colour on every path.
- Results must be finite everywhere in and around the bounds: guard divisions and sqrt of possibly negative values.
- Keep the distance field well-behaved: when adding noise to a shape, keep the amplitude small (< 0.3 m) and multiply the result by ~0.8.
- sdf may use at most ~8000 operations per call; prefer a few primitives and short loops.
- Tags: use lower-case words. "building", "landmark", "tree", "rock", "bush", "grass", "flower", "tallgrass", "prop" … Add "nonsolid" for things the player can walk through (or swim through: weed, kelp). Things that grow by or in water say where in meta.grows (below).

## Parts, insides and tools (optional meta)
Bodies walk on what you draw: floors, stairs, ramps, roofs and ledges are real. A body steps up anything lower than about a quarter of its height (a person: 0.4 m; a stair is 0.15–0.25 m), jumps about half its height, crouches to about 60% of it, and walks through any gap wider than about 0.8 m and taller than it is. So:
- A building people can enter needs a doorway: an opening at ground level cut right through the wall (subtract a box), and an inside: hollow rooms (walls about 0.2–0.4 m thick, a roof or ceiling, a floor at y = 0 or a slab you subtract the rooms from). Keep the doorway empty: the world hangs the door in it.
- Upper floors need stairs (steps of 0.2 m rising 0.25 m apart) or a ramp, with an opening in the floor above where they arrive. A platform up high (a tree-house, a lookout, a ledge) needs a stair or a ladder up to it; a room under the ground a stair or ladder down (see in_ground). A ladder is any stair too steep to walk (rungs, a rope ladder, footholds, pegs up a trunk): people climb it.
- meta.anchors: named points that tell the world what goes where (local metres; face in degrees, 0 = +z, 90 = +x):
  { kind: "door", at: [x, 0, z], face: 0, size: [width, height], holds: "round oak door" }   the bottom middle of a doorway, facing out; the world hangs a door in it that opens and shuts: its own door, named in holds, made to match the building (a round door, an iron-bound gate, a stone slab, a hide flap, a bamboo screen; it is written for it), or without holds a plain wooden door.
  { kind: "hatch", at: [x, y, z], face: 0, size: [width, depth], holds: "iron trapdoor" }   the middle of an opening in a floor, a roof or the ground (keep it open in the shape); the world lays a lid in it, hinged along its back edge and lifting toward face (its own, named in holds, or a plain timber hatch). People open it and go through: give a ladder or stair under it.
  { kind: "slot", at: [x, y, z], holds: "iron sword" }   something real set there (on a shelf, a rack, a table): a type name. It can be picked up and used.
  { kind: "part", at: [x, y, z], face: 0, holds: "rope ladder" }   a separate part fixed there (a sign, a bell, a ladder).
  { kind: "stairs", at: [x, y, z], to: [x, y, z], face: 0 }   the foot and the top of a stair or ladder, where a body stands: at on the floor in front of its foot, to on the floor it reaches, a step past its top (on the platform, beside the hatch). face: the way a climber faces going up. People find their way up and down by it; a ladder is climbed.
  { kind: "bed", at: [...] }, { kind: "seat", at: [...], face: 180 }   where people sleep and sit (a bench, a chair, a throne, a saddle: anything with a seat can be sat on; a saddle or howdah worn by an animal lets it be ridden).
  { kind: "light", at: [...] }   a lamp or a lit window: it lights the ground around it at night.
- meta.in_ground: depth: the shape is set into the ground: the earth is cleared exactly from its box and stays round it and over its roof, meeting its walls with no gap (a cliff dwelling, a house dug into a hillside, an earth house, a bunker, a hobbit hole, a well, a vault). depth is how many metres its floor (y = 0) lies under the ground just outside its doorway: 0 for a doorway that opens onto the surface (in a hillside or rock face it is turned to the open side, its back into the hill); deeper (2–6) for a home under flat ground, and a way is dug down to its doorway. With a hatch instead of a doorway, the hatch sits level with the ground (a ladder down under it). Without either, y = 0 is the ground's level (a well: a ring above, its shaft below). Make its outside a plain block where it meets the earth (no eaves past its walls), with a floor, walls, a roof or ceiling, and its rooms inside.
- meta.hollow: [hx, hz, floor]: the ground under the shape is taken away in the box |x| < hx, |z| < hz down to local y = floor (negative), open to the sky. A sunken yard, a pit house open above. The shape then sits with its y = 0 at the ground (its walls may go down to floor), and should have a floor, walls and a way down. (For a room under the earth, use in_ground.)
- meta.joint: { axis: "y", at: [x, y, z], open: 100 }: it swings on a hinge (a gate, a lid, a trapdoor, a cupboard door) about that axis through `at`, `open` degrees when open. People open and shut it.
- meta.tool: { grip: [x, y, z], tip: [x, y, z], motions: ["swing", "thrust"], sharp: 0.8 }: how a tool is held (grip: where the hand goes) and where it strikes (tip), and the motions it knows: swing (a blade, a club), chop (an axe, a pick), thrust (a spear, a knife), dig (a spade, a hoe), pour (a bucket, a jug), shoot (a launcher, below). A sword: motions ["swing", "thrust"]; an axe ["chop", "swing"]; a shovel ["dig", "swing"]. sharp (optional, 0..1) is how keen its point or edge is: a club 0, an axe 0.6, a sword 0.7, a spear, a knife or an arrow 0.9. A keen thing thrown flies point first (grip to tip) and sticks where it strikes hard; a body it hits is wounded. Small things that fly point first (an arrow, a dart, a bolt, a throwing knife) get meta.tool too, with their tip at the point. A tool is carried in the hands, so its mass is what one person can lift: at most 25 kg, mostly far less (a sword 1.5, a musket 5, a machine gun 12); only something meant to need two (a cannon, a battering ram) weighs more.
  A launcher (a bow, a sling, a crossbow, a musket, a pistol, a cannon, a blowpipe, a wand that shoots) adds launch: { ammo: "arrow", speed: 55, spread: 1, holds: 12, per_shot: 1, ready: 0.8 } to its tool: ammo is the type name of what it shoots (written on its own when it doesn't exist yet), speed in m/s (a sling 30, a bow 55, a crossbow 70, a musket 400, a cannon 250), spread in degrees (a musket 1.5, a blunderbuss 6), holds how many shots it carries when full (a quiver 12, a pistol's magazine 8, a musket 1; 0: it never runs out), per_shot how many fly at once (a blunderbuss 8), ready the seconds to make ready between shots (a bow 0.8, a crossbow 3, a musket 8). Its grip→tip line is the line of fire: the tip is where the shot leaves (a gun's muzzle, a bow's arrow rest just ahead of the handle), its front +z. The shoot motion is added for it.
- meta.burst: { radius: 5, hurt: 0.8, push: 15, crater: 0.8, fire: 0.3, fuse: 4, impact: false }: it goes off (a grenade, a stick of dynamite, a powder keg, a bomb, a shell, a firework). radius in metres it reaches; hurt 0..1 how badly it hurts a body at its heart (fading to nothing at radius); push m/s it throws what is near (default 3 × radius); crater metres of ground it blows out, and the size of holes it tears in walls close by (0: none; a grenade 0.5, dynamite 1, a powder keg 2); fire 0..1 how hot it sets what is near; fuse seconds from being lit (used by hand, or caught by fire) to going off; impact true if it goes off when it hits something hard (a shell, a bomb). A firework: { radius: 3, hurt: 0.05, crater: 0, fire: 0.2, fuse: 3 }.
- meta.grows: { depth: [lo, hi], at: "bed" }: where a thing scattered over the land grows, in metres of water over the ground. Without it, it grows on dry ground only. Reeds and rushes [-0.4, 0.8] (the wet bank and the shallows), rice [0.05, 0.3], water lilies [0.3, 2.5] with at: "surface" (it floats: draw the pads at y = 0, stems hanging below), sea grass [0.5, 6], coral [1, 12], kelp [3, 25] (tall, nonsolid). A negative lo also lets it grow on dry ground up to that high above the water: a willow or mangrove that stands on the bank and in the shallows [-100, 0.6]. Under the water the light fades: something that glows ({ light: 0.5 }) shows in the deep. { at: "cave" } grows only in caves under the ground, on their floors (cave fungus, pale roots, crystals, glow worms' threads); it is never seen on the open land, and in the dark of a cave only what glows shows.
- meta.drive: { speed: 14, on: "land", by: "engine" }: it carries a rider who steers it: a cart, a car, a bicycle, a sled (on: "land"), a boat, a gondola or raft (on: "water"), a submarine or diving bell (on: "under": it goes anywhere in the water, from the bed up to the surface), a flying carpet or airship (on: "air"). speed is its top speed in m/s (a cart 4, a horse carriage 7, a bicycle 6, a car 15–30, a rowing boat 3, a gondola 2, a sailing ship 8, a submarine 6). by is what drives it: "legs" (pedals, a scooter), "oars" (oars, a paddle, a pole), "wind" (a sail), "beast" (a horse or ox in harness), "engine" (anything else: steam, magic). Give it a { kind: "seat", at: [x, y, z], face: 0 } anchor where the driver sits, facing its front (+z), and more seats for passengers (a gondola's, a bus's, a carriage's); keep the space above the seats open (an open top, a cockpit, a windscreen gap) so the driver can see out. Make its front face +z. Its mass is real (a car 1200 kg).

## Style
Recognisable silhouettes beat fine detail: the world is seen at low resolution. Use colour boldly and consistently with the universe's palette. A building is typically 4–10 m wide and 4–12 m tall; a person is 1.75 m tall.
"#;

pub fn builder_system(bible: &str, vocab: &crate::sim::props::Vocab) -> String {
    let lang = LANGUAGE.replace("__PROPS__", &vocab.describe());
    format!("{lang}\n## The universe\nEvery object belongs to this universe; match its tone, era, materials and palette:\n{bible}\n")
}

/// What a near-miss property name means in this world (asked once per name).
pub const PROP_ALIAS_TASK: &str = "Something written for a small simulated world gave values to property names the world doesn't have. The world's properties are listed below. For each unknown name, give the listed property it means, if one clearly does (the same quality, worded differently: \"curse\" for \"cursed\", \"wetness\" for \"wet\", \"weight\" for \"mass\"), or null if none does. Reply with one JSON object only, e.g. {\"curse\": \"cursed\", \"sparkle\": null}.";

pub const GENESIS_TASK: &str = r#"Design the base layer of this universe. Reply with:

1. One ```json block:
{
  "name": "short name of the land",
  "land": "the whole land in one or two sentences",
  "start": "where the traveler begins, in one sentence",
  "rivers": 0.6,
  "caves": 0.3,
  "sea": 0,
  "groundwater": 3,
  "gravity": 9.81,
  "air": 1,
  "palette": {
    "sky_day": [r,g,b], "horizon_day": [r,g,b], "sky_dusk": [r,g,b], "horizon_dusk": [r,g,b],
    "sky_night": [r,g,b], "horizon_night": [r,g,b], "sun": [r,g,b], "water": [r,g,b],
    "rock": [r,g,b], "snow": [r,g,b], "sand": [r,g,b], "fog": 1.0
  },
  "biomes": [
    { "name": "…", "base": 4, "amp": 12, "rough": 0.2, "cliffs": 0, "ground": [r,g,b], "ground2": [r,g,b],
      "scatter": { "tree": 0.3, "bush": 0.5, "rock": 0.2, "grass": 2.0, "flower": 0.6 }, "clump": { "tree": 0.7 }, "craters": 0,
      "grain": { "blades": 0.5, "flecks": 0.3, "fleck": [r,g,b], "fleck2": [r,g,b] } }
  ]
}
- land and start: the description above may name one place or one event; the world is far larger. "land" widens it into a whole country with room for many different places (other villages, wild country, neighbours who live differently): its geography, peoples, ways of life and any strangeness. "start" keeps the particular place and situation the description names (the vanished keeper, the wedding), or "" if it names none. Every region is planned from "land"; only the traveler's first region from "start".
- 3 to 6 biomes. The first is where the traveler begins (the land "start" names) and the most common; the others lie around it. base = mean ground height in metres (-10..30; below the sea's level, 0, makes lakes and coast), amp = hill height in metres (2..50; keep towns, cities and farmland at 2..6 so buildings stand on level ground, and save big hills for wild land), rough 0 (rolling) .. 1 (craggy), ground/ground2 = two ground colours that blend. A biome with base below 0 is a sea or lake floor: its scatter is what grows down there (a kelp forest, a reef, sea-grass meadows, a weedy lake bed), by its own types' tags. What grows on land keeps out of the water by itself, and what grows in water out of the dry, so a biome's scatter may hold both (reeds with the meadow grass by a lake). A world of water (an archipelago, a drowned land, the open sea, a people under the waves) gives most of its biomes base below 0 (-10 for the open sea) and its land one or two (islands, reefs); where the traveler begins under the sea, the first biome is a sea floor.
- cliffs 0..1 = how much of the biome's relief stands as cliffs: the land steps up in ledges with sheer rock faces between (in the palette's rock colour, banded in beds), and here and there a steep gully to climb. 0 = rolling land; 0.3 = the odd crag; 0.7 = canyon, mesa, gorge or fjord country; 1 = cliffs wherever there are hills. Faces only stand where the land has relief, so give cliffs with amp 15 or more (and rough for jagged crags). Keep 0 where people live and farm. Boulders and stones gather at the foot of cliffs by themselves.
- scatter = items per 100 m² by tag (trees 0.1–1.5, rocks 0.1–0.6, bushes 0.2–1, grass 0.5–3, flowers 0.2–1.5, tall grass 0.3–2). Use the tags of your base types below; "grass" tufts, "flower" (wildflowers) and "tallgrass" already exist, and your own types of a tag take its place.
- clump (optional) = how strongly a tag grows in patches, 0 (spread evenly) .. 1 (only in patches, with bare ground between). Without it things gather as in nature: trees 0.6 (woods with clearings and lone trees between), pines 0.45, flowers 0.9 (fields of one colour), tall grass 0.85 (meadows), bushes 0.5, rocks 0.55 (boulder fields). Set it when the land calls for it: a savanna's lone trees 0, an even jungle 0.2, orchard groves 0.9.
- fog: 1 = clear air, up to 3 = misty.
- rivers 0..1 = how many rivers run through the land, from springs in the high ground down to the sea or a pond: 0 = none (a desert, a dry plateau, a world of ice), 0.3 = the odd stream, 0.6 = a usual land, 1 = a land of rivers. They are real water: people cross them by bridges and fords, and boats go on them.
- caves 0..1 = how many caves open in the hillsides (a passage into the hill and a chamber), 0 none … 1 riddled with them. What lives in them is yours to give: a base type that grows only in caves ({ at: "cave" }, listed in a biome's scatter like any other), and beasts with "caves": true.
- sea = the sea's level in metres: 0 for any land with a sea, lakes or a coast (leave it). null for a land with no sea at all, where ground below 0 is only low ground, dry: a world of caves and tunnels deep under the surface, a desert basin, a dead sea bed, the Moon.
- groundwater = how many metres under the land's own surface the ground water lies: anything dug deeper (a well, a pit, a crater, a ditch) fills with water up to it. 0.5 a marsh or a delta, 2–4 a usual green land, 10 dry hills, 30 a desert; null where no water lies under the ground (the Moon, a world of rock and ice).
- grain (per biome, optional) = the ground's texture close underfoot, drawn into its colour: blades 0..1 = fine blades combed one way (0.5 for grass, 0.8 a thick lawn or reeds, 0.3 heather or moss, 0 sand, mud, snow, rock); flecks 0..1 = how thick small spots of two colours lie in clusters, fleck and fleck2 = their colours (clover and dry straw in a meadow, red and gold fallen leaves on a wood's floor, pebbles and shell on a shore, lichen on tundra, ash and cinders on a burnt plain). Left out, it is chosen from the ground colours.
- craters 0..1 (per biome, default 0) = how pocked the land is with round impact craters, bowls with raised rims from a few metres to over a hundred across: 0 none (any earthly land); 0.3 the odd one (a meteor-struck plain, a shelled battlefield); 0.8 the Moon's or Mercury's cratered highlands; 1 overlapping everywhere. Keep 0 where people farm and build.
- gravity = how hard things fall in m/s²: 9.81 anywhere on Earth (leave it so for any earthly place); the Moon 1.62, Mars 3.7, a small asteroid 0.6, a heavy world up to 25. Bodies jump as high and throw as hard as their muscles let them there.
- air 0..1 = how much breathable air there is out in the open: 1 anywhere on Earth (leave it); 0.3 thin (a high Martian dome's leak, a mountain of a dead world); 0 none (the Moon, an asteroid, deep space, under a sea with no air). Without air, fire won't burn, the world's sounds don't carry, and any being that breathes suffocates in about a minute unless it is inside a building that holds air or wears or holds something that gives it (the property "air": a space suit, a helmet, a tank; give such buildings and suits meta.props { air: 1 } when the world needs them). Species that don't breathe say so (see "breathes" below). An airless sky is black even by day (every sky and horizon colour near black, the sun stark white, fog 1), and its climate has one still, clear kind (clouds 0, wind 0, nothing falls).

Optionally, in the same JSON object, the land's large shape: "landforms", when the description names a real place (Santa Monica, the Isle of Skye, Kyoto, the Grand Canyon) or a clear shape of land (an island in the open sea, a valley between two ranges, a crater lake, a long peninsula, a town between the mountains and the sea). Without them the seed shapes the land at random; with them the land is shaped as they say, for tens of kilometres, and the seed only adds its hills, colours and plants:
  "landforms": [
    { "name": "the plain", "biome": "…", "outline": [[-8000, -8000], [8000, -8000], [8000, 8000], [-8000, 8000]], "edge": 1000 },
    { "name": "the bay", "biome": "…", "outline": [[-300, -4000], [-200, 0], [-600, 3000], [-6000, 4000], [-6000, -5000]], "edge": 250 },
    { "name": "the range", "biome": "…", "line": [[-3000, 6000], [4000, 7000]], "width": 3000, "edge": 1200, "rise": 500, "rugged": 0.8 }
  ]
- Metres; x is east, z is north, and the traveler begins at (0, 0): put the place "start" names there. For a real place keep its true distances and directions (the shore 300 m west of the town, the mountains 6 km to the north, the far shore of the bay 25 km away).
- biome: one of your biomes by name; inside its shape the land is that biome (its base, amp, cliffs, ground and plants). "outline" is an area (6–30 points round it); "line" with "width" is a band along points (a ridge, a range, a valley floor, a spit). edge: metres of soft border either side of the shape, half and half on it (a beach 200, hills 800, a range's foothills 1500).
- rise: metres it lifts the land where it is wholly itself, deeper inside than edge (hills 50–300, a range 300–1500, high mountains up to 2500; below 0 it sinks it), over the biome's own base. rugged 0..1: how broken that rise is into ridges and canyons (a smooth mesa 0, mountains 0.8).
- Later landforms lie over earlier ones. Lay the land first (one big outline of the main land biome over all of it, so the seed puts no sea in the middle of it), then the sea or lakes over it, then islands, hills and ranges.
- Usually 2–6. Leave it out for an invented land the seed may shape as it likes.

Also in the same JSON object, the land's weather: 2 to 6 kinds it has, each with how often it comes and how long a spell lasts. Fit them to the land (a desert: fair, haze, the odd sandstorm; a coast: fair, sea fog, squalls; a fae wood: soft sun, glimmer rain; a dead land: grey, ash fall), and let at least one kind make sunsets worth seeing (some cloud, not all of it):
  "climate": { "kinds": [
    { "name": "fair", "often": 3, "hours": [6, 30], "clouds": 0.25, "wind": 0.2 },
    { "name": "drizzle", "often": 1, "hours": [3, 10], "clouds": 0.9, "wind": 0.3, "fog": 1.5, "feel": "cool and grey",
      "falls": { "what": "rain", "amount": 0.4, "color": [170, 185, 205], "look": "streak", "props": { "wet": 1 } } },
    { "name": "thunderstorm", "often": 0.4, "hours": [2, 5], "clouds": 1, "cloud_color": [110, 115, 130], "wind": 0.85, "storm": 0.7,
      "falls": { "what": "rain", "amount": 1, "color": [160, 175, 200], "look": "streak", "props": { "wet": 1 } } }
  ] }
- clouds 0 (clear) .. 1 (overcast); wind 0 (still) .. 1 (a gale); fog 1 (clear air) .. 4 (thick); storm 0 .. 1 (lightning); hours: game hours [shortest, longest] (a game day is 24); optional cloud_color (white by default) and tint (a colour the light takes on: a golden haze, a green storm).
- falls: whatever comes down (rain, ash, petals, glimmering motes), how much (0..1), its colour, its look ("streak" for rain, "flake" for ash or petals, "mote" for specks that drift), and what it does to what it lands on in the open ("props": rain { "wet": 1 }, or this universe's own properties). Fog and mist are the "fog" value, not something that falls.

Optionally, in the same JSON object, the universe's own nature: properties and rules beyond the built-in physics (fire, water, breaking, growing already exist; don't repeat them). Only if this universe really has its own forces (magic, curses, rot, radiation, holiness, static, spores…):
  "properties": [ { "name": "cursed", "default": 0, "meaning": "how cursed it is, 0..1", "range": [0, 1], "scale": "0.2 a hexed trinket, 0.6 a cursed blade, 1 a lich's crown",
                   "rises": "fell under the curse", "falls": "was freed of the curse", "hazard": 0.5,
                   "spreads": { "noun": "curse", "big": "blight", "active": "cursed", "spent": "withered", "ended": "faded away", "stopped": "lifted" } } ],
  "rules": [ { "name": "curses spread by touch", "near": 1.5, "when": "self.cursed > 0.5 && other.cursed < self.cursed", "do": ["other.cursed += 0.05 * dt"] } ]
Rule language: `when` is a condition and `do` a list of assignments (=, +=, -=, *=) on self.<property> or other.<property>; you may use numbers, + - * /, comparisons, && || !, min(a,b), max(a,b), clamp(x,lo,hi), abs(x), dt (seconds), dist (metres apart, with "near"), hour, night (0/1), water (1 when in water), held (1 when held), falling (0..1, how much falls on it from the sky; 0 under a roof), wind (0..1), air (0..1, the air about it to breathe and burn). Without "near" a rule applies to each thing alone; with "near": r (≤ 10 m) to each pair within r. Spread slowly (rates times dt), at most 12 rules.
For each property, "scale": a few things of this world at known values, so everything made later uses one scale. Optionally: "rises"/"falls" are what a thing does when it gets it or loses it (told to people as news: "the oak fell under the curse"); "spreads" if it passes from thing to thing, with the words to tell one outbreak of it as a story (the engine counts it, finds what started it, and people may fight it); "hazard" 0..1 if it harms those near it or wearing it (they keep away); "range" its bounds.
Rules only act on things that have the property, and a property of your own shows nothing by itself: give it to some of your base types (in their meta.props) so the force is in the land from the start, and let it change what can be seen (light, fire, char, wet, growth, health: at 0 a thing breaks).

Optionally, the universe's peoples and beasts beyond plain humans (people always exist; dogs, cats, horses, wolves, goats and deer are built in for earthly worlds). Add species only if this universe has them (elves and orcs, a race of giants, dragons, griffins, lizard folk…), up to 5, in the same JSON object. Where the land has sea, lakes or rivers, add what lives in its water too, if big enough to see (carp or pike in a lake, salmon in a river, seals, dolphins, sharks, turtles, crocodiles, otters; and the strange: merfolk, a river serpent); small fish only as a school drawn as one body:
  "species": [ { "name": "elf", "plural": "elves", "body": "figure", "size": 1.0, "mind": "sapient", "speech": "words", "social": "village",
                 "diet": { "plants": 0.8, "meat": 0.2 }, "temper": { "bold": 0.4, "wary": 0.6, "playful": 0.5, "tame": 0.5 },
                 "move": { "walk": 1.4, "run": 3.5, "fly": 0, "swim": 0.8 }, "life": { "sleep": [23, 6] }, "mass": 60,
                 "look": { "height": [1.85, 2.0], "build": [0.7, 0.85], "skin": [0.0, 0.25] }, "sounds": [], "description": "tall, quiet forest folk" } ],
  "attitudes": [ { "a": "elf", "b": "orc", "affection": -0.4, "trust": -0.3, "rivalry": 0.3 } ],
  "sizes": { "human": 1.0 }, "traveler_height": 1.75,
  "looks": { "human": { "skin": [0.1, 0.35], "undertone": [0.6, 0.9], "eyes": [0.0, 0.15], "eye_shape": [0.6, 0.95], "nose": [0.4, 0.75], "hair_color": [0.0, 0.1], "height": [1.55, 1.8] } }
- body: a body name with "body_description": what it looks like and how it moves (a sleek cat with pointed ears and a long tail, an elephant with a trunk it raises, a slow sloth, a snake, a dragon with wings); bodies are written for you. "figure" (a person: height in metres 1.3–2.0, build 0.7–1.4, skin 0–1, undertone, eyes, eye_shape, nose, shape (man … woman), hair, beard, and clothes drawn on the body: top, skirt, trousers, shoes, hat) and "quadruped" (a plain four-legged animal about 1 m tall at size 1; sliders legs, length, ears, hue, shade, all 0–1) are generic starting points: a species named with one gets its own body written from it. Nothing smaller than a cat (about 0.3 m).
- mind: "sapient" (people: talk, plan, make things), "simple" (clever animals), "instinct" (beasts). speech: "words", "sounds" (noises listed in "sounds") or "none". social: solitary, pair, pack, herd or village. temper and diet values 0–1; size multiplies the generic body, even for a species whose own body is written (against the one-metre quadruped: 1.8 is horse-sized, an otter 0.35, a cat 0.3); mass in kg; move speeds in m/s; sleep: [from hour, to hour].
- optional, for beings of the water: "water": 0–1, how much it lives in water (0 on land, though it may swim across at the top; 0.5 both, on the bank and in the water: an otter, a crocodile, a seal, a frog folk; 1 only in water: a fish, a whale, an octopus, which never leaves it and suffers stranded), "depth": [lo, hi] metres under the surface it swims at (a carp [0.3, 2], a shark [1, 30], a whale [0, 60]; leave it out for one that swims at the top), and "move" "swim" its swimming speed (a carp 1, a seal 4, a dolphin 8). It eats by "diet" like any beast: a pike's meat is the fish smaller than it, a turtle grazes the sea grass.
- optional, for beings that keep odd hours or go after something: "active": "night" or "day" (owls, moths, fireflies; default always about), "want": what it goes after ("traveler", "anyone" or a species name), "touch": what its touch does to what it goes after ({ "glow": 0–1, "needs": { "fatigue": 0.3 } }), "shuns": properties it won't come near (["light"], ["wet"]), "moves_unseen": true for one that moves only while nobody watches, "signs": short lines of what a traveler notices when it is near but out of sight ("An owl calls.", "The air smells of wet fur."; never where it is: the game says that). Most species need none of these.
- optional "weather": how it feels about the weather, by word (a kind's name from "climate", what falls, "rain", "snow", "storm", "wind", "fog", "clear"), -1 (hates it, takes shelter) .. 1 (loves it, goes out in it): { "rain": 0.8, "storm": -0.9 } for a frog folk. Weather it doesn't name: it minds what falls, storms and gales. Optional "comes_with": weather it only comes out in (["rain"] for giant snails, ["fog"] for a fog wraith).
- optional "caves": true for a being that lives in caves, under the ground and out of the light (a bat colony, a blind salamander, cave crickets, a deep-dwelling troll): its home is a cave's floor and it keeps to the caves.
- optional "breathes": false for a being that never draws breath (a golem, a ghost, a creature born of the void): it lives where there is no air. Every other species breathes.
- optional "props": what its body has beyond a plain living body, by property name (a fire spirit { "heat": 600, "light": 1 }, a ghost { "heat": -5 }, a holy beast { "blessed": 1 } with one of this world's own properties); the world's rules then act on it. Most species need none.
- optional "tone", for a species that speaks words and doesn't sound like ordinary people (a character's own "tone" goes over it): numbers for how its voice sounds: "pitch" (Hz; a man about 110, a woman 200, a giant 70), "range" (pitch movement, 1 as people speak, 0.4 flat), "size" (the throat: 1 an adult's, 1.3 bigger and deeper, 0.8 smaller), "pace" (1 ordinary, 0.8 slow), and 0–1 "breath" (airy), "rough" (rasp, growl), "whisper" (breath without voice; over 0.75 a whisper only), "under" (a second voice an octave below it, like something speaking with it), "hollow" (as down a pipe or out of a barrel chest), "waver" (a slow shake) with "waver_hz". Leave out what is ordinary.
- optional "voice": how each of "sounds" sounds, one entry per sound in the same order, each a list of 1–4 parts played in turn: { "kind": "voice" (a throat: calls, growls, groans; "breath": 1 for breathing), "whistle" (a pure tone: birdsong, hoots, insects), "noise" (hiss, rustle, splash) or "knock" (clicks, taps, clatter), "hz": its pitch, or [start, middle, end] for a glide, "len": seconds, "times": repeats, "gap": seconds between repeats, "vowel": the mouth's shape, one to three of a e i o u m n (glided through), and 0–1 sliders "rough" (growl, rasp), "breath" (airy), "nasal" (horn, trunk, whine), "wobble" (vibrato), "swell" (slow start), "loud" }. Think what makes the sound, then use real pitches (a cat's meow about 500–800 Hz rising then falling, a wolf's howl 300–600 Hz, a crow's caw about 1500 Hz and rough, an elephant's rumble 15–30 Hz, a cricket 4–5 kHz chirps of 3–4 pulses). Each one is scaled to its own size by the game.
- attitudes: how peoples start out feeling about each other (-0.8..0.8); only the start, people's own history takes over. sizes: multiply a species everywhere ("everyone is a giant": "human": 1.8). traveler_height: the player's own height in metres (only if the world says they are small or big). looks: how a people looks in this world, slider ranges laid over its own; always give "human" one, true to where and when the world is (people of feudal Japan, of Lagos, of Norway or of a mixed port city look different), with these figure sliders: skin 0 pale … 0.5 brown … 1 deep brown; undertone 0 rosy … 0.5 neutral … 1 golden or olive; eyes (colour) 0 near black, 0.25 brown, 0.45 hazel, 0.6 green, 0.8 grey, 1 blue; eye_shape 0 round and deep-set under a strong brow … 1 narrower, a fold over the lid, a low flat brow; nose 0 narrow with a high bridge … 1 broad with a low bridge; hair_color and hair_style as for characters; height and build. Wide ranges where people are mixed, narrow where they are alike.

2. Then 4 to 8 base object types, each in its own ```js block, following the module rules exactly. These are scattered across the land by the scatter densities (trees, rocks, bushes, flowers, reeds…), so each must be small to medium (bounds under ~8 m) and varied per instance with hash(k.seed). Things of one patch or stand share k.a (0..1): use it for what a whole stand has in common (a flower field's colour, a grove's autumn tint, a reed bed's ripeness), and hash(k.seed) for each one's own variety; k.b is how deep in its patch it stands (0 at the edge .. 1 in the heart). Make flowers and tall grass a clump (several stems, 0.5–1.2 m across) so a field reads as a field. Give each the scatter tag it fills (e.g. "tree", "rock", "bush", "flower", "tallgrass") and fitting meta.props (trees and grass burn and are alive). Where the land has sea, lakes or rivers, make at least one of them grow in or by the water with meta.grows (reeds by the river, lilies on a pond, kelp, sea grass or coral in the sea), and put its tag in the scatter of the biomes it grows in (rivers cross every biome, so a bank plant goes in the land biomes').
"#;

pub const REGION_TASK: &str = r#"Plan the story layer of one region (256 m × 256 m). Reply with one ```json block:
{
  "name": "region name (2–3 words)",
  "mood": "one line",
  "facts": ["short lore lines that people living here know", "..."],
  "new_types": [ { "name": "…", "description": "what it looks like, materials, colours", "size_m": [w, h, d], "tags": ["building"], "props": { "burns": 0.4 } } ],
  "landmarks": [ { "type": "type name", "x": 0-256, "z": 0-256, "rot": degrees, "scale": 1.0, "why": "why it is here" } ],
  "settlement": { "name": "…", "x": 0-256, "z": 0-256, "buildings": [ { "type": "type name", "dx": metres, "dz": metres, "rot": degrees, "height": 0 } ],
                  "walkways": [ { "from": [dx, dz, height], "to": [dx, dz, height], "width": 1.2, "look": "rope" | "timber" | "stone" } ],
                  "road": { "color": [r,g,b], "width": 2.5 } or null, "lamps": "type name" or null,
                  "ground": [ { "what": "canal", "name": "…", "line": [[x, z], [x, z]], "width": 7, "how": "cut", "height": -1.6, "sides": 0.3,
                                "color": [r,g,b], "rim": [r,g,b], "rim_width": 1, "frontage": { "type": "type name", "sides": "both", "every": 11 } } ],
                  "bridge": "type name" or null, "vehicle": "type name" or null },
  "things": [ { "type": "type name", "near": "a character's name, or \"\" for the middle of the settlement" } ],
  "characters": [ {
      "name": "…", "age": 30, "appearance": "…",
      "look": { "height": 1.75, "build": 1.0, "skin": 0.0-1.0, "shape": 0-1, "hair": 0-1, "hair_color": 0-1, "hair_style": 0-1, "beard": 0-1,
                "undertone": 0-1, "eyes": 0-1, "eye_shape": 0-1, "nose": 0-1,
                "top": 0-1.2, "top_shade": 0-1, "top_cover": 0-1, "sleeves": 0-1, "neckline": 0-1, "bottom": 0-1.2, "bottom_shade": 0-1,
                "skirt": 0-1, "legs": 0-1, "shoes": 0-1, "hat": 0-1, "trim": 0-1.2, "underwear": 0 or 1 },
      "personality": "…", "goals": "…", "voice": "how they speak", "tone": { },
      "aims": [ { "text": "win Mara's heart", "want": { "kind": "affection", "with": "Mara", "at_least": 0.6 } } ],
      "traits": { "sociable": 0-1, "playful": 0-1, "curious": 0-1, "brave": 0-1, "generous": 0-1, "crafty": 0-1 },
      "home": "where they live", "home_x": 0-256, "home_z": 0-256,
      "relationships": ["Name: relation"], "species": "human", "variety": "", "layers": [] } ],
  "creatures": [ { "species": "a species listed below", "count": 4, "x": 0-256, "z": 0-256, "names": [], "owner": "Name of their person, or empty", "description": "…" } ],
  "new_species": [ ],
  "varieties": [ ]
}
Rules:
- Coordinates are local to the region: x and z from 0 to 256.
- 0–3 landmarks, 0–1 settlement (with 1–8 buildings), 0–6 characters, 0–8 new_types. Empty regions are fine sometimes: wilderness has value.
- Reuse existing types by exact name when they fit; only invent new_types the region really needs (a settlement needs at least one building type).
- Buildings are places, not props: those its people live and work in can be entered, with rooms, doorways sized for them (the folk height below), stairs to upper floors, and things inside where they belong (a smith's tongs on the bench, a sword on its rack, a pot on the shelf): describe them in the building's description ("inside: a hearth, a bed, a rack holding a sword") and give those things as new_types too (under 1 m, tags ["item"], with "tool" in the description when they are worked: a sword, an axe, a spade). A building can be set into the ground (say "set into the ground" in its description, and how deep its floor lies): a home dug into a hillside or a cliff face with its door to the open side, an earth house or a bunker under flat ground with a way dug down to its door, a home under a hatch in the ground with a ladder down, a well. Give its doorway a door that suits it by name (a round green door, an iron hatch, a stone slab) and the world writes it. A people that lives under the ground (a world whose surface is deadly, a dwarf hold, mole folk) builds its homes so, and its people's days are spent below.
- Up off the ground: a building, landmark or thing may stand "height" metres above the ground, on something placed before it that holds it up there (a platform in a great tree, a ledge, stilts, a roof); list what holds it first. A people of the trees: the great trees themselves are buildings or landmarks (a trunk wide enough to hold a room, a platform round it, a ladder or stair up to it: say so in their description), their huts set on the platforms by height, and walkways between the platforms. walkways: ways off the ground between two points ("from"/"to": dx, dz from the settlement's middle and the height above the ground there; each end rests on what stands there at that height, or on the ground at height 0): rope bridges between trees, a boardwalk along a cliff, stone ways between towers. Give every platform a way up from the ground. Beings too small for a door (mice, sprites) live in closed houses no traveler fits into; say so when it is so.
- road: how the ways between the settlement's doors look (cobbles, packed earth, boardwalk), or null for none (a camp, a wild hamlet); lamps: a type that lights them at night ("lamppost" exists; or a new_type: a torch post, a paper lantern pole), or null if this people has none.
- ground: how the settlement shapes its land, in order (each works on what the ones before left): harbours and lagoons dug, islands, terraces and dykes built up, canals and moats cut, streets and squares laid. Each has one shape: "line" (points, with "width" in m), "circle" [x, z, r], "rect" [x, z, w, d, degrees] or "outline" (points); "how": "cut" (down to height), "fill" (up to it), "level" (make it that height) or "paint" (only cover it); "height" in metres (0 is the sea's level: whatever is cut below it fills with the sea, where the land has one; leave it out for a street or square that follows the land); "sides": metres of slope (0.3 a walled quay, 2 a bank, 10 a shelving shore); "color": what covers it (cobbles, gravel, flagstones, a canal's murky bed); "rim" and "rim_width": edging along its top (quay stones, kerbs); "water": metres, only for water standing above the sea (a mill pond, a canal on a hill); "frontage": buildings of a type lined along a line, doors toward it ("sides": "both", "left" or "right"; "every": metres apart). The ground is real: people walk only where it is dry and boats go only on water, so give people ways and bridges. Where a street crosses a cut a bridge is built ("bridge": a type for them, or null for stone or timber arches). Most settlements need no ground, or only a square; a city of canals, a harbour town, a moated keep, a terraced hill village or a causeway over a marsh need it. A hollow dug into the land (a cave dwelling, a mine, a cellar in a hill, a crypt, a smugglers' tunnel) is a ground entry too: { "what": "cave", "name": "…", "mouth": [x, z], "toward": degrees (0 = +z, 90 = +x, into the hill), "length": metres of passage, "depth": metres it goes down, "room": metres across its chamber }; its mouth is a way in from the surface, and things whose "near" names it lie inside. Streets that follow the land and buildings along them make a town feel built rather than scattered.
- vehicle: the one vehicle a settlement's people share, fitting the place and era (a gondola or a rowing boat where there is water, a cart, a carriage, bicycles, a sled, a sedan chair), a type with meta.drive ("boat" exists), or null for a people with none. Anyone may take it.
- "bench" exists: set benches as things where people sit and wait (a square, a quay, a landing, a stop on a way).
- Put things on dry land (see the terrain notes), settlements on gentle ground, landmarks where they would be seen. A people of the water (a species that "lives in water", or lives "on land and in water") may settle in it: a town on a flat bed under the water (see the terrain notes), stilt houses in the shallows, huts on the shore. Give every building and thing meant to stand in water the tag "water" (a sunken town's houses, a stilt house, a pier, a coral shrine), and say in its description how it suits the water. Their people's homes are in the water too; under it a settlement needs no road or lamps (null), and its vehicle is null or one that goes on water.
- Names: as the people of the place call each other, mixed: mostly a first name or a nickname alone (Tomi, Nonno Beppe, Old Mags), now and then a full name, a title or a name with a trade or place (Don Anselmo, Ada of the lighthouse), never everyone with a first and last name. Each name is its own: never one already living in the land; when a people's short names are taken, use a longer form (a surname, a nickname, a family or trade name).
- Give each character a trade or daily work in "goals" (what they make, mend or tend, and something they want to make or improve), e.g. "mends the fishing nets; wants to build a proper boat".
- "aims": one or two things they are after, as goals; where the world can check it, a "want": {"kind": "hold", "what": "a proper boat"} (have it in hand), {"kind": "has", "who": "Ola", "what": "a doll"} (someone else has it), {"kind": "be", "place": "the shrine"}, or {"kind": "affection", "with": "Mara", "at_least": 0.6}; otherwise words only.
- Characters live near the settlement or a landmark. home_x/home_z is where they stand by day: a spot a few metres outside their house (never the building's own coordinates). Give them distinct voices ("tone": the numbers of a species' "tone", only for one whose voice is out of the ordinary: a booming giant of a man, a hoarse old smuggler, a breathy whisperer; else leave it empty), goals and relationships with each other ("Name: relation", e.g. "Ola: daughter", "Bren: rival", "Tam: husband"): families, couples, friends and rivals make a village come alive. Weave in the region facts and the neighbouring regions.
- things: small loose things lying about where people live and work, that they pick up, use, play with and make other things from. Give each character 1–2 things of their trade (what they work with or on: a fisher's net and a basket of fish, a smith's tongs and an iron bar, a weaver's spindle and a bundle of wool), "near" them. Give a settlement 1 pastime thing its people toss, catch, kick or roll about for fun, in their own style (a leather ball, a hoop, a straw doll; an orc camp might toss a skull), "near": "", with props { "toy": 1 } and a light mass. Each is under 1 m and 25 kg; at most 12 things. Reuse existing types by exact name when they fit (a trade's tools often repeat); new ones go in new_types with fitting props and tags.
- Characters are people by default; give "species" (one of the species listed below) for anyone else who talks and plans (an elf, an orc). Their "look" uses their body's sliders.
- A person's "look" is their body, face and clothes, and must match their "appearance" words. Give them the face, skin and hair of the people of their place, unless their words say they come from elsewhere: skin 0 pale … 0.5 brown … 1 deep brown; undertone 0 rosy … 0.5 neutral … 1 golden or olive; eyes (colour) 0 near black, 0.25 brown, 0.45 hazel, 0.6 green, 0.8 grey, 1 blue; eye_shape 0 round and deep-set under a strong brow … 1 narrower, a fold over the lid, a low flat brow; nose 0 narrow with a high bridge … 1 broad with a low bridge. shape: 0 a man's build … 1 a woman's. hair: 0 shaven, 0.2 cropped, 0.5 to the chin, 0.75 the shoulders, 1 the waist; hair_color: 0 black, 0.25 dark brown, 0.45 brown, 0.6 auburn, 0.75 blond, 0.9 flaxen, 1 grey or white (the old go grey); hair_style: 0 straight, 0.5 curly, 0.8 tied back or braided; beard (men): 0 none … 1 long. Clothes are slider cuts in colours: top_cover 0 bare-chested, 0.2 a band across the chest, 0.4 cropped, 0.6 to the waist, 0.8 the hips, 1 a tunic to mid-thigh; sleeves 0 none … 1 the wrist; neckline 0 a high collar, 0.35 round, 0.7 a deep V, 0.9 off the shoulders; skirt 0 none, 0.2 short, 0.5 the knee, 0.8 the calf, 1 the floor (with a top, a dress or a robe); legs (trousers) 0 none, 0.25 shorts, 0.55 the knee, 1 the ankle; shoes 0 barefoot, 0.2 sandals, 0.45 shoes, 0.7 ankle boots, 1 knee boots; hat 0 none, 0.2 a kerchief, 0.4 a cap, 0.6 a brimmed hat, 0.8 a wide straw hat, 1 a tall hat; underwear 1 (0 for none). Colours: top, bottom and trim (belt, hems, collar, shoes, hat) are hues round the wheel (0 red, 0.08 orange, 0.15 yellow, 0.33 green, 0.5 cyan, 0.66 blue, 0.8 purple; 1–1.2 fades to no colour) with top_shade and bottom_shade from 0 dark to 1 light (undyed linen: hue 1.2, shade 0.8; black: shade 0).
- Dress everyone as their place, era, climate, work and wealth would, and give them some style of their own: a fisherwoman with sleeves rolled and a kerchief, a dandy in a tall hat, a smith bare-armed. Show skin where the world would (heat, a beach, a bathhouse, labour, a culture that does), cover it where it wouldn't. What they wear comes off and goes back on in play.
- creatures: 0–4 groups of beings that don't talk (herds by farms, a dog or cat with its person, wild packs, a beast in its lair), from the species listed below. Pets and working animals name their "owner" (a character of this plan). Wilderness may have wild herds or predators; villages, pets and livestock. Those that live in water go where the water is (a school in the bay, a pike under the bridge, carp in the moat, seals on the rocks by the sea): give x, z in it.
- Everyday clothes of people (tops, dresses, robes, trousers, shoes, hats) are their look sliders, not layers. Gear that the sliders can't show (armour, a cloak, an apron, a sash, a saddle or collar for an animal) are layers: new_types with tags ["layer"], "fits": the body they are worn on ("figure" for people, "quadruped" for four-legged animals, or a species' own body) and their props (armour { mass: 12 }, a wool cloak { burns: 0.7, mass: 2 }). A character's "layers" lists what they wear (at most 3, by type name).
- varieties: what a people looks like in this place, when it differs: { "species": "human", "name": "warrior", "look": { "build": [1.1, 1.35] }, "layers": ["iron breastplate"] }. Give a settlement a "variety" to dress and shape everyone living there (a warrior village, monks in robes, hill folk, a quarter of traders from over the sea with their own skin, eyes and nose); a character's own "variety" overrides it.
- new_species: at most one, only if this region really holds a people or beast the universe doesn't have yet, in the species format of genesis (with "body_description" if no existing body fits).
"#;

pub const DRESS_TASK: &str = r#"People in a small simulated world are drawn with faces, hair and clothes set by numbers. From each one's words (and the world they live in), set theirs. Reply with one ```json block: { "<id>": { "shape": 0-1, "hair": 0-1, ... }, ... } for every person given.
- shape: 0 a man's build … 1 a woman's (from their name, pronouns and words). hair: 0 shaven or bald, 0.2 cropped, 0.5 to the chin, 0.75 the shoulders, 1 the waist; hair_color: 0 black, 0.25 dark brown, 0.45 brown, 0.6 auburn, 0.75 blond, 0.9 flaxen, 1 grey or white (the old go grey); hair_style: 0 straight, 0.5 curly, 0.8 tied back or braided; beard (men): 0 none, 0.2 stubble, 0.5 short, 1 long.
- Clothes are cuts in colours: top_cover 0 bare-chested, 0.2 a band across the chest, 0.4 cropped, 0.6 to the waist, 0.8 the hips, 1 a tunic to mid-thigh; sleeves 0 none … 0.3 short … 1 the wrist (rolled sleeves: 0.5); neckline 0 a high collar, 0.35 round, 0.7 a deep V, 0.9 off the shoulders; skirt 0 none, 0.2 short, 0.5 the knee, 0.8 the calf, 1 the floor (a top with a skirt is a dress or robe); legs (trousers) 0 none, 0.25 shorts, 0.55 the knee, 1 the ankle; shoes 0 barefoot, 0.2 sandals, 0.45 shoes, 0.7 ankle boots, 1 knee boots; hat 0 none, 0.2 a kerchief or headscarf, 0.4 a cap, 0.6 a brimmed hat, 0.8 a wide straw hat, 1 a tall hat; underwear 1 (0 none).
- Colours: top, bottom (skirt and trousers) and trim (belt, hems, collar, shoes, hat) are hues round the wheel: 0 red, 0.08 orange or brown, 0.15 yellow, 0.33 green, 0.5 cyan, 0.66 blue, 0.8 purple; 1 to 1.2 fades to no colour (grey, white, black). top_shade and bottom_shade: 0 dark … 1 light (undyed linen: hue 1.2, shade 0.8; black: shade 0; a leather apron or brown wool: hue 0.07, shade 0.3).
- Follow their words exactly where they say (a grey beard, a red kerchief, sleeves rolled to the shoulder, a straw hat) and dress the rest as their place, era, climate, work and wealth would, with some style of their own. Show skin where the world would (heat, labour, a beach, a culture that does) and cover it where it wouldn't."#;

pub const TYPE_TASK: &str = "Write this object type as one ```js block containing the complete module.";

/// Extra rules for a layer: it is drawn in the body's frame and pose.
pub fn layer_note(body: &str, body_source: &str) -> String {
    format!(
        r#"This is a layer (clothing, armour or gear) worn on the "{body}" body: add `fits: "{body}"` to meta and keep "layer" in its tags. It is drawn in exactly the body's local frame with the same k (look sliders k.a … k.e then k.g … k.z, walk phase k.f, pose roles k.s0 … k.s7), so follow the body's own maths for where its parts are in every pose (scale with its height, lean with its torso, swing with its legs). Make the layer a thin shell just outside the body's surface where it covers it (about 1–3 cm), not a solid block, and only where it is worn. Its bounds should match the body's bounds.
The body's code:
```js
{body_source}
```"#
    )
}

/// The body rules every body follows (written new, from a template, or
/// reshaped).
const BODY_RULES: &str = r#"A body is an object type that beings live in. Besides the usual module rules it has `meta.body` and its shape moves with a pose:
- Draw it at its natural size in metres, standing on y = 0, facing +z. Give it the tags ["body"].
- meta.body = { height: m, eye: m (eye height), radius: m (footprint radius for walking), reach: m, grip: [x, y, z] (where it holds a thing: the right hand at rest, a mouth or a claw), seat: [x, y, z] (only if it can be ridden: where a rider sits), roles: [...], gait: "biped" | "quad" | "slither" | "hover", flies: true|false, arms: true|false (two hands that hold things), look: { name: [lo, hi], ... }, dead: { role: amount, ... } (optional: the pose it lies in dead, when its stance would not lie flat on its side or back; e.g. a hunched body { lean: -0.3 }) }
- gait says how its walk phase runs: "biped" and "quad" step (one cycle of k.f is about one stride), "slither" sends a wave along the body (a snake, an eel, a worm, a fish swimming with its tail), "hover" drifts and bobs even at rest (a wisp, a jellyfish, a ghost). A fish, a seal or a whale lies along +z as it swims, its belly near y = 0.
- Pose roles, read as k.s0 … k.s7 (each 0 at rest; give each the meaning that fits this body and list only the ones it answers to in roles): k.s0 / k.s1 raise_l / raise_r (raise a left / right limb or wing, 0 … 1), k.s2 / k.s3 reach_l / reach_r (reach forward or wrap around someone, 0 … 1: a hug), k.s4 lean (bend forward, radians -0.3 … 0.6), k.s5 head (head down, -1 … 1; negative raises it: an elephant lifting its trunk, a horse throwing its head up), k.s6 crouch (0 standing … 1 lying down; a snake coils), k.s7 spread (tail, wings, frill or hood, -1 … 1: a wag, wings out, a fluffed tail). Use "raise" / "reach" in roles for both sides. Every listed role must visibly move the shape.
- Bodies with hands, a mouth that carries, or a head add `export function parts(x, y, z, k)`: x is a part's number and it returns where that part is, `vec3(px, py, pz)`, in this pose, by the same maths as sdf() (0: where it holds a thing, the right hand, a mouth or a claw; 1: the other hand; 2: the middle of the head; 3: the middle of the chest or back). What it holds goes in that hand and what it wears follows those parts, however its limbs are made.
- k.f is its walk phase in radians (swing legs, undulate a tail or a whole body with sin(k.f)).
- look: up to 25 sliders, read as k.a … k.e then k.g … k.z in that order ([lo, hi], or [lo, hi, usual] for one most beings share, like what they wear), each between its lo and hi (colours, horn length, proportions); vary them per being. Use hash(k.seed) for small details. For a coat, hide or skin colour use "hue": [0, 1.2] (round the colour wheel; above 1 fades to grey) and "shade": [0, 1] (black … white), as the quadruped does, so deeds can colour any being the same way.
- Make it read as what it is from a few metres away at low resolution: the silhouette carries it (ear shape, snout length, tail thickness, neck, how it stands).
- Keep it cheap: about 12–20 primitives. Stay inside meta.bounds in every pose."#;

/// Writing the body of a species: a type with `meta.body` that answers to
/// pose roles, drawn at its natural size. `template`: an existing body (name,
/// code) to start from, or to learn the contract from.
/// `derived`: the body is that template's own kind (a cat's from the
/// quadruped), not just shown it as an example.
pub fn body_task(name: &str, description: &str, species: &str, template: Option<(&str, &str)>, derived: bool) -> String {
    let template = match template {
        Some((tname, src)) => {
            let line = if derived { format!(" Add `from: \"{tname}\"` to meta.body.") } else { " Leave `from` out of meta.body: this body is its own kind.".to_string() };
            format!(
                "\n\nAn existing body, \"{tname}\", as an example of the contract (how roles, sliders and the walk phase move a shape). It is an example, not a limit: keep what fits this species and change anything that doesn't (head, ears, snout, tail, neck, legs, how many limbs, proportions, colours), so it looks like a {name} and nothing else.{line}\n```js\n{src}\n```"
            )
        }
        None => String::new(),
    };
    format!(
        r#"Body type to write: "{name}"
What it looks like and how it moves: {description}
The species that lives in it: {species}

{BODY_RULES}{template}

Write it as one ```js block containing the complete module."#
    )
}

/// Reshaping one being's body: its body's code and the change. The roles
/// stay, so its gestures and walk keep working.
pub fn body_reshape_task(name: &str, from: &str, source: &str, change: &str, roles: &[String]) -> String {
    format!(
        r#"Reshape a body: this being's own body from now on, written from the body "{from}" below.
The change: {change}

Rewrite the module with the change made clearly visible at low resolution (a fluffy tail is much thicker and rounder, pointed ears are tall triangles; exaggerate a little), and everything else as it was. Name it "{name}" and add `from: "{from}"` to meta.body. Keep the frame (standing on y = 0, facing +z), the size, the look sliders (their names and order) and k.f. It must still answer to every role it has now ({roles}), moving the changed parts with them where they belong (a tail with spread, a head part with head). Grow meta.bounds if the change needs room.

{BODY_RULES}

The body now:
```js
{source}
```

Write it as one ```js block containing the complete module."#,
        roles = roles.join(", ")
    )
}

/// Rewriting one thing's shape: the current module, the change, and where.
/// `with`: another thing to work into it (name, module, its size relative to this one).
pub fn edit_task(name: &str, source: &str, change: &str, spot: &str, cuts: &[[f32; 4]], with: Option<&(String, String, f32)>) -> String {
    let spot = if spot.is_empty() { "not given".to_string() } else { spot.to_string() };
    let cuts = if cuts.is_empty() {
        String::new()
    } else {
        let list: Vec<String> = cuts
            .iter()
            .map(|c| if c[3] > 0.0 { format!("a sphere of radius {:.2} at ({:.2}, {:.2}, {:.2})", c[3], c[0], c[1], c[2]) } else { format!("a cube of half-size {:.2} at ({:.2}, {:.2}, {:.2})", -c[3], c[0], c[1], c[2]) })
            .collect();
        format!("\nPieces already cut out of it (subtract these too, so they stay): {}.", list.join("; "))
    };
    let with = match with {
        Some((wname, wsrc, rel)) => format!(
            "\n\nWork this other thing into it, at the touched spot: a {wname}. In the object's units it is {rel:.2} times the size its own module draws it, so scale its shapes by {rel:.2}. Take its shape and colours from its module:\n```js\n{wsrc}\n```"
        ),
        None => String::new(),
    };
    format!(
        "Change this existing object. Here is its current module:\n```js\n{source}\n```\n\nThe change: {change}\nWhere it was touched (in this module's own coordinates, the ones sdf receives): {spot}{cuts}{with}\n\nRewrite the whole module with the change made. Keep everything else the same: the same coordinate frame and origin, the same look and colours, roughly the same bounds (grow them only if the change needs it), the same props and tags. Set meta.name to exactly \"{name}\". Reply with one ```js block containing the complete module."
    )
}

pub const CREATE_TASK: &str = r#"Someone is making something in the world by asking for it. You see what they see (JSON below). Decide what to build and where.

Reply with one ```json block:
{
  "summary": "short past-tense description, e.g. a lighthouse on the hill",
  "reuse": "existing type name, or null to write a new type",
  "placements": [ { "right": metres, "forward": metres, "rot": degrees, "scale": 1.0 } ]
}
and, if "reuse" is null, one ```js block with the new type module.

- Placement offsets are relative to the target point (where the centre of the view meets the ground): "right" is to the viewer's right, "forward" is further away from the viewer. Most requests need one placement at {right: 0, forward: 0}. For "a ring of stones" etc. use several placements.
- rot = 0 makes the object's front (+z) face away from the viewer; rot = 180 faces the viewer.
- Objects are set on the ground automatically. Only add "lift": metres if the object must deliberately hover.
- Keep the player's position free: the target may be close.
- Give a new type fitting meta.props (a ball bounces, bread is edible, a lamp gives light, a wooden thing burns), and "strange" when it doesn't belong in this universe (judge by the universe, not by our world). Small things (under ~1 m) can be picked up and used.
- Something people ride in or on and steer (a cart, a car, a boat, a sled, a flying carpet) needs meta.drive and a seat anchor; the traveler gets in with e and drives it.
"#;

pub fn repair(errors: &str) -> String {
    format!("That failed validation:\n{errors}\n\nFix every problem and reply again in the same format, with the complete corrected answer.")
}

pub const DIALOGUE_RULES: &str = "You are a character in Infinite Pocket, a small living world. Stay in character. Speak in your own voice, in 1–3 short sentences (this is a terminal; keep it brief). No stage directions, no lists, no markdown. You remember earlier conversations with the traveler (the player) from your memories below; refer to them naturally when relevant. Never state clock times; speak of when things happened loosely, as a person would (\"just now\", \"earlier\", \"yesterday\"). You only know what your character would know. If you are asked about things outside your world, respond as your character would. If you agree to do something, say you will do it or are starting on it; it isn't done yet.";

pub const DECIDER_TASK: &str = r#"You decide what a character in a small simulated world does next, given an event and what they know. Reply with one JSON object only:
{"goal": "a few words", "say": "what they say now (one short sentence in their voice) or null", "steps": [ ... ]}
Steps are actions, carried out in order (walking there first when needed). Use names of things and people you were told about:
  {"do": "goto", "target": "Mara"}            {"do": "hold", "target": "ball"}        {"do": "drop"}
  {"do": "throw", "at": "hoop"}                {"do": "give", "to": "Ola"}             {"do": "eat", "target": "apple"}
  {"do": "use", "target": "lantern", "on": "woodpile"}                                {"do": "say", "text": "…", "to": "Ola"}
  {"do": "gesture", "kind": "wave|bow|nod|point|cheer|shrug|dance|sit|handshake|high_five|hug|kiss", "to": "Ola"}
  {"do": "propose", "to": "Ola", "activity": "catch|carry|dance|walk|hug|…", "with": "ball"}   (doing something together)
  {"do": "ride", "target": "gondola"}   (get into a boat, cart or carriage, or onto a horse; or sit on a bench or chair; then goto drives it there, or rides along if someone else drives)   {"do": "dismount"} (get out, get up)
  {"do": "do", "text": "carve a notch in the door"}   (anything else, in words, including making something new: "make a wooden ball"; only when it really fits who they are. Food and goods come from what gives them: "pick a lemon from the lemon tree", "bake bread in the oven", "take cheese from the larder", "fish off the quay with the rod")
  {"do": "follow", "target": "the traveler"}  {"do": "wait", "secs": 5}              {"do": "go_home"}   {"do": "flee", "from": "Bo"}
  {"do": "open", "target": "door"}   {"do": "close", "target": "gate"}   (doors, gates, lids, trapdoors)
  {"do": "pet", "target": "Rex"}   (stroke or scratch an animal; one that trusts them likes it)   {"do": "hold", "target": "Mog"} picks up a small animal that lets them
  {"do": "swing", "at": "wolf"}   {"do": "chop", "at": "pine"}   {"do": "dig", "at": "garden"}   (work the tool they hold: a blade swings, an axe chops, a spade digs; it can hurt)
  {"do": "shoot", "at": "deer"}   (with a bow, a sling or a gun in hand, from where they stand; it can kill)   {"do": "load", "target": "arrow"}   (put ammunition lying about into what they hold)
  {"do": "throw", "at": "wolf"}   (throw what they hold at someone: a stone bruises, a spear or a knife wounds)   {"do": "light", "target": "dynamite"}   (light something that goes off; then throw it, quickly)
  {"do": "apply", "with": "wet cloak", "to": "burning hut"}   (work something against something for a few seconds: beat out flames, press, rub, smear; without "with", their own hands; without "to", the nearest trouble)
  {"do": "ask", "who": ["Rosa", "Ben"], "for": "a grill"}   (ask people, best first, one at a time, to make or give you something; they may say no)
Optional, in the same object: "promise": {"text": "make Ola a ball", "to": "Ola", "what": "a ball" or null (the thing they will hand over), "within_hours": 3} when they agree to do something for someone (they are held to it); "aim": {"text": "a few words", "within_hours": 24} for a longer aim of their own; "kept": true or false when asked whether a goal that came due was met.
When something surprises them (they are told how much, 0–1), react as they would: a little, a glance or a word; a lot, drop what they are doing to go and look, call out, fetch someone, or back away if they are timid. Something that appeared where the traveler stands may be the traveler's doing.
When they fear someone near (they are told how, and what that one holds), act as they would with their life at stake: plead, back away, flee, hide at home, warn others, or, if brave, stand their ground. When they come upon someone dead, react as they would to that death: to the death of someone dear, grief or panic; to a stranger's, shock, and fetching or warning others.
They are told what they could do now, best first: what is really there. Pick from it, or anything else that fits who they are. Keep plans short (1–5 steps), in character, and grounded in what is actually around them. If nothing is worth doing, reply {"goal": "", "steps": []}.
For an event "player_near", a plan may simply be [{"do": "goto", "target": "the traveler"}] with "say" set, or nothing."#;

pub const SUMMARY_TASK: &str = "Update this character's private memory summary. Write at most 120 words in the first person, covering what they know and feel about the traveler (the player), promises, recurring topics, and notable things they witnessed. Keep the important older points. Plain text only.";

pub const INTERPRET_TASK: &str = r#"You are the physics and common sense of a small simulated world. Someone says, in words, what they do or make. Decide what happens, as changes to things, not just words. Reply with one JSON object only:
{
  "narration": "one or two sentences: what visibly happens",
  "changes": [ { "target": "held" | "target", "props": { "<property>": number }, "state": { "s0": number } } ],
  "create": [ { "name": "new thing", "replace": "held" | "target" | null, "description": "looks, materials", "size_m": [w, h, d], "props": { "<property>": number } } ],
  "remove": [ "held" | "target" ],
  "make": [ { "text": "what to make and where, in words, e.g. a stone well by the path" } ],
  "cut": [ { "target": "target", "size_m": 0.25, "shape": "round" | "square" } ],
  "ground": [ { "depth_m": 2, "wide_m": 3, "long_m": null, "sides": "sheer" | "sloped", "way": "" | "canal" | "road" | "causeway" | "tunnel" | …, "color": [r,g,b] or null, "fill": false } ],
  "reshape": [ { "target": "target", "name": "a new name for the changed thing, different from its current name, e.g. slate cottage with the door open", "change": "what changes about its shape, precisely", "with": "held" | null } ],
  "being": { "needs": { "hunger": -0.5 }, "feel": { "affection": 0.1, "trust": 0.1 }, "look": { "<slider>": number }, "grow": 1.5 | null, "wear": [ { "name": "red cloak", "description": "…", "props": { "burns": 0.7 } } ], "take_off": [ "helmet" ], "learn": "sit" | null, "become": "toad" | null, "reshape": "what changes about its body's shape" | null, "hurt": 0-1 | null, "how": "what the hurt was" | null, "on_self": true | false },
  "beings": [ { "species": "dog", "name": "…", "description": "…", "look": { "<slider>": number }, "size": 1.0 } ],
  "say": "a few words the actor says, or null",
  "needs": { "kind": "place" | "thing" | "someone" | "time", "what": "…", "hour": null } or null (only for a character, when it can't be done as things are),
  "weather": { "name": "a short name", "clouds": 0-1, "wind": 0-1, "fog": 1-4, "storm": 0-1, "cloud_color": [r,g,b] or null, "tint": [r,g,b] or null, "falls": { "what": "rain", "amount": 0-1, "color": [r,g,b], "look": "streak" | "flake" | "mote", "props": { "<property>": number } } or null, "feel": "a few words", "hours": game hours it lasts } or null,
  "source": false,
  "cache": true
}
- "held" is what the actor holds; "target" is what the middle of their view points at (a thing with its distance, or the ground or a far point). Use only property names from the list. Set "cache": false if the result depends on chance or the moment. A "note" in the context, when there is one, says what the engine needs settled about this deed; follow it.
- Prefer small, plausible results. Things can be made from things (carving wood makes a carving; replace the wood). Materials worked into something new are used up ("replace" one, "remove" any other); tools used to work them are not. A made thing can be made into something else again. If nothing would happen, say so in the narration and change nothing.
- The world only holds things about the size of a cat and up. Smaller inputs that are common in this world (ingredients, tortillas, foil, thread, nails, paint, seeds, small tools) are never there to see but are always at hand: never refuse or fail for lack of them. Rare or special small things (a phoenix feather, a dragon's scale, a royal seal) are not at hand. Real things that are there (the held thing, the target, wood, stone, a building) are used when the words name them.
- "actor_is" says who acts. The traveler makes what they ask outright, from nothing if need be: a burrito, a lantern, a tree, a house. Always do it, in the universe's own style, and never refuse it.
- A character follows the world's laws: they can make or do what someone like them ("actor_about") could, in this world, as things are. When it can't be done as things are, change nothing, say why in the narration, and name the one thing it needs in "needs":
  - "place": where the work is done (cooking needs a grill or kitchen, forging a forge, a blessing an altar)
  - "thing": something real it is made from or needs (a big log for a canoe, a rare small thing, a key)
  - "someone": a person with a skill they lack (a herbalist for a potion, a smith for a blade)
  - "time": an hour it can only be done at (at night, at dawn); give "hour" 0–23
  If something in "around" fits (a taco truck has a grill, a smithy has a forge), give its exact name from the list, without the distance or tags; otherwise say what it is in a few words. Nearby is fine: the character walks there.
- Decide from the words whether they change what they point at or make something new; the target is only a hint. "Make a stool", "a lighthouse on that hill", "build a fence here" are "make" (it is placed where they look), even when a thing is pointed at. "Fix the roof", "open it", "remove the cover", "add this stick to the wall" change the target. "Carve a bowl from this log" makes from it: "create" with "replace": "target". "make" is for things that stand in the world (buildings, furniture, structures, landmarks); "create" is for small loose things that come from the deed (a piece that comes off, a carving, a spark).
- The world only shows what your changes do, never what the narration says. If the target visibly changes form (something on it is removed, opened, broken off, added, bent, dug), you must change its shape with "cut" or "reshape"; props alone change nothing you can see.
- Changing a thing's shape happens where it was touched ("touched_at", in the thing's own coordinates):
  - "cut": take a piece out (punch a hole, dig, bite, chip, carve a notch). size_m is the radius in metres; a cut deeper than a wall is thick goes right through. It shows at once; the engine places it, you only say how big.
  - "reshape": change the shape itself: remove a part of it (the door cover, a roof tile, a branch), open or bend it, make the roof a dome, add a chimney. With "with": "held", work the held thing into it (add the stick to the wall, mount the wheel on the boat, hang the lantern on the post): it becomes part of the target and is used up. Describe the change precisely, including where.
- "ground" digs into the land itself where they point (never "cut" for the ground): a hole, a pit, a well shaft, a crater, a pond's bed, a ditch or trench ("long_m": its length, along the way they look), a cave's mouth dug into a hillside or rock face. depth_m and wide_m in metres; "sheer" sides stand straight (a pit, a shaft), "sloped" makes a bowl anyone can walk down into and out of (a crater, a hollow, a quarry). Dug below the sea or the ground water it fills with water up to it. "fill": true makes a hole hold water to its brim (a pond, a lake, a pool, a moat round a keep): the water rises in it until it would spill over the lowest point of its rim, so it holds water anywhere, a lake on a mountain too (dig it wide and give it "fill"). With "way" it is laid along the land from the actor the way they look (or to where they point), "long_m" long: a canal, ditch or moat is cut "depth_m" deep (joining a river or the sea it fills with its water), a road, path or street is laid level along the land in "color", a causeway, dyke or embankment is built up "depth_m" high. "way": "tunnel" (or mine, burrow) digs a passage into the ground "long_m" long, going down "depth_m" to a chamber "wide_m" across: a real hollow under the land, with a roof, that people walk into. A character only digs or lays what one person could in a go (a few metres; they come back to do more).
- Taking a piece off a thing is two changes: "reshape" the target without the piece, and "create" the piece as a new thing (it lands nearby).
- Some things hold or give many of something and don't run out: a fruit tree or berry bush gives a fruit, an oven or a kitchen with a cook gives what is baked or cooked, a fridge, larder, cellar or barrel gives what is kept in it, a stall or shop gives what it sells, a hive gives honey, a well gives water, the sea or a river gives a fish to someone fishing with a rod or net. Taking or making one from such a thing is "create" alone, with "source": true and no change to the target's shape.
- "weather" is for deeds that change the sky over the whole land (make it rain, clear the clouds, call a storm, a sky of falling petals): it comes at once and holds for "hours" (1–24), then the land's own weather returns. "weather" in the context is what it is now. "falls" is anything coming down, with what it does to what it lands on ("props", e.g. rain { "wet": 1 }); "look": "streak" for rain, "flake" for ash or petals, "mote" for drifting specks. Fog and mist are the "fog" value, not something that falls. The traveler can always do it; anyone else only through this world's own magic.
- When the target is a person or creature, use "being" (never the top-level "cut" or "reshape" on them): "needs" are added to what they need (feeding lowers hunger), "feel" is added to how they feel about the actor, "look" sets their look sliders within the ranges given (their colour too), "reshape" changes the shape of their body itself, for this one being, when no look slider does it (a poofy tail, pointed ears, a longer neck, horns, a trunk, wings; say precisely what and how much), "grow" multiplies their size (2 = twice as big, 0.7 = smaller), "wear" puts clothing, armour or gear on them (a layer; it is made if nobody has made one, for their body), "take_off" removes what they wear (a layer by name, or the clothes a person's body draws: "shirt", "dress", "trousers", "boots", "hat", "underwear"; "wear" with that name puts it back on), "learn" teaches a gesture they can do from now on (sit, bow, wave, a new one), "become" turns them into another species listed in species_here, only when world_has_magic is true (a curse, a spell). They may refuse what they don't want from someone they don't trust; the world checks that.
- "hurt" in "being" is health a deed takes from a person or creature: 0.05 a bruise, 0.2 a deep cut or a bad burn, 0.5 a grave wound, 1 kills. Deeds that would hurt do hurt, whoever does them and to whomever, the traveler included (they can hurt themselves): never soften one into a flinch or a change of heart; tell it plainly, without gore. With "hurt", "how" says what it was in a few words, as it would end "they were …" ("stabbed in the eye with a stick", "kicked in the ribs by the traveler", "poisoned by nightshade berries"): it is what witnesses remember. "on_self": true when the deed is done to the actor's own body (stab myself, burn my hand, eat the poison berries): "being" then applies to the actor, whatever they point at. "self" is the traveler's own body: changing how they look (darker skin, a red shirt, broader) is "being" with "look" (their sliders in "self") and "on_self": true.
- "beings" brings new beings of a listed species into the world (a conjured hound, a clay golem), only when beings_can_be_made is true; they belong to the actor. Give each the "look" sliders of its species ("species_looks") that match what was asked (colour, build), and "size" when asked bigger or smaller than usual (1 = usual, 0.5–4).
- Beings in "beings_nearby" can be meant by name ("make Remy bigger"): the target is then that being.
- Colours on look sliders: a hue slider runs round the colour wheel (0 red, 0.08 orange or ginger, 0.15 yellow, 0.33 green, 0.5 cyan, 0.66 blue, 0.8 purple); on a "quadruped", hue above 1 fades the colour out (1.2 = grey with no colour at all, e.g. a grey or silver cat), and shade runs from 0 black to 1 white (a white cat: shade 1, hue 1.2; black: shade 0)."#;

pub const CHAT_TASK: &str = "Two characters in a small living world meet and talk briefly, in their own voices, about what is on their minds (what they saw, what they are doing, each other). 2 to 4 short lines, plain speech, no stage directions. Reply with one JSON object only: {\"lines\": [{\"who\": \"Name\", \"text\": \"…\"}]}";

pub const GESTURE_TASK: &str = r#"You animate a simple body in a small 3D world (a person, unless the body is described below). Write the gesture named below as a few key poses. Reply with one JSON object only:
{"duration": seconds (0.5–8), "contact": false, "distance": metres to the other person (0.4–3; only matters with contact), "intimacy": affection needed to agree, -1..1 (contact gestures only),
 "frames": [{"t": 0, "pose": {}}, {"t": 0.3, "pose": {"raise_r": 0.6, "reach_r": 0.2}}, …, {"t": 1, "pose": {}}]}
Pose roles (all default 0, the body at rest). For a person: raise_l / raise_r: left / right arm raised sideways, 0 down … 0.5 level … 1 straight up. reach_l / reach_r: arm reaching forward, 0 … 1 straight ahead. lean: bend forward at the hips in radians, -0.3 … 0.6. head: head tipped down, -1 … 1. crouch: 0 standing … 1 crouching. spread: a tail, wings or frill (-1 … 1), for bodies that have one. Other bodies give each role their own meaning (described with the body); use only the roles that body has.
Start and end at rest ({}), 3 to 8 frames, t from 0 to 1. Set "contact": true only for things done touching another (an embrace, a dance hold, a forehead touch)."#;


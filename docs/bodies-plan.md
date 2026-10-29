# Use spots and a humanoid rig

Agreed 2026-10-28 as steps 2 and 3 of four (1: paths and spacing, on main in
`d3176e1`; 4: a longer day, game-time action lengths and daily routines, later).
The aim: bodies use things the way bodies do (lie in a bed, sit on a chair,
stand at a stove with hands on it), and people bend like people (knees and
spine to pet a cat, not a plank lean). As always: primitives the AI fills in,
no per-thing or per-species code, and any request ("a hammock", "a hug") must
visibly work.

Step 2 ships first on today's nine pose roles. Step 3 makes the same postures
and actions look right.

---

## What there is now

- **Anchors** (`meta.anchors`, `src/lang/ir.rs`): `door`, `hatch`, `slot`,
  `part`, `stairs`, `bed`, `seat`, `light`. Rooms collect beds and seats
  (`sim/rooms.rs`).
- **Sitting** is boarding: `Sim::board` (`sim/vehicle.rs`) puts a body
  `aboard` a thing with `seat` anchors, `seat_no` picks one. The seated pose is
  `crouch` 0.55 (`Actor::update_pose`). Not saved over a restart.
- **Sleeping**: at night `GoHome` walks to `bed_for` (`sim/rooms.rs`) and stops
  0.8 m short; `Sleep` sets `asleep`, posed as `crouch` 1.0 with eyes shut. Nobody
  lies down. `Verb::Rest` (`sim/scorer.rs`) is a "sit" gesture on the ground.
- **Lying** exists for the dead: `meta.body.dead` (per-role values) plus a tilt
  from `remains::fall_tilt`, drawn through `GpuInst::tilt`. Living bodies get a
  tilt from `Actor::sway`.
- **Poses**: nine roles in `k.s0`…`k.s8` (`sim/actor.rs`: raise, reach, lean,
  head, crouch, spread, grip). `k.s9` mouth, `k.s10` eyes, `k.s11` clock. All 12
  live floats of `GpuInst` (`s0`, `s1`, `s2`) are used.
- **Gestures** are role keyframes; new ones are data (`CustomGesture`, written by
  the AI, kept as `name@species` when a body needs its own). Contact gestures
  (`contact`, `distance`, `intimacy`) cover hugs.
- **`figure.js`**: legs are thigh and calf capsules whose knee comes only from
  `crouch` and the walk swing; arms are straight from the shoulder (no elbow);
  `lean` turns the whole upper body at the hip. `parts()` repeats the same maths
  for the hands, head and chest.
- **Seeing poses**: `pocket snapshot --hands POSE --pitch DEG` (first person),
  `--png`, and `--shot` on every screen.
- **Bug seen in the Venice world**: e on a building with a `seat` anchor inside
  (a stool) sits you on it from outside. `board` takes the whole thing, not the
  seat you point at, and doesn't ask for reach to that seat.

---

## Step 2: use spots

### The primitive

A **spot** is an anchor saying how a body uses a place on a thing:

```js
{ kind: "spot", at: [x, y, z], face: 180, posture: "lie", use: "sleep",
  size: [0.9, 2.0], holds: 2, reach: [0, 0.9, 0.4] }
```

| Field | |
|---|---|
| `at`, `face` | the surface a body rests on (a mattress top, a seat top, the floor before a stove) and which way it faces |
| `posture` | `lie`, `lie_side`, `sit`, `kneel`, `stand`, `lean`, `perch` (small set; grows only with a body contract for it) |
| `use` | what the spot is for, in words the needs and planner already speak: `sleep`, `rest`, `work`, `cook`, `eat`, `pray`, … |
| `size`, `holds` | its extent and how many bodies at once (a double bed 2, a pew 4) |
| `reach` | where the hands go (a stove top, a loom, a counter), local to the thing |

`bed` and `seat` stay as shorthand (`spot` with `lie`/`sleep` and `sit`/`rest`),
so every world made so far works.

### Bodies say how they hold a posture

Each body gets a `postures` map in its `body` meta: role values per posture plus
a tilt, the way `rest` and `dead` work now (`{ lie: { roles: [...], tilt: [90, 0, 0], lift: 0.12 } }`).
`figure` and `quadruped` get defaults; AI-written bodies add theirs, and the probe
checks each like a role (sound, in bounds, changes). A dog's `lie` is curled, a
person's on the back, a swan's sitting on its belly.

### Engine (`sim/spots.rs`, new)

- `Actor.spot: Option<SpotRef>` (thing or instance, anchor index, place in it).
  Using a spot: walk to its approach point (from `face`, via `way_to`), blend
  0.5 s into the posture (position, yaw, tilt), stay until the use ends, blend
  back out to the approach point.
- Claims: a spot's places are taken in order; full spots aren't offered.
  `holds: 2` lets a couple share a bed; anyone else looks elsewhere.
- Boarding things with seats stays for vehicles; benches, chairs and beds move
  to spots. Driving keeps `seat_no` 0 for the driver.
- Saved with the actor (fixes "sitting isn't saved"). Mirrored to guests (new
  field on the wire: bump the wire hash).

### Who uses spots

- **Sleep**: `bed_for` returns a spot; `Sleep` lies in it. No bed: lie on the
  ground in the body's `lie` posture, not a squat.
- **Rest** (`Verb::Rest`): the nearest free `rest` spot within ~15 m, else the
  ground.
- **Work and planner steps**: a step `{"do": "use", "target": "the stove"}`
  resolves to that thing's spot for its `use`, stands there and puts hands to
  `reach` (step 2: the `reach` roles aimed at it; step 3: real hand targets).
- **The traveler**: e on a thing with spots takes the free spot nearest where
  they point, within reach of it (fixes the stool-through-the-wall bug). e again
  gets up.

### Prompts

The maker (`src/prompts.rs`) learns spots: where `at` sits (the surface, not the
middle of the mattress), `holds`, `reach`, and that furniture in a room needs them
(beds, chairs, benches, stoves, workbenches, altars, hammocks, thrones). The
interpreter's "use" step names things by what they are for.

### Tests (lean, one world each)

- A villager at night lies in their bed: tilted near flat, on the mattress top,
  in its bounds; gets up at dawn.
- Two strangers don't share a single bed; a couple shares a double one.
- A dog lies on its own bed in its own posture.
- The traveler sits on a bench with e, gets up with e, and can't sit on a stool
  through a wall.
- A cook at a stove stands facing it with hands near `reach`.
- Before finishing, try varied cases with a real AI: a hammock, a throne, a pew
  holding four, a canoe's seats, a bunk bed upstairs (levels), a merfolk's
  sleeping shell under water.

---

## Step 3: a humanoid rig

### The primitive

A **rig** is joint points the engine solves each frame, in body-local metres.
A body opts in with `body.rig: "biped"`; its `sdf` draws around the joints
instead of computing limbs itself:

`pelvis, chest, neck, head, shoulder_l/r, elbow_l/r, wrist_l/r, hand_l/r,
hip_l/r, knee_l/r, ankle_l/r, toe_l/r` (22 points).

Bone lengths come from the body's rest pose (measured once by the probe), so a
tall elf and a stocky dwarf solve with their own proportions.

### Solver (`sim/rig.rs`, new, CPU)

- **Targets in, joints out**: pelvis height and tilt, hand targets, foot plants,
  look-at, spine bend. Two-bone analytic IK per limb with pole vectors (knees
  forward, elbows back and down), the spine as 2–3 segments sharing a bend,
  joint limits (knee and elbow 0–150°, no hyperextension), and the pelvis
  shifted so the weight stays over the feet.
- **Gait from parameters**: a procedural cycle places the foot targets; stride,
  lift, duty factor and flight time make walk, run, hop, skip and leap. Stairs
  and ladders plant feet on treads and rungs (`sim/climb.rs` knows them).
- **Roles map to targets**, so every existing gesture, including AI-written
  ones, plays on the rig: `raise_*` puts a hand up and out, `reach_*` forward,
  `crouch` lowers the pelvis and bends knees, `lean` bends the spine, `head`
  tilts the neck. Bodies without a rig keep the role path unchanged.
- **Far bodies** skip IK (rest pose plus a cheap cycle, at the medium tick rate).

### Getting joints to the GPU

All instance live slots are used, and 22 points × 3 floats is too much to add to
every instance. A **rig buffer** (storage buffer, one block per rigged body this
frame) holds them, indexed from `GpuInst`. The language gets `joint(k, i)` →
`vec3` (WGSL reads the buffer; the CPU VM reads the same block), so `parts()`
becomes a joint lookup. `lang/wgsl.rs`, `lang/vm.rs` and `lang/api.rs` change;
the probe gives a rest pose when no rig is bound.

### Actions as targets

`CustomGesture` frames gain targets beside role values: `hand_l: { to:
"other.back" }`, `hand_r: { to: "thing.reach" }`, `pelvis: 0.45`, `look:
"other.head"`, `feet: "planted"`. Pet = kneel or squat, hand to the animal's
back. Hug = step in, both hands to the other's back, chests close. Cook = stand
at the spot, hands to `reach`. The AI writes new actions as these frames; the
solver keeps them physically sane.

### Checks instead of human QA

Every new action is solved at its frames and measured: hand-to-target error,
feet sinking or floating (> 3 cm), joint limits hit, limbs through the torso
(capsule test), weight outside the feet (unless the action is a leap or a
fall). A failing action goes back to the AI once with the numbers; failing
again, the nearest library action is used instead.

### Review sheet and library

`pocket poses [--action NAME]` renders each action at six frames from two angles
into one PNG grid, failed checks marked (built on `render_once` and the
`--shot` path). Actions approved by a human go into a shared library
(`src/builtin/actions.json`) every world starts from; the AI writes new ones
only when nothing in it fits. This is the loop for refining people's actions by
hand.

### Phases

1. Solver and a rigged `figure` behind a flag; the review sheet shows old and
   new side by side for every built-in gesture; walking with planted feet.
2. Roles to targets on by default for people; gait parameters (run, hop, skip,
   leap); stairs and ladders.
3. Target frames: pet, hug, cook, carry rebuilt; step 2's spots use real hand
   targets; the checks.
4. The library and AI-written actions with check-and-retry.
5. AI-written humanoid species bodies on the rig. Then a quadruped rig (four
   two-bone legs, spine, neck, tail): golems are bipeds with heavy settings,
   dragons quadrupeds with wings. Branch by rig, never by species.

### Tests

- Solver unit tests: a hand reaches a target in range and stops at full
  stretch out of range; knees never bend backwards; feet stay planted through
  a step cycle.
- A person petting a cat on the ground bends the knees (pelvis below 0.6 of
  standing height) with the hand on the cat's back.
- A hug puts both hands within 10 cm of the other's back.
- Every built-in gesture still plays, and passes the checks, on the rigged
  figure.
- The first-person hands (`pocket snapshot --hands`) come from the same joints.

### Open questions for the user

- Joints as points (simpler SDFs, planned here) or as angles (smaller to send)?
- Must new AI-written humanoid species use the rig, or may they still write
  free-form bodies on roles?
- Does the action library live in the repo (shared by everyone) or per player?

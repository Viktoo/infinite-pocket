#!/usr/bin/env python3
"""Maple Cove's answers to the builder, written from one place.

A cozy fishing village on a northern coast in autumn: single-storey
cottages with bright roofs round a green and along three lanes, a tavern,
an inn, a store, a bakery, a smithy and a fish shed, a wooden pier out
into a cove that faces the sunset; meadows of asters and goldenrod, apple
orchards, and the woodcutters' hamlet of Pine Hollow in the maple woods up
the road. The plan is written with the sea to the south and turned a
quarter round into the world (see `W`), so the cove opens to the west.

Run it (it builds the land once with `pocket author --genesis-only` to read the
natural heights the village sits on), then `pocket author worlds/starter`.
"""
import json
import math
import os
import random
import subprocess
import sys

import hamlet
import people
import village

HERE = os.path.dirname(os.path.abspath(__file__))
REGION = 256.0
POCKET = os.path.join(HERE, "..", "..", "target", "release", "pocket")
WORK = os.path.join(HERE, ".work")


# ---------------------------------------------------------------------------
# The land's large shape.

def ring(cx, cz, r, n=18, wob=0.0, seed=1):
    rnd = random.Random(seed)
    return [[round(cx + math.cos(i / n * 6.2832) * r * (1 + rnd.uniform(-wob, wob)), 1),
             round(cz + math.sin(i / n * 6.2832) * r * (1 + rnd.uniform(-wob, wob)), 1)] for i in range(n)]


# The shore, west to east: a sandy cove, the village's harbour, the headland.
COAST = [[-3000, -70], [-900, -60], [-420, -58], [-240, -52], [-190, -40], [-150, -34], [-112, -30], [-96, -26],
         [-60, -24], [0, -24], [60, -24], [100, -26], [135, -32], [160, -60], [185, -130], [215, -205], [245, -215],
         [275, -170], [300, -95], [340, -60], [520, -58], [900, -64], [3000, -70]]
FOREST_AT = (130.0, 520.0)
CAPE = (240.0, -184.0)


def landforms():
    return [
        {"name": "the coast", "biome": "autumn meadows", "outline": [[-3000, -2500], [3000, -2500], [3000, 3000], [-3000, 3000]], "edge": 400},
        {"name": "the orchard hills", "biome": "orchard hills", "outline": ring(-560, 320, 330, 16, 0.2, 3), "edge": 160, "rise": 8, "rugged": 0.3},
        {"name": "the far orchards", "biome": "orchard hills", "outline": ring(600, 260, 260, 16, 0.2, 4), "edge": 160, "rise": 6, "rugged": 0.3},
        {"name": "the village slope", "biome": "autumn meadows", "outline": ring(0, 60, 150, 16, 0.08, 5), "edge": 120, "rise": 4},
        {"name": "the west wood", "biome": "maple woods", "outline": ring(-150, 170, 80, 14, 0.2, 13), "edge": 60, "rise": 3, "rugged": 0.2},
        {"name": "the east wood", "biome": "maple woods", "outline": ring(140, 175, 75, 14, 0.2, 14), "edge": 60, "rise": 3, "rugged": 0.2},
        {"name": "the maple woods", "biome": "maple woods",
         "outline": ring(FOREST_AT[0], FOREST_AT[1] + 140, 300, 20, 0.15, 6), "edge": 110, "rise": 12, "rugged": 0.25},
        {"name": "the cove", "biome": "the sea", "outline": COAST + [[3000, -3000], [-3000, -3000]], "edge": 45},
        {"name": "Lantern Point", "biome": "rocky point",
         "outline": [[140, -30], [170, -110], [215, -205], [262, -196], [300, -100], [320, -40], [230, 10]], "edge": 30, "rise": 12, "rugged": 0.5},
    ]


# The plan is written with the sea to the south (+x east, +z north); the
# world turns it a quarter round so the cove opens to the west, into the
# sunset: (x, z) here is (z, -x) in the world, and a bearing gains 90.
def W(x, z):
    return (z, -x)


def WB(b):
    return (b + 90.0) % 360.0


def turned(pts):
    return [list(W(x, z)) for x, z in pts]


class Plan:
    def __init__(self):
        self.ground = []
        self.things = []
        self.people = []
        self.creatures = []
        self.items = []

    def line(self, what, pts, width, how, height=None, sides=None, color=None, rim=None, rim_width=None, name="", water=None, pattern=None, pattern_size=None):
        e = {"what": what, "name": name, "line": [[round(a, 2), round(b, 2)] for a, b in turned(pts)], "width": width, "how": how}
        if height is not None:
            e["height"] = height
        if sides is not None:
            e["sides"] = sides
        if color:
            e["color"] = list(color)
        if rim:
            e["rim"], e["rim_width"] = list(rim), rim_width or 0.6
        if water is not None:
            e["water"] = water
        if pattern:
            e["pattern"] = pattern
            if pattern_size:
                e["pattern_size"] = pattern_size
        self.ground.append(e)

    def area(self, what, pts, how, height=None, sides=None, color=None, rim=None, rim_width=None, name="", water=None, pattern=None, pattern_size=None):
        e = {"what": what, "name": name, "outline": [[round(a, 2), round(b, 2)] for a, b in turned(pts)], "how": how}
        if height is not None:
            e["height"] = height
        if sides is not None:
            e["sides"] = sides
        if color:
            e["color"] = list(color)
        if rim:
            e["rim"], e["rim_width"] = list(rim), rim_width or 0.6
        if water is not None:
            e["water"] = water
        if pattern:
            e["pattern"] = pattern
            if pattern_size:
                e["pattern_size"] = pattern_size
        self.ground.append(e)

    def rect(self, what, x0, z0, x1, z1, how, **kw):
        self.area(what, [(x0, z0), (x1, z0), (x1, z1), (x0, z1)], how, **kw)

    def item(self, ty, near, x, z):
        """A small loose thing lying by its person (`near`), at their home."""
        self.items.append((ty, near, *W(x, z)))

    def put(self, ty, x, z, face=180.0, y=None):
        """`face`: the bearing its front looks along (180: south, to the sea)."""
        self.things.append((ty, *W(x, z), WB(face), y))


def natural_heights(points):
    out = subprocess.run([POCKET, "author", "heights", os.path.join(WORK, "land.pocket"), "-"],
                         input="\n".join("%s %s" % W(x, z) for x, z in points), capture_output=True, text=True, check=True).stdout
    return [float(l.split()[2]) for l in out.splitlines()]


def build_land():
    os.makedirs(WORK, exist_ok=True)
    env = dict(os.environ, POCKET_HOME=WORK)
    subprocess.run([POCKET, "author", HERE, "--genesis-only", "--out", os.path.join(WORK, "land.pocket")], env=env, check=False, capture_output=True)


COBBLE = (150, 142, 132)
STONE = (168, 164, 156)
DIRT = (150, 126, 92)
GRAVEL = (176, 168, 150)
SAND = (214, 200, 166)


def height_map():
    """The land's natural heights over the village (before anything is laid),
    read once on a 4 m grid; `nat(x, z)` in plan metres between them."""
    x0, z0 = W(-180, -50)
    x1, z1 = W(180, 150)
    # On the 4 m grid itself, so every lookup lands on its samples.
    snap = lambda v, f: int(f(v / 4) * 4)
    box = f"{snap(min(x0, x1), math.floor)},{snap(min(z0, z1), math.floor)},{snap(max(x0, x1), math.ceil)},{snap(max(z0, z1), math.ceil)}"
    out = subprocess.run([POCKET, "author", "heights", os.path.join(WORK, "land.pocket"), box, "4"],
                         capture_output=True, text=True, check=True).stdout.splitlines()
    xs = [float(v) for v in out[0].split()[1:]]
    grid = {}
    for row in out[1:]:
        if not row.strip():
            continue
        v = row.split()
        zz = float(v[0])
        for xx, h in zip(xs, v[1:]):
            grid[(xx, zz)] = float(h.rstrip("~"))

    def nat(x, z):
        wx, wz = W(x, z)
        gx, gz = math.floor(wx / 4) * 4, math.floor(wz / 4) * 4
        tx, tz = (wx - gx) / 4, (wz - gz) / 4
        g = lambda a, b: grid.get((a, b), grid.get((gx, gz), 6.0))
        return (g(gx, gz) * (1 - tx) + g(gx + 4, gz) * tx) * (1 - tz) + (g(gx, gz + 4) * (1 - tx) + g(gx + 4, gz + 4) * tx) * tz
    return nat


def lineup(p, nat):
    """Every small type in rows on a paved pad, to look them over (only with
    LINEUP=1; never in the shipped world)."""
    import re
    small = []
    for f in sorted(os.listdir(os.path.join(HERE, "types"))):
        src = open(os.path.join(HERE, "types", f)).read()
        m = re.search(r'name: "([^"]+)"', src)
        b = re.search(r"bounds: \[([\d.]+), ([\d.]+), ([\d.]+)\]", src)
        if m and b and max(float(v) for v in b.groups()) < 2.7 and '"body"' not in src and "sign" not in m.group(1):
            small.append(m.group(1))
    small = ["tall grass", "blueberry bush", "autumn wildflowers", "leaf litter", "bracken"] + small
    h = round(nat(60, 140), 2)
    p.rect("lineup", 40, 126, 100, 172, "level", height=h, sides=3, color=(150, 150, 150))
    for i, ty in enumerate(small):
        p.put(ty, 44 + (i % 12) * 4.5, 131 + (i // 12) * 5.0, 270, y=h)
    print("lineup", len(small), h)


def lay_ground(p, nat, gnat):
    import village
    # The landing: a granite quay laid in setts along the water, the pier
    # going out from it; the cove under the pier kept shallow for the piles.
    h = village.LANDING_H
    p.rect("cove floor", -40, -86, 40, -36, "level", height=-3.5, sides=5)
    p.rect("landing", -44, -31, 44, -10, "level", height=h, sides=0.7, color=COBBLE, rim=STONE, rim_width=0.5, name="the landing", pattern="setts")
    # A grassy bank from the landing up to the village.
    for x in range(-48, 49, 8):
        p.line("bank", [(x, -10.5), (x, 8)], 9.0, "level", height=[h, round(nat(x, 8), 2)], sides=3)
    p.line("lane", [(-8, -12), (-8, 8), (-4, 21)], 4.0, "level", height=[h, round(nat(-8, 8), 2), round(nat(-4, 21), 2)], sides=2.0, color=COBBLE, name="Pier Lane", pattern="cobbles")
    # The cove beach along the shore past the west lane: shingle and sand.
    import beach
    for x in range(-152, -51, 8):
        zs = [z for z, _ in beach.PROFILE]
        hs = [round(nat(x, -6), 2)] + [hh for _, hh in beach.PROFILE[1:]]
        p.line("beach", [(x, z) for z in zs], 10, "level", height=hs, sides=5)
    p.rect("sand", -154, -64, -50, -12, "paint", color=SAND, name="Sunset Beach")
    # The green, level, and the lanes round it and away.
    gx, gz = village.GREEN
    gh = round(nat(gx, gz), 2)
    p.rect("green", gx - 21, gz - 13, gx + 21, gz + 14, "level", height=gh, sides=6, color=(112, 146, 62), name="the green")
    p.line("lane", [(-22, 20), (22, 20), (22, 49), (-22, 49), (-22, 20)], 3.5, "paint", color=GRAVEL, name="Green Lane")
    for name, pts in village.LANES.items():
        p.line("lane", pts, 3.5, "paint", color=GRAVEL, name={"west": "Bluff Lane", "east": "Orchard Lane", "north": "Wood Road"}[name])
    # Roads out: north to the woods, east and west along the coast, and the
    # path out to the lighthouse on the point.
    p.line("road", [(30, 112), (34, 210), (70, 300), (100, 400), (124, 458), (128, 492)], 4.0, "level", color=DIRT, name="the woodcutters' road")
    p.line("road", [(34, 210), (-90, 226), (-220, 240), (-380, 262)], 4.0, "level", color=DIRT, name="the coast road")
    p.line("road", [(34, 210), (150, 196), (290, 190), (420, 186)], 4.0, "level", color=DIRT, name="the coast road")
    p.line("path", [(-126, 15), (-150, 60), (-176, 150), (-200, 236)], 2.6, "level", color=GRAVEL, name="the beach path")
    p.line("path", [(124, 12), (190, 60), (250, 100), (250, 0), (238, -110), (236, -178)], 2.6, "level", color=GRAVEL, name="the lighthouse path")
    # The woodcutters' clearing, levelled, and their pond.
    fx, fz = FOREST_AT
    h = round(gnat["clearing"], 2)
    p.area("clearing", ring(fx, fz, 30, 14, 0.12, 9), "level", height=h, sides=8, color=(150, 140, 70), name="Pine Hollow")
    p.area("pond", ring(fx + 34, fz + 18, 9, 12, 0.15, 10), "cut", height=h - 2.0, sides=2.0, water=h - 0.5, name="the woodcutters' pond")
    # The lighthouse's level on the ridge of the point.
    p.area("cape top", ring(CAPE[0], CAPE[1], 9, 12, 0.0, 12), "level", height=round(gnat["cape"], 2), sides=3, color=GRAVEL)
    return h, gh


# ---------------------------------------------------------------------------
# Cutting the plan into regions.

def clip_line(pts, hs, box):
    x0, z0, x1, z1 = box
    pieces, cur = [], []

    def flush():
        nonlocal cur
        if len(cur) >= 2:
            pieces.append(cur)
        cur = []

    for i in range(len(pts) - 1):
        (ax, az), (bx, bz) = pts[i], pts[i + 1]
        ha = hs[i] if hs else None
        hb = hs[i + 1] if hs else None
        dx, dz = bx - ax, bz - az
        t0, t1 = 0.0, 1.0
        ok = True
        for pp, qq in ((-dx, ax - x0), (dx, x1 - ax), (-dz, az - z0), (dz, z1 - az)):
            if pp == 0:
                if qq < 0:
                    ok = False
                    break
            else:
                r = qq / pp
                if pp < 0:
                    t0 = max(t0, r)
                else:
                    t1 = min(t1, r)
        if not ok or t0 > t1:
            flush()
            continue
        at = lambda t: ([ax + dx * t, az + dz * t], None if ha is None else ha + (hb - ha) * t)
        a, b = at(t0), at(t1)
        if not cur or t0 > 0:
            flush()
            cur = [a]
        cur.append(b)
        if t1 < 1:
            flush()
    flush()
    return pieces


def clip_poly(pts, box):
    x0, z0, x1, z1 = box
    out = pts
    for inside, cut in ((lambda p: p[0] >= x0, lambda a, b: x0), (lambda p: p[0] <= x1, lambda a, b: x1)):
        src, out = out, []
        for i, a in enumerate(src):
            b = src[(i + 1) % len(src)]
            if inside(a):
                out.append(a)
            if inside(a) != inside(b):
                x = cut(a, b)
                t = (x - a[0]) / (b[0] - a[0])
                out.append([x, a[1] + (b[1] - a[1]) * t])
    for inside, cut in ((lambda p: p[1] >= z0, lambda a, b: z0), (lambda p: p[1] <= z1, lambda a, b: z1)):
        src, out = out, []
        for i, a in enumerate(src):
            b = src[(i + 1) % len(src)]
            if inside(a):
                out.append(a)
            if inside(a) != inside(b):
                z = cut(a, b)
                t = (z - a[1]) / (b[1] - a[1])
                out.append([a[0] + (b[0] - a[0]) * t, z])
    return out


def region_of(x, z):
    return (math.floor(x / REGION), math.floor(z / REGION))


KNOWN = {"sugar maple", "paper birch", "red oak", "white pine", "apple tree", "autumn wildflowers", "tall grass", "bracken", "blueberry bush", "granite boulder", "eelgrass", "leaf litter",
         "sea grass", "bench", "lamppost", "boat", "bridge", "door", "hatch", "walkway"}
FACTS = [
    "Maple Cove's pier was rebuilt in oak after the big storm years ago; everyone in the village drove a piling.",
    "The woodcutters of Pine Hollow bring firewood, timber and charcoal down the road to the village; the village sends fish, bread and news up.",
    "Every autumn the village holds a harvest supper on the green, with the fiddle playing in the gazebo, and on clear evenings people gather on the landing to watch the sun go down over the cove.",
]


def write_regions(p):
    regions = {}

    def reg(r):
        return regions.setdefault(r, {"ground": [], "buildings": [], "characters": [], "creatures": [], "things": []})

    margin = 58.0
    for e in p.ground:
        key = "line" if "line" in e else "outline"
        pts = e[key]
        hs = e["height"] if isinstance(e.get("height"), list) else None
        xs, zs = [q[0] for q in pts], [q[1] for q in pts]
        for rx in range(math.floor((min(xs) - margin) / REGION), math.floor((max(xs) + margin) / REGION) + 1):
            for rz in range(math.floor((min(zs) - margin) / REGION), math.floor((max(zs) + margin) / REGION) + 1):
                ox, oz = rx * REGION, rz * REGION
                box = (ox - margin, oz - margin, ox + REGION + margin, oz + REGION + margin)
                pieces = clip_line(pts, hs, box) if key == "line" else ([[[q, None] for q in c]] if len(c := clip_poly(pts, box)) >= 3 else [])
                for piece in pieces:
                    q = dict(e)
                    q[key] = [[round(a[0][0] - ox, 2), round(a[0][1] - oz, 2)] for a in piece]
                    if hs:
                        q["height"] = [round(a[1], 2) for a in piece]
                    reg((rx, rz))["ground"].append(q)
    for ty, near, x, z in p.items:
        reg(region_of(x, z))["things"].append({"type": ty, "near": near})
    for ty, x, z, rot, y in p.things:
        r = region_of(x, z)
        b = {"type": ty, "dx": round(x - r[0] * REGION - 128, 2), "dz": round(z - r[1] * REGION - 128, 2), "rot": round(rot, 1)}
        if y is not None:
            b["y"] = round(y, 3)
        reg(r)["buildings"].append(b)
    for c in p.people:
        hx, hz = W(c["home_x"], c["home_z"])
        r = region_of(hx, hz)
        reg(r)["characters"].append(dict(c, home_x=round(hx - r[0] * REGION, 1), home_z=round(hz - r[1] * REGION, 1)))
    for c in p.creatures:
        cx, cz = W(c["x"], c["z"])
        r = region_of(cx, cz)
        reg(r)["creatures"].append(dict(c, x=round(cx - r[0] * REGION, 1), z=round(cz - r[1] * REGION, 1)))

    order = sorted(regions)
    seen, new = set(KNOWN), {}
    for r in order:
        for b in regions[r]["buildings"] + regions[r]["things"]:
            if b["type"] not in seen:
                seen.add(b["type"])
                new.setdefault(r, []).append(b["type"])
    os.makedirs(os.path.join(HERE, "regions"), exist_ok=True)
    for f in os.listdir(os.path.join(HERE, "regions")):
        os.remove(os.path.join(HERE, "regions", f))
    for (rx, rz), r in regions.items():
        plan = {
            "name": "Pine Hollow" if (rx, rz) == region_of(*W(*FOREST_AT)) else "Maple Cove",
            "mood": "cozy, peaceful, golden autumn, woodsmoke and falling leaves",
            "facts": FACTS,
            "new_types": [{"name": n, "tags": []} for n in new.get((rx, rz), [])],
            "landmarks": [],
            "settlement": {"name": "Maple Cove", "x": 128, "z": 128, "buildings": r["buildings"], "ground": r["ground"],
                           "road": None, "lamps": None, "bridge": None, "vehicle": None},
            "things": r["things"],
            "characters": r["characters"],
            "creatures": r["creatures"],
        }
        with open(os.path.join(HERE, "regions", f"{rx}_{rz}.md"), "w") as f:
            f.write("```json\n" + json.dumps(plan, indent=1) + "\n```\n")
    return order


def main():
    path = os.path.join(HERE, "genesis.json")
    with open(path) as f:
        g = json.load(f)
    g["landforms"] = [dict(f, outline=turned(f["outline"])) for f in landforms()]
    with open(path, "w") as f:
        json.dump(g, f, indent=1)
        f.write("\n")
    w = {"seed": 31, "prompt": "A cozy, peaceful autumn fishing village on a northern coast: bright-roofed single-storey cottages round a green above a wooden pier, a cove facing the sunset, meadows of goldenrod and asters, apple orchards, and a woodcutters' hamlet in woods of red maple, gold birch and pine.",
         "spawn": list(W(*village.SPAWN)), "regions": [], "difficulty": "peaceful",
         # The first look: dawn, facing the gazebo, so a new traveler has the whole day.
         "start_hour": 5.5, "facing": 90}
    with open(os.path.join(HERE, "world.json"), "w") as f:
        json.dump(w, f, indent=1)
    build_land()
    nat = height_map()
    gnat = dict(zip(["clearing", "cape", "cross"], natural_heights([FOREST_AT, CAPE, (38.5, 205.5)])))
    p = Plan()
    clearing, gh = lay_ground(p, nat, gnat)
    village.lay(p, nat, round(gnat["cape"], 2), round(gnat["cross"], 2))
    import beach
    beach.lay(p)
    if os.environ.get("LINEUP"):
        lineup(p, nat)
    hamlet.lay(p, clearing)
    people.lay(p, round(gnat["cape"], 2), clearing)
    regions = write_regions(p)
    w["regions"] = [list(r) for r in regions]
    with open(os.path.join(HERE, "world.json"), "w") as f:
        json.dump(w, f, indent=1)
        f.write("\n")
    print(f"landforms {len(g['landforms'])}, ground {len(p.ground)}, things {len(p.things)}, people {len(p.people)}, regions {len(regions)}")


if __name__ == "__main__":
    main()

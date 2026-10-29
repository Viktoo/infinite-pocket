//! Shapes set into the ground (src/inground.rs): an earth house under flat
//! ground, reached by a ramp dug down to its door; the earth meets its walls
//! and stays over its roof; a villager walks down into it and out again.

use super::*;

fn earth_house(w: &W) -> (Vec3, crate::model::CommitOk, u32) {
    let house = add_type(w, &fixture("physics/earth_house.js"));
    let h = flat_spot(w, 50.0);
    (h, commit_places(w, vec![pl(house, h, 0.0)], None), house)
}

#[test]
fn an_earth_house_under_flat_ground() {
    let w = world_with("earth", 72, bare());
    let (h, ok, house) = earth_house(&w);
    let snap = &ok.snapshot;
    let t = &snap.terrain;
    let p = snap.instances.iter().find(|p| p.type_id == house).expect("placed");
    let surface = t.height(h.x, h.z + 4.5);
    assert!((p.pos.y - (surface - 3.0)).abs() < 0.3, "its floor 3 m under the ground before its door: {} vs {surface}", p.pos.y);
    // Air in its room, earth over its roof and against its walls.
    let inside = Vec3::new(h.x, p.pos.y + 1.2, h.z);
    assert!(!t.solid(inside), "air inside");
    assert!(t.solid(Vec3::new(h.x, p.pos.y + 2.9, h.z)), "earth over the roof");
    assert!(t.solid(Vec3::new(h.x - 3.05, p.pos.y + 1.2, h.z)) && !t.solid(Vec3::new(h.x - 2.95, p.pos.y + 1.2, h.z)), "the earth meets its west wall");
    // No pit: the ground over it is the land's own.
    assert!((t.height(h.x, h.z) - t.natural_height(h.x, h.z)).abs() < 0.01, "the surface over it is untouched");
    // A ramp opens the ground in front of the door.
    let ramp = Vec3::new(h.x, 0.0, h.z + 5.0);
    let floor = t.floor(ramp.x, t.height(ramp.x, ramp.z) + 0.5, ramp.z);
    assert!(floor < t.height(ramp.x, ramp.z) - 0.6 && floor > p.pos.y - 0.2, "the way down: floor {floor:.2} under {:.2}", t.height(ramp.x, ramp.z));

    // A villager walks in (down the ramp, through the door) and out again.
    let ola = add_char(&w, "Ola", "kind", &[], Vec3::new(h.x + 6.0, 0.0, h.z + 14.0));
    let mut s = session(&w, 72, None);
    calm(&mut s);
    // From the ramp the traveler points at the door (the hill over the ramp is no wall).
    let eye = Vec3::new(h.x, t.floor(h.x, t.height(h.x, h.z + 5.5) + 0.5, h.z + 5.5) + 1.6, h.z + 5.5);
    let door = Vec3::new(h.x, p.pos.y + 1.0, h.z + 3.0);
    let seen = s.sim.pick(eye, door - eye, 8.0, 0.02, Some(ActorId::Player)).map(|q| q.name);
    assert_eq!(seen.as_deref(), Some("door"), "the door is pointed at from the ramp");
    let o = ActorId::Npc(ola);
    let goal = Vec3::new(h.x - 1.0, p.pos.y, h.z - 1.0);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(goal.to_array()), run: false });
    s.run(60.0, 0.1);
    let at = s.sim.actor(o).unwrap().pos;
    assert!(Vec3::new(at.x - goal.x, 0.0, at.z - goal.z).length() < 1.5, "she got inside: {at} vs {goal}");
    assert!((at.y - p.pos.y).abs() < 0.4, "on its floor: {} vs {}", at.y, p.pos.y);
    let out = Vec3::new(h.x + 5.0, t.height(h.x + 5.0, h.z + 16.0), h.z + 16.0);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(out.to_array()), run: false });
    s.run(60.0, 0.1);
    let at = s.sim.actor(o).unwrap().pos;
    assert!((at - out).length() < 2.0, "and out again: {at} vs {out}");
}

/// Pictures of the earth house (look at them): its ramp and door from the
/// surface, and inside.
#[test]
#[ignore]
fn earth_house_png() {
    let dir = std::env::var("POCKET_PNG_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let w = world_with("earthpng", 72, bare());
    let (h, _, _) = earth_house(&w);
    let mut s = session(&w, 72, None);
    s.sim.t = crate::render::sky::DAY_SECONDS * 0.42;
    let gpu = crate::render::gpu::Gpu::new().ok();
    let live = Arc::new(Mutex::new(crate::model::Live::default()));
    let mut model = crate::model::WorldModel::load(w.db.clone(), gpu.clone(), live).unwrap();
    let snap = model.snapshot().unwrap();
    s.sim.flip(snap.clone());
    let t = snap.terrain.clone();
    let look = |from: Vec3, to: Vec3| {
        let d = to - from;
        crate::render::Camera { pos: from, yaw: d.x.atan2(d.z), pitch: (d.y / Vec3::new(d.x, 0.0, d.z).length()).atan(), fov_y: 1.05, roll: 0.0 }
    };
    let g = t.height(h.x, h.z);
    let eye = Vec3::new(h.x + 4.0, t.height(h.x + 4.0, h.z + 16.0) + 2.2, h.z + 16.0);
    png_of(&mut s, &snap, &gpu, look(eye, Vec3::new(h.x, g - 2.0, h.z + 3.0)), &format!("{dir}/pocket-earth-house.png"));
    png_of(&mut s, &snap, &gpu, look(Vec3::new(h.x + 0.3, g + 1.7, h.z + 11.5), Vec3::new(h.x, g - 2.2, h.z + 3.0)), &format!("{dir}/pocket-earth-house-ramp.png"));
    let above = Vec3::new(h.x + 9.0, g + 7.0, h.z + 12.0);
    png_of(&mut s, &snap, &gpu, look(above, Vec3::new(h.x, g - 1.0, h.z + 2.0)), &format!("{dir}/pocket-earth-house-above.png"));
    png_of(&mut s, &snap, &gpu, look(Vec3::new(h.x + 2.0, g - 1.4, h.z - 2.0), Vec3::new(h.x - 1.0, g - 2.5, h.z + 3.0)), &format!("{dir}/pocket-earth-house-inside.png"));
}

/// A home under the ground entered by a hatch level with the ground, a
/// ladder under it: the hatch is hung (shut), a villager opens it, climbs
/// down to her bed and up and out again; the traveler climbs the ladder by
/// walking into it, waits under the shut hatch, and goes on up once it is
/// open.
#[test]
fn a_hatch_and_a_ladder_down_into_the_ground() {
    let w = world_with("hatch", 73, bare());
    let home = add_type(&w, &fixture("physics/hatch_home.js"));
    let h = flat_spot(&w, 50.0);
    let ok = commit_places(&w, vec![pl(home, h, 0.0)], None);
    let snap = &ok.snapshot;
    let t = &snap.terrain;
    let p = snap.instances.iter().find(|p| p.type_id == home).expect("placed");
    let g = t.height(h.x + 2.1, h.z);
    assert!((p.pos.y + 2.7 - g).abs() < 0.05, "its hatch level with the ground: roof {:.2}, ground {g:.2}", p.pos.y + 2.7);
    let hatch = builtin_id(&w, "hatch");
    let lid = snap.instances.iter().find(|q| q.type_id == hatch).expect("a hatch hung in it");
    assert!((lid.pos.y - g).abs() < 0.05, "lying in the opening: {}", lid.pos);
    assert!(!t.solid(Vec3::new(h.x, p.pos.y + 1.2, h.z)) && t.solid(Vec3::new(h.x - 3.05, p.pos.y + 1.2, h.z)), "a room in the earth");

    // A villager goes down to the bed and comes up again.
    let ola = add_char(&w, "Ola", "kind", &[], Vec3::new(h.x + 9.0, 0.0, h.z + 4.0));
    let mut s = session(&w, 73, None);
    calm(&mut s);
    let o = ActorId::Npc(ola);
    let bed = Vec3::new(h.x - 1.5, p.pos.y, h.z - 1.5);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(bed.to_array()), run: false });
    let mut climbed = false;
    for _ in 0..600 {
        s.step(0.1);
        climbed |= s.sim.actor(o).unwrap().ladder.is_some();
    }
    assert!(climbed, "by the ladder");
    assert!(s.sim.shut_door_near(Vec3::new(h.x + 2.1, g, h.z), 1.0).is_none(), "she opened the hatch");
    let at = s.sim.actor(o).unwrap().pos;
    assert!(Vec3::new(at.x - bed.x, 0.0, at.z - bed.z).length() < 1.5 && (at.y - p.pos.y).abs() < 0.4, "down by the bed: {at} vs {bed}");
    let out = Vec3::new(h.x + 9.0, s.sim.snap.terrain.height(h.x + 9.0, h.z - 3.0), h.z - 3.0);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(out.to_array()), run: false });
    s.run(60.0, 0.1);
    let at = s.sim.actor(o).unwrap().pos;
    assert!((at - out).length() < 2.0, "up and out again: {at} vs {out}");
}

/// The traveler climbs a ladder by walking into it; a shut hatch over it
/// holds them under it until it is opened.
#[test]
fn the_traveler_climbs_a_ladder_to_a_shut_hatch() {
    let w = world_with("climb", 74, bare());
    let home = add_type(&w, &fixture("physics/hatch_home.js"));
    let h = flat_spot(&w, 50.0);
    let ok = commit_places(&w, vec![pl(home, h, 0.0)], None);
    let p = ok.snapshot.instances.iter().find(|p| p.type_id == home).unwrap().clone();
    let mut s = session(&w, 74, None);
    calm(&mut s);
    // Standing in the room before the ladder, walking east into it.
    s.sim.player.pos = Vec3::new(h.x + 1.6, p.pos.y, h.z);
    for _ in 0..240 {
        s.sim.walk(ActorId::Player, Vec3::new(1.4 / 60.0, 0.0, 0.0));
        s.sim.step(1.0 / 60.0);
    }
    let me = s.sim.player.pos;
    assert!(s.sim.player.ladder.is_some(), "on the ladder: {me}");
    assert!(me.y > p.pos.y + 0.5 && me.y + s.sim.player.dims.height < p.pos.y + 2.7, "up it, held under the shut hatch: {} (floor {})", me.y, p.pos.y);
    // Open it, and go on up and out onto the ground.
    let lid = s.sim.shut_door_near(Vec3::new(h.x + 2.1, p.pos.y + 2.7, h.z), 1.0).expect("the hatch");
    s.sim.open(ActorId::Player, &lid, Some(false)).expect("opens");
    for _ in 0..240 {
        s.sim.walk(ActorId::Player, Vec3::new(1.4 / 60.0, 0.0, 0.0));
        s.sim.step(1.0 / 60.0);
    }
    let me = s.sim.player.pos;
    assert!(s.sim.player.ladder.is_none() && (me.y - (p.pos.y + 2.7)).abs() < 0.3, "out on the ground: {me}");
}

const FUNGUS: &str = r#"export const meta = { name: "glowcap", bounds: [0.2, 0.25, 0.2], tags: ["base", "fungus", "nonsolid"], props: { alive: 1, light: 0.3 }, grows: { at: "cave" } };
export function sdf(x, y, z, k) { return min(capsule(x, y, z, 0, 0, 0, 0, 0.2, 0, 0.03), ellipsoid(x, y - 0.2, z, 0.12, 0.05, 0.12)); }
export function color(x, y, z, k) { return rgb(120, 220, 200); }
"#;

/// A lawn with fungus in its scatter (it grows only in caves) and a cave
/// dug under it.
fn cave_land() -> (W, crate::settle::CaveSpec) {
    let mut look = bare();
    look.biomes[0].scatter.insert("fungus".into(), 6.0);
    let w = world_with("cavelife", 76, look);
    add_type(&w, FUNGUS);
    let h = flat_spot(&w, 40.0);
    let r = crate::world::region_of(h.x, h.z);
    let (ox, oz) = (r.0 as f32 * crate::world::REGION, r.1 as f32 * crate::world::REGION);
    let plan = serde_json::json!([{ "what": "cave", "name": "the Deep", "mouth": [h.x - ox, h.z - oz], "toward": 90, "length": 10, "depth": 5, "room": 8 }]);
    let caves = crate::settle::parse_caves(plan.as_array().unwrap(), ox, oz);
    let spec = caves[0].clone();
    let st = crate::model::Settlement { x: h.x, z: h.z, road: None, caves, ..Default::default() };
    let region = crate::model::RegionCommit { r, plan_json: "{}".into(), info: Default::default(), characters: vec![], settlement: Some(st) };
    commit_places(&w, vec![], Some(region));
    (w, spec)
}

/// What grows in caves grows on their floors and nowhere on the open land;
/// a creature of the caves is at home on a cave's floor and stays down there.
#[test]
fn life_in_the_caves() {
    let (w, spec) = cave_land();
    let cricket = serde_json::json!({ "name": "cave cricket", "plural": "cave crickets", "body": "quadruped", "size": 0.3, "mind": "instinct", "speech": "none", "social": "herd",
        "diet": { "plants": 1.0 }, "move": { "walk": 0.6, "run": 1.5 }, "caves": true, "description": "a pale long-legged cricket" });
    w.db.with(|c| db::put_species(c, "cave cricket", &cricket.to_string())).unwrap();
    let room = spec.room_at();
    let id = add_being(&w, "Pale One", "cave cricket", &[], Vec3::new(room.x, 0.0, room.y));
    let mut s = session(&w, 76, None);
    let snap = s.sim.snap.clone();
    let t = snap.terrain.clone();
    let c0 = crate::world::chunk_of(room.x, room.y);
    let mut items = Vec::new();
    for dz in -2..=2 {
        for dx in -2..=2 {
            items.extend(s.sim.cache.get(&snap, (c0.0 + dx, c0.1 + dz)).iter().cloned());
        }
    }
    let fungus: Vec<Vec3> = items.iter().filter(|it| snap.type_of(it.inst.info[0]).is_some_and(|ty| ty.has_tag("fungus"))).map(|it| it.inst.pos()).collect();
    assert!(fungus.len() >= 3, "fungus grows in the cave: {}", fungus.len());
    for p in &fungus {
        assert!(p.y < t.height(p.x, p.z) - 1.0 && t.void(*p + Vec3::Y * 0.15) < 0.0, "in the cave, not on the land: {p}");
        assert!((p.y - t.floor(p.x, p.y + 0.5, p.z)).abs() < 0.2, "on its floor: {p}");
    }
    // The cricket lives down there.
    let me = ActorId::Npc(id);
    let surface = t.height(room.x, room.y);
    let start = s.sim.actor(me).unwrap().pos;
    assert!(start.y < surface - 2.0, "at home on the cave's floor: {} (surface {surface})", start.y);
    let mut moved = 0.0f32;
    for _ in 0..1200 {
        s.step(0.1);
        let p = s.sim.actor(me).unwrap().pos;
        moved = moved.max((p - start).length());
        assert!(p.y < t.height(p.x, p.z) - 1.0, "it keeps to the cave: {p}");
    }
    assert!(moved > 0.5, "and gets about in it: {moved}");
}

/// A picture of a home dug into a bank (look at it): planned facing into
/// the bank, it turns its door to the open side; the bank stays over it.
#[test]
#[ignore]
fn hillside_home_png() {
    let dir = std::env::var("POCKET_PNG_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let w = world_with("hillpng", 77, bare());
    let src = fixture("physics/earth_house.js").replace("in_ground: 3", "in_ground: 0").replace("earth house", "bank house");
    let house = add_type(&w, &src);
    let h = flat_spot(&w, 50.0);
    let r = crate::world::region_of(h.x, h.z);
    let (ox, oz) = (r.0 as f32 * crate::world::REGION, r.1 as f32 * crate::world::REGION);
    let top = h.y + 4.5;
    let plan = serde_json::json!([{ "what": "bank", "rect": [h.x - ox, h.z - oz + 9.0, 30, 18, 0], "how": "fill", "height": top, "sides": 0.6 }]);
    let ground: Vec<_> = crate::settle::parse(plan.as_array().unwrap(), ox, oz).into_iter().map(|p| (p, None)).collect();
    let st = crate::model::Settlement { x: h.x, z: h.z, road: None, ground, ..Default::default() };
    let region = crate::model::RegionCommit { r, plan_json: "{}".into(), info: Default::default(), characters: vec![], settlement: Some(st) };
    let ok = commit_places(&w, vec![pl(house, h + Vec3::new(0.0, 0.0, 2.5), 0.0)], Some(region));
    let p = ok.snapshot.instances.iter().find(|p| p.type_id == house).unwrap().clone();
    eprintln!("turned {:.0}°, floor {:.2}, ground before {:.2}", p.rot_y.to_degrees(), p.pos.y, h.y);
    let mut s = session(&w, 77, None);
    s.sim.t = crate::render::sky::DAY_SECONDS * 0.42;
    let gpu = crate::render::gpu::Gpu::new().ok();
    let live = Arc::new(Mutex::new(crate::model::Live::default()));
    let mut model = crate::model::WorldModel::load(w.db.clone(), gpu.clone(), live).unwrap();
    let snap = model.snapshot().unwrap();
    s.sim.flip(snap.clone());
    let look = |from: Vec3, to: Vec3| {
        let d = to - from;
        crate::render::Camera { pos: from, yaw: d.x.atan2(d.z), pitch: (d.y / Vec3::new(d.x, 0.0, d.z).length()).atan(), fov_y: 1.05, roll: 0.0 }
    };
    png_of(&mut s, &snap, &gpu, look(Vec3::new(h.x + 5.0, h.y + 2.5, h.z - 12.0), Vec3::new(h.x, h.y + 1.5, h.z + 3.0)), &format!("{dir}/pocket-hillside.png"));
}

/// A house with a basement (a suburban home): its ground floor at the
/// ground before its door, its basement under the ground with the earth
/// round it, and a villager going down the stairs to the basement and up.
#[test]
fn a_house_with_a_basement() {
    let w = world_with("basement", 78, bare());
    let house = add_type(&w, &fixture("physics/basement_house.js"));
    let h = flat_spot(&w, 50.0);
    let ok = commit_places(&w, vec![pl(house, h, 0.0)], None);
    let t = &ok.snapshot.terrain;
    let p = ok.snapshot.instances.iter().find(|p| p.type_id == house).unwrap().clone();
    let g = t.height(h.x, h.z + 4.0);
    assert!((p.pos.y - g).abs() < 0.1 && p.rot_y == 0.0, "its ground floor at the ground: {} vs {g}", p.pos.y);
    assert!(!t.solid(Vec3::new(h.x + 1.0, p.pos.y - 1.5, h.z)), "air in the basement");
    assert!(t.solid(Vec3::new(h.x + 3.05, p.pos.y - 1.5, h.z)), "earth against its walls");
    assert!((t.height(h.x + 5.0, h.z) - t.natural_height(h.x + 5.0, h.z)).abs() < 0.01, "no pit round it");
    let ola = add_char(&w, "Ola", "kind", &[], Vec3::new(h.x + 4.0, 0.0, h.z + 8.0));
    let mut s = session(&w, 78, None);
    calm(&mut s);
    let o = ActorId::Npc(ola);
    let bed = Vec3::new(h.x + 1.5, p.pos.y - 2.6, h.z - 1.5);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(bed.to_array()), run: false });
    s.run(60.0, 0.1);
    let at = s.sim.actor(o).unwrap().pos;
    assert!(Vec3::new(at.x - bed.x, 0.0, at.z - bed.z).length() < 1.5 && (at.y - bed.y).abs() < 0.4, "down in the basement: {at} vs {bed}");
    let out = Vec3::new(h.x + 4.0, t.height(h.x + 4.0, h.z + 9.0), h.z + 9.0);
    s.sim.cast.get_mut(ola).unwrap().plan.push_back(Action::Goto { target: Target::Point(out.to_array()), run: false });
    s.run(60.0, 0.1);
    let at = s.sim.actor(o).unwrap().pos;
    assert!((at - out).length() < 2.0, "and out again: {at} vs {out}");
}

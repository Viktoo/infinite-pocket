//! Things that fly and strike, and things that go off: a spear thrown point
//! first sticks in the ground, a bow shoots someone and is loaded again, a
//! musket ball stops at a wall instead of passing through it between two
//! steps, a grenade lit and thrown blows out a crater and hurts who is
//! near, a shell goes off where it lands, fire sets off what lies in it,
//! and characters do all of it through the same actions.

use super::*;

#[test]
fn a_spear_flies_point_first_sticks_and_pulls_out() {
    let w = world_with("spear", 71, bare());
    let spear = add_type(&w, &fixture("physics/spear.js"));
    let p = flat_spot(&w, 30.0);
    let si = place(&w, spear, p + Vec3::new(0.5, 0.0, 0.0), 0.0);
    let mut s = session(&w, 71, None);
    s.sim.player.pos = p;
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Instance(si) }, 0.5);
    let id = s.sim.player.held.expect("holding the spear");
    let d = Vec3::new(0.0, -0.3, 1.0).normalize();
    let r = act_once(&mut s, ActorId::Player, Action::Throw { at: None, dir: Some(d.to_array()), force: Some(12.0) }, 0.05);
    assert_eq!(r["ok"].as_bool(), Some(true), "thrown: {r}");
    // In flight it points the way it goes.
    let mut pointed = false;
    for _ in 0..8 {
        s.run(0.05, 0.05);
        let t = s.sim.things.get(id).unwrap();
        if t.point_first {
            let along = s.sim.point_dir(id).unwrap().dot(t.vel.normalize());
            assert!(along > 0.95, "it flies point first: {along:.2}");
            pointed = true;
        }
    }
    assert!(pointed, "it flew point first");
    s.run(3.0, 0.05);
    let t = s.sim.things.get(id).unwrap().clone();
    assert!(t.shape.stuck && t.anchored, "it stuck where it landed");
    assert!(!events(&s.sim, "stuck").is_empty(), "told");
    let (tip, _) = s.sim.flight_tip(id).unwrap();
    assert!(tip.y < s.sim.snap.terrain.height(tip.x, tip.z), "its point is in the ground: {:.2} under", s.sim.snap.terrain.height(tip.x, tip.z) - tip.y);
    assert!(s.sim.point_dir(id).unwrap().y < -0.15, "it stands slanting into the ground");
    // Pulled out.
    s.sim.player.pos = ground(&w, t.pos.x, t.pos.z - 0.8);
    let r = act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Thing(id) }, 0.5);
    assert_eq!(r["ok"].as_bool(), Some(true), "pulled out: {r}");
    let t = s.sim.things.get(id).unwrap();
    assert!(!t.shape.stuck && !t.anchored && t.holder == Some(ActorId::Player), "in hand again");
    sound(&s);
}

#[test]
fn a_bow_shoots_hurts_runs_out_and_is_loaded_again() {
    let w = world_with("bow", 72, bare());
    let bow = add_type(&w, &fixture("physics/bow.js"));
    let arrow = add_type(&w, &fixture("physics/arrow.js"));
    let p = flat_spot(&w, 30.0);
    let bi = place(&w, bow, p + Vec3::new(0.5, 0.0, 0.0), 0.0);
    let cid = add_char(&w, "Bo", "brave", &[], p + Vec3::new(0.0, 0.0, 14.0));
    let mut s = session(&w, 72, None);
    s.sim.player.pos = p;
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Instance(bi) }, 0.5);
    let bow_id = s.sim.player.held.unwrap();
    assert_eq!(s.sim.shots_left(bow_id), Some(2), "a full quiver of two");
    let bo = ActorId::Npc(cid);
    s.sim.cast.get_mut(cid).unwrap().a.pos = ground(&w, p.x, p.z + 14.0);
    let health = |s: &Session| s.sim.cast.get(cid).unwrap().props[P_HEALTH];
    let before = health(&s);
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Actor(bo)), at: None }, 2.0);
    assert_eq!(r["ok"].as_bool(), Some(true), "aimed: {r}");
    assert!(!events(&s.sim, "shot").is_empty(), "loosed");
    assert!(health(&s) < before - 0.15, "the arrow wounded Bo: {before:.2} → {:.2}", health(&s));
    assert!(s.sim.log.recent.iter().any(|e| e.kind == "struck" && e.actor == Some(ActorId::Player) && e.data["motion"] == "shot"), "a shot, told as one");
    assert!(s.sim.social.fear(bo, ActorId::Player) > 0.0, "Bo fears the archer");
    // The shot is an arrow of the world's own (it has one by that name).
    assert!(s.sim.things.live().any(|t| t.type_id == arrow && t.origin.made_from.iter().any(|m| m == "test bow")), "an arrow flew");
    assert_eq!(s.sim.shots_left(bow_id), Some(1));
    // Shooting where they look doesn't turn the traveler's view.
    let yaw = s.sim.player.yaw;
    s.sim.act(ActorId::Player, Action::Use { target: None, on: None, at: None }).unwrap();
    s.run(1.0, 0.05);
    assert!((s.sim.player.yaw - yaw).abs() < 1e-4, "the view stayed put: {:.4} → {:.4}", yaw, s.sim.player.yaw);
    act_once(&mut s, ActorId::Player, Action::Drop, 0.2);
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Thing(bow_id) }, 0.3);
    let lying = s.sim.spawn_thing(arrow, s.sim.player.pos + Vec3::new(0.6, 0.0, 0.3), 0.0, 1.0, Default::default(), true).unwrap();
    act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(lying)), at: None }, 0.3);
    assert_eq!(s.sim.shots_left(bow_id), Some(1));
    // Into the air ahead, and then it is empty.
    let far = ground(&w, p.x + 20.0, p.z - 20.0);
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Point(far.to_array())), at: Some(far.to_array()) }, 4.0);
    assert_eq!(r["ok"].as_bool(), Some(true), "shot at the ground: {r}");
    assert_eq!(s.sim.shots_left(bow_id), Some(0));
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Point(far.to_array())), at: Some(far.to_array()) }, 0.5);
    assert_eq!(r["ok"].as_bool(), Some(false), "nothing left: {r}");
    assert!(r["msg"].as_str().unwrap_or("").contains("nothing left"), "{r}");
    // An arrow lying by: loaded.
    let lying = s.sim.spawn_thing(arrow, s.sim.player.pos + Vec3::new(0.6, 0.0, 0.3), 0.0, 1.0, Default::default(), true).unwrap();
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(lying)), at: None }, 0.3);
    assert_eq!(r["ok"].as_bool(), Some(true), "loaded: {r}");
    assert_eq!(s.sim.shots_left(bow_id), Some(1));
    assert!(s.sim.things.get(lying).is_none(), "the arrow went into the quiver");
    sound(&s);
}

/// What is loaded is what flies: a fire arrow put in the bow comes out a
/// fire arrow; a quiver of arrows fills the bow and is used up, and never
/// itself flies.
#[test]
fn what_is_loaded_flies_and_a_quiver_fills() {
    let w = world_with("quiver", 77, bare());
    let bow = add_type(&w, &fixture("physics/bow.js"));
    let arrow = add_type(&w, &fixture("physics/arrow.js"));
    let fire = add_type(&w, &fixture("physics/arrow.js").replace("test arrow", "flaming test arrow").replace("mass: 0.05", "mass: 0.05, fire: 0.8, burns: 0.6"));
    let quiver = add_type(&w, r#"export const meta = { name: "quiver of test arrows", bounds: [0.08, 0.35, 0.08], tags: ["item"], props: { mass: 1 } };
export function sdf(x, y, z, k) { return cylinder(x, y, z, 0.07, 0.33); }
export function color(x, y, z, k) { return rgb(110, 70, 40); }"#);
    let p = flat_spot(&w, 30.0);
    let mut s = session(&w, 77, None);
    s.sim.player.pos = p;
    let b = s.sim.spawn_thing(bow, p + Vec3::new(0.4, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Thing(b) }, 0.3);
    let far = ground(&w, p.x + 15.0, p.z + 15.0);
    let shoot = |s: &mut Session| act_once(s, ActorId::Player, Action::Use { target: None, on: Some(Target::Point(far.to_array())), at: Some(far.to_array()) }, 1.0);
    shoot(&mut s);
    shoot(&mut s);
    assert_eq!(s.sim.shots_left(b), Some(0));
    // A quiver of arrows: it fills the bow, and is gone.
    let q = s.sim.spawn_thing(quiver, s.sim.player.pos + Vec3::new(0.5, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(q)), at: None }, 0.3);
    assert_eq!(r["ok"].as_bool(), Some(true), "filled: {r}");
    assert_eq!(s.sim.shots_left(b), Some(2));
    assert!(s.sim.things.get(q).is_none(), "the quiver went into it");
    // Empty one, put a flaming arrow in, and it is what flies.
    shoot(&mut s);
    let f = s.sim.spawn_thing(fire, s.sim.player.pos + Vec3::new(0.5, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(f)), at: None }, 0.3);
    assert_eq!(r["ok"].as_bool(), Some(true), "loaded: {r}");
    let before = s.sim.things.live().filter(|t| t.type_id == fire).count();
    shoot(&mut s);
    assert_eq!(s.sim.things.live().filter(|t| t.type_id == fire).count(), before + 1, "a flaming arrow flew");
    // And the next is a plain one again.
    let plain = s.sim.things.live().filter(|t| t.type_id == arrow).count();
    shoot(&mut s);
    assert_eq!(s.sim.things.live().filter(|t| t.type_id == arrow).count(), plain + 1, "then its own arrows");
    assert!(!s.sim.things.live().any(|t| t.type_id == quiver), "no quiver ever flew");
    // A flaming arrow shot into a crate of TNT sets it off.
    let tnt = add_type(&w, r#"export const meta = { name: "crate of tnt", bounds: [0.3, 0.25, 0.3], tags: ["item"], props: { mass: 12, burns: 0.2 }, burst: { radius: 5, hurt: 0.9, crater: 1, fuse: 0.5 } };
export function sdf(x, y, z, k) { return box(x, y, z, 0.3, 0.25, 0.3); }
export function color(x, y, z, k) { return rgb(170, 40, 30); }"#);
    let _ = tnt;
    let mut s = session(&w, 78, None);
    let tnt = s.sim.type_by_name("crate of tnt").unwrap().id;
    s.sim.player.pos = p;
    let b = s.sim.spawn_thing(bow, p + Vec3::new(0.4, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Thing(b) }, 0.3);
    shoot(&mut s);
    let f = s.sim.spawn_thing(fire, s.sim.player.pos + Vec3::new(0.5, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(f)), at: None }, 0.3);
    let crate_at = ground(&w, p.x + 12.0, p.z);
    let c = s.sim.spawn_thing(tnt, crate_at, 0.0, 1.0, Default::default(), true).unwrap();
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Thing(c)), at: None }, 3.0);
    assert_eq!(r["ok"].as_bool(), Some(true), "shot at the crate: {r}");
    s.run(1.5, 0.05);
    assert!(s.sim.things.get(c).is_none(), "the crate went off");
    assert!(!events(&s.sim, "burst").is_empty());
    sound(&s);
}

#[test]
fn a_musket_ball_stops_at_the_wall() {
    let w = world_with("musket", 73, bare());
    let musket = add_type(&w, &fixture("physics/musket.js"));
    let hut = add_type(&w, &fixture("physics/hut.js"));
    let h = flat_spot(&w, 50.0);
    let hi = place(&w, hut, h, 0.0);
    let p = ground(&w, h.x - 16.0, h.z);
    let mi = place(&w, musket, p + Vec3::new(0.5, 0.0, 0.0), 0.0);
    let mut s = session(&w, 73, None);
    s.sim.player.pos = p;
    // A wooden hut of some weight.
    let hut_id = s.sim.liven(&Target::Instance(hi)).unwrap();
    let ht = s.sim.things.get_mut(hut_id).unwrap();
    (ht.props[P_BURNS], ht.props[P_MASS]) = (0.6, 4000.0);
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Instance(mi) }, 0.5);
    let wall = Vec3::new(h.x - 3.2, h.y + 1.4, h.z);
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Point(wall.to_array())), at: Some(wall.to_array()) }, 1.5);
    assert_eq!(r["ok"].as_bool(), Some(true), "fired: {r}");
    // No lead ball is written yet (no LLM): the built-in slug flies for it.
    assert!(events(&s.sim, "shot").iter().any(|e| e.data["ammo"] == "slug"), "a slug for a gun");
    let stuck = events(&s.sim, "stuck");
    let at = Vec3::from_array(stuck.first().expect("it stuck in the wall").pos.unwrap());
    assert!(at.x < h.x - 2.0, "this side of the wall: {:.2} m from the hut's middle", at.x - h.x);
    // Too small to see, it is cleared away; nothing got through.
    let slug = builtin_id(&w, "slug");
    assert!(!s.sim.things.live().any(|t| t.type_id == slug && t.pos.x > h.x - 2.0), "nothing passed through");
    // The hut took the knock: a little worn, and a hole bored through its
    // wooden wall, where there was wall before.
    let ht = s.sim.things.get(hut_id).unwrap();
    assert!(ht.props[P_HEALTH] < 1.0, "worn: {:.3}", ht.props[P_HEALTH]);
    let b = *ht.shape.bores.first().expect("a hole bored through");
    let ty = s.sim.snap.type_of(ht.type_id).unwrap().clone();
    let holed = crate::sim::render::thing_solid(ht, ty.clone());
    let plain = crate::world::Solid::new(holed.inst, ty);
    let mid = holed.inst.from_local(Vec3::new(b[0] + b[4], b[1] + b[5], b[2] + b[6]) * 0.5);
    assert!(plain.exact_sdf(mid) < 0.0, "the bore runs through the wall");
    // All along it, nothing is left: you can see through.
    let (a, z) = (holed.inst.from_local(Vec3::new(b[0], b[1], b[2])), holed.inst.from_local(Vec3::new(b[4], b[5], b[6])));
    assert!((0..=20).all(|i| holed.exact_sdf(a.lerp(z, i as f32 / 20.0)) > 0.0), "a hole right through");
    sound(&s);
}

#[test]
fn a_grenade_lit_and_thrown_blows_out_a_crater() {
    let w = world_with("grenade", 74, bare());
    let grenade = add_type(&w, &fixture("physics/grenade.js"));
    let ball = add_type(&w, &fixture("sims/ball.js"));
    let p = flat_spot(&w, 30.0);
    let gi = place(&w, grenade, p + Vec3::new(0.5, 0.0, 0.0), 0.0);
    let spot = ground(&w, p.x, p.z + 9.0);
    // Bo stands a little past where it lands, where it rolls on to.
    let cid = add_char(&w, "Bo", "brave", &[], spot + Vec3::new(1.5, 0.0, 3.0));
    let bi = place(&w, ball, spot + Vec3::new(-1.2, 0.0, 3.0), 0.0);
    let mut s = session(&w, 74, None);
    s.sim.player.pos = p;
    s.sim.cast.get_mut(cid).unwrap().a.pos = ground(&w, spot.x + 1.5, spot.z + 3.0);
    act_once(&mut s, ActorId::Player, Action::Hold { target: Target::Instance(gi) }, 0.3);
    let id = s.sim.player.held.unwrap();
    let r = act_once(&mut s, ActorId::Player, Action::Use { target: Some(Target::Thing(id)), on: None, at: None }, 0.05);
    assert_eq!(r["ok"].as_bool(), Some(true), "lit: {r}");
    assert!(s.sim.things.get(id).unwrap().shape.fuse_at.is_some());
    let ball_id = s.sim.liven(&Target::Instance(bi)).unwrap();
    let ball_at = s.sim.things.get(ball_id).unwrap().pos;
    let health = |s: &Session| s.sim.cast.get(cid).unwrap().props[P_HEALTH];
    let before = health(&s);
    // Heard: the fuse fizzing, the whoosh as it flies, the blast.
    let mut foley = crate::audio::foley::Foley::default();
    let mut cmds = Vec::new();
    let (mut fizz, mut whoosh, mut boom) = (false, false, 0.0f32);
    let r = s.sim.act(ActorId::Player, Action::Throw { at: Some(Target::Point(spot.to_array())), dir: None, force: None });
    assert!(r.is_ok_and(|o| o.ok), "thrown");
    for _ in 0..180 {
        s.sim.step(1.0 / 60.0);
        for c in &s.sim.sounds {
            if let crate::audio::Heard::Hit { mass, .. } = c.what {
                boom = boom.max(mass);
            }
        }
        foley.frame(&mut s.sim, 1.0 / 60.0, &mut cmds);
        for c in cmds.drain(..) {
            if let crate::audio::mix::Cmd::Texture { key, drive, .. } = c {
                fizz |= key >> 48 == 6 && drive > 0.0;
                whoosh |= key >> 48 == 5 && drive > 0.0;
            }
        }
    }
    assert!(fizz && whoosh, "fizz {fizz}, whoosh {whoosh}");
    assert!(boom > 500.0, "a great bang: {boom}");
    let burst = events(&s.sim, "burst");
    assert_eq!(burst.len(), 1, "it went off");
    assert_eq!(burst[0].actor, Some(ActorId::Player), "by the one who lit it");
    assert!(s.sim.things.get(id).is_none(), "and is gone");
    let at = Vec3::from_array(burst[0].pos.unwrap());
    let dug = s.sim.snap.terrain.natural_height(at.x, at.z) - s.sim.snap.terrain.height(at.x, at.z);
    assert!(dug > 0.4, "a crater {dug:.2} m deep");
    assert!(health(&s) < before - 0.1, "Bo was hurt: {before:.2} → {:.2}", health(&s));
    assert!(s.sim.things.get(ball_id).is_some_and(|t| (t.pos - ball_at).length() > 1.0), "the ball was thrown");
    sound(&s);
}

#[test]
fn a_shell_goes_off_where_it_lands_and_fire_sets_off_a_fuse() {
    let w = world_with("shell", 75, bare());
    let shell = add_type(&w, &fixture("physics/shell.js"));
    let grenade = add_type(&w, &fixture("physics/grenade.js"));
    let p = flat_spot(&w, 30.0);
    let mut s = session(&w, 75, None);
    s.sim.player.pos = p;
    let at = p + Vec3::new(10.0, 6.0, 0.0);
    let id = s.sim.spawn_thing(shell, at, 0.0, 1.0, Default::default(), false).unwrap();
    if let Some(t) = s.sim.things.get_mut(id) {
        t.vel = Vec3::new(0.0, -12.0, 0.0);
        t.asleep = false;
        t.thrown_by = Some((ActorId::Player, s.sim.t));
    }
    s.run(1.5, 0.05);
    assert_eq!(events(&s.sim, "burst").len(), 1, "it went off on landing");
    assert!(s.sim.things.get(id).is_none());
    // A grenade in a fire: it lights, and goes off after its fuse.
    let g = s.sim.spawn_thing(grenade, p + Vec3::new(-12.0, 0.0, 0.0), 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.things.get_mut(g).unwrap().props[P_TEMP] = 400.0;
    s.run(0.5, 0.05);
    assert!(s.sim.things.get(g).is_some_and(|t| t.shape.fuse_at.is_some()), "the heat lit it");
    assert_eq!(events(&s.sim, "burst").len(), 1, "not yet");
    s.run(2.5, 0.05);
    assert_eq!(events(&s.sim, "burst").len(), 2, "then it went off");
    sound(&s);
}

#[test]
fn characters_shoot_light_and_load_through_their_plans() {
    let steps = |v: serde_json::Value| super::super::npc::parse_step(&v).unwrap();
    assert!(matches!(steps(serde_json::json!({ "do": "shoot", "at": "Bo" })), Action::Use { target: None, on: Some(_), .. }));
    assert!(matches!(steps(serde_json::json!({ "do": "load", "target": "arrow" })), Action::Use { target: None, on: Some(_), .. }));
    assert!(matches!(steps(serde_json::json!({ "do": "light", "target": "dynamite" })), Action::Use { target: Some(_), on: None, .. }));
    assert!(matches!(steps(serde_json::json!({ "do": "hurl", "at": "wolf" })), Action::Throw { at: Some(_), .. }));

    let w = world_with("archer", 76, bare());
    let bow = add_type(&w, &fixture("physics/bow.js"));
    add_type(&w, &fixture("physics/arrow.js"));
    let p = flat_spot(&w, 30.0);
    let ola = add_char(&w, "Ola", "a hunter", &[], p);
    let bo = add_char(&w, "Bo", "brave", &[], p + Vec3::new(0.0, 0.0, 12.0));
    let mut s = session(&w, 76, None);
    s.sim.player.pos = p + Vec3::new(-30.0, 0.0, 0.0);
    s.sim.cast.get_mut(ola).unwrap().a.pos = p;
    s.sim.cast.get_mut(bo).unwrap().a.pos = ground(&w, p.x, p.z + 12.0);
    let b = s.sim.spawn_thing(bow, p, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.hand_to(ActorId::Npc(ola), b);
    assert_eq!(s.sim.cast.get(ola).unwrap().a.held, Some(b), "Ola holds the bow");
    assert!(s.sim.armed(ActorId::Npc(ola)).is_some(), "a bow with arrows in it is a weapon");
    assert!(s.sim.arms_note(b).unwrap().contains("shoots test arrows"), "{:?}", s.sim.arms_note(b));
    let before = s.sim.cast.get(bo).unwrap().props[P_HEALTH];
    s.sim.plan(ActorId::Npc(ola), vec![steps(serde_json::json!({ "do": "shoot", "at": "Bo" }))], "shoot Bo", false);
    s.run(3.0, 0.05);
    assert!(s.sim.log.recent.iter().any(|e| e.kind == "shot" && e.actor == Some(ActorId::Npc(ola))), "Ola shot");
    assert!(s.sim.cast.get(bo).unwrap().props[P_HEALTH] < before - 0.1, "and hit Bo");
    sound(&s);
}

#[test]
fn shapes_say_how_they_shoot_and_go_off() {
    let ok = crate::lang::compile(&fixture("physics/bow.js")).unwrap();
    let tool = ok.meta.tool.unwrap();
    assert!(tool.motions.iter().any(|m| m == "shoot"), "a launcher shoots: {:?}", tool.motions);
    assert_eq!(tool.launch.unwrap().holds, 2);
    let g = crate::lang::compile(&fixture("physics/grenade.js")).unwrap().meta.burst.unwrap();
    assert_eq!(g.push, 18.0);
    let s = crate::lang::compile(&fixture("physics/shell.js")).unwrap().meta.burst.unwrap();
    assert!(s.impact && (s.push - 12.0).abs() < 1e-4, "push defaults to 3 × radius");
    let bad = r#"export const meta = { name: "gun", bounds: [0.1, 0.1, 0.5], tool: { grip: [0, 0, 0], tip: [0, 0, 0.4], motions: ["shoot"] } };
export function sdf(x, y, z, k) { return box(x, y, z, 0.05, 0.05, 0.4); }"#;
    let e = crate::lang::compile(bad).unwrap_err();
    assert!(crate::lang::format_diags(&e).contains("launch"), "{}", crate::lang::format_diags(&e));
}

#[test]
fn a_broken_thing_falls_into_different_parts_and_a_grenade_does_not_shatter() {
    let w = world_with("smash", 77, bare());
    let lantern = add_type(&w, &fixture("sims/lantern.js"));
    let grenade = add_type(&w, &fixture("physics/grenade.js"));
    let p = flat_spot(&w, 30.0);
    let li = place(&w, lantern, p + Vec3::new(0.5, 0.0, 0.0), 0.0);
    let gi = place(&w, grenade, p + Vec3::new(-0.5, 0.0, 0.0), 0.0);
    let mut s = session(&w, 77, None);
    let l = s.sim.liven(&Target::Instance(li)).unwrap();
    s.sim.break_thing(l);
    let pieces: Vec<_> = s.sim.things.live().filter(|t| t.type_id == lantern).map(|t| t.shape.cuts.clone()).collect();
    assert!(pieces.len() >= 2, "it broke into pieces");
    assert!(pieces.iter().all(|c| !c.is_empty()), "each piece is cut out of the whole");
    assert!(pieces.windows(2).all(|w| w[0] != w[1]), "no two pieces alike");
    // Struck hard, something that goes off doesn't become more of itself.
    let g = s.sim.liven(&Target::Instance(gi)).unwrap();
    s.sim.things.get_mut(g).unwrap().props[P_FRAGILE] = 1.0;
    s.sim.impact(g, 20.0, None);
    assert_eq!(s.sim.things.live().filter(|t| t.type_id == grenade).count(), 1, "still one grenade");
}

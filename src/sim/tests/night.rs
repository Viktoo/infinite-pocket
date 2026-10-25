//! The night hunt: night walkers that hurt, phantoms that arm themselves,
//! strike, fall, rise and learn; armour that softens a blow; the traveler's
//! health.

use super::*;
use crate::sim::Request;

/// The phantoms about (alive).
fn phantoms(s: &Session) -> Vec<i64> {
    s.sim.cast.npcs.iter().filter(|n| !n.dead && n.species.hostile).map(|n| n.def.id).collect()
}

/// Events of `kind` done by `who`.
fn by(all: &Arc<Mutex<Vec<SimEvent>>>, kind: &str, who: i64) -> Vec<SimEvent> {
    of(all, kind).into_iter().filter(|e| e.actor == Some(ActorId::Npc(who))).collect()
}

/// Send every phantom far off, where it can't find the traveler (so a
/// test of the walker is about the walker).
fn send_phantoms_away(s: &mut Session) {
    let far = s.sim.player.pos + Vec3::new(400.0, 0.0, 400.0);
    for id in phantoms(s) {
        let n = s.sim.cast.get_mut(id).unwrap();
        n.a.pos = far;
        n.plan.clear();
        n.a.task = None;
        n.think_at = f64::MAX;
    }
}

/// On normal, a night walker comes out of the dark at dusk, creeps up from
/// behind, hurts the traveler, and is gone at dawn; the next night the same
/// one comes back. A phantom comes too.
#[test]
fn the_dark_comes_at_dusk_hurts_you_and_leaves_at_dawn() {
    let w = world("dark", 7);
    let mut s = session(&w, 7, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = w.spawn;
    s.sim.player.yaw = 0.0;
    s.sim.t = before_dusk(1.0, 2.0);
    let all = record(&mut s);
    for _ in 0..40 {
        s.step(0.1);
    }
    assert!(s.sim.dark(), "night has fallen");
    let dark = the_dark(&s);
    assert_eq!(dark.len(), 1, "one walker on easy: {:?}", of(&all, "dark_came"));
    assert_eq!(phantoms(&s).len(), 1, "and one phantom");
    send_phantoms_away(&mut s);
    let h = s.sim.cast.get(dark[0]).unwrap();
    assert!(h.here() && h.species.about(true) && !h.species.about(false));
    let d0 = (h.a.pos - s.sim.player.pos).length();
    assert!(d0 > 35.0 && !s.sim.player_sees(h.a.pos), "it comes out of sight, behind: {d0:.0} m");
    assert_eq!(s.sim.health(), 1.0);
    // The traveler stands still, looking away: it reaches them.
    let mut touched = false;
    for _ in 0..(120.0 / 0.1) as usize {
        s.step(0.1);
        if !by(&all, "touched", dark[0]).is_empty() {
            touched = true;
            break;
        }
    }
    assert!(touched, "it reached the traveler; it is {:?}", s.sim.cast.get(dark[0]).map(|n| (n.doing.clone(), (n.a.pos - s.sim.player.pos).length())));
    let hp = s.sim.health();
    assert!((hp - 0.75).abs() < 0.01, "its touch takes a quarter of the traveler's health: {hp:.2}");
    assert!(s.sim.night_status().iter().any(|p| p.contains("1 walker")), "{:?}", s.sim.night_status());
    // It draws back after.
    for _ in 0..30 {
        s.step(0.1);
    }
    assert_eq!(by(&all, "touched", dark[0]).len(), 1, "one touch, then it draws back");
    // Dawn: it is gone (out of sight), the night is got through.
    s.sim.player.yaw = std::f32::consts::PI;
    s.sim.t = before_dawn(1.0, 1.0);
    for _ in 0..(40.0 / 0.1) as usize {
        s.step(0.1);
    }
    assert!(!s.sim.dark());
    assert!(s.sim.cast.get(dark[0]).is_some_and(|n| n.away), "gone with the light");
    let got = of(&all, "survived_night");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].data["untouched"], false);
    let low = s.sim.health();
    run_for(&mut s, 30.0);
    assert!(s.sim.health() > low, "the traveler mends by day");
    // The next dusk it comes back (the same one), near the traveler again.
    s.sim.t = before_dusk(2.0, 1.0);
    for _ in 0..30 {
        s.step(0.1);
    }
    assert_eq!(the_dark(&s), dark, "the same one comes back");
    assert!(s.sim.cast.get(dark[0]).is_some_and(|n| n.here() && (n.a.pos - s.sim.player.pos).length() < 70.0));
    sound(&s);
}

/// Watched, the night walker stands still however near it is; only the
/// middle of the view counts as watching, so a little to the side it
/// creeps closer.
#[test]
fn the_dark_freezes_when_watched_even_up_close() {
    let w = world("dark close", 9);
    let mut s = session(&w, 9, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = w.spawn;
    s.sim.player.yaw = 0.0;
    s.sim.t = before_dusk(1.0, 2.0);
    for _ in 0..40 {
        s.step(0.1);
    }
    send_phantoms_away(&mut s);
    let dark = the_dark(&s);
    assert_eq!(dark.len(), 1);
    let put = |s: &mut Session, p: Vec3| {
        let p = Vec3::new(p.x, s.sim.snap.terrain.height(p.x, p.z), p.z);
        let n = s.sim.cast.get_mut(dark[0]).unwrap();
        n.a.pos = p;
        n.think_at = 0.0;
    };
    let me = s.sim.player.pos;
    let f = s.sim.player.forward();
    let all = record(&mut s);
    put(&mut s, me + f * 2.5);
    let at = s.sim.cast.get(dark[0]).unwrap().a.pos;
    for _ in 0..30 {
        s.step(0.1);
    }
    let moved = (s.sim.cast.get(dark[0]).unwrap().a.pos - at).length();
    assert!(moved < 0.05, "looked at from 2.5 m it stays put: moved {moved:.2} m");
    assert!(of(&all, "touched").is_empty(), "out of arm's reach: no touch");
    // Within arm's reach looking doesn't save you.
    put(&mut s, me + f * 1.2);
    s.step(0.1);
    assert_eq!(of(&all, "touched").len(), 1, "within arm's reach it touches you, watched or not");
    if let Some(n) = s.sim.cast.get_mut(dark[0]) {
        n.touched_at = -1e9;
    }
    // Over half way out to the edge of the view (once counted as looked at), it comes on.
    let r = s.sim.player.right();
    let slope = 1.0;
    put(&mut s, me + f * 10.0 + r * 10.0 * slope * 0.6);
    let from = (s.sim.cast.get(dark[0]).unwrap().a.pos - me).length();
    for _ in 0..30 {
        s.step(0.1);
    }
    let to = (s.sim.cast.get(dark[0]).unwrap().a.pos - me).length();
    assert!(to < from - 0.3, "a little to the side it creeps closer: {from:.1} → {to:.1} m");
}

/// The night walker is the one built-in kind, at the one pace; signs never
/// say where, and what the game says of where is true.
#[test]
fn night_walkers_are_named_paced_and_placed_truly() {
    let h = super::super::night::walker_species();
    assert_eq!(h.name, "night walker");
    assert!(h.touch.harms() && h.moves_unseen && h.want == "traveler" && h.shuns == vec!["light".to_string()]);
    assert!(h.signs.len() >= 4 && h.signs.iter().all(|l| !l.to_lowercase().contains("behind")), "stock signs don't say where");
    let w = world("walkers", 21);
    let mut s = session(&w, 21, None);
    s.sim.cfg.difficulty = 3;
    s.sim.player.pos = w.spawn;
    s.sim.player.yaw = 0.0;
    let me = s.sim.player.pos;
    let (f, r) = (s.sim.player.forward(), s.sim.player.right());
    assert_eq!(s.sim.where_is(me - f * 30.0), "behind you");
    assert_eq!(s.sim.where_is(me - f * 5.0), "close behind you");
    assert_eq!(s.sim.where_is(me + r * 30.0), "off to your right");
    assert_eq!(s.sim.where_is(me - r * 30.0), "off to your left");
    assert_eq!(s.sim.where_is(me + f * 30.0), "ahead of you, in the dark");
    // On hard three come at dusk, and three phantoms.
    s.sim.t = before_dusk(1.0, 2.0);
    for _ in 0..40 {
        s.step(0.1);
    }
    let dark = the_dark(&s);
    assert_eq!(dark.len(), 3);
    assert_eq!(phantoms(&s).len(), 3);
    let n = s.sim.cast.get(dark[0]).unwrap();
    assert_eq!(n.species.name, "night walker");
    assert_eq!(s.sim.actor_name(ActorId::Npc(dark[0])), "the night walker");
    let names: Vec<String> = phantoms(&s).iter().map(|id| s.sim.actor_name(ActorId::Npc(*id))).collect();
    assert!(names[0] != names[1], "each phantom its own name: {names:?}");
}

/// Gone with the light stays gone: reopened by day, the night walker isn't
/// back, and nothing says it left again.
#[test]
fn the_dark_stays_away_by_day_after_reopening() {
    let w = world("dark away", 13);
    let h = {
        let mut s = session(&w, 13, None);
        s.sim.cfg.difficulty = 3;
        s.sim.player.pos = w.spawn;
        s.sim.t = before_dusk(1.0, 1.0);
        run_for(&mut s, 3.0);
        let h = the_dark(&s)[0];
        s.sim.t = before_dawn(1.0, 1.0);
        run_for(&mut s, 3.0);
        assert!(s.sim.cast.get(h).is_some_and(|n| n.away), "gone at dawn");
        s.sim.t = crate::render::sky::DAY_SECONDS * 2.45;
        s.save();
        h
    };
    let mut s = session(&w, 13, None);
    assert!(!s.sim.dark());
    s.step(0.1);
    assert!(s.sim.cast.get(h).is_some_and(|n| n.away && !n.here()), "still away after reopening");
    let told: Vec<String> = s.sim.drain_notes().iter().map(|n| n.text()).filter(|t| t.contains("gone with the light")).collect();
    assert!(told.is_empty(), "nothing tells of it leaving again: {told:?}");
}

/// Peaceful: nothing comes, and the status line says nothing of the night.
#[test]
fn peaceful_nights_are_quiet() {
    let w = world("peace", 8);
    let mut s = session(&w, 8, None);
    assert_eq!(s.sim.cfg.difficulty, 0, "peaceful by default");
    s.sim.t = before_dusk(1.0, 1.0);
    for _ in 0..(60.0 / 0.1) as usize {
        s.step(0.1);
    }
    assert!(s.sim.dark());
    assert!(the_dark(&s).is_empty() && phantoms(&s).is_empty() && events(&s.sim, "dark_came").is_empty());
    assert!(s.sim.night_status().is_empty(), "{:?}", s.sim.night_status());
    assert_eq!(s.sim.health(), 1.0);
}

/// Older worlds' night horrors (LLM-written, draining charges and
/// corrupting) come back as night walkers that hurt, keeping their names.
#[test]
fn old_horrors_become_night_walkers() {
    let w = world("old dark", 14);
    let mut s = session(&w, 14, None);
    s.sim.cfg.difficulty = 2;
    let old: crate::world::species::Species = serde_json::from_value(serde_json::json!({
        "name": "night walker 2", "body": "night walker", "active": "night", "want": "traveler",
        "touch": { "charges": 1.0, "corruption": 0.5 }, "description": "a thing of many joints"
    }))
    .unwrap();
    assert!(!old.touch.harms(), "an old touch harms no more by itself");
    s.sim.store_species(old);
    let at = w.spawn + Vec3::new(30.0, 0.0, 0.0);
    let persona = crate::world::Persona { name: "the night walker 2".into(), species: "night walker 2".into(), ..Default::default() };
    let id = s.sim.add_being(persona, at, Default::default()).unwrap();
    s.sim.night.walkers = vec![id];
    s.sim.night.was_night = None;
    s.step(0.1);
    let n = s.sim.cast.get(id).unwrap();
    assert_eq!(n.species.name, "night walker 2");
    assert!(n.species.touch.harms() && n.species.moves_unseen && n.species.touch.hurt > 0.2, "{:?}", n.species.touch);
}

/// What is worn softens a blow (by its `protect`), and the traveler mends.
#[test]
fn armour_softens_blows_and_the_traveler_mends() {
    let w = world_with("armour", 72, bare());
    let plate = add_type(&w, &fixture("layers/breastplate.js"));
    let p = flat_spot(&w, 20.0);
    let mut s = session(&w, 72, None);
    s.sim.player.pos = p;
    let walker = super::super::night::walker_species();
    s.sim.store_species(walker);
    let persona = crate::world::Persona { name: "the night walker".into(), species: "night walker".into(), ..Default::default() };
    let id = s.sim.add_being(persona, p + Vec3::new(0.0, 0.0, 1.5), Default::default()).unwrap();
    s.sim.touch(id, ActorId::Player);
    s.sim.cast.get_mut(id).unwrap().think_at = f64::MAX;
    let bare_hurt = 1.0 - s.sim.health();
    assert!((bare_hurt - 0.25).abs() < 0.01, "{bare_hurt}");
    // An iron breastplate that protects (as its maker said), put on.
    let pid = s.sim.spawn_thing(plate, p + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.things.get_mut(pid).unwrap().props[P_PROTECT] = 0.5;
    s.sim.wear(ActorId::Player, ActorId::Player, pid).unwrap();
    assert!((s.sim.protection(ActorId::Player) - 0.5).abs() < 1e-4);
    let before = s.sim.health();
    s.sim.touch(id, ActorId::Player);
    let worn_hurt = before - s.sim.health();
    assert!((worn_hurt - bare_hurt * 0.5).abs() < 0.01, "half the hurt through the plate: {worn_hurt:.3}");
    // A day mends it (with the walker gone).
    s.sim.cast.get_mut(id).unwrap().a.pos = p + Vec3::new(300.0, 0.0, 300.0);
    s.sim.t = crate::render::sky::DAY_SECONDS * 1.4;
    let low = s.sim.health();
    run_for(&mut s, 120.0);
    assert!(s.sim.health() > low, "mending: {low:.3} → {:.3}", s.sim.health());
    // At none left, nothing more happens (for now): the traveler stays.
    s.sim.night.wounds = 0.95;
    s.sim.touch(id, ActorId::Player);
    assert_eq!(s.sim.health(), 0.0);
    s.step(0.1);
    assert_eq!(s.sim.health(), 0.0f32.max(s.sim.health()));
}

/// A phantom comes at dusk, takes up a sword lying near, and strikes the
/// traveler with it; it fears nothing, and doesn't run when struck.
#[test]
fn a_phantom_arms_itself_and_strikes() {
    let w = world_with("phantom", 73, bare());
    let sword = add_type(&w, &fixture("physics/sword.js"));
    let p = flat_spot(&w, 30.0);
    let mut s = session(&w, 73, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = p;
    s.sim.player.yaw = 0.0;
    s.sim.t = before_dusk(1.0, 1.0);
    run_for(&mut s, 2.0);
    let ph = phantoms(&s);
    assert_eq!(ph.len(), 1, "one phantom on easy");
    let id = ph[0];
    let n = s.sim.cast.get(id).unwrap();
    assert_eq!(n.species.name, "phantom");
    assert!(n.species.hostile && n.species.mind == crate::world::species::Mind::Sapient);
    assert!(s.sim.drive_line(id).is_some_and(|l| l.contains("cannot truly die")));
    assert!(s.sim.decide_context(id, "x").contains("You mean harm"), "its mind knows what it is");
    send_phantoms_away(&mut s);
    // Bring it back 16 m off with a sword lying a few steps from it.
    for w in the_dark(&s) {
        s.sim.cast.get_mut(w).unwrap().a.pos = p + Vec3::new(300.0, 0.0, -300.0);
        s.sim.cast.get_mut(w).unwrap().think_at = f64::MAX;
    }
    let from = ground(&w, p.x + 16.0, p.z);
    {
        let n = s.sim.cast.get_mut(id).unwrap();
        n.a.pos = from;
        n.think_at = 0.0;
    }
    let sw = s.sim.spawn_thing(sword, ground(&w, p.x + 13.0, p.z + 2.0) + Vec3::Y * 0.2, 0.0, 1.0, Default::default(), true).unwrap();
    let all = record(&mut s);
    let mut struck = false;
    for _ in 0..(90.0 / 0.1) as usize {
        s.step(0.1);
        if by(&all, "struck", id).iter().any(|e| e.subject.as_deref() == Some("player")) {
            struck = true;
            break;
        }
    }
    let n = s.sim.cast.get(id).unwrap();
    assert_eq!(n.a.held, Some(sw), "it took up the sword: {}", n.doing);
    assert!(struck, "it struck the traveler with it: {} at {:.1} m", n.doing, (n.a.pos - s.sim.player.pos).length());
    assert!(s.sim.health() < 1.0, "it hurt");
    // It fears nothing: struck, it doesn't run.
    assert_eq!(s.sim.menace(ActorId::Npc(id), ActorId::Player), 0.0);
    sound(&s);
}

/// Struck down, a phantom leaves its gear where it falls; at dawn it
/// learns from its night (a memory of who struck it down, with what); at
/// dusk it rises again, whole, without what it dropped.
#[test]
fn a_struck_down_phantom_drops_its_gear_learns_and_rises() {
    let w = world_with("phantom falls", 74, bare());
    let sword = add_type(&w, &fixture("physics/sword.js"));
    let plate = add_type(&w, &fixture("layers/breastplate.js"));
    let p = flat_spot(&w, 30.0);
    let mut s = session(&w, 74, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = p;
    s.sim.t = before_dusk(1.0, 1.0);
    run_for(&mut s, 2.0);
    let id = phantoms(&s)[0];
    for x in the_dark(&s) {
        s.sim.cast.get_mut(x).unwrap().a.pos = p + Vec3::new(300.0, 0.0, -300.0);
        s.sim.cast.get_mut(x).unwrap().think_at = f64::MAX;
    }
    // It wears a breastplate.
    let at = ground(&w, p.x, p.z + 1.6);
    {
        let n = s.sim.cast.get_mut(id).unwrap();
        n.a.pos = at;
        n.think_at = f64::MAX;
    }
    let pl = s.sim.spawn_thing(plate, at + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.wear(ActorId::Npc(id), ActorId::Npc(id), pl).unwrap();
    // The traveler strikes it down with a sword.
    let sw = s.sim.spawn_thing(sword, p + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.hand_to(ActorId::Player, sw);
    for _ in 0..80 {
        if s.sim.cast.get(id).unwrap().dead {
            break;
        }
        {
            let n = s.sim.cast.get_mut(id).unwrap();
            n.a.pos = at;
            n.a.task = None;
            n.plan.clear();
            n.think_at = f64::MAX;
        }
        act_once(&mut s, ActorId::Player, Action::Use { target: None, on: Some(Target::Actor(ActorId::Npc(id))), at: None }, 1.2);
    }
    assert!(s.sim.cast.get(id).unwrap().dead, "struck down (the dark's own can always be)");
    assert!(s.sim.things.get(pl).is_some_and(|t| t.worn.is_none()), "its breastplate is left for the taking");
    let died = events(&s.sim, "died");
    assert!(died.iter().any(|e| e.data["by"] == "player"), "{:?}", died.iter().map(|e| e.data.clone()).collect::<Vec<_>>());
    // Dawn: it learns.
    s.sim.drain_requests();
    s.sim.t = before_dawn(1.0, 0.5);
    let mut mems: Vec<String> = Vec::new();
    for _ in 0..12 {
        s.sim.step(0.25);
        mems.extend(s.sim.drain_requests().into_iter().filter_map(|r| match r {
            Request::Witness { cid, text, .. } if cid == id => Some(text),
            _ => None,
        }));
    }
    let lesson = mems.iter().find(|m| m.starts_with("Night 1:")).unwrap_or_else(|| panic!("a lesson: {mems:?}"));
    assert!(lesson.contains("the traveler with the test sword struck me down") && lesson.contains("Next night I will"), "{lesson}");
    // Dusk: it rises, whole, out of sight, without what it dropped.
    s.sim.t = before_dusk(2.0, 1.0);
    run_for(&mut s, 3.0);
    let n = s.sim.cast.get(id).unwrap();
    assert!(!n.dead && n.props[P_HEALTH] >= 0.99, "it rose again, whole");
    assert!(n.here() && !s.sim.player_sees(n.a.pos) || n.away, "{:?}", n.a.pos);
    assert!(s.sim.worn_by(ActorId::Npc(id)).is_empty(), "without the breastplate it dropped");
    assert!(s.sim.things.live().all(|t| t.origin.remains != Some(id)), "its body is gone");
}

/// A phantom makes one thing a night; the next night it may make another.
#[test]
fn a_phantom_makes_one_thing_a_night() {
    let w = world("phantom makes", 75);
    let mut s = session(&w, 75, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = w.spawn;
    s.sim.t = before_dusk(1.0, 1.0);
    run_for(&mut s, 2.0);
    let id = phantoms(&s)[0];
    let me = ActorId::Npc(id);
    s.sim.has_llm = true;
    assert!(s.sim.drive_line(id).is_some_and(|l| l.contains("may still make one thing")));
    s.sim.act(me, Action::Create { text: "a long spear".into() }).expect("its one making");
    assert!(s.sim.act(me, Action::Create { text: "a second spear".into() }).is_err(), "only one a night");
    assert!(s.sim.drive_line(id).is_some_and(|l| l.contains("made your one thing")));
    // Others make as they like.
    assert!(s.sim.may_make(ActorId::Player).is_ok());
    s.sim.has_llm = false;
    s.sim.t = before_dawn(1.0, 1.0);
    run_for(&mut s, 3.0);
    s.sim.t = before_dusk(2.0, 1.0);
    run_for(&mut s, 3.0);
    assert!(s.sim.may_make(me).is_ok(), "a new night, a new making");
}

/// The phantom's own body compiles, and is written from the human figure
/// (so what fits a person fits it).
#[test]
fn the_phantom_body_is_built_in_and_fits_what_people_wear() {
    let w = world("phantom body", 76);
    let s = session(&w, 76, None);
    let body = s.sim.snap.body_named("phantom").expect("the phantom body is built in");
    assert_eq!(body.ct.meta.body.as_ref().and_then(|b| b.from.clone()).as_deref(), Some("figure"));
    assert!(s.sim.snap.layer_fits("figure", "phantom"));
}

/// Render a person, a phantom (bare, and in a breastplate) and a night
/// walker side by side, by day (look at it).
#[test]
#[ignore]
fn night_lineup_picture() {
    let out = std::env::var("POCKET_PNG").unwrap_or_else(|_| std::env::temp_dir().join("pocket-night.png").to_string_lossy().into_owned());
    let w = world("night lineup", 43);
    let plate = add_type(&w, &fixture("layers/breastplate.js"));
    let me = w.spawn;
    let ola = add_char(&w, "Ola", "kind", &[], ground(&w, me.x - 2.4, me.z + 4.0));
    let mut s = session(&w, 17, None);
    calm(&mut s);
    s.sim.store_species(super::super::night::walker_species());
    s.sim.store_species(super::super::phantom::phantom_species());
    let mut ids = vec![ola];
    for (i, (name, sp)) in [("the first phantom", "phantom"), ("the second phantom", "phantom"), ("the night walker", "night walker")].iter().enumerate() {
        let at = ground(&w, me.x - 0.8 + i as f32 * 1.6, me.z + 4.0);
        let persona = crate::world::Persona { name: name.to_string(), species: sp.to_string(), ..Default::default() };
        ids.push(s.sim.add_being(persona, at, Default::default()).unwrap());
    }
    let pl = s.sim.spawn_thing(plate, me, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.wear(ActorId::Npc(ids[2]), ActorId::Npc(ids[2]), pl).unwrap();
    calm(&mut s);
    // POCKET_HOUR=22 shows them at night.
    let hour: f32 = std::env::var("POCKET_HOUR").ok().and_then(|h| h.parse().ok()).unwrap_or(10.8);
    s.sim.t = crate::render::sky::DAY_SECONDS * (hour / 24.0) as f64;
    for id in &ids {
        let n = s.sim.cast.get_mut(*id).unwrap();
        n.away = false;
        // Facing the camera.
        n.a.yaw = std::env::var("POCKET_YAW").ok().and_then(|y| y.parse().ok()).unwrap_or(std::f32::consts::PI);
    }
    s.run(0.2, 0.05);
    // Night beings shown by day.
    for id in &ids {
        let n = s.sim.cast.get_mut(*id).unwrap();
        let mut sp = (*n.species).clone();
        sp.active = crate::world::species::Active::Always;
        n.species = Arc::new(sp);
        n.away = false;
    }
    // Settle into their stances.
    for _ in 0..20 {
        let t = s.sim.t;
        for id in &ids {
            s.sim.cast.get_mut(*id).unwrap().a.update_pose(t, 0.05, None);
        }
    }
    let cam = crate::render::Camera { pos: me + Vec3::Y * 1.5 + Vec3::new(0.0, 0.0, 0.5), yaw: 0.0, pitch: -0.05, fov_y: 1.0, roll: 0.0 };
    render_png(&mut s, &w, cam, &out);
}

/// Struck down, the traveler falls where they stand and lets go of what
/// they held; the dark turns away from them, the one who did it remembers,
/// and whoever saw it does too. They can do nothing till they wake: at the
/// world's beginning, in the morning, whole. A night the traveler fell in
/// isn't one got through.
#[test]
fn the_traveler_falls_and_wakes_at_the_beginning_in_the_morning() {
    let w = world_with("fall", 77, bare());
    let sword = add_type(&w, &fixture("physics/sword.js"));
    let p = flat_spot(&w, 30.0);
    let ada = add_char(&w, "Ada", "calm", &[], p + Vec3::new(6.0, 0.0, 0.0));
    let mut s = session(&w, 77, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = p;
    s.sim.t = before_dusk(1.0, 1.0);
    run_for(&mut s, 2.0);
    let id = phantoms(&s)[0];
    for x in the_dark(&s) {
        s.sim.cast.get_mut(x).unwrap().a.pos = p + Vec3::new(300.0, 0.0, -300.0);
        s.sim.cast.get_mut(x).unwrap().think_at = f64::MAX;
    }
    s.sim.cast.get_mut(ada).unwrap().a.pos = ground(&w, p.x + 6.0, p.z);
    let sw = s.sim.spawn_thing(sword, p + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.hand_to(ActorId::Player, sw);
    s.sim.drain_requests();
    // Nearly spent; the phantom's touch finishes it.
    s.sim.night.wounds = 0.97;
    s.sim.cast.get_mut(id).unwrap().a.pos = ground(&w, p.x, p.z + 1.2);
    s.sim.touch(id, ActorId::Player);
    assert!(s.sim.fallen() && s.sim.health() == 0.0, "the traveler fell");
    assert_eq!(s.sim.night.fallen.as_deref(), Some("clawed by the first phantom"));
    assert!(s.sim.player.held.is_none() && s.sim.things.get(sw).is_some_and(|t| t.holder.is_none()), "the sword fell from their hand");
    assert!(s.sim.act(ActorId::Player, Action::Jump).is_err(), "a fallen traveler does nothing");
    let reqs = s.sim.drain_requests();
    let mem = |who: i64| reqs.iter().filter_map(|r| match r {
        Request::Witness { cid, text, .. } if *cid == who => Some(text.clone()),
        _ => None,
    }).collect::<Vec<_>>();
    assert!(mem(id).iter().any(|m| m.contains("struck the traveler down")), "the phantom remembers: {:?}", mem(id));
    assert!(mem(ada).iter().any(|m| m.contains("the traveler was clawed by the first phantom, and fell")), "Ada saw how: {:?}", mem(ada));
    assert!(!events(&s.sim, "traveler_fell").is_empty());
    // The dark turns from them: the phantom goes for whoever else is about.
    assert!(s.sim.cast.get(id).is_some_and(|n| n.species.hostile));
    s.sim.cast.get_mut(id).unwrap().think_at = 0.0;
    run_for(&mut s, 3.0);
    assert!(events(&s.sim, "hurt").iter().all(|e| e.data["health"].as_f64().unwrap_or(1.0) <= 0.03), "no more blows on the fallen traveler");
    assert!(s.sim.fallen(), "still lying there");
    // Wake.
    s.sim.wake_traveler();
    assert!(!s.sim.fallen() && s.sim.health() == 1.0);
    let at = s.sim.player.pos;
    assert!((Vec3::new(at.x, 0.0, at.z) - Vec3::new(s.sim.snap.spawn.x, 0.0, s.sim.snap.spawn.z)).length() < 0.01, "at the world's beginning");
    let phase = crate::render::sky::day_phase(s.sim.t);
    assert!((phase - super::super::night::MORNING).abs() < 0.01 && !s.sim.dark(), "in the morning: {phase:.2}");
    assert!(s.sim.t > before_dusk(1.0, 0.0), "time went on, not back");
    run_for(&mut s, 3.0);
    assert!(events(&s.sim, "survived_night").is_empty(), "a night you fell in isn't got through");
    assert!(s.sim.things.get(sw).is_some_and(|t| (t.pos - p).length() < 3.0), "the sword lies where they fell");
}

/// A jump on flat ground never hurts; a drop from higher than 1.5 times one's own
/// height does, more the further; again and again it kills.
#[test]
fn falls_from_a_height_hurt_and_can_kill() {
    let w = world_with("heights", 78, bare());
    let p = flat_spot(&w, 20.0);
    let ada = add_char(&w, "Ada", "calm", &[], p + Vec3::new(8.0, 0.0, 0.0));
    let mut s = session(&w, 78, None);
    calm(&mut s);
    s.sim.player.pos = p;
    // A jump in place.
    assert!(s.sim.jump(ActorId::Player));
    run_for(&mut s, 2.0);
    assert_eq!(s.sim.health(), 1.0, "a jump doesn't hurt");
    // Dropped from 4.5 m (a roof): a little.
    let drop = |s: &mut Session, who: ActorId, h: f32| {
        let a = s.sim.actor_mut(who).unwrap();
        a.pos.y += h;
        a.grounded = false;
        a.vy = 0.0;
        run_for(s, 3.0);
    };
    drop(&mut s, ActorId::Player, 4.5);
    let hurt = 1.0 - s.sim.health();
    assert!(hurt > 0.05 && hurt < 0.35, "a roof's height: {hurt:.2}");
    assert!(!events(&s.sim, "fell_hard").is_empty());
    // Out of a tall tree: much worse; and again, dead.
    drop(&mut s, ActorId::Player, 8.0);
    assert!(!s.sim.fallen() && s.sim.health() < 0.4, "a tree: {:.2}", s.sim.health());
    drop(&mut s, ActorId::Player, 8.0);
    assert!(s.sim.fallen(), "again: dead");
    assert!(s.sim.night.fallen.as_deref().is_some_and(|h| h.starts_with("broken by a fall of 8 m")), "{:?}", s.sim.night.fallen);
    // Beings too.
    for _ in 0..3 {
        drop(&mut s, ActorId::Npc(ada), 10.0);
    }
    assert!(s.sim.cast.get(ada).unwrap().dead || !s.sim.cfg.hunting, "Ada fell to her death");
}

/// A deed that would hurt does: done to oneself ("on_self", whatever is
/// pointed at), or to someone, who then fears the one who did it; what is
/// worn softens it, and it can kill.
#[test]
fn deeds_can_hurt_the_traveler_themselves_and_others() {
    use super::super::interp::{BeingFx, InterpEffect, PendingInterp};
    let w = world_with("deed hurts", 79, bare());
    let p = flat_spot(&w, 20.0);
    let ada = add_char(&w, "Ada", "calm", &[], p + Vec3::new(1.5, 0.0, 0.0));
    let mut s = session(&w, 79, None);
    calm(&mut s);
    s.sim.cfg.hunting = true;
    s.sim.player.pos = p;
    let deed = |s: &mut Session, target: Option<Target>, hurt: f32, on_self: bool| {
        let pi = PendingInterp { actor: ActorId::Player, text: "stab".into(), held: None, target, hit: None, key: String::new(), at: 0.0, aim: None };
        let how = (hurt >= 1.0 && !on_self).then(|| "stabbed in the eye with a stick".to_string());
        let fx = InterpEffect { narration: "It hurts.".into(), being: Some(BeingFx { hurt: Some(hurt), how, on_self, ..Default::default() }), ..Default::default() };
        s.sim.apply_interp(&pi, &fx);
    };
    // "Stab this stick into my eye", pointing at Ada: it lands on the traveler.
    deed(&mut s, Some(Target::Actor(ActorId::Npc(ada))), 0.2, true);
    assert!((s.sim.health() - 0.8).abs() < 1e-3, "{}", s.sim.health());
    assert!(s.sim.cast.get(ada).unwrap().props[P_HEALTH] >= 0.999, "not Ada");
    // Kicking Ada hurts her, and she fears the traveler for it.
    deed(&mut s, Some(Target::Actor(ActorId::Npc(ada))), 0.3, false);
    let h = s.sim.cast.get(ada).unwrap().props[P_HEALTH];
    assert!((h - 0.7).abs() < 0.02, "{h}");
    assert!(s.sim.social.fear(ActorId::Npc(ada), ActorId::Player) > 0.0);
    assert!(!events(&s.sim, "hurt_by_deed").is_empty());
    // Enough of it kills.
    deed(&mut s, Some(Target::Actor(ActorId::Npc(ada))), 1.0, false);
    assert!(s.sim.cast.get(ada).unwrap().dead, "Ada died of it");
    assert!(events(&s.sim, "died").iter().any(|e| e.text == "Ada was stabbed in the eye with a stick, and died"), "{:?}", events(&s.sim, "died").iter().map(|e| e.text.clone()).collect::<Vec<_>>());
    // And the traveler can do it to themselves.
    deed(&mut s, None, 1.0, true);
    assert!(s.sim.fallen() && s.sim.night.fallen.as_deref() == Some("hurt by their own hand"), "{:?}", s.sim.night.fallen);
}

/// A fall says where it was from: off a hut's roof, its name goes with it.
#[test]
fn a_fall_from_a_roof_says_so() {
    let w = world_with("roof", 80, bare());
    let hut = add_type(&w, &fixture("sims/hut.js"));
    let p = flat_spot(&w, 20.0);
    place(&w, hut, p, 0.0);
    let mut s = session(&w, 80, None);
    calm(&mut s);
    // Let down onto the roof (gently: the landing isn't the test).
    s.sim.player.pos = p + Vec3::Y * 12.0;
    s.sim.player.grounded = false;
    run_for(&mut s, 3.0);
    let roof = s.sim.player.pos.y;
    assert!(roof > p.y + 2.0 && s.sim.player.grounded, "on the roof: {:.1} m up", roof - p.y);
    s.sim.night.wounds = 0.0;
    // Jump off it, out over the grass.
    assert!(s.sim.jump(ActorId::Player));
    s.sim.player.pos += Vec3::new(9.0, 0.0, 0.0);
    run_for(&mut s, 3.0);
    let hurt: Vec<String> = events(&s.sim, "hurt").into_iter().map(|e| e.text).filter(|t| t.contains("fall")).collect();
    assert!(hurt.last().is_some_and(|t| t.contains("from the wooden hut")), "{hurt:?}");
}

/// Pictures of the dark's own, up close: a phantom and a night walker by
/// day (to see their shape) and by night, standing (the phantom in a
/// helmet, breastplate and cloak, holding a sword), then struck down and
/// lying (look at them: frightening alive, flat on the ground dead).
#[test]
#[ignore]
fn night_bodies_png() {
    let dir = std::env::var("POCKET_PNG_DIR").unwrap_or_else(|_| std::env::temp_dir().display().to_string());
    let w = world("night bodies", 7);
    let sword = add_type(&w, &fixture("physics/sword.js"));
    let plate = add_type(&w, &fixture("layers/breastplate.js"));
    let cloak = add_type(&w, &fixture("layers/cloak.js"));
    let helmet = add_type(&w, &fixture("layers/helmet.js"));
    let mut s = session(&w, 7, None);
    s.sim.cfg.difficulty = 2;
    s.sim.player.pos = w.spawn;
    s.sim.t = before_dusk(1.0, 2.0);
    for _ in 0..40 {
        s.step(0.1);
    }
    let (ph, wk) = (phantoms(&s)[0], the_dark(&s)[0]);
    // Armed and armoured, as a phantom goes hunting.
    {
        let at = s.sim.cast.get(ph).unwrap().a.pos;
        for ty in [plate, cloak, helmet] {
            let id = s.sim.spawn_thing(ty, at + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
            s.sim.wear(ActorId::Npc(ph), ActorId::Npc(ph), id).unwrap();
        }
        let sw = s.sim.spawn_thing(sword, at + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
        s.sim.hand_to(ActorId::Npc(ph), sw);
    }
    let gpu = crate::render::gpu::Gpu::new().ok();
    let live = Arc::new(Mutex::new(crate::model::Live::default()));
    let mut model = crate::model::WorldModel::load(w.db.clone(), gpu.clone(), live).unwrap();
    let snap = model.snapshot().unwrap();
    let t = snap.terrain.clone();
    let at = Vec3::new(w.spawn.x + 6.0, 0.0, w.spawn.z + 6.0);
    let g = |x: f32, z: f32| Vec3::new(x, t.height(x, z), z);
    let spots = [(ph, g(at.x - 0.9, at.z)), (wk, g(at.x + 0.9, at.z))];
    let place = |s: &mut Session| {
        for (id, p) in spots {
            let n = s.sim.cast.get_mut(id).unwrap();
            n.a.pos = p;
            n.a.yaw = std::f32::consts::PI;
            n.plan.clear();
            n.a.task = None;
            n.a.motion = None;
            n.think_at = f64::MAX;
        }
    };
    for _ in 0..30 {
        place(&mut s);
        s.step(0.05);
    }
    place(&mut s);
    let cam_at = |from: Vec3, to: Vec3| {
        let eye = Vec3::new(from.x, t.height(from.x, from.z) + from.y, from.z);
        let d = to - eye;
        crate::render::Camera { pos: eye, yaw: d.x.atan2(d.z), pitch: (d.y / Vec3::new(d.x, 0.0, d.z).length()).atan(), fov_y: 0.8, roll: 0.0 }
    };
    let mid = g(at.x, at.z);
    let front = cam_at(Vec3::new(at.x, 1.6, at.z - 4.5), mid + Vec3::Y * 1.1);
    let face = |p: Vec3| cam_at(Vec3::new(p.x, 1.8, p.z - 1.3), p + Vec3::Y * 1.75);
    s.sim.player.pos = g(at.x, at.z - 6.0);
    let night = s.sim.t;
    for (name, hour) in [("day", 0.45), ("night", -1.0)] {
        s.sim.t = if hour < 0.0 { night } else { crate::render::sky::DAY_SECONDS * (s.sim.t / crate::render::sky::DAY_SECONDS).floor() + crate::render::sky::DAY_SECONDS * hour };
        png_hd(&mut s, &snap, &gpu, front, &format!("{dir}/pocket-night-{name}-both.png"));
        png_hd(&mut s, &snap, &gpu, face(spots[0].1), &format!("{dir}/pocket-night-{name}-phantom-face.png"));
        png_hd(&mut s, &snap, &gpu, face(spots[1].1), &format!("{dir}/pocket-night-{name}-walker-face.png"));
        let p = spots[0].1;
        png_hd(&mut s, &snap, &gpu, cam_at(Vec3::new(p.x - 2.6, 1.3, p.z - 0.4), p + Vec3::Y * 1.1), &format!("{dir}/pocket-night-{name}-phantom-side.png"));
    }
    s.sim.t = night;
    for (id, p) in spots {
        s.sim.kill(id, "struck down", None);
        let _ = p;
    }
    s.sim.t += 3.0;
    let side = cam_at(Vec3::new(at.x - 4.0, 1.4, at.z - 2.5), mid + Vec3::Y * 0.2);
    let above = cam_at(Vec3::new(at.x, 2.6, at.z - 2.6), mid);
    for (name, hour) in [("day", 0.45), ("night", -1.0)] {
        s.sim.t = if hour < 0.0 { night + 3.0 } else { crate::render::sky::DAY_SECONDS * ((night / crate::render::sky::DAY_SECONDS).floor() + 1.0 + hour) };
        png_hd(&mut s, &snap, &gpu, side, &format!("{dir}/pocket-night-{name}-dead-side.png"));
        png_hd(&mut s, &snap, &gpu, above, &format!("{dir}/pocket-night-{name}-dead-above.png"));
    }
}

/// As `png_of`, at 960×540.
fn png_hd(s: &mut Session, snap: &Arc<crate::world::WorldSnapshot>, gpu: &Option<Arc<crate::render::gpu::Gpu>>, cam: crate::render::Camera, out: &str) {
    let (pw, ph) = (960u32, 540u32);
    let drawn = s.sim.draw_first_person(&cam, crate::render::VIEW_DIST);
    let culled = crate::world::cull::cull(snap, &mut s.sim.cache, &cam, pw as f32 / ph as f32, &drawn.insts, &Default::default());
    let light = crate::render::sky::lighting(s.sim.t, &snap.look.palette);
    let sp = crate::render::SceneParams { terrain: &snap.terrain, palette: &snap.look.palette, camera: cam, width: pw, height: ph, pixel_aspect: 1.0, light, time: 1.0, frame: 0, shadows: true, lights: &drawn.lights, weather: Default::default() };
    let globals = crate::render::build_globals(&sp, culled.insts.len(), culled.grid.as_ref());
    let req = crate::render::FrameRequest { id: 1, width: pw, height: ph, globals, instances: culled.insts, holes: drawn.holes.clone(), grid: culled.grid, scene: snap.scene.clone(), terrain: snap.terrain.clone(), look: snap.look.clone() };
    let mut handle = match gpu {
        Some(g) => crate::render::gpu::spawn(g.clone()),
        None => crate::render::cpu::spawn(),
    };
    handle.tx.send(crate::render::RenderMsg::Frame(Box::new(req))).unwrap();
    let f = handle.rx.recv_timeout(std::time::Duration::from_secs(60)).unwrap();
    handle.shutdown();
    let mut rgb = Vec::new();
    for p in &f.pixels {
        rgb.extend_from_slice(&crate::render::unpack(*p));
    }
    std::fs::write(out, crate::png::encode(f.width, f.height, &rgb)).unwrap();
}

/// Struck down, the dark's own lie flat on the ground, not caught mid-fall
/// in their stoop: their dead pose straightens them, and their lights go out.
#[test]
fn the_dark_lies_flat_when_struck_down() {
    let w = world("flat dead", 7);
    let mut s = session(&w, 7, None);
    s.sim.cfg.difficulty = 2;
    s.sim.player.pos = w.spawn;
    s.sim.t = before_dusk(1.0, 2.0);
    for _ in 0..40 {
        s.step(0.1);
    }
    for id in [phantoms(&s)[0], the_dark(&s)[0]] {
        s.sim.kill(id, "struck down", Some(ActorId::Player));
        let t = s.sim.things.live().find(|t| t.origin.remains == Some(id)).cloned().expect("a body is left");
        let ty = s.sim.snap.type_of(t.type_id).unwrap().clone();
        let g = super::super::render::thing_inst(&t, &ty, [0.0; 4]);
        assert!(g.s1[0] < 0.0, "its stoop is straightened as its body says the dead lie");
        // The highest point of the body, marched down from above.
        let (c, r) = (g.center(), g.radius());
        let ground = s.sim.snap.terrain.height(t.pos.x, t.pos.z);
        let mut high = f32::MIN;
        for i in 0..21 {
            for j in 0..21 {
                let x = c.x + r * ((i as f32 + 0.5) / 21.0 * 2.0 - 1.0);
                let z = c.z + r * ((j as f32 + 0.5) / 21.0 * 2.0 - 1.0);
                let mut y = c.y + r;
                while y > c.y - r {
                    if g.sdf(&ty.ct, Vec3::new(x, y, z)) < 0.01 {
                        high = high.max(y);
                        break;
                    }
                    y -= 0.02;
                }
            }
        }
        let name = s.sim.actor_name(ActorId::Npc(id));
        assert!(high - ground < 0.7, "{name} lies flat: its highest point is {:.2} m up", high - ground);
    }
}

/// A body that says where its parts are holds things in its own hands and
/// wears what was made for the body it was written from on its own parts:
/// a phantom's long arms hold a sword where its hand is (not where a
/// person's hand would be), and a person's helmet sits on its hanging head.
#[test]
fn a_phantom_holds_and_wears_on_its_own_body() {
    let w = world("own parts", 7);
    let sword = add_type(&w, &fixture("physics/sword.js"));
    let helmet = add_type(&w, &fixture("layers/helmet.js"));
    let mut s = session(&w, 7, None);
    s.sim.cfg.difficulty = 1;
    s.sim.player.pos = w.spawn;
    s.sim.t = before_dusk(1.0, 2.0);
    for _ in 0..40 {
        s.step(0.1);
    }
    let ph = phantoms(&s)[0];
    let at = s.sim.cast.get(ph).unwrap().a.pos;
    let hl = s.sim.spawn_thing(helmet, at + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.wear(ActorId::Npc(ph), ActorId::Npc(ph), hl).unwrap();
    let sw = s.sim.spawn_thing(sword, at + Vec3::Y * 0.3, 0.0, 1.0, Default::default(), true).unwrap();
    s.sim.hand_to(ActorId::Npc(ph), sw);
    {
        let n = s.sim.cast.get_mut(ph).unwrap();
        n.think_at = f64::MAX;
        n.plan.clear();
        n.a.task = None;
    }
    for _ in 0..20 {
        s.step(0.05);
    }
    let n = s.sim.cast.get(ph).unwrap();
    let body = s.sim.snap.type_of(n.body_ty).unwrap().clone();
    let g = n.gpu(&body);
    let k = g.k();
    // Its hands are where its body draws them, well away from a person's.
    let [r, l] = n.a.hands.expect("its body says where its hands are");
    let own = n.a.pos + n.a.right() * r.x + Vec3::Y * r.y + n.a.forward() * r.z;
    assert!((g.from_local(body.ct.part(0, &k).unwrap()) - own).length() < 0.05);
    let mut person = n.a.clone();
    person.hands = None;
    assert!((person.hand(false) - n.a.hand(false)).length() > 0.15, "its long arms reach further than a person's");
    // The sword is held between them (it is big), by its grip.
    let mid = n.a.pos + n.a.right() * ((r.x + l.x) * 0.5) + Vec3::Y * ((r.y + l.y) * 0.5) + n.a.forward() * ((r.z + l.z) * 0.5);
    let t = s.sim.things.get(sw).unwrap();
    let ty = s.sim.snap.type_of(t.type_id).unwrap();
    let grip = t.pos + glam::Quat::from_rotation_y(t.yaw) * (t.hold_tilt.unwrap_or_default() * (Vec3::from_array(ty.ct.meta.tool.as_ref().unwrap().grip) * t.scale));
    assert!((grip - mid).length() < 0.2, "held in its hands: {:.2} m off", (grip - mid).length());
    // The helmet moves from a person's head to its own.
    let hty = s.sim.snap.type_of(s.sim.things.get(hl).unwrap().type_id).unwrap().clone();
    let shift = s.sim.layer_shift(&g, &body, &hty).expect("the helmet follows its head");
    let figure = s.sim.snap.body_named("figure").unwrap().clone();
    let want = g.from_local(body.ct.part(2, &k).unwrap()) - g.from_local(figure.ct.part(2, &k).unwrap());
    assert!(shift.length() > 0.1 && (shift - want).length() < 1e-3, "to its own head: {shift:?}");
}

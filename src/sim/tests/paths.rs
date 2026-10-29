//! Finding the way (src/sim/paths.rs): out of a pen and round it to a spot
//! straight behind its back wall; several sent to one spot stand apart
//! there, not on top of each other.

use super::*;

#[test]
fn a_walker_finds_its_way_out_of_a_pen_and_round() {
    let w = world_with("pen", 81, bare());
    let pen = add_type(&w, &fixture("physics/pen.js"));
    let c = flat_spot(&w, 40.0);
    place(&w, pen, c, 0.0);
    let ola = add_char(&w, "Ola", "calm", &[], c + Vec3::new(0.0, 0.0, 1.5));
    let mut s = session(&w, 81, None);
    calm(&mut s);
    let o = ActorId::Npc(ola);
    s.sim.cast.get_mut(ola).unwrap().a.pos = ground(&w, c.x, c.z + 1.5);
    // The spot is outside, straight through the back wall.
    let goal = ground(&w, c.x, c.z + 6.0);
    assert!(!s.sim.open_at(o, ground(&w, c.x, c.z + 3.0)), "nobody stands in the wall");
    assert!(s.sim.open_at(o, goal), "the spot behind it is open");
    let r = act_once(&mut s, o, Action::Goto { target: Target::Point(goal.to_array()), run: false }, 20.0);
    let at = s.sim.actor(o).unwrap().pos;
    assert!(Vec3::new(at.x - goal.x, 0.0, at.z - goal.z).length() < 1.5, "Ola walked out and round: {r}, at {:?} from the pen", at - c);
}

#[test]
fn several_sent_to_one_spot_stand_apart() {
    let w = world_with("crowd", 82, bare());
    let c = flat_spot(&w, 30.0);
    let ids: Vec<i64> = (0..5).map(|i| add_char(&w, &format!("P{i}"), "calm", &[], c + Vec3::new(i as f32 * 2.0 - 4.0, 0.0, -6.0))).collect();
    let mut s = session(&w, 82, None);
    calm(&mut s);
    for (i, id) in ids.iter().enumerate() {
        s.sim.cast.get_mut(*id).unwrap().a.pos = ground(&w, c.x + i as f32 * 2.0 - 4.0, c.z - 6.0);
        s.sim.plan(ActorId::Npc(*id), vec![Action::Goto { target: Target::Point(c.to_array()), run: false }], "", false);
    }
    s.run(14.0, 0.05);
    let at: Vec<Vec3> = ids.iter().map(|id| s.sim.cast.get(*id).unwrap().a.pos).collect();
    for (i, p) in at.iter().enumerate() {
        assert!(Vec3::new(p.x - c.x, 0.0, p.z - c.z).length() < 3.0, "P{i} got there: {:?}", *p - c);
        for q in &at[i + 1..] {
            let d = Vec3::new(p.x - q.x, 0.0, p.z - q.z).length();
            assert!(d > 0.75, "they keep a little room: {d:.2} m apart");
        }
    }
    assert!(events(&s.sim, "plan_failed").is_empty(), "nobody gave up on the way");
}

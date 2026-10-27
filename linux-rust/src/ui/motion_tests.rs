use super::*;
use std::time::Duration;

fn at(start: Instant, secs: f32) -> Instant {
    start + Duration::from_secs_f32(secs)
}

#[test]
fn first_sight_without_from_does_not_animate() {
    let now = Instant::now();
    let mut m = Motion::default();
    m.target("tab", 2.0, None, SNAPPY, now);
    assert_eq!(m.value("tab", 0.0, now), 2.0);
    assert!(!m.is_animating(now));
}

#[test]
fn unknown_key_gives_fallback() {
    let m: Motion<&str> = Motion::default();
    assert_eq!(m.value("nope", 0.7, Instant::now()), 0.7);
}

#[test]
fn critically_damped_spring_settles_without_overshoot() {
    let t0 = Instant::now();
    let mut m = Motion::default();
    m.target("ring", 0.8, Some(0.0), SMOOTH, t0);
    assert!(m.is_animating(t0));
    let mut last = 0.0;
    for i in 1..=60 {
        let v = m.value("ring", 0.0, at(t0, i as f32 * 0.025));
        assert!(v >= last - 1e-6, "monotonic");
        assert!(v <= 0.8 + 1e-4, "no overshoot");
        last = v;
    }
    assert_eq!(m.value("ring", 0.0, at(t0, 3.0)), 0.8);
    assert!(!m.is_animating(at(t0, 3.0)));
}

#[test]
fn snappy_spring_overshoots_a_little_then_settles() {
    let t0 = Instant::now();
    let mut m = Motion::default();
    m.target("seg", 1.0, Some(0.0), SNAPPY, t0);
    let peak = (1..100)
        .map(|i| m.value("seg", 0.0, at(t0, i as f32 * 0.005)))
        .fold(0.0f32, f32::max);
    assert!(peak > 1.0 && peak < 1.06, "peak {peak}");
    assert_eq!(m.value("seg", 0.0, at(t0, 2.0)), 1.0);
}

#[test]
fn retarget_mid_flight_keeps_position() {
    let t0 = Instant::now();
    let mut m = Motion::default();
    m.target("seg", 1.0, Some(0.0), SNAPPY, t0);
    let mid = at(t0, 0.08);
    let before = m.value("seg", 0.0, mid);
    m.target("seg", 3.0, None, SNAPPY, mid);
    let after = m.value("seg", 0.0, mid);
    assert!((before - after).abs() < 1e-4);
    assert!((m.value("seg", 0.0, at(mid, 3.0)) - 3.0).abs() < 1e-6);
}

#[test]
fn kick_springs_back_to_target() {
    let t0 = Instant::now();
    let mut m = Motion::default();
    m.target("glow", 0.4, None, SMOOTH, t0);
    m.kick("glow", 1.0, t0);
    assert!((m.value("glow", 0.0, t0) - 1.0).abs() < 1e-6);
    assert!(m.is_animating(at(t0, 0.1)));
    assert_eq!(m.value("glow", 0.0, at(t0, 4.0)), 0.4);
    m.kick("missing", 1.0, t0); // no-op
    m.reset();
    assert_eq!(m.value("glow", 0.25, t0), 0.25);
}

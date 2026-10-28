//! Island shapes, content transitions and the motion model.

use super::*;

fn buds() -> OverlayState {
    let c = |level| {
        Some(ComponentState {
            level,
            present: true,
            ..Default::default()
        })
    };
    OverlayState {
        name: "AirPods 4".into(),
        connected: true,
        left: c(80),
        right: c(70),
        case: c(40),
        ..Default::default()
    }
}

fn max() -> OverlayState {
    OverlayState {
        name: "AirPods Max".into(),
        connected: true,
        headphone: Some(ComponentState {
            level: 60,
            present: true,
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn settle(m: &mut Motion) -> f32 {
    let mut t = 0.0;
    while m.step(1.0 / 60.0) {
        t += 1.0 / 60.0;
        assert!(t < 5.0, "motion never settled");
    }
    t
}

#[test]
fn shapes_are_pills_or_a_card() {
    let s = buds();
    let (w, h, r) = shape(&Content::Compact, &s);
    assert_eq!((h, r), (COMPACT_H, COMPACT_H / 2.0));
    let (hw, _, _) = shape(&Content::Compact, &max());
    assert!(hw < w, "one battery is narrower than three");
    let (ew, eh, er) = shape(&Content::Expanded, &s);
    assert_eq!((ew, eh, er), (EXPANDED_W_BUDS, EXPANDED_H, EXPANDED_R));
    assert_eq!(shape(&Content::Expanded, &max()).0, EXPANDED_W_HEADPHONE);
    let (aw, ah, ar) = shape(&Content::Alert(Alert::Back), &s);
    assert_eq!((ah, ar), (ALERT_H, ALERT_H / 2.0));
    assert!((ALERT_MIN_W..=ALERT_MAX_W).contains(&aw));
}

#[test]
fn alerts_fit_their_words_and_batteries() {
    let s = buds();
    let short = shape(&Content::Alert(Alert::Listening(ListeningMode::Off)), &s).0;
    let long = shape(
        &Content::Alert(Alert::Elsewhere(Some("David's iPhone 17 Pro".into()))),
        &s,
    )
    .0;
    assert!(long > short);
    let one = Parts {
        case: true,
        ..Default::default()
    };
    let two = Parts {
        left: true,
        right: true,
        ..Default::default()
    };
    let w1 = shape(&Content::Alert(Alert::Charging(one)), &s).0;
    let w2 = shape(&Content::Alert(Alert::Charging(two)), &s).0;
    assert!(w2 > w1, "two batteries take more room than one");
    let huge = shape(&Content::Alert(Alert::Elsewhere(Some("x".repeat(200)))), &s).0;
    assert_eq!(huge, ALERT_MAX_W);
}

#[test]
fn text_width_tracks_length_and_letter_shape() {
    assert!(text_width("Transparency", 15.0) > text_width("Off", 15.0));
    assert!(text_width("mmmm", 15.0) > text_width("iiii", 15.0));
    assert_eq!(text_width("", 15.0), 0.0);
    assert!(text_width("AirPods", 30.0) > text_width("AirPods", 15.0) * 1.9);
}

#[test]
fn easing_curves_start_and_end_in_place() {
    for f in [ease_out_cubic, smoothstep, ease_out_back] {
        assert!(f(0.0).abs() < 1e-6);
        assert!((f(1.0) - 1.0).abs() < 1e-6);
        assert_eq!(f(-1.0), f(0.0));
        assert_eq!(f(2.0), f(1.0));
    }
    let peak = (0..=100)
        .map(|i| ease_out_back(i as f32 / 100.0))
        .fold(0.0_f32, f32::max);
    assert!(peak > 1.0 && peak < 1.08, "a small overshoot, got {peak}");
}

#[test]
fn appearing_grows_from_the_seed_with_a_little_bounce() {
    let s = buds();
    let mut m = Motion::default();
    let (w, h, r) = shape(&Content::Expanded, &s);
    m.switch(Some(Content::Expanded), Some((w, h, r)));
    assert_eq!(m.size().0, SEED_W);
    let mut peak = 0.0_f32;
    let mut t = 0.0;
    while m.step(1.0 / 60.0) {
        peak = peak.max(m.w.value);
        t += 1.0 / 60.0;
    }
    assert!(
        peak > w + 2.0,
        "springs overshoot when growing (peak {peak})"
    );
    assert!(peak < w * 1.08, "but only a little (peak {peak})");
    assert!(t < 1.8, "settles in {t}s");
    assert_eq!(m.size(), (w, h, r));
    assert_eq!(m.alpha.value, 1.0);
    assert_eq!(m.incoming(), (1.0, 1.0));
    assert_eq!(m.reveal(), 1.0);
}

#[test]
fn shrinking_does_not_bounce_past_the_target() {
    let s = buds();
    let mut m = Motion::default();
    m.switch(Some(Content::Expanded), Some(shape(&Content::Expanded, &s)));
    settle(&mut m);
    let small = shape(&Content::Compact, &s);
    m.switch(Some(Content::Compact), Some(small));
    let mut low = f32::MAX;
    while m.step(1.0 / 60.0) {
        low = low.min(m.w.value);
    }
    assert!(low > small.0 - 3.0, "barely undershoots (low {low})");
}

#[test]
fn contents_cross_fade() {
    let s = buds();
    let mut m = Motion::default();
    m.switch(Some(Content::Compact), Some(shape(&Content::Compact, &s)));
    settle(&mut m);
    m.switch(
        Some(Content::Alert(Alert::Back)),
        Some(shape(&Content::Alert(Alert::Back), &s)),
    );
    // Right after the switch: the old content is fully there, the new one not yet.
    let (_, out_a, _) = m.leaving().unwrap();
    assert_eq!(out_a, 1.0);
    assert_eq!(m.incoming().0, 0.0);
    m.step(CONTENT_OUT / 2.0);
    let (_, out_a, out_s) = m.leaving().unwrap();
    assert!(out_a < 1.0 && out_s < 1.0);
    m.step(CONTENT_OUT);
    assert!(m.leaving().is_none(), "the old content is gone quickly");
    m.step(CONTENT_IN_DELAY + CONTENT_IN);
    assert_eq!(m.incoming().0, 1.0);
}

#[test]
fn folding_away_ends_gone() {
    let s = buds();
    let mut m = Motion::default();
    m.switch(Some(Content::Compact), Some(shape(&Content::Compact, &s)));
    settle(&mut m);
    assert!(!m.gone());
    m.switch(None, None);
    assert!(!m.gone(), "still fading");
    let t = settle(&mut m);
    assert!(m.gone());
    assert!(t < 1.2, "folds away in {t}s");
    assert_eq!(m.size().1, SEED_H);
}

#[test]
fn switching_to_the_same_content_is_a_no_op() {
    let s = buds();
    let mut m = Motion::default();
    m.switch(Some(Content::Compact), Some(shape(&Content::Compact, &s)));
    settle(&mut m);
    m.switch(Some(Content::Compact), Some(shape(&Content::Compact, &s)));
    assert!(m.outgoing.is_none());
    assert!(!m.step(1.0 / 60.0));
}

#[test]
fn levels_snap_first_then_roll_and_refill() {
    let mut m = Motion::default();
    let mut s = buds();
    m.update_levels(&s, true);
    let l = m.levels[LEFT];
    assert_eq!(l.value, Some(80));
    assert_eq!(l.prev, None, "the first reading appears without a roll");
    assert_eq!(l.fill.value, 0.8);
    s.left.as_mut().unwrap().level = 72;
    m.update_levels(&s, true);
    assert_eq!(m.levels[LEFT].prev, Some(80));
    assert_eq!(m.levels[LEFT].roll(), 0.0);
    settle(&mut m);
    let l = m.levels[LEFT];
    assert_eq!((l.value, l.prev, l.fill.value), (Some(72), None, 0.72));
    assert_eq!(l.roll(), 1.0);
    // An absent pod shows no number.
    s.left.as_mut().unwrap().present = false;
    m.update_levels(&s, true);
    assert_eq!(m.levels[LEFT].value, None);
    assert_eq!(m.levels[LEFT].prev, None);
}

#[test]
fn caption_rolls_to_new_text() {
    let mut m = Motion::default();
    m.set_caption("Connected", true);
    assert_eq!(m.caption.prev, None, "nothing to roll from");
    m.set_caption("Case charging", true);
    assert_eq!(m.caption.prev.as_deref(), Some("Connected"));
    assert!(m.step(ROLL / 2.0));
    let r = m.caption.roll();
    assert!(r > 0.0 && r < 1.0);
    settle(&mut m);
    assert_eq!(m.caption.prev, None);
    m.set_caption("Connected", false);
    assert_eq!(m.caption.prev, None);
}

#[test]
fn bump_wobbles_and_returns() {
    let mut m = Motion::default();
    m.bump();
    let mut peak = 1.0_f32;
    while m.step(1.0 / 60.0) {
        peak = peak.max(m.pulse.value);
    }
    assert!(peak > 1.01 && peak < 1.06, "peak {peak}");
    assert_eq!(m.pulse.value, 1.0);
}

#[test]
fn reset_keeps_levels() {
    let mut m = Motion::default();
    m.update_levels(&buds(), false);
    m.switch(Some(Content::Compact), Some((100.0, 36.0, 18.0)));
    m.reset();
    assert_eq!(m.shown, None);
    assert_eq!(m.alpha.value, 0.0);
    assert_eq!(m.levels[LEFT].value, Some(80));
}

// ---------------------------------------------------------------------------
// Purist border light and shake
// ---------------------------------------------------------------------------

#[test]
fn each_event_has_its_own_light() {
    let s = buds();
    let glow = |c: Content, connected| glow_for(&c, &s, connected).map(|g| (g.kind, g.tint));
    assert_eq!(
        glow(Content::Compact, false),
        None,
        "the resting island is still"
    );
    assert_eq!(
        glow(Content::Expanded, true),
        Some((GlowKind::Flow, Tint::Spectrum))
    );
    assert_eq!(
        glow(Content::Expanded, false),
        Some((GlowKind::Sweep, Tint::White))
    );
    let both = Parts {
        left: true,
        right: true,
        ..Default::default()
    };
    assert_eq!(
        glow(Content::Alert(Alert::Charging(both)), false),
        Some((GlowKind::Sweep, Tint::Green))
    );
    assert_eq!(
        glow(Content::Alert(Alert::Mic(MicChoice::AirPods)), false),
        Some((GlowKind::Ping, Tint::Orange))
    );
    assert_eq!(
        glow(Content::Alert(Alert::MicUnavailable), false),
        Some((GlowKind::Ping, Tint::Red))
    );
    assert_eq!(
        glow(Content::Alert(Alert::Back), false),
        Some((GlowKind::Sweep, Tint::Blue))
    );
}

#[test]
fn low_battery_light_is_amber_then_red() {
    let mut s = buds();
    let left = Parts {
        left: true,
        ..Default::default()
    };
    s.left.as_mut().unwrap().level = 18;
    let g = glow_for(&Content::Alert(Alert::Low(left)), &s, false).unwrap();
    assert_eq!((g.kind, g.tint), (GlowKind::Breathe, Tint::Amber));
    s.left.as_mut().unwrap().level = 9;
    let g = glow_for(&Content::Alert(Alert::Low(left)), &s, false).unwrap();
    assert_eq!(g.tint, Tint::Red);
}

#[test]
fn every_glow_ends_within_two_seconds_and_then_frames_stop() {
    for kind in [
        GlowKind::Sweep,
        GlowKind::Breathe,
        GlowKind::Flow,
        GlowKind::Ping,
    ] {
        let g = Glow::new(kind, Tint::White);
        assert!(g.duration() < 2.0, "{kind:?} is too long for Apple's limit");
        let mut m = Motion::default();
        m.light(g);
        let t = settle(&mut m);
        assert!(m.glow.is_none());
        assert!((t - g.duration()).abs() < 0.05, "{kind:?} settled at {t}");
    }
}

#[test]
fn glow_progress_runs_from_zero_to_one() {
    let mut g = Glow::new(GlowKind::Sweep, Tint::Green);
    assert_eq!(g.progress(), 0.0);
    g.age = g.duration() / 2.0;
    assert!((g.progress() - 0.5).abs() < 1e-6);
    g.age = 10.0;
    assert_eq!(g.progress(), 1.0);
    assert!(g.done());
}

#[test]
fn shake_swings_both_ways_and_comes_home() {
    let mut m = Motion::default();
    m.shake();
    let (mut lo, mut hi) = (0.0_f32, 0.0_f32);
    let mut t = 0.0;
    while m.step(1.0 / 60.0) {
        lo = lo.min(m.shake.value);
        hi = hi.max(m.shake.value);
        t += 1.0 / 60.0;
    }
    assert!(lo < -4.0 && hi > 1.0, "swing {lo}..{hi}");
    assert!(lo > -20.0, "a nudge, not a jump: {lo}");
    assert!(t < 1.0, "settles quickly ({t}s)");
    assert_eq!(m.shake.value, 0.0);
}

#[test]
fn reset_keeps_a_waiting_glow() {
    let mut m = Motion::default();
    m.light(Glow::new(GlowKind::Flow, Tint::Spectrum));
    m.reset();
    assert!(
        m.glow.is_some(),
        "a glow asked for before mapping still plays"
    );
}

//! When the island shows what, and that every transition finishes and stops drawing.

use super::*;
use kase::overlay_protocol::{ListeningMode, OverlayPosition};

fn comp(level: u8, charging: bool, present: bool) -> Option<ComponentState> {
    Some(ComponentState {
        level,
        charging,
        present,
        ..Default::default()
    })
}

fn buds(l: u8, r: u8, c: u8) -> OverlayState {
    OverlayState {
        name: "AirPods 4".into(),
        kind: "airpods4".into(),
        connected: true,
        left: comp(l, false, true),
        right: comp(r, false, true),
        case: comp(c, false, true),
        ..Default::default()
    }
}

fn alert(a: Alert) -> Option<Content> {
    Some(Content::Alert(a))
}

fn state(c: &mut Controller, s: OverlayState, at: Instant) {
    c.apply(OverlayCommand::State { state: s }, at);
}

/// Ticks at 60 Hz for `ms`, returning the surface actions and the end time.
fn run(c: &mut Controller, from: Instant, ms: u64) -> (Vec<SurfaceAction>, Instant) {
    let mut out = vec![];
    let mut t = from;
    for _ in 0..(ms / 16) {
        t += Duration::from_millis(16);
        let a = c.tick(t);
        if a != SurfaceAction::None {
            out.push(a);
        }
    }
    (out, t)
}

// ---------------------------------------------------------------------------
// What is worth showing
// ---------------------------------------------------------------------------

#[test]
fn connecting_opens_the_card_and_disconnecting_says_goodbye() {
    let a = buds(80, 75, 40);
    assert_eq!(event(None, &a), Some(Content::Expanded));
    let mut off = a.clone();
    off.connected = false;
    assert_eq!(event(Some(&off), &a), Some(Content::Expanded));
    assert_eq!(event(Some(&a), &off), alert(Alert::Disconnected));
    // Never connected: nothing to say goodbye to.
    assert_eq!(event(None, &off), None);
    assert_eq!(event(Some(&off), &off), None);
}

#[test]
fn small_drift_is_quiet_but_dropping_into_a_low_band_alerts() {
    let a = buds(80, 75, 40);
    assert_eq!(event(Some(&a), &buds(79, 74, 40)), None);
    assert_eq!(
        event(Some(&buds(21, 75, 40)), &buds(20, 75, 40)),
        alert(Alert::Low(Parts {
            left: true,
            ..Default::default()
        }))
    );
    assert_eq!(event(Some(&buds(20, 75, 40)), &buds(19, 75, 40)), None);
    // Both pods at once name both.
    assert_eq!(
        event(Some(&buds(11, 11, 40)), &buds(10, 10, 40)),
        alert(Alert::Low(Parts {
            left: true,
            right: true,
            ..Default::default()
        }))
    );
    // Climbing out of a band (charging back up) is not news.
    assert_eq!(event(Some(&buds(20, 75, 40)), &buds(21, 75, 40)), None);
    // A low level while charging is not an alarm.
    let mut charging = buds(21, 75, 40);
    charging.left = comp(21, true, true);
    let mut lower = charging.clone();
    lower.left = comp(20, true, true);
    assert_eq!(event(Some(&charging), &lower), None);
}

#[test]
fn pods_coming_and_going_reopen_the_card() {
    let a = buds(80, 75, 40);
    let mut b = a.clone();
    b.left = comp(80, false, false);
    assert_eq!(event(Some(&a), &b), Some(Content::Expanded));
    assert_eq!(event(Some(&b), &a), Some(Content::Expanded));
    b.left = None;
    assert_eq!(event(Some(&a), &b), Some(Content::Expanded));
    // Changes on an absent component are ignored.
    let mut d = b.clone();
    d.left = Some(ComponentState {
        level: 5,
        ..Default::default()
    });
    assert_eq!(event(Some(&b), &d), None);
}

#[test]
fn charging_starts_alert_but_stopping_does_not() {
    let a = buds(80, 75, 40);
    let mut c = a.clone();
    c.case = comp(40, true, true);
    assert_eq!(
        event(Some(&a), &c),
        alert(Alert::Charging(Parts {
            case: true,
            ..Default::default()
        }))
    );
    assert_eq!(event(Some(&c), &a), None);
}

#[test]
fn route_and_noise_control_changes_alert() {
    let a = buds(80, 80, 50);
    let mut b = a.clone();
    b.elsewhere = true;
    b.elsewhere_name = Some("iPhone".into());
    assert_eq!(
        event(Some(&a), &b),
        alert(Alert::Elsewhere(Some("iPhone".into())))
    );
    assert_eq!(event(Some(&b), &b), None);
    assert_eq!(event(Some(&b), &a), alert(Alert::Back));

    let mut m = a.clone();
    m.listening_mode = Some(ListeningMode::NoiseCancellation);
    let mut t = m.clone();
    t.listening_mode = Some(ListeningMode::Transparency);
    assert_eq!(
        event(Some(&m), &t),
        alert(Alert::Listening(ListeningMode::Transparency))
    );
    // The first report after connecting is not a change.
    assert_eq!(event(Some(&a), &m), None);
}

#[test]
fn mic_switch_changes_pop_an_alert() {
    use kase::overlay_protocol::MicChoice;
    let mut desk = buds(80, 75, 40);
    desk.mic = Some(MicChoice::Desk);
    desk.listening_mode = Some(ListeningMode::NoiseCancellation);
    let mut airpods = desk.clone();
    airpods.mic = Some(MicChoice::AirPods);
    assert_eq!(
        event(Some(&desk), &airpods),
        alert(Alert::Mic(MicChoice::AirPods))
    );
    assert_eq!(
        event(Some(&airpods), &desk),
        alert(Alert::Mic(MicChoice::Desk))
    );
    // Turning the switch on or off is not a mic change.
    let mut off = desk.clone();
    off.mic = None;
    assert_eq!(event(Some(&off), &desk), None);
    // A failed attempt says so, even though the mic stays the same.
    let mut failed = desk.clone();
    failed.mic_failures = 1;
    assert_eq!(event(Some(&desk), &failed), alert(Alert::MicUnavailable));
    // The mic beats a noise control change at the same moment.
    let mut both = airpods.clone();
    both.listening_mode = Some(ListeningMode::Transparency);
    assert_eq!(
        event(Some(&desk), &both),
        alert(Alert::Mic(MicChoice::AirPods))
    );
}

#[test]
fn the_most_important_change_wins() {
    let a = buds(21, 75, 40);
    let mut b = buds(20, 75, 40);
    b.case = comp(40, true, true);
    b.listening_mode = Some(ListeningMode::Adaptive);
    // Low beats charging.
    assert!(matches!(
        event(Some(&a), &b),
        Some(Content::Alert(Alert::Low(_)))
    ));
    // The route beats everything but connecting.
    b.elsewhere = true;
    assert!(matches!(
        event(Some(&a), &b),
        Some(Content::Alert(Alert::Elsewhere(None)))
    ));
}

#[test]
fn alert_titles() {
    let s = buds(80, 80, 40);
    let p = |left, right, case| Parts {
        left,
        right,
        case,
        headphone: false,
    };
    assert_eq!(
        Alert::Charging(p(false, false, true)).title(&s),
        "Case charging"
    );
    assert_eq!(
        Alert::Charging(p(true, true, false)).title(&s),
        "AirPods charging"
    );
    assert_eq!(Alert::Charging(p(true, true, true)).title(&s), "Charging");
    assert_eq!(
        Alert::Low(p(false, true, false)).title(&s),
        "Right AirPod low"
    );
    assert_eq!(
        Alert::Low(p(true, false, false)).title(&s),
        "Left AirPod low"
    );
    let hp = Parts {
        headphone: true,
        ..Default::default()
    };
    assert_eq!(Alert::Low(hp).title(&s), "Low battery");
    assert_eq!(Alert::Elsewhere(None).title(&s), "On another device");
    assert_eq!(
        Alert::Elsewhere(Some(" ".into())).title(&s),
        "On another device"
    );
    assert_eq!(
        Alert::Elsewhere(Some("David's iPhone".into())).title(&s),
        "Playing on David's iPhone"
    );
    assert_eq!(Alert::Disconnected.title(&s), "AirPods 4");
}

// ---------------------------------------------------------------------------
// Modes and timing
// ---------------------------------------------------------------------------

#[test]
fn popup_mode_opens_folds_and_unmaps_once() {
    let t0 = Instant::now();
    let mut c = Controller::default();
    assert_eq!(c.tick(t0), SurfaceAction::None);
    state(&mut c, buds(80, 75, 40), t0);
    assert_eq!(c.target(t0), Some(Content::Expanded));
    assert_eq!(c.tick(t0), SurfaceAction::Map);
    assert!(c.is_animating(t0));
    // Long enough for the connect glow (under 2 s) to finish too.
    let (acts, t1) = run(&mut c, t0, 2100);
    assert!(acts.is_empty());
    assert_eq!(c.motion.alpha.value, 1.0);
    assert!(!c.is_animating(t1), "settled: no more frames");
    assert!(c.next_deadline().is_some());
    // After auto_hide_secs (4 s) it folds away and unmaps exactly once.
    let (acts, t2) = run(&mut c, t1, 2900);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);
    assert!(!c.is_mapped());
    assert!(!c.is_animating(t2));
    assert_eq!(c.next_deadline(), None);
    // An insignificant update keeps it hidden.
    state(&mut c, buds(79, 75, 40), t2);
    assert_eq!(c.tick(t2), SurfaceAction::None);
    // `show` brings the card back.
    c.apply(
        OverlayCommand::Show {
            reason: "test".into(),
        },
        t2,
    );
    assert_eq!(c.tick(t2), SurfaceAction::Map);
}

#[test]
fn popup_mode_shows_alerts_as_a_pill() {
    let t0 = Instant::now();
    let mut c = Controller::default();
    let mut s = buds(80, 75, 40);
    state(&mut c, s.clone(), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 6000);
    assert!(!c.is_mapped());
    s.case = comp(40, true, true);
    state(&mut c, s, t1);
    assert_eq!(c.tick(t1), SurfaceAction::Map);
    assert!(matches!(
        c.motion.shown,
        Some(Content::Alert(Alert::Charging(_)))
    ));
    // Alerts are shorter than the card.
    let (acts, _) = run(&mut c, t1, 3600);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);
}

#[test]
fn show_while_folding_reopens_without_remapping() {
    let t0 = Instant::now();
    let mut c = Controller::default();
    state(&mut c, buds(80, 75, 40), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 4100);
    assert!(c.is_mapped() && c.motion.alpha.value < 1.0, "folding");
    c.apply(
        OverlayCommand::Show {
            reason: String::new(),
        },
        t1,
    );
    let (acts, _) = run(&mut c, t1, 1200);
    assert!(acts.is_empty());
    assert_eq!(c.motion.alpha.value, 1.0);
    assert_eq!(c.motion.shown, Some(Content::Expanded));
}

#[test]
fn hide_off_and_full_mode() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Always,
        position: OverlayPosition::BottomLeft,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    assert_eq!(c.tick(t0), SurfaceAction::Map);
    let (acts, t1) = run(&mut c, t0, 10_000);
    assert!(acts.is_empty(), "full mode never hides by itself");
    assert_eq!(c.motion.shown, Some(Content::Expanded));

    c.apply(OverlayCommand::Hide, t1);
    let (acts, t2) = run(&mut c, t1, 1000);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);
    c.apply(
        OverlayCommand::Show {
            reason: String::new(),
        },
        t2,
    );
    assert_eq!(c.tick(t2), SurfaceAction::Map);

    // Disconnect: a goodbye, then gone, even in full mode.
    let mut s = buds(80, 75, 40);
    s.connected = false;
    state(&mut c, s, t2);
    assert_eq!(c.target(t2), alert(Alert::Disconnected));
    let (acts, t3) = run(&mut c, t2, 3500);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);

    // Off never shows.
    c.apply(
        OverlayCommand::Config(OverlayConfig {
            mode: OverlayMode::Off,
            ..OverlayConfig::default()
        }),
        t3,
    );
    state(&mut c, buds(80, 75, 40), t3);
    c.apply(
        OverlayCommand::Show {
            reason: String::new(),
        },
        t3,
    );
    let (acts, _) = run(&mut c, t3, 1000);
    assert!(acts.is_empty());
}

#[test]
fn full_mode_keeps_the_card_and_bumps_for_news() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Always,
        ..OverlayConfig::default()
    });
    let mut s = buds(80, 75, 40);
    state(&mut c, s.clone(), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 2100);
    assert!(!c.is_animating(t1));
    s.case = comp(40, true, true);
    state(&mut c, s, t1);
    assert_eq!(c.target(t1), Some(Content::Expanded));
    assert!(c.is_animating(t1), "the bump needs frames");
    assert_eq!(c.headline(t1), "Case charging");
    c.tick(t1);
    assert_eq!(c.motion.caption.prev.as_deref(), Some("Connected"));
    let (_, t2) = run(&mut c, t1, 4000);
    assert_eq!(c.headline(t2), "Connected");
    assert!(!c.is_animating(t2));
    assert_eq!(c.motion.pulse.value, 1.0);
}

#[test]
fn leaving_full_mode_folds_after_one_period() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Always,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 500);
    c.apply(OverlayCommand::Config(OverlayConfig::default()), t1);
    let (acts, _) = run(&mut c, t1, 5000);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);
}

#[test]
fn idle_gap_does_not_skip_the_opening() {
    let t0 = Instant::now();
    let mut c = Controller::default();
    c.tick(t0);
    let later = t0 + Duration::from_secs(3600);
    state(&mut c, buds(80, 75, 40), later);
    assert_eq!(c.tick(later), SurfaceAction::Map);
    c.tick(later + Duration::from_secs(1));
    assert!(
        c.motion.alpha.value < 0.9,
        "one late tick advances at most 50 ms"
    );
}

#[test]
fn preview_shows_without_data_and_in_any_mode() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Off,
        ..OverlayConfig::default()
    });
    assert!(c.display_state(t0).is_none());
    c.apply(
        OverlayCommand::Show {
            reason: "preview".into(),
        },
        t0,
    );
    assert_eq!(c.tick(t0), SurfaceAction::Map);
    assert!(c.display_state(t0).is_some_and(|s| s.left.is_some()));
    assert_eq!(c.headline(t0), "Waiting for battery");
    let (acts, t1) = run(&mut c, t0, 5000);
    assert_eq!(acts, vec![SurfaceAction::Unmap]);
    assert!(c.display_state(t1).is_none());
}

#[test]
fn compact_mode_rests_as_the_slim_island() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    assert_eq!(c.tick(t0), SurfaceAction::Map);
    assert_eq!(
        c.motion.shown,
        Some(Content::Expanded),
        "connect opens the card"
    );
    let (acts, t1) = run(&mut c, t0, 6000);
    assert!(acts.is_empty(), "compact never unmaps while connected");
    assert_eq!(c.motion.shown, Some(Content::Compact));
    assert!(!c.is_animating(t1));
    assert_eq!(c.next_deadline(), None, "nothing scheduled at rest");
    let (w, h, _) = c.motion.size();
    assert_eq!(
        (w, h),
        (shape(&Content::Compact, &buds(80, 75, 40)).0, 36.0)
    );

    // A pod leaving opens the card again, then it folds back.
    let mut s = buds(80, 75, 40);
    s.left.as_mut().unwrap().present = false;
    state(&mut c, s, t1);
    let (acts, t2) = run(&mut c, t1, 600);
    assert!(acts.is_empty());
    assert_eq!(c.motion.shown, Some(Content::Expanded));
    let (_, _) = run(&mut c, t2, 5000);
    assert_eq!(c.motion.shown, Some(Content::Compact));
}

#[test]
fn compact_mode_morphs_into_an_alert_and_back() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        ..OverlayConfig::default()
    });
    let mut s = buds(80, 75, 40);
    s.listening_mode = Some(ListeningMode::NoiseCancellation);
    state(&mut c, s.clone(), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 6000);
    s.listening_mode = Some(ListeningMode::Adaptive);
    state(&mut c, s, t1);
    c.tick(t1);
    assert_eq!(
        c.motion.shown,
        alert(Alert::Listening(ListeningMode::Adaptive))
    );
    assert_eq!(
        c.motion.outgoing.as_ref().map(|(c, _)| c),
        Some(&Content::Compact),
        "the old content fades out while the new one comes in"
    );
    let (_, t2) = run(&mut c, t1, 2600);
    assert_eq!(c.motion.shown, Some(Content::Compact));
    let (_, t3) = run(&mut c, t2, 1200);
    assert!(!c.is_animating(t3));
}

#[test]
fn an_interrupted_transition_keeps_moving_smoothly() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        ..OverlayConfig::default()
    });
    let mut s = buds(80, 75, 40);
    state(&mut c, s.clone(), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 6000);
    // Charging starts, and 100 ms into the morph the iPhone takes the audio.
    s.case = comp(40, true, true);
    state(&mut c, s.clone(), t1);
    let (_, t2) = run(&mut c, t1, 100);
    let mut last_w = c.motion.w.value;
    let v = c.motion.w.velocity;
    s.elsewhere = true;
    state(&mut c, s, t2);
    c.tick(t2);
    assert_eq!(
        c.motion.w.velocity, v,
        "velocity carried into the new target"
    );
    // No frame-to-frame jump bigger than the spring can move in one frame.
    let mut t = t2;
    for _ in 0..60 {
        t += Duration::from_millis(16);
        c.tick(t);
        let w = c.motion.w.value;
        assert!((w - last_w).abs() < 30.0, "jump from {last_w} to {w}");
        last_w = w;
    }
}

#[test]
fn level_changes_roll_while_shown_and_snap_while_hidden() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 6000);
    state(&mut c, buds(79, 75, 40), t1);
    assert!(c.is_animating(t1), "the number rolls");
    assert_eq!(c.motion.levels[crate::island::LEFT].prev, Some(80));
    let (_, t2) = run(&mut c, t1, 1500);
    assert_eq!(c.motion.levels[crate::island::LEFT].prev, None);
    assert!(!c.is_animating(t2));

    // Hidden (pop-up mode, folded): values just snap.
    let mut p = Controller::default();
    state(&mut p, buds(80, 75, 40), t0);
    p.tick(t0);
    let (_, t3) = run(&mut p, t0, 6000);
    state(&mut p, buds(70, 75, 40), t3);
    assert_eq!(p.motion.levels[crate::island::LEFT].prev, None);
    assert!(!p.is_animating(t3));
}

#[test]
fn surface_extent_covers_every_island_and_its_overshoot() {
    let t0 = Instant::now();
    let (sw, sh) = Controller::default().extent();
    for mode in [OverlayMode::Compact, OverlayMode::Always, OverlayMode::Auto] {
        let mut c = Controller::new(OverlayConfig {
            mode,
            ..OverlayConfig::default()
        });
        let mut s = buds(80, 75, 40);
        s.name = "A very long AirPods name that will not fit".into();
        state(&mut c, s.clone(), t0);
        c.tick(t0);
        let mut t = t0;
        let step = |c: &mut Controller, t: &mut Instant, ms: u64| {
            for _ in 0..ms / 8 {
                *t += Duration::from_millis(8);
                c.tick(*t);
                let (w, h, _) = c.motion.size();
                let p = c.motion.pulse.value.max(1.0);
                assert!(w * p <= sw && h * p <= sh, "{w}x{h} x{p} > {sw}x{sh}");
            }
        };
        step(&mut c, &mut t, 1000);
        s.elsewhere = true;
        s.elsewhere_name = Some("x".repeat(80));
        state(&mut c, s.clone(), t);
        step(&mut c, &mut t, 1000);
        s.case = comp(40, true, true);
        s.elsewhere = false;
        state(&mut c, s.clone(), t);
        step(&mut c, &mut t, 300);
        c.apply(
            OverlayCommand::Show {
                reason: String::new(),
            },
            t,
        );
        step(&mut c, &mut t, 2000);
    }
}

#[test]
fn surface_lost_restarts_cleanly() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 500);
    c.surface_lost();
    assert!(!c.is_mapped());
    assert_eq!(c.tick(t1), SurfaceAction::Map);
}

// ---------------------------------------------------------------------------
// Purist style and size
// ---------------------------------------------------------------------------

fn purist() -> Controller {
    Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        style: OverlayStyle::Purist,
        ..OverlayConfig::default()
    })
}

#[test]
fn purist_lights_the_border_for_events_and_then_rests() {
    use crate::island::{GlowKind, Tint};
    let t0 = Instant::now();
    let mut c = purist();
    state(&mut c, buds(80, 75, 40), t0);
    let g = c.motion.glow.expect("connecting lights the border");
    assert_eq!((g.kind, g.tint), (GlowKind::Flow, Tint::Spectrum));
    c.tick(t0);
    let (_, t1) = run(&mut c, t0, 2500);
    assert!(c.motion.glow.is_none());

    let mut s = buds(80, 75, 40);
    s.case = comp(40, true, true);
    state(&mut c, s, t1);
    let g = c.motion.glow.expect("charging lights the border");
    assert_eq!((g.kind, g.tint), (GlowKind::Sweep, Tint::Green));
    assert!(c.is_animating(t1));
    // The pop-up stays for three seconds; the light is long gone by then and the
    // island goes still once it folds back.
    let (_, t2) = run(&mut c, t1, 6000);
    assert!(c.motion.glow.is_none());
    assert!(!c.is_animating(t2), "no frames at rest");
}

#[test]
fn classic_never_spends_frames_on_light() {
    let t0 = Instant::now();
    let mut c = Controller::new(OverlayConfig {
        mode: OverlayMode::Compact,
        style: OverlayStyle::Classic,
        ..OverlayConfig::default()
    });
    state(&mut c, buds(80, 75, 40), t0);
    assert!(c.motion.glow.is_none());
    let mut s = buds(80, 75, 40);
    s.case = comp(40, true, true);
    state(&mut c, s, t0);
    assert!(c.motion.glow.is_none());
}

#[test]
fn purist_shakes_when_the_mic_is_unavailable() {
    let t0 = Instant::now();
    let mut c = purist();
    let mut s = buds(80, 75, 40);
    state(&mut c, s.clone(), t0);
    let (_, t1) = run(&mut c, t0, 2500);
    s.mic_failures = 1;
    state(&mut c, s, t1);
    assert!(c.motion.shake.velocity != 0.0, "the island shakes its head");
    assert_eq!(c.target(t1), alert(Alert::MicUnavailable));
}

#[test]
fn purist_preview_shows_the_light_once() {
    let t0 = Instant::now();
    let mut c = purist();
    let preview = || OverlayCommand::Show {
        reason: "preview".into(),
    };
    c.apply(preview(), t0);
    assert!(c.motion.glow.is_some());
    c.motion.glow = None;
    // Repeated previews while one is open (dragging a slider) don't restart it.
    c.apply(preview(), t0 + Duration::from_millis(100));
    assert!(c.motion.glow.is_none());
}

#[test]
fn surface_room_follows_the_size_setting() {
    use kase::overlay_protocol::OverlaySize;
    let at = |size| {
        Controller::new(OverlayConfig {
            size,
            ..OverlayConfig::default()
        })
        .extent()
    };
    let m = at(OverlaySize::Medium);
    for size in OverlaySize::ALL {
        let e = at(size);
        assert!((e.0 - m.0 * size.scale()).abs() < 0.01);
        assert!((e.1 - m.1 * size.scale()).abs() < 0.01);
    }
    assert!(at(OverlaySize::Small).0 < m.0 && at(OverlaySize::ExtraLarge).0 > m.0);
}

#[test]
fn opening_the_case_pops_a_short_card_and_closing_it_is_quiet() {
    let t0 = Instant::now();
    let in_case = || OverlayState {
        in_case: true,
        ..buds(80, 75, 40)
    };
    let mut c = Controller::default();
    state(&mut c, in_case(), t0);
    assert_eq!(c.target(t0), Some(Content::Expanded));
    assert_eq!(c.headline(t0), "Case open");
    assert_eq!(
        c.target(t0 + Duration::from_millis(5900)),
        Some(Content::Expanded)
    );
    assert_eq!(c.target(t0 + Duration::from_millis(6100)), None);
    // Routine news from the case (the pods start charging) doesn't pop it again.
    let mut charging = in_case();
    charging.left = comp(80, true, true);
    let t1 = t0 + Duration::from_secs(7);
    state(&mut c, charging.clone(), t1);
    assert_eq!(c.target(t1), None);
    // The lid closes: no "Disconnected" goodbye.
    charging.connected = false;
    state(&mut c, charging, t1);
    assert_eq!(c.target(t1), None);
}

#[test]
fn case_card_folds_soon_after_a_pod_comes_out() {
    let t0 = Instant::now();
    let mut c = Controller::default();
    state(
        &mut c,
        OverlayState {
            in_case: true,
            ..buds(80, 75, 40)
        },
        t0,
    );
    let t1 = t0 + Duration::from_secs(1);
    state(&mut c, buds(80, 75, 40), t1);
    assert_eq!(
        c.target(t1 + Duration::from_millis(1900)),
        Some(Content::Expanded)
    );
    assert_eq!(c.target(t1 + Duration::from_millis(2100)), None);
}

#[test]
fn no_case_card_when_switched_off_or_over_a_game() {
    let t0 = Instant::now();
    let in_case = OverlayState {
        in_case: true,
        ..buds(80, 75, 40)
    };
    let mut off = Controller::new(OverlayConfig {
        case_card: false,
        ..Default::default()
    });
    state(&mut off, in_case.clone(), t0);
    assert_eq!(off.target(t0), None);
    let mut game = Controller::default();
    game.apply(OverlayCommand::Fullscreen { active: true }, t0);
    state(&mut game, in_case, t0);
    assert_eq!(game.target(t0), None);
}

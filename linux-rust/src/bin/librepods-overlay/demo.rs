//! Fake states for `--demo`, so the design can be tried without AirPods.

use crate::overlay_protocol::{ComponentState, OverlayCommand, OverlayState};

fn c(level: u8, charging: bool, present: bool) -> Option<ComponentState> {
    Some(ComponentState {
        level,
        charging,
        present,
        ..Default::default()
    })
}

fn buds(name: &str, kind: &str) -> OverlayState {
    OverlayState {
        name: name.into(),
        kind: kind.into(),
        connected: true,
        left: c(82, false, true),
        right: c(77, false, true),
        case: c(46, false, true),
        headphone: None,
        ..Default::default()
    }
}

pub fn states() -> Vec<(&'static str, OverlayState)> {
    let normal = buds("AirPods 4", "airpods4");

    let mut case_charging = normal.clone();
    case_charging.case = c(46, true, true);

    let mut pod_out = normal.clone();
    pod_out.left = c(82, false, false);

    let mut low = buds("David's AirPods Pro", "airpods_pro2");
    low.left = c(18, false, true);
    low.right = c(9, false, true);
    low.case = c(64, false, true);

    let mut stale_case = normal.clone();
    stale_case.case = Some(ComponentState {
        level: 46,
        present: false,
        stale: true,
        last_seen_secs: Some(12 * 60),
        ..Default::default()
    });

    let mut all_charging = buds("AirPods Pro", "airpods_pro2");
    all_charging.left = c(64, true, true);
    all_charging.right = c(100, true, true);
    all_charging.case = c(31, true, true);

    let mut elsewhere = normal.clone();
    elsewhere.elsewhere = true;
    elsewhere.elsewhere_name = Some("iPhone".into());

    let max = OverlayState {
        name: "AirPods Max".into(),
        kind: "airpods_max".into(),
        connected: true,
        headphone: c(68, false, true),
        ..Default::default()
    };

    vec![
        ("normal", normal),
        ("case-charging", case_charging),
        ("pod-out", pod_out),
        ("low", low),
        ("stale-case", stale_case),
        ("all-charging", all_charging),
        ("elsewhere", elsewhere),
        ("max", max),
    ]
}

/// A scripted run through every island state, for recordings: (seconds from start,
/// command). Starts in compact mode.
pub fn story() -> Vec<(f32, OverlayCommand)> {
    use crate::overlay_protocol::ListeningMode;
    let mut s = buds("AirPods 4", "airpods4_anc");
    s.listening_mode = Some(ListeningMode::NoiseCancellation);
    s.mic = Some(crate::overlay_protocol::MicChoice::Desk);
    let mut out = vec![];
    let mut at = |t: f32, state: &OverlayState| {
        out.push((
            t,
            OverlayCommand::State {
                state: state.clone(),
            },
        ))
    };
    // Connect: the full card opens, then folds into the compact island.
    at(0.6, &s);
    // The case goes on the charger.
    s.case = c(46, true, true);
    at(6.4, &s);
    // Noise control changes on the AirPods.
    s.listening_mode = Some(ListeningMode::Transparency);
    at(10.6, &s);
    // A level ticks down: the number rolls.
    s.left = c(81, false, true);
    at(13.4, &s);
    // The right pod drops into the low band.
    s.right = c(19, false, true);
    at(15.0, &s);
    // The iPhone takes the audio, then gives it back.
    s.elsewhere = true;
    s.elsewhere_name = Some("iPhone".into());
    at(21.0, &s);
    s.elsewhere = false;
    s.elsewhere_name = None;
    at(25.4, &s);
    // The left stem hold swaps to the AirPods mic.
    s.mic = Some(crate::overlay_protocol::MicChoice::AirPods);
    at(33.5, &s);
    // Disconnect.
    s.connected = false;
    at(36.0, &s);
    out.push((
        29.0,
        OverlayCommand::Show {
            reason: "demo".into(),
        },
    ));
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    out
}

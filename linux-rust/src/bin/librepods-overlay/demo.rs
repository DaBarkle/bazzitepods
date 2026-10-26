//! Fake states for `--demo`, so the design can be tried without AirPods.

use crate::overlay_protocol::{ComponentState, OverlayState};

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

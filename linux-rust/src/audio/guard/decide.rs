//! Pure decisions and tunables: which profile, sink and codec the AirPods should be on.

use super::*;

/// How long after a take-back stalls are acted on quickly.
pub(super) const FAST_WINDOW: Duration = Duration::from_secs(10);
/// Stall threshold inside that window.
pub(super) const FAST_STALL: Duration = Duration::from_millis(800);
/// Minimum time between forced output rebuilds on take-back.
/// After a rebuild, how long to keep the speakers muted while WirePlumber moves the
/// streams back onto the AirPods.
pub(super) const UNHUSH_WAIT: Duration = Duration::from_secs(2);
pub(super) const UNHUSH_MARGIN: Duration = Duration::from_millis(250);
/// The longest the speakers stay muted after [`Cmd::Hush`] without a rebuild.
pub(super) const HUSH_LIMIT: Duration = Duration::from_secs(8);
/// After a rebuild, how long the AirPods get to start playing before the codec is
/// switched instead. Right after the mic stream stops they sometimes refuse to start
/// the rebuilt AAC stream ("Start error"), and only a fresh transport plays again.
pub(super) const REBUILD_FLOW_WAIT: Duration = Duration::from_millis(1200);
/// After falling back to a lesser codec, switch back to the best one once nothing is
/// playing, or after this long if music keeps playing.
pub(super) const BEST_CODEC_RETURN: Duration = Duration::from_secs(20);
/// Two take-backs this close together share one rebuild. Bouncing between devices is
/// prevented where it starts (devices/airpods.rs), so this only merges duplicates: a
/// longer limit left the AirPods silent after a quick switch back.
pub(super) const REPAIR_COOLDOWN: Duration = Duration::from_secs(3);

pub(super) const PREFERRED_A2DP: [&str; 4] = [
    "a2dp-sink-aac",
    "a2dp-sink",
    "a2dp-sink-sbc_xq",
    "a2dp-sink-sbc",
];

/// PipeWire names profiles `a2dp-sink-aac`, PulseAudio `a2dp_sink` / `a2dp_sink_aac`;
/// compare in PipeWire's spelling.
fn pw_name(profile: &str) -> String {
    profile.replace('_', "-")
}

fn is_a2dp_sink(profile: &str) -> bool {
    pw_name(profile).starts_with("a2dp-sink")
}

pub fn is_a2dp(profile: &str) -> bool {
    profile.starts_with("a2dp")
}

pub fn is_headset(profile: &str) -> bool {
    profile.starts_with("headset") || profile.starts_with("handsfree")
}

pub fn is_off(profile: Option<&str>) -> bool {
    matches!(profile, None | Some("") | Some("off"))
}

/// A2DP sink profiles of a card, best first: AAC, auto, SBC-XQ, SBC, then the rest
/// by priority. Unavailable profiles are dropped unless nothing else is left.
pub fn a2dp_candidates(card: &CardInfo) -> Vec<String> {
    let rank = |avail_only: bool| {
        let mut list: Vec<&crate::audio::pulse::ProfileInfo> = card
            .profiles
            .iter()
            .filter(|p| is_a2dp_sink(&p.name) && (!avail_only || p.available))
            .collect();
        list.sort_by_key(|p| {
            let pref = PREFERRED_A2DP
                .iter()
                .position(|n| pw_name(n) == pw_name(&p.name))
                .unwrap_or(PREFERRED_A2DP.len());
            (pref, std::cmp::Reverse(p.priority))
        });
        list.into_iter().map(|p| p.name.clone()).collect::<Vec<_>>()
    };
    let available = rank(true);
    if available.is_empty() {
        rank(false)
    } else {
        available
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileAction {
    /// Already on a usable profile.
    Keep,
    /// On HFP with the mic in use: do not touch.
    LeaveHeadset,
    /// Switch, trying these in order.
    Set(Vec<String>),
}

pub fn decide_profile(card: &CardInfo, recording: bool) -> ProfileAction {
    let active = card.active_profile.as_deref();
    match active {
        a if is_off(a) => ProfileAction::Set(a2dp_candidates(card)),
        Some(p) if is_a2dp(p) => ProfileAction::Keep,
        Some(p) if is_headset(p) => {
            if recording {
                ProfileAction::LeaveHeadset
            } else {
                ProfileAction::Set(a2dp_candidates(card))
            }
        }
        // Something else the user picked on purpose (pro-audio etc.): respect it.
        _ => ProfileAction::Keep,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SinkAction {
    Nothing,
    MakeDefault {
        sink_name: String,
        sink_index: u32,
        move_inputs: Vec<u32>,
    },
}

/// Restore our sink as default only when it was the default before the link
/// dropped (so the current default is just WirePlumber's fallback); never
/// override a default the user picked while we were away... or one we never had.
pub fn decide_sink(snap: &PulseSnapshot, ours: &SinkInfo, we_were_default: bool) -> SinkAction {
    if snap.default_sink.as_deref() == Some(ours.name.as_str()) || !we_were_default {
        return SinkAction::Nothing;
    }
    let fallback = snap
        .default_sink
        .as_deref()
        .and_then(|n| snap.sink_by_name(n))
        .map(|s| s.index);
    let move_inputs = snap
        .sink_inputs
        .iter()
        .filter(|i| Some(i.sink) == fallback)
        .map(|i| i.index)
        .collect();
    SinkAction::MakeDefault {
        sink_name: ours.name.clone(),
        sink_index: ours.index,
        move_inputs,
    }
}

/// Codec for display: PipeWire's `api.bluez5.codec`, else derived from the profile.
pub fn codec_for(profile: &str, sink: Option<&SinkInfo>) -> Option<String> {
    if let Some(c) = sink.and_then(|s| s.codec.clone()) {
        return Some(c);
    }
    let profile = pw_name(profile);
    for prefix in ["a2dp-sink-", "headset-head-unit-"] {
        if let Some(c) = profile.strip_prefix(prefix) {
            return Some(c.to_string());
        }
    }
    None
}

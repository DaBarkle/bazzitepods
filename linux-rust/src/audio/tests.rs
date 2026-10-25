//! State machine tests against fake pulse/BlueZ backends (real time, short timings).

use super::bluez::BluezApi;
use super::guard::*;
use super::pulse::*;
use super::{AudioGuard, AudioGuardHandle, AudioHealth, ensure_a2dp_profile_with};
use bluer::{Address, Uuid};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAC: &str = "A0:B1:C2:D3:E4:F5";
const CARD: u32 = 10;
const BT_SINK: u32 = 20;
const SPEAKERS: u32 = 1;
const SPEAKERS_NAME: &str = "alsa_output.speakers";

fn bt_sink_name() -> String {
    format!("bluez_output.{}.1", MAC.replace(':', "_"))
}

#[derive(Default)]
struct World {
    snap: PulseSnapshot,
    transports: Vec<String>,
    log: Vec<String>,
    /// ConnectProfile makes the card appear (on a2dp-sink).
    connect_creates_card: bool,
    /// The bluez sink, when present, is RUNNING with an uncorked stream.
    playing: bool,
    /// Transport state after a successful a2dp profile set following "off".
    transport_after_toggle: Option<String>,
    toggled_off: bool,
}

impl World {
    fn new() -> Self {
        let mut w = World::default();
        w.snap.sinks.push(SinkInfo {
            index: SPEAKERS,
            name: SPEAKERS_NAME.into(),
            card: Some(1),
            mac: None,
            state: SinkState::Idle,
            codec: None,
        });
        w.snap.default_sink = Some(SPEAKERS_NAME.into());
        w
    }

    fn add_card(&mut self, active: &str) {
        let names = [
            "off",
            "a2dp-sink-sbc",
            "a2dp-sink-sbc_xq",
            "a2dp-sink",
            "a2dp-sink-aac",
            "headset-head-unit",
        ];
        self.snap.cards.push(CardInfo {
            index: CARD,
            name: format!("bluez_card.{}", MAC.replace(':', "_")),
            mac: Some(MAC.into()),
            profiles: names
                .iter()
                .map(|n| ProfileInfo {
                    name: n.to_string(),
                    available: true,
                    priority: 1,
                })
                .collect(),
            active_profile: Some(active.into()),
        });
        self.sync_sink();
    }

    fn remove_card(&mut self) {
        self.snap.cards.retain(|c| c.index != CARD);
        self.sync_sink();
    }

    fn active(&self) -> Option<String> {
        self.snap
            .cards
            .iter()
            .find(|c| c.index == CARD)
            .and_then(|c| c.active_profile.clone())
    }

    /// Mimics PipeWire/WirePlumber: sink exists iff the card has a non-off profile;
    /// when it goes away its streams and the default fall back to the speakers.
    fn sync_sink(&mut self) {
        let active = self.active();
        let want = matches!(active.as_deref(), Some(p) if p != "off");
        let have = self.snap.sinks.iter().any(|s| s.index == BT_SINK);
        if want {
            let state = if self.playing {
                SinkState::Running
            } else {
                SinkState::Idle
            };
            let codec = active
                .as_deref()
                .and_then(|p| p.strip_prefix("a2dp-sink-"))
                .map(str::to_string);
            self.snap.sinks.retain(|s| s.index != BT_SINK);
            self.snap.sinks.push(SinkInfo {
                index: BT_SINK,
                name: bt_sink_name(),
                card: Some(CARD),
                mac: Some(MAC.into()),
                state,
                codec,
            });
            if self.playing && !self.snap.sink_inputs.iter().any(|i| i.sink == BT_SINK) {
                self.snap.sink_inputs.push(SinkInputInfo {
                    index: 100,
                    sink: BT_SINK,
                    corked: false,
                });
            }
        } else if have {
            self.snap.sinks.retain(|s| s.index != BT_SINK);
            for i in self
                .snap
                .sink_inputs
                .iter_mut()
                .filter(|i| i.sink == BT_SINK)
            {
                i.sink = SPEAKERS;
            }
            if self.snap.default_sink.as_deref() == Some(bt_sink_name().as_str()) {
                self.snap.default_sink = Some(SPEAKERS_NAME.into());
            }
        }
    }

    fn count(&self, entry: &str) -> usize {
        self.log.iter().filter(|l| *l == entry).count()
    }
}

type Shared = Arc<Mutex<World>>;

struct FakePulse(Shared);
struct FakeBluez(Shared);

fn lock(w: &Shared) -> std::sync::MutexGuard<'_, World> {
    w.lock().unwrap_or_else(|p| p.into_inner())
}

impl PulseApi for FakePulse {
    async fn snapshot(&self) -> Result<PulseSnapshot, PulseError> {
        Ok(lock(&self.0).snap.clone())
    }

    async fn set_card_profile(&self, card_index: u32, profile: &str) -> Result<(), PulseError> {
        let mut w = lock(&self.0);
        w.log.push(format!("profile:{profile}"));
        let Some(card) = w.snap.cards.iter_mut().find(|c| c.index == card_index) else {
            return Err(PulseError::Failed("no such card".into()));
        };
        card.active_profile = Some(profile.to_string());
        if profile == "off" {
            w.toggled_off = true;
        } else if w.toggled_off
            && let Some(t) = w.transport_after_toggle.clone()
        {
            w.transports = vec![t];
        }
        w.sync_sink();
        Ok(())
    }

    async fn set_default_sink(&self, name: &str) -> Result<(), PulseError> {
        let mut w = lock(&self.0);
        w.log.push(format!("default:{name}"));
        w.snap.default_sink = Some(name.to_string());
        Ok(())
    }

    async fn move_sink_input(&self, input_index: u32, sink_index: u32) -> Result<(), PulseError> {
        let mut w = lock(&self.0);
        w.log.push(format!("move:{input_index}->{sink_index}"));
        for i in w
            .snap
            .sink_inputs
            .iter_mut()
            .filter(|i| i.index == input_index)
        {
            i.sink = sink_index;
        }
        Ok(())
    }
}

impl BluezApi for FakeBluez {
    async fn transport_states(&self, _mac: Address) -> Result<Vec<String>, String> {
        Ok(lock(&self.0).transports.clone())
    }

    async fn connect_profile(&self, _mac: Address, _uuid: Uuid) -> Result<(), String> {
        let mut w = lock(&self.0);
        w.log.push("connect".into());
        if w.connect_creates_card && w.snap.cards.is_empty() {
            w.add_card("a2dp-sink");
            w.transports = vec!["idle".into()];
        }
        Ok(())
    }

    async fn disconnect_profile(&self, _mac: Address, _uuid: Uuid) -> Result<(), String> {
        lock(&self.0).log.push("disconnect".into());
        Ok(())
    }
}

fn fast() -> GuardConfig {
    GuardConfig {
        link_debounce: Duration::from_millis(20),
        card_wait: Duration::from_millis(100),
        poll: Duration::from_millis(10),
        connect_profile_retries: 2,
        monitor_interval: Duration::from_millis(20),
        stall_threshold: Duration::from_millis(60),
        toggle_gap: Duration::from_millis(5),
        reconnect_gap: Duration::from_millis(5),
        verify_timeout: Duration::from_millis(60),
        ladder_cooldown: Duration::from_secs(30),
        max_ladders_per_link: 3,
        retry_after_failure: Duration::from_secs(30),
        max_ensure_attempts: 3,
    }
}

fn mac() -> Address {
    MAC.parse().unwrap()
}

fn start(world: World, cfg: GuardConfig) -> (Shared, AudioGuardHandle) {
    let shared = Arc::new(Mutex::new(world));
    let handle = AudioGuard::spawn_with(
        mac(),
        FakePulse(shared.clone()),
        FakeBluez(shared.clone()),
        cfg,
    );
    (shared, handle)
}

/// Records every health value the guard publishes.
fn record(handle: &AudioGuardHandle) -> Arc<Mutex<Vec<AudioHealth>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let mut rx = handle.health();
    let s = seen.clone();
    tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let h = rx.borrow_and_update().clone();
            s.lock().unwrap().push(h);
        }
    });
    seen
}

async fn wait_health(
    handle: &AudioGuardHandle,
    what: &str,
    pred: impl FnMut(&AudioHealth) -> bool,
) -> AudioHealth {
    let mut rx = handle.health();
    let res = tokio::time::timeout(Duration::from_secs(5), async {
        rx.wait_for(pred).await.map(|h| h.clone())
    })
    .await;
    match res {
        Ok(Ok(h)) => h,
        _ => panic!(
            "timed out waiting for {what}; health = {:?}",
            *handle.health().borrow()
        ),
    }
}

fn ready_with(h: &AudioHealth, profile: &str) -> bool {
    matches!(h, AudioHealth::Ready { profile: p, .. } if p == profile)
}

async fn settle(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn startup_cleanup_fixes_card_left_off_by_old_version() {
    let mut w = World::new();
    w.add_card("off");
    let (world, handle) = start(w, fast());
    for _ in 0..200 {
        if lock(&world).active().as_deref() == Some("a2dp-sink-aac") {
            break;
        }
        settle(5).await;
    }
    assert_eq!(lock(&world).active().as_deref(), Some("a2dp-sink-aac"));
    assert_eq!(*handle.health().borrow(), AudioHealth::Disconnected);
    handle.shutdown();
}

#[tokio::test]
async fn saved_off_profile_restored_on_reconnect() {
    let (world, handle) = start(World::new(), fast());
    handle.link_up();
    // WirePlumber restores the poisoned "off" profile when the card reappears.
    lock(&world).add_card("off");
    let h = wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    assert_eq!(
        h,
        AudioHealth::Ready {
            profile: "a2dp-sink-aac".into(),
            codec: Some("aac".into())
        }
    );
    let w = lock(&world);
    assert_eq!(w.active().as_deref(), Some("a2dp-sink-aac"));
    assert_eq!(w.count("connect"), 0, "card was present; no ConnectProfile");
    assert_eq!(w.count("profile:off"), 0);
}

#[tokio::test]
async fn missing_card_triggers_connect_profile() {
    let mut w = World::new();
    w.connect_creates_card = true;
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink")).await;
    assert_eq!(lock(&world).count("connect"), 1);
}

#[tokio::test]
async fn missing_card_connect_profile_is_bounded() {
    let (world, handle) = start(World::new(), fast());
    handle.link_up();
    let h = wait_health(&handle, "failed", |h| {
        matches!(h, AudioHealth::Failed { .. })
    })
    .await;
    assert!(matches!(h, AudioHealth::Failed { reason } if reason.contains("A2DP")));
    settle(300).await;
    assert_eq!(
        lock(&world).count("connect"),
        2,
        "exactly connect_profile_retries attempts"
    );
}

#[tokio::test]
async fn transport_present_without_card_does_not_connect_profile() {
    let mut w = World::new();
    w.transports = vec!["idle".into()];
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "failed", |h| {
        matches!(h, AudioHealth::Failed { .. })
    })
    .await;
    assert_eq!(lock(&world).count("connect"), 0);
}

fn stalled_world() -> World {
    let mut w = World::new();
    w.playing = true;
    w.add_card("a2dp-sink-aac");
    w.transports = vec!["idle".into()];
    w
}

#[tokio::test]
async fn stall_runs_ladder_in_order_and_never_ends_off() {
    let (world, handle) = start(stalled_world(), fast());
    let seen = record(&handle);
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    lock(&world).log.clear();
    let h = wait_health(&handle, "failed", |h| {
        matches!(h, AudioHealth::Failed { .. })
    })
    .await;
    assert!(matches!(h, AudioHealth::Failed { reason } if reason.contains("stalled")));

    let w = lock(&world);
    let ops: Vec<&str> = w
        .log
        .iter()
        .map(String::as_str)
        .filter(|l| l.starts_with("profile:") || *l == "connect" || *l == "disconnect")
        .collect();
    assert_eq!(
        ops,
        vec![
            "profile:off",           // step 1: toggle ...
            "profile:a2dp-sink-aac", // ... back to the same A2DP profile
            "profile:a2dp-sink",     // step 2: alternate codec
            "disconnect",            // step 3: reconnect A2DP
            "connect",
        ]
    );
    let active = w.active().unwrap_or_default();
    assert!(active.starts_with("a2dp"), "card left on {active}");
    drop(w);

    let steps: Vec<u8> = seen
        .lock()
        .unwrap()
        .iter()
        .filter_map(|h| match h {
            AudioHealth::Recovering { step } => Some(*step),
            _ => None,
        })
        .collect();
    assert_eq!(steps, vec![1, 2, 3]);
}

#[tokio::test]
async fn stall_recovered_by_toggle_stops_ladder() {
    let mut w = stalled_world();
    w.transport_after_toggle = Some("active".into());
    let (world, handle) = start(w, fast());
    let seen = record(&handle);
    handle.link_up();
    wait_health(&handle, "recovering", |h| {
        matches!(h, AudioHealth::Recovering { .. })
    })
    .await;
    wait_health(&handle, "ready again", |h| ready_with(h, "a2dp-sink-aac")).await;
    settle(200).await;
    let w = lock(&world);
    assert_eq!(w.count("profile:off"), 1);
    assert_eq!(w.count("disconnect"), 0);
    assert!(
        !seen
            .lock()
            .unwrap()
            .iter()
            .any(|h| matches!(h, AudioHealth::Recovering { step: 2 | 3 }))
    );
}

#[tokio::test]
async fn headset_with_active_recording_is_left_alone() {
    let mut w = World::new();
    w.add_card("headset-head-unit");
    w.snap.sources.push(SourceInfo {
        index: 30,
        card: Some(CARD),
        is_monitor: false,
    });
    w.snap.source_outputs.push(SourceOutputInfo {
        index: 31,
        source: 30,
        corked: false,
    });
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "headset-head-unit")).await;
    settle(100).await;
    let w = lock(&world);
    assert!(
        !w.log.iter().any(|l| l.starts_with("profile:")),
        "log: {:?}",
        w.log
    );
    assert_eq!(w.active().as_deref(), Some("headset-head-unit"));
}

#[tokio::test]
async fn headset_without_recording_is_switched_to_a2dp() {
    let mut w = World::new();
    w.add_card("headset-head-unit");
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    assert_eq!(lock(&world).active().as_deref(), Some("a2dp-sink-aac"));
}

#[tokio::test]
async fn recovery_is_rate_limited() {
    // Default cooldown in fast() is 30 s: the stall persists but only one ladder runs.
    let (world, handle) = start(stalled_world(), fast());
    handle.link_up();
    wait_health(&handle, "failed", |h| {
        matches!(h, AudioHealth::Failed { .. })
    })
    .await;
    settle(500).await;
    let w = lock(&world);
    assert_eq!(w.count("profile:off"), 1);
    assert_eq!(w.count("disconnect"), 1);
}

#[tokio::test]
async fn recovery_ladders_per_link_are_bounded() {
    let mut cfg = fast();
    cfg.ladder_cooldown = Duration::from_millis(30);
    cfg.max_ladders_per_link = 2;
    let (world, handle) = start(stalled_world(), cfg);
    handle.link_up();
    settle(2500).await;
    let w = lock(&world);
    assert_eq!(w.count("disconnect"), 2, "log: {:?}", w.log);
    assert!(w.active().unwrap_or_default().starts_with("a2dp"));
    drop(w);
    assert!(matches!(
        *handle.health().borrow(),
        AudioHealth::Failed { .. }
    ));
}

#[tokio::test]
async fn taking_buds_out_does_not_touch_profile() {
    let mut w = World::new();
    w.add_card("a2dp-sink-aac");
    let (world, handle) = start(w, fast());
    handle.link_up();
    handle.wearing_changed(true);
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    handle.wearing_changed(false);
    settle(150).await;
    let w = lock(&world);
    assert!(
        !w.log.iter().any(|l| l.starts_with("profile:")),
        "log: {:?}",
        w.log
    );
    assert_eq!(w.active().as_deref(), Some("a2dp-sink-aac"));
}

#[tokio::test]
async fn default_sink_and_streams_restored_after_range_loss() {
    let mut w = World::new();
    w.add_card("a2dp-sink-aac");
    w.snap.default_sink = Some(bt_sink_name());
    w.snap.sink_inputs.push(SinkInputInfo {
        index: 100,
        sink: BT_SINK,
        corked: true,
    });
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    settle(60).await;

    // Walk away: link drops, card disappears, WirePlumber falls back to speakers.
    handle.link_down();
    lock(&world).remove_card();
    wait_health(&handle, "disconnected", |h| *h == AudioHealth::Disconnected).await;
    {
        let w = lock(&world);
        assert_eq!(w.snap.default_sink.as_deref(), Some(SPEAKERS_NAME));
        assert_eq!(w.snap.sink_inputs[0].sink, SPEAKERS);
    }

    // Come back.
    lock(&world).add_card("a2dp-sink-aac");
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    let w = lock(&world);
    assert_eq!(
        w.snap.default_sink.as_deref(),
        Some(bt_sink_name().as_str())
    );
    assert_eq!(w.snap.sink_inputs[0].sink, BT_SINK);
    assert_eq!(w.count(&format!("move:100->{BT_SINK}")), 1);
}

#[tokio::test]
async fn default_restored_by_a_new_guard_after_reconnect() {
    // Real reconnects create a fresh device task and guard; the "we were default" memory
    // must survive that.
    let mut w = World::new();
    w.add_card("a2dp-sink-aac");
    w.snap.default_sink = Some(bt_sink_name());
    w.snap.sink_inputs.push(SinkInputInfo {
        index: 100,
        sink: BT_SINK,
        corked: true,
    });
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    settle(60).await;
    handle.link_down();
    lock(&world).remove_card();
    wait_health(&handle, "disconnected", |h| *h == AudioHealth::Disconnected).await;
    handle.shutdown();

    lock(&world).add_card("a2dp-sink-aac");
    let handle2 = AudioGuard::spawn_with(
        mac(),
        FakePulse(world.clone()),
        FakeBluez(world.clone()),
        fast(),
    );
    handle2.link_up();
    wait_health(&handle2, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    let w = lock(&world);
    assert_eq!(
        w.snap.default_sink.as_deref(),
        Some(bt_sink_name().as_str())
    );
    assert_eq!(w.snap.sink_inputs[0].sink, BT_SINK);
}

#[tokio::test]
async fn user_chosen_default_is_respected() {
    // We were never the default: do not steal it.
    let mut w = World::new();
    w.add_card("a2dp-sink-aac");
    let (world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    let w = lock(&world);
    assert_eq!(w.snap.default_sink.as_deref(), Some(SPEAKERS_NAME));
    assert!(!w.log.iter().any(|l| l.starts_with("default:")));
}

#[tokio::test]
async fn link_down_and_shutdown() {
    let mut w = World::new();
    w.add_card("a2dp-sink-aac");
    let (_world, handle) = start(w, fast());
    handle.link_up();
    wait_health(&handle, "ready", |h| ready_with(h, "a2dp-sink-aac")).await;
    handle.link_down();
    wait_health(&handle, "disconnected", |h| *h == AudioHealth::Disconnected).await;
    handle.shutdown();
    let mut rx = handle.health();
    // The guard drops its sender when it stops.
    let closed = tokio::time::timeout(Duration::from_secs(2), async {
        while rx.changed().await.is_ok() {}
    })
    .await;
    assert!(closed.is_ok(), "guard task did not stop");
}

#[tokio::test]
async fn one_shot_helper_fixes_off() {
    let mut w = World::new();
    w.add_card("off");
    let shared = Arc::new(Mutex::new(w));
    let pulse = FakePulse(shared.clone());
    assert_eq!(
        ensure_a2dp_profile_with(&pulse, &MAC.to_lowercase()).await,
        Ok("a2dp-sink-aac".to_string())
    );
    assert_eq!(
        ensure_a2dp_profile_with(&pulse, MAC).await,
        Ok("a2dp-sink-aac".to_string())
    );
    assert_eq!(lock(&shared).count("profile:a2dp-sink-aac"), 1);
}

// --- pure helpers ----------------------------------------------------------

fn card_with(profiles: &[(&str, bool, u32)], active: Option<&str>) -> CardInfo {
    CardInfo {
        index: 1,
        name: "bluez_card.x".into(),
        mac: Some(MAC.into()),
        profiles: profiles
            .iter()
            .map(|(n, a, p)| ProfileInfo {
                name: n.to_string(),
                available: *a,
                priority: *p,
            })
            .collect(),
        active_profile: active.map(str::to_string),
    }
}

#[test]
fn candidates_prefer_aac_then_auto_then_sbc_xq_then_sbc() {
    let card = card_with(
        &[
            ("a2dp-sink-sbc", true, 9),
            ("a2dp-sink-ldac", true, 50),
            ("a2dp-sink-sbc_xq", true, 9),
            ("a2dp-sink", true, 1),
            ("a2dp-sink-aac", true, 1),
            ("headset-head-unit", true, 99),
            ("off", true, 0),
        ],
        None,
    );
    assert_eq!(
        a2dp_candidates(&card),
        vec![
            "a2dp-sink-aac",
            "a2dp-sink",
            "a2dp-sink-sbc_xq",
            "a2dp-sink-sbc",
            "a2dp-sink-ldac"
        ]
    );
    let card = card_with(
        &[("a2dp-sink-aac", false, 1), ("a2dp-sink-sbc", true, 1)],
        None,
    );
    assert_eq!(a2dp_candidates(&card), vec!["a2dp-sink-sbc"]);
    let card = card_with(&[("a2dp-sink-aac", false, 1)], None);
    assert_eq!(
        a2dp_candidates(&card),
        vec!["a2dp-sink-aac"],
        "fallback when none available"
    );
}

#[test]
fn profile_decisions() {
    let profiles = [("a2dp-sink", true, 1), ("headset-head-unit", true, 1)];
    let set = ProfileAction::Set(vec!["a2dp-sink".into()]);
    assert_eq!(
        decide_profile(&card_with(&profiles, Some("off")), false),
        set
    );
    assert_eq!(decide_profile(&card_with(&profiles, None), true), set);
    assert_eq!(
        decide_profile(&card_with(&profiles, Some("a2dp-sink-sbc")), false),
        ProfileAction::Keep
    );
    assert_eq!(
        decide_profile(&card_with(&profiles, Some("headset-head-unit")), true),
        ProfileAction::LeaveHeadset
    );
    assert_eq!(
        decide_profile(&card_with(&profiles, Some("headset-head-unit-msbc")), false),
        set
    );
    assert_eq!(
        decide_profile(&card_with(&profiles, Some("pro-audio")), false),
        ProfileAction::Keep
    );
}

#[test]
fn codec_names() {
    assert_eq!(codec_for("a2dp-sink-aac", None).as_deref(), Some("aac"));
    assert_eq!(codec_for("a2dp-sink", None), None);
    assert_eq!(
        codec_for("headset-head-unit-msbc", None).as_deref(),
        Some("msbc")
    );
    let sink = SinkInfo {
        index: 1,
        name: "s".into(),
        card: None,
        mac: None,
        state: SinkState::Idle,
        codec: Some("aac".into()),
    };
    assert_eq!(codec_for("a2dp-sink", Some(&sink)).as_deref(), Some("aac"));
}

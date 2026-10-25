//! MPRIS pause/resume around ear detection, playback takeover, and conversational
//! awareness volume. This module never changes the bluez card profile to `off`
//! (WirePlumber would persist it and audio would stay dead on reconnect); the A2DP
//! path is owned by `crate::audio::AudioGuard`.

use crate::bluetooth::aacp::AACPManager;
use crate::bluetooth::aacp::EarDetectionStatus;
use dbus::blocking::Connection;
use dbus::blocking::stdintf::org_freedesktop_dbus::Properties;
use dbus::message::MatchRule;
use log::{debug, error, info, warn};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc, watch};

const DBUS_TIMEOUT: Duration = Duration::from_secs(2);
const MPRIS_PATH: &str = "/org/mpris/MediaPlayer2";
const MPRIS_PLAYER: &str = "org.mpris.MediaPlayer2.Player";

struct MediaControllerState {
    connected_device_mac: String,
    local_mac: String,
    is_playing: bool,
    paused_by_app_services: Vec<String>,
    old_in_ear_data: Vec<bool>,
    user_played_the_media: bool,
    i_paused_the_media: bool,
    ear_detection_enabled: bool,
    conv_original_volume: Option<u32>,
    conv_conversation_started: bool,
    playback_listener_running: bool,
    listener_stop: Option<watch::Sender<bool>>,
}

impl MediaControllerState {
    fn new() -> Self {
        MediaControllerState {
            connected_device_mac: String::new(),
            local_mac: String::new(),
            is_playing: false,
            paused_by_app_services: Vec::new(),
            old_in_ear_data: vec![false, false],
            user_played_the_media: false,
            i_paused_the_media: false,
            ear_detection_enabled: true,
            conv_original_volume: None,
            conv_conversation_started: false,
            playback_listener_running: false,
            listener_stop: None,
        }
    }
}

#[derive(Clone)]
pub struct MediaController {
    state: Arc<Mutex<MediaControllerState>>,
}

impl MediaController {
    pub fn new(connected_mac: String, local_mac: String) -> Self {
        let mut state = MediaControllerState::new();
        state.connected_device_mac = connected_mac;
        state.local_mac = local_mac;
        MediaController {
            state: Arc::new(Mutex::new(state)),
        }
    }

    /// Local MPRIS players that are playing right now (phone players via KDE Connect are
    /// never included).
    pub async fn playing_players() -> Vec<String> {
        tokio::task::spawn_blocking(|| {
            mpris::with_players(|conn, players| {
                players
                    .into_iter()
                    .filter(|s| mpris::playback_status(conn, s).as_deref() == Some("Playing"))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
        })
        .await
        .unwrap_or_default()
    }

    /// Presses play on those of `players` that are paused now.
    pub async fn resume_if_paused(players: Vec<String>) {
        let _ = tokio::task::spawn_blocking(move || {
            mpris::with_bus(|conn| {
                for service in players {
                    if mpris::playback_status(conn, &service).as_deref() == Some("Paused")
                        && mpris::call(conn, &service, "Play")
                    {
                        info!("Resumed {} after the audio output was rebuilt", service);
                    }
                }
            })
        })
        .await;
    }

    /// Starts watching MPRIS playback (signal based, no polling). Stops when
    /// [`MediaController::stop_playback_listener`] is called or `control_tx` closes.
    pub async fn start_playback_listener(
        &self,
        aacp_manager: AACPManager,
        control_tx: tokio::sync::mpsc::UnboundedSender<(
            crate::bluetooth::aacp::ControlCommandIdentifiers,
            Vec<u8>,
        )>,
    ) {
        let mut state = self.state.lock().await;
        if state.playback_listener_running {
            debug!("Playback listener already running");
            return;
        }
        state.playback_listener_running = true;
        let (stop_tx, stop_rx) = watch::channel(false);
        state.listener_stop = Some(stop_tx);
        drop(state);

        let (events_tx, events_rx) = mpsc::unbounded_channel();
        mpris::spawn_playback_watcher(events_tx);

        let controller_clone = self.clone();
        tokio::spawn(async move {
            controller_clone
                .playback_listener_loop(aacp_manager, control_tx, events_rx, stop_rx)
                .await;
        });
    }

    /// Stops the playback listener started by `start_playback_listener`. Idempotent.
    /// Call it when the device task ends so the listener and its D-Bus thread exit.
    #[allow(dead_code)] // called by the device supervisor once integrated
    pub async fn stop_playback_listener(&self) {
        let mut state = self.state.lock().await;
        if let Some(stop) = state.listener_stop.take() {
            let _ = stop.send(true);
        }
        state.playback_listener_running = false;
    }

    async fn playback_listener_loop(
        &self,
        aacp_manager: AACPManager,
        control_tx: tokio::sync::mpsc::UnboundedSender<(
            crate::bluetooth::aacp::ControlCommandIdentifiers,
            Vec<u8>,
        )>,
        mut events: mpsc::UnboundedReceiver<bool>,
        mut stop: watch::Receiver<bool>,
    ) {
        info!("Starting playback listener loop");
        loop {
            let is_playing = tokio::select! {
                _ = stop.changed() => break,
                ev = events.recv() => match ev {
                    Some(p) => p,
                    None => break,
                },
            };
            if control_tx.is_closed() {
                break;
            }

            let mut state = self.state.lock().await;
            let was_playing = state.is_playing;
            state.is_playing = is_playing;
            drop(state);

            if !was_playing && is_playing {
                // Copy what we need and release the lock before any await: send_packet
                // takes the same (non-reentrant) mutex, so holding it here deadlocks.
                let any_in_ear = aacp_manager
                    .state
                    .lock()
                    .await
                    .ear_detection_status
                    .contains(&EarDetectionStatus::InEar);
                if !any_in_ear {
                    info!("Media playback started but buds not in ear, skipping takeover");
                    continue;
                }
                info!("Media playback started, taking ownership and checking A2DP");
                // The device task notifies the other devices once this is sent.
                let _ = control_tx.send((
                    crate::bluetooth::aacp::ControlCommandIdentifiers::OwnsConnection,
                    vec![0x01],
                ));
                self.activate_a2dp_profile().await;
                debug!("completed playback takeover process");
            }
        }
        info!("Playback listener loop stopped");
        let mut state = self.state.lock().await;
        state.playback_listener_running = false;
        state.listener_stop = None;
    }

    pub async fn handle_ear_detection(
        &self,
        old_statuses: Vec<EarDetectionStatus>,
        new_statuses: Vec<EarDetectionStatus>,
    ) {
        debug!(
            "Entering handle_ear_detection with old_statuses: {:?}, new_statuses: {:?}",
            old_statuses, new_statuses
        );

        let old_in_ear_data: Vec<bool> = old_statuses
            .iter()
            .map(|s| *s == EarDetectionStatus::InEar)
            .collect();
        let new_in_ear_data: Vec<bool> = new_statuses
            .iter()
            .map(|s| *s == EarDetectionStatus::InEar)
            .collect();

        let in_ear = new_in_ear_data.iter().all(|&b| b);
        let old_all_out = old_in_ear_data.iter().all(|&b| !b);
        let new_has_at_least_one_in = new_in_ear_data.iter().any(|&b| b);
        let new_all_out = new_in_ear_data.iter().all(|&b| !b);

        debug!(
            "Computed states: in_ear={}, old_all_out={}, new_has_at_least_one_in={}, new_all_out={}",
            in_ear, old_all_out, new_has_at_least_one_in, new_all_out
        );

        {
            let state = self.state.lock().await;
            if !state.ear_detection_enabled {
                debug!("Ear detection disabled, skipping");
                return;
            }
        }

        if new_has_at_least_one_in && old_all_out {
            debug!("Condition met: buds inserted, checking A2DP and play state");
            self.activate_a2dp_profile().await;
            {
                let mut state = self.state.lock().await;
                if state.is_playing {
                    state.user_played_the_media = true;
                    debug!("Set user_played_the_media to true as media was playing");
                }
            }
        } else if new_all_out {
            // Only pause. The card profile is deliberately left alone: switching it to
            // "off" here is what made audio silent after walking away and coming back.
            debug!("Condition met: buds removed, pausing media");
            self.pause().await;
        }

        let reset_user_played = (old_in_ear_data.iter().any(|&b| !b)
            && new_in_ear_data.iter().all(|&b| b))
            || (new_in_ear_data.iter().any(|&b| !b) && old_in_ear_data.iter().all(|&b| b));
        if reset_user_played {
            debug!("Transition detected, resetting user_played_the_media");
            let mut state = self.state.lock().await;
            state.user_played_the_media = false;
        }

        info!(
            "Ear Detection - old_in_ear_data: {:?}, new_in_ear_data: {:?}",
            old_in_ear_data, new_in_ear_data
        );

        let mut old_sorted = old_in_ear_data.clone();
        old_sorted.sort();
        let mut new_sorted = new_in_ear_data.clone();
        new_sorted.sort();
        if new_sorted != old_sorted {
            debug!("Ear data changed, checking resume/pause logic");
            if in_ear {
                debug!("Resuming media as buds are in ear");
                self.resume().await;
                self.state.lock().await.i_paused_the_media = false;
            } else if !old_all_out {
                debug!("Pausing media as buds are not fully in ear");
                self.pause().await;
                self.state.lock().await.i_paused_the_media = true;
            } else {
                debug!("Playing media");
                self.resume().await;
                self.state.lock().await.i_paused_the_media = false;
            }
        }

        {
            let mut state = self.state.lock().await;
            state.old_in_ear_data = new_in_ear_data;
            debug!("Updated old_in_ear_data to {:?}", state.old_in_ear_data);
        }
    }

    /// Makes sure the AirPods card is on an A2DP profile when it is `off` (or on HFP
    /// with no microphone in use). Looks the card up fresh by MAC every time, so a
    /// card recreated by PipeWire after a reconnect is found. Never restarts
    /// WirePlumber.
    pub async fn activate_a2dp_profile(&self) {
        let mac = self.state.lock().await.connected_device_mac.clone();
        if mac.is_empty() {
            warn!("Connected device MAC is empty, cannot activate A2DP profile");
            return;
        }
        match crate::audio::ensure_a2dp_profile(&mac).await {
            Ok(profile) => debug!("Audio profile for {} is {}", mac, profile),
            Err(e) => warn!("Could not ensure A2DP profile for {}: {}", mac, e),
        }
    }

    /// Kept for API compatibility. This used to set the card profile to `off`, which
    /// WirePlumber persisted, leaving the AirPods silent on every reconnect. It is now
    /// a no-op: losing ownership or taking the buds out only pauses media.
    pub async fn deactivate_a2dp_profile(&self) {
        info!("Not switching the audio profile off (keeps A2DP alive for reconnects)");
    }

    async fn pause(&self) {
        debug!("Pausing playback");
        let paused_services = tokio::task::spawn_blocking(|| {
            mpris::with_players(|conn, players| {
                let mut paused = Vec::new();
                for service in players {
                    if mpris::playback_status(conn, &service).as_deref() == Some("Playing") {
                        if mpris::call(conn, &service, "Pause") {
                            info!("Paused playback for: {}", service);
                            paused.push(service);
                        } else {
                            error!("Failed to pause {}", service);
                        }
                    }
                }
                paused
            })
            .unwrap_or_default()
        })
        .await
        .unwrap_or_default();

        if !paused_services.is_empty() {
            info!("Paused {} media player(s) via DBus", paused_services.len());
            let mut state = self.state.lock().await;
            state.paused_by_app_services = paused_services;
            state.is_playing = false;
        } else {
            info!("No playing media players found to pause");
        }
    }

    pub async fn pause_all_media(&self) {
        debug!("Pausing all playing media (without tracking for resume)");
        let paused_count = tokio::task::spawn_blocking(|| {
            mpris::with_players(|conn, players| {
                let mut count = 0usize;
                for service in players {
                    if mpris::playback_status(conn, &service).as_deref() == Some("Playing") {
                        if mpris::call(conn, &service, "Pause") {
                            info!("Paused playback for: {}", service);
                            count += 1;
                        } else {
                            error!("Failed to pause {}", service);
                        }
                    }
                }
                count
            })
            .unwrap_or(0)
        })
        .await
        .unwrap_or(0);

        if paused_count > 0 {
            info!(
                "Paused {} media player(s) due to ownership loss",
                paused_count
            );
            self.state.lock().await.is_playing = false;
        } else {
            debug!("No playing media players found to pause");
        }
    }

    async fn resume(&self) {
        debug!("Resuming playback");
        let services = self.state.lock().await.paused_by_app_services.clone();
        if services.is_empty() {
            debug!("No services to resume");
            return;
        }

        let resumed_count = tokio::task::spawn_blocking(move || {
            mpris::with_bus(|conn| {
                let mut count = 0usize;
                for service in services {
                    if mpris::call(conn, &service, "Play") {
                        info!("Resumed playback for: {}", service);
                        count += 1;
                    } else {
                        warn!("Failed to resume {}", service);
                    }
                }
                count
            })
            .unwrap_or(0)
        })
        .await
        .unwrap_or(0);

        if resumed_count > 0 {
            info!("Resumed {} media player(s) via DBus", resumed_count);
            self.state.lock().await.paused_by_app_services.clear();
        } else {
            error!("Failed to resume any media players via DBus");
        }
    }

    pub async fn next_track(&self) {
        info!("Skipping to next track");
        self.mpris_command("Next").await;
    }

    pub async fn previous_track(&self) {
        info!("Going to previous track");
        self.mpris_command("Previous").await;
    }

    pub async fn handle_conversational_awareness(&self, status: u8) {
        debug!(
            "Entering handle_conversational_awareness with status: {}",
            status
        );

        let mac = self.state.lock().await.connected_device_mac.clone();
        if mac.is_empty() {
            debug!("No connected device MAC, skipping conversational awareness");
            return;
        }

        let sink = match get_sink_name_by_mac(&mac).await {
            Some(s) => s,
            None => {
                warn!(
                    "Could not find sink for MAC {}, skipping conversational awareness",
                    mac
                );
                return;
            }
        };

        let current_volume_opt = tokio::task::spawn_blocking({
            let sink = sink.clone();
            move || get_sink_volume_percent_by_name_sync(&sink)
        })
        .await
        .unwrap_or(None);

        match status {
            1 => {
                let original = current_volume_opt.unwrap_or(0);
                debug!("Conversation start (1). Current volume: {}", original);
                {
                    let mut state = self.state.lock().await;
                    if !state.conv_conversation_started {
                        state.conv_original_volume = Some(original);
                        state.conv_conversation_started = true;
                    } else {
                        debug!(
                            "Conversation already started; not overwriting conv_original_volume"
                        );
                    }
                }
                if original > 25 {
                    let sink_clone = sink.clone();
                    tokio::task::spawn_blocking(move || transition_sink_volume(&sink_clone, 25))
                        .await
                        .unwrap_or(false);
                    info!(
                        "Conversation start: lowered volume to 25% (original {})",
                        original
                    );
                } else {
                    debug!("Original volume {} <= 25, not reducing to 25", original);
                }
            }
            2 => {
                let original = self.state.lock().await.conv_original_volume;
                if let Some(orig) = original {
                    debug!("Conversation reduce (2). Original: {}", orig);
                    if orig > 15 {
                        let sink_clone = sink.clone();
                        tokio::task::spawn_blocking(move || {
                            transition_sink_volume(&sink_clone, 15)
                        })
                        .await
                        .unwrap_or(false);
                        info!(
                            "Conversation reduce: lowered volume to 15% (original {})",
                            orig
                        );
                    } else {
                        debug!("Original {} <= 15, not reducing to 15", orig);
                    }
                } else {
                    debug!("No original volume known for status 2, skipping");
                }
            }
            3 => {
                let (conv_started, conv_original) = {
                    let state = self.state.lock().await;
                    (state.conv_conversation_started, state.conv_original_volume)
                };
                if !conv_started {
                    debug!("Received status 3 but conversation was not started; ignoring increase");
                    return;
                }
                if let Some(orig) = conv_original {
                    let target = orig.min(25);
                    let sink_clone = sink.clone();
                    tokio::task::spawn_blocking(move || {
                        transition_sink_volume(&sink_clone, target)
                    })
                    .await
                    .unwrap_or(false);
                    info!(
                        "Conversation partial increase (3): set volume to {} (original {})",
                        target, orig
                    );
                } else if let Some(orig_from_current) = current_volume_opt {
                    let target = orig_from_current.min(25);
                    let sink_clone = sink.clone();
                    tokio::task::spawn_blocking(move || {
                        transition_sink_volume(&sink_clone, target)
                    })
                    .await
                    .unwrap_or(false);
                    info!(
                        "Conversation partial increase (3) with fallback current: set volume to {} (measured {})",
                        target, orig_from_current
                    );
                } else {
                    debug!("No original volume known for status 3, skipping");
                }
            }
            4 | 6 | 7 => {
                debug!("Conversation end ({}), restoring volume if needed", status);
                self.restore_volume_if_needed(&sink).await;
            }
            _ => {
                debug!("Conversation status ({}), ignoring", status);
            }
        }
    }

    async fn mpris_command(&self, command: &'static str) {
        let _ = tokio::task::spawn_blocking(move || {
            mpris::with_players(|conn, players| {
                let target = players
                    .iter()
                    .find(|s| mpris::playback_status(conn, s).as_deref() == Some("Playing"))
                    .or_else(|| players.first());
                if let Some(service) = target {
                    if mpris::call(conn, service, command) {
                        info!("Sent {} to: {}", command, service);
                    } else {
                        debug!("Failed to send {} to: {}", command, service);
                    }
                }
            })
        })
        .await;
    }

    async fn restore_volume_if_needed(&self, sink: &str) {
        let maybe_original = {
            let mut state = self.state.lock().await;
            if state.conv_conversation_started {
                let orig = state.conv_original_volume;
                state.conv_original_volume = None;
                state.conv_conversation_started = false;
                orig
            } else {
                return;
            }
        };

        if let Some(orig) = maybe_original {
            let sink = sink.to_string();
            tokio::task::spawn_blocking(move || transition_sink_volume(&sink, orig))
                .await
                .unwrap_or(false);
        }
    }
}

/// MPRIS over one shared session-bus connection for commands, plus a signal
/// watcher thread for playback state.
mod mpris {
    use super::*;
    use std::sync::Mutex as StdMutex;

    static BUS: StdMutex<Option<Connection>> = StdMutex::new(None);

    pub fn is_kdeconnect_service(service: &str) -> bool {
        service.starts_with("org.mpris.MediaPlayer2.kdeconnect.mpris_")
    }

    pub fn list_players(conn: &Connection) -> Result<Vec<String>, dbus::Error> {
        let proxy = conn.with_proxy(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            DBUS_TIMEOUT,
        );
        let (names,): (Vec<String>,) =
            proxy.method_call("org.freedesktop.DBus", "ListNames", ())?;
        Ok(names
            .into_iter()
            .filter(|s| s.starts_with("org.mpris.MediaPlayer2.") && !is_kdeconnect_service(s))
            .collect())
    }

    /// Runs `f` with the shared connection, (re)connecting when needed.
    pub fn with_bus<T>(f: impl FnOnce(&Connection) -> T) -> Option<T> {
        with_players(|conn, _| f(conn))
    }

    /// Runs `f` with the shared connection and the current MPRIS players. A broken
    /// connection is replaced once.
    pub fn with_players<T>(f: impl FnOnce(&Connection, Vec<String>) -> T) -> Option<T> {
        let mut guard = BUS.lock().unwrap_or_else(|p| p.into_inner());
        for _ in 0..2 {
            if guard.is_none() {
                match Connection::new_session() {
                    Ok(c) => *guard = Some(c),
                    Err(e) => {
                        warn!("Cannot connect to the session bus: {}", e);
                        return None;
                    }
                }
            }
            let conn = guard.as_ref()?;
            match list_players(conn) {
                Ok(players) => return Some(f(conn, players)),
                Err(e) => {
                    debug!("Session bus connection unusable ({}), reconnecting", e);
                    *guard = None;
                }
            }
        }
        None
    }

    pub fn playback_status(conn: &Connection, service: &str) -> Option<String> {
        conn.with_proxy(service, MPRIS_PATH, DBUS_TIMEOUT)
            .get::<String>(MPRIS_PLAYER, "PlaybackStatus")
            .ok()
    }

    pub fn call(conn: &Connection, service: &str, method: &str) -> bool {
        conn.with_proxy(service, MPRIS_PATH, DBUS_TIMEOUT)
            .method_call::<(), _, &str, &str>(MPRIS_PLAYER, method, ())
            .is_ok()
    }

    fn any_playing(conn: &Connection) -> bool {
        list_players(conn)
            .unwrap_or_default()
            .iter()
            .any(|s| playback_status(conn, s).as_deref() == Some("Playing"))
    }

    /// Watches PropertiesChanged on MPRIS players and player (dis)appearance on its
    /// own connection and thread; sends "is anything playing" on every change. Exits
    /// when the receiver is dropped.
    pub fn spawn_playback_watcher(tx: mpsc::UnboundedSender<bool>) {
        let spawned = std::thread::Builder::new()
            .name("mpris-watch".into())
            .spawn(move || {
                while !tx.is_closed() {
                    if let Err(e) = watch_session(&tx) {
                        warn!("MPRIS watcher: {}; retrying in 5 s", e);
                    }
                    for _ in 0..5 {
                        if tx.is_closed() {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                }
                debug!("MPRIS watcher stopped");
            });
        if let Err(e) = spawned {
            error!("Could not start the MPRIS watcher thread: {}", e);
        }
    }

    fn watch_session(tx: &mpsc::UnboundedSender<bool>) -> Result<(), dbus::Error> {
        let conn = Connection::new_session()?;
        let dirty = Arc::new(AtomicBool::new(true));

        let d = dirty.clone();
        let rule = MatchRule::new_signal("org.freedesktop.DBus.Properties", "PropertiesChanged")
            .with_path(MPRIS_PATH);
        conn.add_match(rule, move |_: (), _, msg| {
            if msg.read1::<&str>().is_ok_and(|iface| iface == MPRIS_PLAYER) {
                d.store(true, Ordering::Relaxed);
            }
            true
        })?;

        let d = dirty.clone();
        let rule = MatchRule::new_signal("org.freedesktop.DBus", "NameOwnerChanged")
            .with_sender("org.freedesktop.DBus");
        conn.add_match(rule, move |_: (), _, msg| {
            if msg
                .read1::<&str>()
                .is_ok_and(|name| name.starts_with("org.mpris.MediaPlayer2."))
            {
                d.store(true, Ordering::Relaxed);
            }
            true
        })?;

        let mut last: Option<bool> = None;
        let mut last_full = Instant::now();
        while !tx.is_closed() {
            conn.process(Duration::from_secs(1))?;
            // Safety net for players that do not emit PropertiesChanged reliably.
            let periodic = last_full.elapsed() >= Duration::from_secs(30);
            if dirty.swap(false, Ordering::Relaxed) || periodic {
                last_full = Instant::now();
                let playing = any_playing(&conn);
                if last != Some(playing) {
                    last = Some(playing);
                    if tx.send(playing).is_err() {
                        break;
                    }
                }
            }
        }
        Ok(())
    }
}

// --- PulseAudio helpers (bounded by a deadline; call from spawn_blocking) ---

fn get_sink_volume_percent_by_name_sync(sink_name: &str) -> Option<u32> {
    match crate::audio::pulse::sink_volume_percent_sync(
        sink_name,
        crate::audio::pulse::DEFAULT_DEADLINE,
    ) {
        Ok(v) => Some(v),
        Err(e) => {
            warn!("Could not read volume of {}: {}", sink_name, e);
            None
        }
    }
}

pub fn transition_sink_volume(sink_name: &str, target_volume: u32) -> bool {
    match crate::audio::pulse::set_sink_volume_percent_sync(
        sink_name,
        target_volume,
        crate::audio::pulse::DEFAULT_DEADLINE,
    ) {
        Ok(()) => true,
        Err(e) => {
            error!("Could not set volume of {}: {}", sink_name, e);
            false
        }
    }
}

async fn get_sink_name_by_mac(mac: &str) -> Option<String> {
    use crate::audio::pulse::{LibPulse, PulseApi};
    if mac.is_empty() {
        return None;
    }
    let snap = match LibPulse.snapshot().await {
        Ok(s) => s,
        Err(e) => {
            warn!("Could not query sinks: {}", e);
            return None;
        }
    };
    let sink = snap
        .card_for(mac)
        .and_then(|card| snap.sink_for_card(card))
        .or_else(|| {
            snap.sinks.iter().find(|s| {
                s.mac
                    .as_deref()
                    .is_some_and(|m| m.eq_ignore_ascii_case(mac))
            })
        });
    match sink {
        Some(s) => {
            debug!("Found sink name for MAC {}: {}", mac, s.name);
            Some(s.name.clone())
        }
        None => {
            warn!("No matching sink found for MAC address: {}", mac);
            None
        }
    }
}

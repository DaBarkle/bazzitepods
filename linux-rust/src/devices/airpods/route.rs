//! Who has the AirPods' audio: claiming it, noticing when another device takes it, and
//! putting the PC's audio back together when it returns.

use super::*;

/// The audio came back from another device. While it was away the AirPods usually broke
/// the A2DP transport to this PC, so have the audio guard check at once and repair it.
/// The repair rebuilds the output, which makes players pause; resume the ones that were
/// playing when the audio came back, so one press of play is enough.
pub(super) fn route_regained(audio: Option<&AudioGuardHandle>, from_other_device: bool) {
    info!("Audio route is back on this PC; re-checking the audio setup");
    let Some(audio) = audio.cloned() else {
        return;
    };
    // While another device had the audio, the AirPods broke this PC's transport and the
    // output node went into an error state that never stalls, so rebuild it now.
    if from_other_device {
        audio.repair();
    } else {
        audio.route_back();
    }
    resume_after_rebuild(audio);
}

/// The AirPods mic stream stopped: rebuild the music output once (the right pod goes
/// silent otherwise) and resume the players that the rebuild pauses.
pub fn rebuild_output(audio: Option<&AudioGuardHandle>) {
    let Some(audio) = audio.cloned() else {
        return;
    };
    audio.rebuild();
    resume_after_rebuild(audio);
}

/// Players that are playing now get pressed play again once the guard's rebuild is done.
pub(super) fn resume_after_rebuild(audio: AudioGuardHandle) {
    tokio::spawn(async move {
        let playing = MediaController::playing_players().await;
        if playing.is_empty() {
            return;
        }
        let mut health = audio.health();
        let deadline = sleep(Duration::from_secs(15));
        tokio::pin!(deadline);
        let mut repaired = false;
        loop {
            tokio::select! {
                _ = &mut deadline => return,
                changed = health.changed() => {
                    if changed.is_err() {
                        return;
                    }
                    match *health.borrow() {
                        AudioHealth::Recovering { .. } => repaired = true,
                        AudioHealth::Ready { .. } if repaired => break,
                        _ => {}
                    }
                }
            }
        }
        // Let the new output settle before pressing play.
        sleep(Duration::from_millis(800)).await;
        MediaController::resume_if_paused(playing).await;
    });
}

/// Tell every other device on the AirPods that this PC is taking the audio route, so
/// they hand it over (the same exchange an iPhone does when you pick it in Control Centre).
/// Runs after `OwnsConnection = 1` was sent, whether from playback starting or from the
/// "Use on this PC" button.
pub(super) async fn claim_audio_route(aacp: &AACPManager, local: &str) {
    let mut state = aacp.state.lock().await;
    // Whatever the other device said it was playing is over once we take the AirPods.
    state.peer_streaming = false;
    let others: Vec<String> = state
        .connected_devices
        .iter()
        .filter(|d| !d.mac.eq_ignore_ascii_case(local))
        .map(|d| d.mac.clone())
        .collect();
    // Sending takes the same lock.
    drop(state);
    for peer in others {
        if let Err(e) = aacp.send_media_information(local, &peer, true).await {
            warn!("Failed to send media information to {peer}: {e}");
        }
        if let Err(e) = aacp.send_smart_routing_show_ui(&peer).await {
            warn!("Failed to send smart routing UI to {peer}: {e}");
        }
        if let Err(e) = aacp.send_hijack_request(&peer).await {
            warn!("Failed to send hijack request to {peer}: {e}");
        }
    }
}

/// Another device has the AirPods now. When it is playing media (YouTube on the iPhone),
/// pause here right away, like an iPhone does, so the next press of play is a fresh start
/// that takes them straight back. A notification only borrows them for a few seconds, so
/// then keep playing and pause only if they stay away.
pub(super) async fn route_lost(
    source: &AudioSource,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    media: &Arc<Mutex<MediaController>>,
    route: &RouteWatch,
    audio: Option<&AudioGuardHandle>,
) {
    let borrowed =
        source.r#type != AudioSourceType::Call && !aacp.state.lock().await.peer_streaming;
    route.lost(borrowed);
    if let Some(audio) = audio {
        audio.route_away();
    }
    if source.r#type == AudioSourceType::Call {
        info!(
            "{} has a call on the AirPods; pausing local media",
            source.mac
        );
        media.lock().await.pause_all_media().await;
        return;
    }
    if aacp.state.lock().await.peer_streaming {
        info!(
            "{} is playing media on the AirPods; pausing local media",
            source.mac
        );
        media.lock().await.pause_all_media().await;
        return;
    }
    info!(
        "{} borrowed the AirPods; keeping local media playing for now",
        source.mac
    );
    route.lock().borrowed_until = Some(Instant::now() + BORROW_GRACE);
    let (hub, mac, media) = (ctx.hub.clone(), mac_s.to_string(), media.clone());
    tokio::spawn(async move {
        sleep(BORROW_GRACE).await;
        if owns_audio(&hub, &mac) == Some(false) {
            info!("The AirPods stayed on the other device; pausing local media");
            media.lock().await.pause_all_media().await;
        }
    });
}

/// What the route watcher remembers between ticks.
#[derive(Default)]
pub(super) struct RouteState {
    /// Until when another device is only borrowing the AirPods (a notification), if it is.
    pub(super) borrowed_until: Option<Instant>,
    /// The streaming state last announced to the other devices.
    pub(super) announced: Option<bool>,
    /// Audio streams seen playing, to catch players without MPRIS (the Plex app).
    pub(super) streams: StreamTracker,
    /// Whether something plays here, as of the last tick.
    pub(super) local_playing: bool,
    /// When this PC last took the AirPods.
    pub(super) claimed_at: Option<Instant>,
    /// Another device has a phone call on the AirPods: never take them back mid-call.
    pub(super) call: bool,
    /// When this PC last lost the AirPods, and whether only to a notification.
    pub(super) lost_at: Option<Instant>,
    pub(super) lost_to_borrow: bool,
    /// The other device said its media stopped.
    pub(super) peer_stopped: bool,
}

/// Follows playing audio streams between ticks. A stream counts once it has played for
/// two ticks in a row, so a notification sound doesn't. Streams already playing when
/// tracking starts are background noise (idle voice chat and the like) until they stop.
#[derive(Default)]
pub(super) struct StreamTracker {
    pub(super) started: bool,
    /// Stream index -> ticks seen playing, capped at 2.
    pub(super) seen: HashMap<u32, u8>,
}

impl StreamTracker {
    /// Feeds the streams playing now. Returns (a stream is playing, one just started).
    /// Streams already playing when tracking begins count as playing, but never as just
    /// started: media that was playing before the iPhone joined still brings the AirPods
    /// back when the iPhone stops, without counting as a fresh press of play.
    pub(super) fn update(&mut self, playing: &[u32]) -> (bool, bool) {
        let baseline = !std::mem::replace(&mut self.started, true);
        let mut seen = HashMap::with_capacity(playing.len());
        let (mut active, mut started) = (false, false);
        for &index in playing {
            let count = match self.seen.get(&index) {
                _ if baseline => 2,
                Some(&count) => (count + 1).min(2),
                None => 1,
            };
            if count == 2 {
                active = true;
                started |= self.seen.get(&index) == Some(&1);
            }
            seen.insert(index, count);
        }
        self.seen = seen;
        (active, started)
    }

    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
}

#[derive(Clone, Default)]
pub(super) struct RouteWatch(Arc<std::sync::Mutex<RouteState>>);

impl RouteWatch {
    pub(super) fn lock(&self) -> std::sync::MutexGuard<'_, RouteState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Records that another device took the AirPods; `borrowed` when it plays no media
    /// (a notification).
    pub(super) fn lost(&self, borrowed: bool) {
        let mut state = self.lock();
        state.lost_at = Some(Instant::now());
        state.lost_to_borrow = borrowed;
        state.peer_stopped = false;
    }

    /// Whether "the other device let go" may be taken as a cue to take the AirPods back:
    /// it only borrowed them or has stopped its media, and the hand-over gap is over.
    pub(super) fn may_reclaim(&self) -> bool {
        let state = self.lock();
        let settled = state.lost_at.is_none_or(|t| t.elapsed() >= HANDOVER_GAP);
        settled && !state.call && (state.lost_to_borrow || state.peer_stopped)
    }

    /// True for a moment after this PC took the AirPods, while reports from before the
    /// switch may still arrive.
    pub(super) fn just_claimed(&self) -> bool {
        self.lock()
            .claimed_at
            .is_some_and(|t| t.elapsed() < CLAIM_GRACE)
    }
}

/// How long after taking the AirPods to ignore reports that say another device has them.
pub(super) const CLAIM_GRACE: Duration = Duration::from_secs(3);

/// A hand-over to another device can pass through "nobody" for a moment; ignore that.
pub(super) const HANDOVER_GAP: Duration = Duration::from_secs(2);

/// How often to compare local playback with who has the AirPods.
pub(super) const ROUTE_WATCH_INTERVAL: Duration = Duration::from_millis(500);

/// Runs only while another device shares the AirPods. It reads local playback directly
/// instead of relying on catching each play press, which a quick pause/play can hide.
/// - This PC has the AirPods: tell the other devices whenever playback starts or stops, so
///   an iPhone doesn't take them for a notification while we play.
/// - Another device has them and something here plays: the user pressed play here (we
///   paused when it took over), so take them back. While a notification only borrows
///   them, wait for it to let go instead.
pub(super) async fn watch_route(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
    command_tx: &CommandSender,
) {
    let peers: Vec<String> = aacp
        .state
        .lock()
        .await
        .connected_devices
        .iter()
        .filter(|d| !d.mac.eq_ignore_ascii_case(&ctx.local_mac))
        .map(|d| d.mac.clone())
        .collect();
    if peers.is_empty() {
        let mut state = route.lock();
        state.announced = None;
        state.streams.reset();
        return;
    }
    let mpris = !MediaController::playing_players().await.is_empty();
    let streams = tokio::task::spawn_blocking(|| {
        crate::audio::pulse::snapshot_sync(crate::audio::pulse::DEFAULT_DEADLINE)
            .map(|s| s.playing_streams())
            .unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let (stream_playing, stream_started) = route.lock().streams.update(&streams);
    let playing = mpris || stream_playing;
    route.lock().local_playing = playing;
    match owns_audio(&ctx.hub, mac_s) {
        Some(true) => {
            if route.lock().announced == Some(playing) {
                return;
            }
            route.lock().announced = Some(playing);
            debug!("Telling the other devices this PC is streaming: {playing}");
            for peer in peers {
                if let Err(e) = aacp
                    .send_media_information(&ctx.local_mac, &peer, playing)
                    .await
                {
                    debug!("Could not tell {peer} the streaming state: {e}");
                }
            }
        }
        // MPRIS players were paused when the other device took over, so one playing now
        // was started here. A player without MPRIS can't be paused, so only a stream
        // that just started counts for those.
        // Not right after pausing for the other device: that playback is the tail end of
        // what was paused, not a fresh press of play.
        Some(false)
            if (mpris || stream_started) && !crate::media_controller::auto_paused_recently() =>
        {
            route.lock().announced = None;
            let borrowed = route
                .lock()
                .borrowed_until
                .is_some_and(|until| Instant::now() < until);
            let worn = aacp
                .state
                .lock()
                .await
                .ear_detection_status
                .contains(&EarDetectionStatus::InEar);
            let call = route.lock().call;
            if worn && !borrowed && !call {
                info!(
                    "Media is playing here while another device has the AirPods; taking them back"
                );
                let _ = command_tx.send((ControlCommandIdentifiers::OwnsConnection, vec![0x01]));
            }
        }
        _ => route.lock().announced = None,
    }
}

/// How long a device may borrow the AirPods without media (a notification) before local
/// playback is paused.
pub(super) const BORROW_GRACE: Duration = Duration::from_secs(6);

/// Who has the audio route according to an Audio Source report: `Some(true)` for this PC,
/// `Some(false)` for another device, `None` when nothing is playing.
pub(super) fn route_owner(source: &AudioSource, local_mac: &str) -> Option<bool> {
    if source.r#type == AudioSourceType::None || source.mac == "00:00:00:00:00:00" {
        return None;
    }
    Some(source.mac.eq_ignore_ascii_case(local_mac))
}

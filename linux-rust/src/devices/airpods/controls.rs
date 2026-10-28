//! The control channel once connected: handshake, stem and mic settings, listening mode,
//! and reacting to every AACP event.

use super::*;

pub(super) async fn handshake(aacp: &AACPManager) -> bluer::Result<()> {
    aacp.send_handshake().await?;
    sleep(Duration::from_millis(300)).await;
    aacp.send_set_feature_flags_packet().await?;
    sleep(Duration::from_millis(300)).await;
    aacp.send_notification_request().await?;
    if let Err(e) = aacp.send_some_packet().await {
        debug!("Optional init packet failed: {}", e);
    }
    if let Err(e) = aacp
        .send_proximity_keys_request(vec![ProximityKeyType::Irk, ProximityKeyType::EncKey])
        .await
    {
        warn!("Failed to request proximity keys: {}", e);
    }
    apply_stem_settings(aacp, &StemSettings::read()).await;
    Ok(())
}

/// The settings that decide which stem presses this PC handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StemSettings {
    /// Double/triple press: next/previous track.
    pub(super) track_control: bool,
    /// Left hold: swap mics; right hold: noise control.
    pub(super) mic_switch: bool,
}

impl StemSettings {
    pub(super) fn read() -> Self {
        Self {
            track_control: setting_bool("stem_control"),
            mic_switch: setting_bool("airpods_mic_switch"),
        }
    }
}

/// Claims the presses the settings ask for, and gives the rest back to the AirPods. The
/// AirPods keep the claim across reconnects, so it is always sent, 0 included.
pub(super) async fn apply_stem_settings(aacp: &AACPManager, s: &StemSettings) {
    let mask = crate::mic::gestures::stem_claims(s.track_control, s.mic_switch);
    if let Err(e) = aacp
        .send_control_command(ControlCommandIdentifiers::StemConfig, &[mask])
        .await
    {
        warn!("Failed to set stem press handling: {}", e);
    }
}

pub(super) fn spawn_mic_switch(
    aacp: &AACPManager,
    audio: Option<&AudioGuardHandle>,
    ctx: &DeviceContext,
    mac_s: &str,
    name: &str,
) -> MicSwitchHandle {
    let (hub, mac, name) = (ctx.hub.clone(), mac_s.to_string(), name.to_string());
    let report = move |r: MicReport| {
        hub.update_device(&mac, &name, |d| {
            if d.mic.is_some() || r.route == MicRoute::AirPods {
                d.mic = Some(r.route);
            }
            if r.problem.is_some() {
                d.mic_failures = d.mic_failures.wrapping_add(1);
            }
            d.mic_problem = r.problem.clone();
        });
    };
    crate::mic::switch::spawn(
        PulseMicHost,
        AacpMicLink {
            aacp: aacp.clone(),
            audio: audio.cloned(),
        },
        SwitchConfig::default(),
        Arc::new(report),
    )
}

/// Whether the AirPods mic may be used right now: this PC has the AirPods, at least one
/// is worn, and no call is on them.
pub(super) async fn mic_allowed(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
) -> Result<(), &'static str> {
    if owns_audio(&ctx.hub, mac_s) == Some(false) {
        return Err("The AirPods are in use on another device");
    }
    if route.lock().call {
        return Err("A call is on the AirPods");
    }
    let ears = aacp.state.lock().await.ear_detection_status.clone();
    if !ears.is_empty() && !ears.contains(&EarDetectionStatus::InEar) {
        return Err("The AirPods aren't in your ears");
    }
    Ok(())
}

/// Back to the desk mic as soon as the AirPods mic stops being usable.
pub(super) async fn keep_mic_allowed(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    route: &RouteWatch,
    mic: &MicSwitchHandle,
) {
    let on_airpods = ctx
        .hub
        .snapshot()
        .devices
        .get(mac_s)
        .is_some_and(|d| d.mic == Some(MicRoute::AirPods));
    if on_airpods && let Err(why) = mic_allowed(aacp, ctx, mac_s, route).await {
        mic.request(MicRequest::Desk(why));
    }
}

/// Right stem hold with the mic switch on: the AirPods no longer change noise control
/// themselves, so step to the next of the modes picked for press and hold.
pub(super) async fn cycle_listening_mode(aacp: &AACPManager, command_tx: &CommandSender) {
    let current = aacp
        .control_value(ControlCommandIdentifiers::ListeningMode)
        .await
        .and_then(|v| v.first().copied());
    let enabled = aacp
        .control_value(ControlCommandIdentifiers::ListeningModeConfigs)
        .await
        .and_then(|v| v.first().copied());
    let next = crate::mic::gestures::next_listening_mode(current, enabled);
    info!("Right stem hold: listening mode {current:?} -> {next:#04x}");
    let _ = command_tx.send((ControlCommandIdentifiers::ListeningMode, vec![next]));
}

/// Mirror control-command state (listening mode, etc.) into the hub, and react to ownership changes.
#[allow(clippy::too_many_arguments)]
pub(super) async fn subscribe_controls(
    aacp: &AACPManager,
    ctx: &DeviceContext,
    mac_s: &str,
    name: &str,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    route: &RouteWatch,
    tasks: &mut JoinSet<()>,
) {
    let watch = [
        ControlCommandIdentifiers::ListeningMode,
        ControlCommandIdentifiers::AllowOffOption,
        ControlCommandIdentifiers::ConversationDetectConfig,
    ];
    for id in watch {
        let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
        aacp.subscribe_to_control_command(id, tx).await;
        let hub = ctx.hub.clone();
        let mac = mac_s.to_string();
        let name = name.to_string();
        tasks.spawn(async move {
            while let Some(value) = rx.recv().await {
                let Some(&byte) = value.first() else { continue };
                hub.update_device(&mac, &name, |d| match id {
                    ControlCommandIdentifiers::ListeningMode => d.listening_mode = Some(byte),
                    ControlCommandIdentifiers::AllowOffOption => {
                        d.allow_off_mode = Some(byte == 0x01)
                    }
                    ControlCommandIdentifiers::ConversationDetectConfig => {
                        d.conversation_awareness = Some(byte == 0x01)
                    }
                    _ => {}
                });
            }
        });
    }

    // Another Apple device took the AirPods. Pause what we were playing, but never touch the
    // audio profile: setting it "off" is what used to leave audio dead after reconnecting.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    aacp.subscribe_to_control_command(ControlCommandIdentifiers::OwnsConnection, tx)
        .await;
    let media = media.clone();
    let hub = ctx.hub.clone();
    let audio = audio.cloned();
    let route = route.clone();
    let owner_aacp = aacp.clone();
    let (mac, name) = (mac_s.to_string(), name.to_string());
    tasks.spawn(async move {
        let aacp = owner_aacp;
        while let Some(value) = rx.recv().await {
            let owns = value.first().copied().unwrap_or(0) != 0;
            let had = owns_audio(&hub, &mac);
            hub.update_device(&mac, &name, |d| d.owns_audio = Some(owns));
            if had == Some(true) && !owns {
                info!("Another device took the AirPods; pausing local media");
                // Picked in the iPhone's Control Center, this is the only sign of the
                // switch, so record it like any other: the PC takes the AirPods back when
                // the iPhone lets go, and leaves the dead output alone until then.
                let streaming = aacp.state.lock().await.peer_streaming;
                route.lost(!streaming);
                if let Some(audio) = &audio {
                    audio.route_away();
                }
                media.lock().await.pause_all_media().await;
            } else if had == Some(false) && owns {
                route_regained(audio.as_ref(), true);
                // The other device may have given the stem holds back to the AirPods.
                apply_stem_settings(&aacp, &StemSettings::read()).await;
            }
        }
    });

    // Another device (an iPhone setting itself up) can reset which presses the AirPods hand
    // over, and the left hold then changes noise control instead of swapping mics. Claim
    // them again, at most every 10 s so two devices never fight over it.
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();
    aacp.subscribe_to_control_command(ControlCommandIdentifiers::StemConfig, tx)
        .await;
    let aacp = aacp.clone();
    tasks.spawn(async move {
        let mut last: Option<Instant> = None;
        while let Some(value) = rx.recv().await {
            let wanted = StemSettings::read();
            let mask = crate::mic::gestures::stem_claims(wanted.track_control, wanted.mic_switch);
            let now = value.first().copied().unwrap_or(0);
            if now == mask || last.is_some_and(|t| t.elapsed() < Duration::from_secs(10)) {
                continue;
            }
            info!("Stem presses were reset ({now:#04x}); claiming {mask:#04x} again");
            last = Some(Instant::now());
            apply_stem_settings(&aacp, &wanted).await;
        }
    });
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn handle_event(
    event: AACPEvent,
    mac: Address,
    name: &str,
    aacp: &AACPManager,
    ctx: &DeviceContext,
    media: &Arc<Mutex<MediaController>>,
    audio: Option<&AudioGuardHandle>,
    command_tx: &CommandSender,
    route: &RouteWatch,
    stems: &StemSettings,
    mic: &MicSwitchHandle,
) {
    let mac_s = mac.to_string();
    match &event {
        AACPEvent::BatteryInfo(info) => {
            ctx.battery.send(BatteryInput::Aacp(mac, info.clone()));
        }
        AACPEvent::EarDetection(old, new) => {
            note_ear_status(&mac_s, new);
            // A pod just went into the case. The first battery report then often says
            // "case unknown" (255) and the real level may never follow, so ask the AirPods
            // to resend their state a little later.
            let in_case = |v: &Vec<EarDetectionStatus>| v.contains(&EarDetectionStatus::InCase);
            if in_case(new) && !in_case(old) {
                let aacp = aacp.clone();
                tokio::spawn(async move {
                    for delay in [2, 4] {
                        sleep(Duration::from_secs(delay)).await;
                        if let Err(e) = aacp.send_notification_request().await {
                            debug!("case battery refresh failed: {e}");
                            return;
                        }
                    }
                });
            }
            let in_ear = |i: usize| new.get(i).map(|s| *s == EarDetectionStatus::InEar);
            // AACP reports primary/secondary; the UI only needs "is anything in an ear", and
            // left/right placement is refined by BLE. Store as (primary, secondary).
            ctx.hub
                .update_device(&mac_s, name, |d| d.in_ear = (in_ear(0), in_ear(1)));
            if let Some(audio) = audio {
                audio.wearing_changed(new.contains(&EarDetectionStatus::InEar));
            }
            // The AirPods repeat their status; only a change means something moved.
            if old != new {
                media
                    .lock()
                    .await
                    .handle_ear_detection(old.clone(), new.clone())
                    .await;
            }
        }
        AACPEvent::ControlCommand(status) => {
            if let Some(&value) = status.value.first() {
                let id = status.identifier as u8;
                ctx.hub.update_device(&mac_s, name, |d| {
                    d.controls.insert(id, value);
                });
            }
        }
        AACPEvent::ConversationalAwareness(status) => {
            media
                .lock()
                .await
                .handle_conversational_awareness(*status)
                .await;
        }
        AACPEvent::ConnectedDevices(old_devices, new_devices) => {
            let local = ctx.local_mac.clone();
            let others = new_devices
                .iter()
                .filter(|n| !n.mac.eq_ignore_ascii_case(&local))
                .count();
            ctx.hub
                .update_device(&mac_s, name, |d| d.other_devices = others);
            for device in new_devices
                .iter()
                .filter(|n| n.mac != local && old_devices.iter().all(|o| o.mac != n.mac))
            {
                info!("New device on AirPods: {}", device.mac);
                note_peer_joined(&mac_s);
                let aacp = aacp.clone();
                let local = local.clone();
                let peer = device.mac.clone();
                tokio::spawn(async move {
                    if let Err(e) = aacp.send_media_information_new_device(&local, &peer).await {
                        debug!("send_media_information_new_device failed: {}", e);
                    }
                    if let Err(e) = aacp.send_add_tipi_device(&local, &peer).await {
                        debug!("send_add_tipi_device failed: {}", e);
                    }
                    // Tell it we are playing, so it doesn't take the AirPods for a ding.
                    let ours = aacp
                        .state
                        .lock()
                        .await
                        .audio_source
                        .as_ref()
                        .and_then(|s| route_owner(s, &local));
                    if ours == Some(true)
                        && !MediaController::playing_players().await.is_empty()
                        && let Err(e) = aacp.send_media_information(&local, &peer, true).await
                    {
                        debug!("send_media_information failed: {}", e);
                    }
                });
            }
        }
        AACPEvent::AudioSource(source) => {
            // The AirPods say whose audio they are playing. An iPhone that takes them often
            // never sends OwnsConnection = 0, so this is the reliable signal.
            let had = owns_audio(&ctx.hub, &mac_s);
            match route_owner(source, &ctx.local_mac) {
                // Nobody is playing. If another device let go while something here is still
                // playing, take the AirPods back now instead of waiting for them to drift
                // back. Only after a notification borrowed them or the other device stopped
                // its media: the gap during a hand-over to an iPhone that starts playing
                // looks the same, and taking them back then makes the two fight.
                None if had == Some(false) && route.may_reclaim() => {
                    let worn = aacp
                        .state
                        .lock()
                        .await
                        .ear_detection_status
                        .contains(&EarDetectionStatus::InEar);
                    let playing = route.lock().local_playing
                        || !MediaController::playing_players().await.is_empty();
                    if worn && playing {
                        info!(
                            "The other device let go and media is playing here; taking the AirPods back"
                        );
                        let _ = command_tx
                            .send((ControlCommandIdentifiers::OwnsConnection, vec![0x01]));
                    }
                }
                None => {}
                Some(true) => {
                    ctx.hub
                        .update_device(&mac_s, name, |d| d.owns_audio = Some(true));
                    if had == Some(false) {
                        route_regained(audio, true);
                    }
                }
                // We just took the AirPods; this report is from before the switch.
                Some(false) if route.just_claimed() => {
                    debug!(
                        "Ignoring Audio Source {} right after taking the AirPods",
                        source.mac
                    );
                }
                Some(false) => {
                    let peer = aacp
                        .state
                        .lock()
                        .await
                        .peer_names
                        .get(&source.mac.to_ascii_uppercase())
                        .cloned();
                    ctx.hub.update_device(&mac_s, name, |d| {
                        d.owns_audio = Some(false);
                        d.elsewhere_name = peer;
                    });
                    if had != Some(false) {
                        route_lost(source, aacp, ctx, &mac_s, media, route, audio).await;
                    }
                }
            }
            route.lock().call = source.r#type == AudioSourceType::Call
                && route_owner(source, &ctx.local_mac) == Some(false);
        }
        AACPEvent::PeerStreaming(streaming) => {
            {
                let mut state = route.lock();
                state.peer_stopped = !streaming;
                // Playing media is not a borrow, whatever the switch first looked like.
                if *streaming {
                    state.lost_to_borrow = false;
                }
            }
            if *streaming && owns_audio(&ctx.hub, &mac_s) == Some(false) {
                info!("The other device started playing media; pausing local media");
                if let Some(audio) = audio {
                    audio.route_away();
                }
                media.lock().await.pause_all_media().await;
            }
        }
        AACPEvent::OwnershipToFalseRequest if route.just_claimed() => {
            debug!("Ignoring a release request right after taking the AirPods");
        }
        AACPEvent::OwnershipToFalseRequest => {
            info!("AirPods asked us to release ownership; pausing local media");
            route.lost(false);
            if let Some(audio) = audio {
                audio.route_away();
            }
            let _ = command_tx.send((ControlCommandIdentifiers::OwnsConnection, vec![0x00]));
            media.lock().await.pause_all_media().await;
        }
        AACPEvent::StemPress(press, bud) => {
            use crate::bluetooth::aacp::{StemPressBudType, StemPressType};
            match press {
                StemPressType::DoublePress | StemPressType::TriplePress if stems.track_control => {
                    let controller = media.lock().await;
                    if *press == StemPressType::DoublePress {
                        controller.next_track().await;
                    } else {
                        controller.previous_track().await;
                    }
                }
                StemPressType::LongPress if stems.mic_switch => match bud {
                    StemPressBudType::Left => {
                        let allowed = mic_allowed(aacp, ctx, &mac_s, route).await;
                        mic.request(MicRequest::Toggle { allowed });
                    }
                    StemPressBudType::Right => {
                        let has_anc = ctx
                            .hub
                            .snapshot()
                            .devices
                            .get(&mac_s)
                            .is_none_or(|d| d.model.info().has_anc);
                        if has_anc {
                            cycle_listening_mode(aacp, command_tx).await;
                        }
                    }
                },
                _ => {}
            }
        }
    }
}

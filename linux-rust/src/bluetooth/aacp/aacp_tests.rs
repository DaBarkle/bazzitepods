//! AACP control channel tests without Bluetooth: incoming packets are fed straight into
//! `receive_packet`, and outgoing packets are caught on the channel the send thread would
//! normally drain.

use super::*;
use tokio::sync::mpsc::error::TryRecvError;

const HEADER: [u8; 4] = [0x04, 0x00, 0x04, 0x00];

fn packet(body: &[u8]) -> Vec<u8> {
    [HEADER.as_slice(), body].concat()
}

/// A manager with an event channel attached, as the device task sets it up.
async fn manager() -> (AACPManager, mpsc::UnboundedReceiver<AACPEvent>) {
    let m = AACPManager::new();
    let (tx, rx) = mpsc::unbounded_channel();
    m.set_event_channel(tx).await;
    (m, rx)
}

/// A manager whose outgoing packets land in the returned receiver.
async fn connected() -> (AACPManager, mpsc::Receiver<Vec<u8>>) {
    let m = AACPManager::new();
    let (tx, rx) = mpsc::channel(16);
    m.state.lock().await.sender = Some(tx);
    (m, rx)
}

fn next(rx: &mut mpsc::UnboundedReceiver<AACPEvent>) -> AACPEvent {
    rx.try_recv().expect("an event")
}

fn none(rx: &mut mpsc::UnboundedReceiver<AACPEvent>) {
    match rx.try_recv() {
        Err(TryRecvError::Empty) => {}
        other => panic!("expected no event, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Incoming packets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn battery_packets_are_merged_before_they_are_reported() {
    let (m, mut rx) = manager().await;
    // Left 80% not charging, right 75% not charging, case 40% charging.
    m.receive_packet(&packet(&[
        0x04, 0x00, 0x03, //
        0x04, 0x01, 80, 0x02, 0x01, //
        0x02, 0x01, 75, 0x02, 0x01, //
        0x08, 0x01, 40, 0x01, 0x01,
    ]))
    .await;
    let AACPEvent::BatteryInfo(first) = next(&mut rx) else {
        panic!("not a battery event");
    };
    assert_eq!(first.len(), 3);

    // The case goes quiet (lid closed, both pods out): its last level must survive.
    m.receive_packet(&packet(&[
        0x04, 0x00, 0x02, //
        0x04, 0x01, 79, 0x02, 0x01, //
        0x08, 0x01, 0, 0x04, 0x01,
    ]))
    .await;
    let AACPEvent::BatteryInfo(merged) = next(&mut rx) else {
        panic!("not a battery event");
    };
    let get = |c| merged.iter().find(|b| b.component == c).unwrap();
    assert_eq!(get(BatteryComponent::Left).level, 79);
    assert_eq!(get(BatteryComponent::Right).level, 75);
    assert_eq!(get(BatteryComponent::Case).level, 40);
    assert_eq!(
        get(BatteryComponent::Case).status,
        BatteryStatus::Disconnected
    );
    assert_eq!(m.state.lock().await.battery_info, merged);
}

#[tokio::test]
async fn malformed_battery_packet_changes_nothing() {
    let (m, mut rx) = manager().await;
    // Claims two records but carries one.
    m.receive_packet(&packet(&[0x04, 0x00, 0x02, 0x04, 0x01, 50, 0x02, 0x01]))
        .await;
    none(&mut rx);
    assert!(m.state.lock().await.battery_info.is_empty());
}

#[tokio::test]
async fn control_command_updates_state_subscribers_and_ownership() {
    let (m, mut rx) = manager().await;
    let (sub_tx, mut sub_rx) = mpsc::unbounded_channel();
    m.subscribe_to_control_command(ControlCommandIdentifiers::ListeningMode, sub_tx)
        .await;
    assert!(sub_rx.try_recv().is_err(), "no initial value yet");

    // Listening mode = 0x02 (noise cancellation); trailing zero bytes are trimmed.
    m.receive_packet(&packet(&[0x09, 0x00, 0x0D, 0x02, 0x00, 0x00, 0x00]))
        .await;
    assert_eq!(sub_rx.try_recv().unwrap(), vec![0x02]);
    let AACPEvent::ControlCommand(status) = next(&mut rx) else {
        panic!("not a control command");
    };
    assert_eq!(status.identifier, ControlCommandIdentifiers::ListeningMode);
    assert_eq!(status.value, vec![0x02]);

    // A second value replaces the first rather than adding a duplicate entry.
    m.receive_packet(&packet(&[0x09, 0x00, 0x0D, 0x03, 0x00, 0x00, 0x00]))
        .await;
    assert_eq!(sub_rx.try_recv().unwrap(), vec![0x03]);
    {
        let state = m.state.lock().await;
        assert_eq!(state.control_command_status_list.len(), 1);
        assert_eq!(state.control_command_status_list[0].value, vec![0x03]);
    }

    // A late subscriber gets the current value straight away.
    let (late_tx, mut late_rx) = mpsc::unbounded_channel();
    m.subscribe_to_control_command(ControlCommandIdentifiers::ListeningMode, late_tx)
        .await;
    assert_eq!(late_rx.try_recv().unwrap(), vec![0x03]);

    // All-zero value is reported as a single zero byte, and OwnsConnection drives `owns`.
    m.receive_packet(&packet(&[0x09, 0x00, 0x06, 0x01, 0x00, 0x00, 0x00]))
        .await;
    assert!(m.state.lock().await.owns);
    m.receive_packet(&packet(&[0x09, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00]))
        .await;
    assert!(!m.state.lock().await.owns);
    let _ = next(&mut rx); // listening mode 0x03
    let _ = next(&mut rx); // owns = 1
    let AACPEvent::ControlCommand(off) = next(&mut rx) else {
        panic!("not a control command");
    };
    assert_eq!(off.value, vec![0x00]);
}

#[tokio::test]
async fn unknown_or_short_control_commands_are_ignored() {
    let (m, mut rx) = manager().await;
    m.receive_packet(&packet(&[0x09, 0x00, 0xEE, 0x01, 0x00, 0x00, 0x00]))
        .await;
    m.receive_packet(&packet(&[0x09, 0x00, 0x0D, 0x01])).await;
    none(&mut rx);
    assert!(m.state.lock().await.control_command_status_list.is_empty());
}

#[tokio::test]
async fn ear_detection_reports_previous_and_new_state() {
    let (m, mut rx) = manager().await;
    // Not said yet: maybe worn, so a press of play here can still take them back.
    assert!(m.state.lock().await.maybe_worn());
    m.receive_packet(&packet(&[0x06, 0x00, 0x00, 0x00])).await;
    let AACPEvent::EarDetection(old, new) = next(&mut rx) else {
        panic!("not ear detection");
    };
    assert!(old.is_empty());
    assert_eq!(
        new,
        vec![EarDetectionStatus::InEar, EarDetectionStatus::InEar]
    );

    // One pod taken out, the other put in the case.
    m.receive_packet(&packet(&[0x06, 0x00, 0x01, 0x02])).await;
    let AACPEvent::EarDetection(old, new) = next(&mut rx) else {
        panic!("not ear detection");
    };
    assert_eq!(
        old,
        vec![EarDetectionStatus::InEar, EarDetectionStatus::InEar]
    );
    assert_eq!(
        new,
        vec![EarDetectionStatus::OutOfEar, EarDetectionStatus::InCase]
    );
    assert!(!m.state.lock().await.maybe_worn());

    // Unknown status bytes are ignored: no event, and the last known state is kept, so
    // media is not paused for a status we can't read.
    m.receive_packet(&packet(&[0x06, 0x00, 0x09, 0x03])).await;
    none(&mut rx);
    assert_eq!(
        m.state.lock().await.ear_detection_status,
        vec![EarDetectionStatus::OutOfEar, EarDetectionStatus::InCase]
    );

    m.receive_packet(&packet(&[0x06, 0x00, 0x00])).await;
    none(&mut rx);
}

#[tokio::test]
async fn conversation_awareness_needs_exact_length() {
    let (m, mut rx) = manager().await;
    m.receive_packet(&packet(&[0x4B, 0x00, 0x02, 0x00, 0x01, 0x01]))
        .await;
    assert!(matches!(
        next(&mut rx),
        AACPEvent::ConversationalAwareness(0x01)
    ));
    assert_eq!(m.state.lock().await.conversational_awareness_status, 0x01);
    m.receive_packet(&packet(&[0x4B, 0x00, 0x02, 0x00, 0x01, 0x01, 0x00]))
        .await;
    none(&mut rx);
}

#[tokio::test]
async fn stem_presses() {
    let (m, mut rx) = manager().await;
    for (byte, press) in [
        (0x05, StemPressType::SinglePress),
        (0x06, StemPressType::DoublePress),
        (0x07, StemPressType::TriplePress),
        (0x08, StemPressType::LongPress),
    ] {
        m.receive_packet(&packet(&[0x19, 0x00, byte, 0x02])).await;
        let AACPEvent::StemPress(p, bud) = next(&mut rx) else {
            panic!("not a stem press");
        };
        assert_eq!(p, press);
        assert_eq!(bud, StemPressBudType::Right);
    }
    m.receive_packet(&packet(&[0x19, 0x00, 0x06, 0x01])).await;
    assert!(matches!(
        next(&mut rx),
        AACPEvent::StemPress(StemPressType::DoublePress, StemPressBudType::Left)
    ));
    // Unknown press or bud byte, or a short packet: nothing.
    m.receive_packet(&packet(&[0x19, 0x00, 0x01, 0x01])).await;
    m.receive_packet(&packet(&[0x19, 0x00, 0x05, 0x03])).await;
    m.receive_packet(&packet(&[0x19, 0x00, 0x05])).await;
    none(&mut rx);
}

#[tokio::test]
async fn audio_source_reads_the_reversed_mac() {
    let (m, mut rx) = manager().await;
    // MAC 10:A2:D3:BF:3D:76 sent least significant byte first, then type Media.
    m.receive_packet(&packet(&[
        0x0E, 0x00, 0x76, 0x3D, 0xBF, 0xD3, 0xA2, 0x10, 0x02,
    ]))
    .await;
    let AACPEvent::AudioSource(src) = next(&mut rx) else {
        panic!("not an audio source");
    };
    assert_eq!(src.mac, "10:A2:D3:BF:3D:76");
    assert_eq!(src.r#type, AudioSourceType::Media);
    assert_eq!(m.state.lock().await.audio_source, Some(src));

    // Unknown type byte reads as None; short packets are dropped.
    m.receive_packet(&packet(&[0x0E, 0x00, 1, 2, 3, 4, 5, 6, 0x7F]))
        .await;
    let AACPEvent::AudioSource(src) = next(&mut rx) else {
        panic!("not an audio source");
    };
    assert_eq!(src.r#type, AudioSourceType::None);
    m.receive_packet(&packet(&[0x0E, 0x00, 1, 2, 3, 4, 5, 6]))
        .await;
    none(&mut rx);
}

#[tokio::test]
async fn connected_devices_list_and_previous_list() {
    let (m, mut rx) = manager().await;
    let iphone = [0x10, 0xA2, 0xD3, 0xBF, 0x3D, 0x76, 0x01, 0x02];
    let pc = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x03, 0x04];
    let body = [&[0x2E, 0x00, 0x00, 0x00, 0x02][..], &iphone, &pc].concat();
    m.receive_packet(&packet(&body)).await;
    let AACPEvent::ConnectedDevices(old, new) = next(&mut rx) else {
        panic!("not connected devices");
    };
    assert!(old.is_empty());
    assert_eq!(new.len(), 2);
    assert_eq!(new[0].mac, "10:A2:D3:BF:3D:76");
    assert_eq!((new[0].info1, new[0].info2), (0x01, 0x02));
    assert_eq!(new[1].mac, "AA:BB:CC:DD:EE:FF");

    // iPhone leaves.
    let body = [&[0x2E, 0x00, 0x00, 0x00, 0x01][..], &pc].concat();
    m.receive_packet(&packet(&body)).await;
    let AACPEvent::ConnectedDevices(old, new) = next(&mut rx) else {
        panic!("not connected devices");
    };
    assert_eq!(old.len(), 2);
    assert_eq!(new.len(), 1);

    // Count larger than the data: rejected, state unchanged.
    let body = [&[0x2E, 0x00, 0x00, 0x00, 0x03][..], &pc].concat();
    m.receive_packet(&packet(&body)).await;
    m.receive_packet(&packet(&[0x2E, 0x00, 0x00])).await;
    none(&mut rx);
    assert_eq!(m.state.lock().await.connected_devices.len(), 1);
}

#[tokio::test]
async fn smart_routing_streaming_and_ownership_requests() {
    let (m, mut rx) = manager().await;
    let yes = b"\x11\x00..hostStreamingStateCYESplayingAppVcom.apple.Music";
    m.receive_packet(&packet(yes)).await;
    assert!(matches!(next(&mut rx), AACPEvent::PeerStreaming(true)));
    assert!(m.state.lock().await.peer_streaming);

    let no = b"\x11\x00..hostStreamingStateBNO";
    m.receive_packet(&packet(no)).await;
    assert!(matches!(next(&mut rx), AACPEvent::PeerStreaming(false)));
    assert!(!m.state.lock().await.peer_streaming);

    let take = b"\x11\x00..audioRoutingSetOwnershipToFalse\x01";
    m.receive_packet(&packet(take)).await;
    assert!(matches!(next(&mut rx), AACPEvent::OwnershipToFalseRequest));

    // A system-sound blip ("Unknown" app) is not a real stream.
    let blip = b"\x11\x00..hostStreamingStateCYESplayingAppGUnknown";
    m.receive_packet(&packet(blip)).await;
    none(&mut rx);
}

#[tokio::test]
async fn keys_and_information_without_a_known_mac_are_not_saved() {
    let (m, mut rx) = manager().await;
    // Proximity keys: one IRK of 16 bytes. MAC unknown, so nothing is written or sent.
    let mut keys = vec![0x31, 0x00, 0x01, 0x01, 0x00, 0x10, 0x00];
    keys.extend_from_slice(&[0xAB; 16]);
    m.receive_packet(&packet(&keys)).await;
    m.receive_packet(&packet(&[0x1D, 0x00, 0x00, 0x00, 0x01, 0x00, b'A', 0x00]))
        .await;
    none(&mut rx);
}

#[tokio::test]
async fn ignores_foreign_unknown_and_truncated_packets() {
    let (m, mut rx) = manager().await;
    m.receive_packet(&[0x01, 0x00, 0x04, 0x00, 0x04]).await;
    m.receive_packet(&HEADER).await;
    m.receive_packet(&packet(&[0x53, 0x00])).await; // EQ data
    m.receive_packet(&packet(&[0xF0, 0x00, 0x01])).await;
    none(&mut rx);
}

/// Every opcode with every short length and pseudo-random contents: no panics.
#[tokio::test]
async fn never_panics_on_noise() {
    let (m, _rx) = manager().await;
    let opcodes = [
        0x04, 0x06, 0x09, 0x0E, 0x10, 0x11, 0x19, 0x1D, 0x2E, 0x31, 0x4B, 0x53,
    ];
    let mut seed = 0x2545_F491_u32;
    for op in opcodes {
        for len in 0..48usize {
            let mut body = vec![op];
            for _ in 0..len {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                body.push(seed as u8);
            }
            // Keys and information would be saved if a MAC were known; it is not here.
            m.receive_packet(&packet(&body)).await;
        }
    }
}

// ---------------------------------------------------------------------------
// Outgoing packets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sending_without_a_link_is_an_error_not_a_panic() {
    let m = AACPManager::new();
    assert!(!m.is_connected().await);
    assert!(m.send_handshake().await.is_err());
    assert!(m.send_notification_request().await.is_err());
    assert!(
        m.send_control_command(ControlCommandIdentifiers::ListeningMode, &[1])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn simple_requests_have_the_documented_bytes() {
    let (m, mut rx) = connected().await;
    assert!(m.is_connected().await);

    m.send_handshake().await.unwrap();
    let hs = rx.try_recv().unwrap();
    assert_eq!(hs.len(), 16);
    assert_eq!(&hs[..6], &[0x00, 0x00, 0x04, 0x00, 0x01, 0x00]);

    m.send_notification_request().await.unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        packet(&[0x0F, 0x00, 0xFF, 0xFF, 0xFF, 0xFF])
    );

    m.send_set_feature_flags_packet().await.unwrap();
    let ff = rx.try_recv().unwrap();
    assert_eq!(&ff[..6], &packet(&[0x4D, 0x00])[..]);
    assert_eq!(ff.len(), 4 + 2 + 8);

    m.send_proximity_keys_request(vec![ProximityKeyType::Irk, ProximityKeyType::EncKey])
        .await
        .unwrap();
    assert_eq!(rx.try_recv().unwrap(), packet(&[0x30, 0x00, 0x05, 0x00]));
}

#[tokio::test]
async fn control_command_values_are_padded_to_four_bytes() {
    let (m, mut rx) = connected().await;
    m.send_control_command(ControlCommandIdentifiers::ListeningMode, &[0x02])
        .await
        .unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        packet(&[0x09, 0x00, 0x0D, 0x02, 0x00, 0x00, 0x00])
    );
    // Longer values are cut at four bytes.
    m.send_control_command(ControlCommandIdentifiers::StemConfig, &[1, 2, 3, 4, 5])
        .await
        .unwrap();
    assert_eq!(
        rx.try_recv().unwrap(),
        packet(&[0x09, 0x00, 0x39, 1, 2, 3, 4])
    );
}

#[tokio::test]
async fn rename_carries_length_and_utf8_name() {
    let (m, mut rx) = connected().await;
    m.send_rename_packet("Dave’s Pods").await.unwrap();
    let p = rx.try_recv().unwrap();
    let name = "Dave’s Pods".as_bytes();
    assert_eq!(&p[4..9], &[0x1A, 0x00, 0x01, name.len() as u8, 0x00]);
    assert_eq!(&p[9..], name);
}

#[tokio::test]
async fn smart_routing_packets_target_the_reversed_mac_and_keep_their_size() {
    let (m, mut rx) = connected().await;
    let me = "AA:BB:CC:DD:EE:FF";
    let iphone = "10:A2:D3:BF:3D:76";
    let target = [0x76, 0x3D, 0xBF, 0xD3, 0xA2, 0x10];

    m.send_hijack_request(iphone).await.unwrap();
    m.send_hijack_reversed(iphone).await.unwrap();
    m.send_smart_routing_show_ui(iphone).await.unwrap();
    m.send_media_information(me, iphone, true).await.unwrap();
    m.send_media_information_new_device(me, iphone)
        .await
        .unwrap();
    m.send_add_tipi_device(me, iphone).await.unwrap();

    let mut sizes = Vec::new();
    while let Ok(p) = rx.try_recv() {
        assert_eq!(&p[..6], &packet(&[0x10, 0x00])[..]);
        assert_eq!(&p[6..12], &target);
        sizes.push(p.len() - 6);
    }
    // Fixed-size bodies (after opcode) the AirPods expect.
    assert_eq!(&sizes[..4], &[106, 97, 134, 138]);
    assert_eq!(sizes.len(), 6);

    // A bad MAC fails cleanly and sends nothing.
    assert!(m.send_hijack_request("not a mac").await.is_err());
    assert!(m.send_media_information(me, "", false).await.is_err());
    assert!(rx.try_recv().is_err());
}

#[tokio::test]
async fn media_information_tells_whether_this_pc_is_streaming() {
    for (streaming, expect) in [(true, Some(true)), (false, Some(false))] {
        let p =
            media_information_packet("AA:BB:CC:DD:EE:FF", "10:A2:D3:BF:3D:76", streaming).unwrap();
        let text = String::from_utf8_lossy(&p);
        // An iPhone reading our message must see the same state we meant. "playingApp" is
        // a real app here, so the YES is not mistaken for a system-sound blip.
        assert_eq!(peer_streaming_state(&text), expect);
        assert!(text.contains("AA:BB:CC:DD:EE:FF"));
    }
}

#[tokio::test]
async fn shutdown_disconnects_and_drops_subscribers() {
    let (m, _rx) = connected().await;
    let (tx, _sub) = mpsc::unbounded_channel();
    m.subscribe_to_control_command(ControlCommandIdentifiers::ListeningMode, tx)
        .await;
    let (etx, _erx) = mpsc::unbounded_channel();
    m.set_event_channel(etx).await;
    m.shutdown().await;
    m.shutdown().await; // safe twice
    assert!(!m.is_connected().await);
    let state = m.state.lock().await;
    assert!(state.control_command_subscribers.is_empty());
    assert!(state.event_tx.is_none());
}

// ---------------------------------------------------------------------------
// Small parsers and tables
// ---------------------------------------------------------------------------

#[test]
fn control_command_identifiers_round_trip_and_have_names() {
    let mut known = 0;
    for byte in 0..=u8::MAX {
        if let Some(id) = ControlCommandIdentifiers::from_u8(byte) {
            known += 1;
            assert_eq!(id as u8, byte);
            assert!(!id.to_string().is_empty());
        }
    }
    assert_eq!(known, 35);
    assert_eq!(
        ControlCommandIdentifiers::ListeningMode.to_string(),
        "Listening Mode"
    );
}

#[test]
fn proximity_keys_reject_truncated_entries() {
    // Two keys announced, second one cut short.
    let mut p = vec![0x31, 0x00, 0x02];
    p.extend_from_slice(&[0x01, 0x00, 0x02, 0x00, 0xAA, 0xBB]);
    p.extend_from_slice(&[0x04, 0x00, 0x10, 0x00, 0xCC]);
    assert_eq!(parse_proximity_keys(&p), None);
    // Zero keys is a valid, empty answer.
    assert_eq!(parse_proximity_keys(&[0x31, 0x00, 0x00]), Some(vec![]));
}

#[test]
fn battery_count_limits() {
    // Count 0 or more than four records is not a battery packet.
    assert_eq!(parse_battery_info(&[0x04, 0x00, 0x00]), None);
    let five = [
        &[0x04u8, 0x00, 0x05][..],
        &[0x04, 0x01, 50, 0x02, 0x01].repeat(5),
    ]
    .concat();
    assert_eq!(parse_battery_info(&five), None);
    // Wrong opcode.
    assert_eq!(
        parse_battery_info(&[0x05, 0x00, 0x01, 0x04, 0x01, 50, 0x02, 0x01]),
        None
    );
}

#[test]
fn merge_never_stores_impossible_levels() {
    let mut list = Vec::new();
    merge_battery_info(
        &mut list,
        &[BatteryInfo {
            component: BatteryComponent::Left,
            level: 200,
            status: BatteryStatus::NotCharging,
        }],
    );
    // Stored as sent; the battery tracker ignores levels above 100.
    assert_eq!(list[0].level, 200);
    merge_battery_info(
        &mut list,
        &[BatteryInfo {
            component: BatteryComponent::Left,
            level: 55,
            status: BatteryStatus::Charging,
        }],
    );
    merge_battery_info(
        &mut list,
        &[BatteryInfo {
            component: BatteryComponent::Left,
            level: 0,
            status: BatteryStatus::Unknown,
        }],
    );
    assert_eq!(list[0].level, 55);
    assert_eq!(list[0].status, BatteryStatus::Unknown);
}

#[tokio::test]
async fn a_dead_send_task_means_not_connected() {
    let (m, rx) = connected().await;
    assert!(m.is_connected().await);
    // The send task exits on a hard send error and drops its receiver.
    drop(rx);
    assert!(!m.is_connected().await);
}

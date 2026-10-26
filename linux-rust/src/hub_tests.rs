//! The hub every view reads from: when it notifies, and which device it features.

use super::*;
use std::time::Duration;

#[test]
fn adapter_changes_notify_once() {
    let hub = Hub::new();
    let mut rx = hub.subscribe();
    assert_eq!(hub.snapshot().adapter, AdapterState::Missing);
    hub.set_adapter(AdapterState::Missing);
    assert!(!rx.has_changed().unwrap());
    hub.set_adapter(AdapterState::Off);
    assert!(rx.has_changed().unwrap());
    rx.borrow_and_update();
    hub.set_adapter(AdapterState::Off);
    assert!(!rx.has_changed().unwrap());
    hub.set_adapter(AdapterState::On);
    assert_eq!(hub.snapshot().adapter, AdapterState::On);
}

#[test]
fn disconnect_clears_who_has_the_audio() {
    let hub = Hub::new();
    hub.update_device("AA", "Pods", |d| {
        d.link = LinkState::Connected;
        d.owns_audio = Some(false);
        d.other_devices = 1;
    });
    let d = hub.snapshot().devices["AA"].clone();
    assert!(d.in_use_elsewhere());

    // A control-channel blip keeps the routing state.
    hub.set_link("AA", LinkState::Reconnecting { attempt: 1 });
    let d = hub.snapshot().devices["AA"].clone();
    assert_eq!(d.owns_audio, Some(false));
    assert!(d.in_use_elsewhere());

    hub.set_link("AA", LinkState::Disconnected);
    let d = hub.snapshot().devices["AA"].clone();
    assert_eq!(d.owns_audio, None);
    assert_eq!(d.other_devices, 0);
    assert!(!d.in_use_elsewhere());
    assert!(!d.is_connected());
}

#[test]
fn set_link_ignores_unknown_devices_and_repeats() {
    let hub = Hub::new();
    let mut rx = hub.subscribe();
    hub.set_link("ZZ", LinkState::Connected);
    assert!(!rx.has_changed().unwrap());
    assert!(hub.snapshot().devices.is_empty());

    hub.update_device("AA", "Pods", |_| {});
    rx.borrow_and_update();
    hub.set_link("AA", LinkState::Disconnected); // new devices start disconnected
    assert!(!rx.has_changed().unwrap());
}

#[test]
fn every_link_state_but_disconnected_counts_as_connected() {
    let mut d = DeviceSnapshot::new("AA".into(), "Pods".into());
    for (link, connected) in [
        (LinkState::Connecting, true),
        (LinkState::Connected, true),
        (LinkState::Reconnecting { attempt: 3 }, true),
        (LinkState::Limited, true),
        (LinkState::Disconnected, false),
    ] {
        d.link = link;
        assert_eq!(d.is_connected(), connected, "{link:?}");
    }
}

#[test]
fn audio_owned_here_or_unknown_is_not_elsewhere() {
    let mut d = DeviceSnapshot::new("AA".into(), "Pods".into());
    d.link = LinkState::Connected;
    for (owns, elsewhere) in [(None, false), (Some(true), false), (Some(false), true)] {
        d.owns_audio = owns;
        assert_eq!(d.in_use_elsewhere(), elsewhere);
    }
}

#[test]
fn featured_device_is_the_most_recent_connected_one() {
    let now = SystemTime::now();
    let mut snap = AppSnapshot::default();
    assert!(snap.primary().is_none());
    let mut add = |mac: &str, link, age: u64| {
        let mut d = DeviceSnapshot::new(mac.into(), mac.into());
        d.link = link;
        d.last_seen = Some(now - Duration::from_secs(age));
        snap.devices.insert(mac.into(), d);
    };
    add("OLD", LinkState::Disconnected, 5);
    add("A", LinkState::Connected, 60);
    add("B", LinkState::Limited, 30);
    assert_eq!(snap.primary().unwrap().mac, "B");

    // Nothing connected: the most recently seen one.
    for d in snap.devices.values_mut() {
        d.link = LinkState::Disconnected;
    }
    assert_eq!(snap.primary().unwrap().mac, "OLD");
}

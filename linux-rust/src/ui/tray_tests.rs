//! The tray menu: what it lists for each state, and what its items send to the AirPods.

use super::*;
use crate::core::battery::ComponentBattery;
use crate::core::models::ModelKind;
use crate::hub::{AdapterState, LinkState};
use ksni::Tray;
use ksni::menu::MenuItem;
use tokio::sync::mpsc;

type Commands = mpsc::UnboundedReceiver<(ControlCommandIdentifiers, Vec<u8>)>;
type UiMessages = mpsc::UnboundedReceiver<BluetoothUIMessage>;

fn pods(model: ModelKind, link: LinkState) -> DeviceSnapshot {
    let mut d = DeviceSnapshot::new("AA:BB:CC:DD:EE:FF".into(), "David_Pods".into());
    d.link = link;
    d.model = model;
    let c = ComponentBattery {
        level: Some(80),
        present: true,
        ..Default::default()
    };
    d.battery.left = c.clone();
    d.battery.right = c;
    d
}

fn tray(device: Option<DeviceSnapshot>) -> (LibreTray, Commands, UiMessages) {
    let mut snap = AppSnapshot {
        adapter: AdapterState::On,
        ..Default::default()
    };
    if let Some(d) = device {
        snap.devices.insert(d.mac.clone(), d);
    }
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    let t = LibreTray {
        snap,
        commands: Some(cmd_tx),
        ui_tx,
        cache: Default::default(),
    };
    (t, cmd_rx, ui_rx)
}

fn labels(items: &[MenuItem<LibreTray>]) -> Vec<String> {
    items
        .iter()
        .map(|i| match i {
            MenuItem::Standard(s) => s.label.clone(),
            MenuItem::Separator => "---".into(),
            MenuItem::Checkmark(c) => {
                format!("[{}] {}", if c.checked { "x" } else { " " }, c.label)
            }
            MenuItem::RadioGroup(r) => format!(
                "radio({}): {}",
                r.selected,
                r.options
                    .iter()
                    .map(|o| o.label.as_str())
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            MenuItem::SubMenu(s) => format!("sub {}", s.label),
        })
        .collect()
}

#[test]
fn airpods4_menu_has_battery_but_no_noise_control() {
    let (t, _, _) = tray(Some(pods(ModelKind::AirPods4, LinkState::Connected)));
    let menu = labels(&t.menu());
    assert_eq!(
        menu[0], "David__Pods — Connected",
        "underscores escaped for mnemonics"
    );
    assert!(menu[1].starts_with("L 80%"), "{menu:?}");
    assert!(!menu.iter().any(|l| l.starts_with("radio")));
    assert!(!menu.iter().any(|l| l.contains("Conversation")));
    assert_eq!(&menu[menu.len() - 2..], ["Open Kase", "Quit"]);
}

#[test]
fn pro_menu_selects_the_current_listening_mode_and_sends_a_new_one() {
    let mut d = pods(ModelKind::AirPodsPro2, LinkState::Connected);
    d.listening_mode = Some(0x03); // transparency
    d.conversation_awareness = Some(true);
    let (mut t, mut cmds, _) = tray(Some(d));
    let menu = t.menu();
    let names = labels(&menu);
    assert!(
        names.contains(&"radio(0): Transparency|Adaptive|Noise Cancellation".to_string()),
        "{names:?}"
    );
    assert!(names.contains(&"[x] Conversation Awareness".to_string()));

    for item in menu {
        match item {
            MenuItem::RadioGroup(r) => (r.select)(&mut t, 2),
            MenuItem::Checkmark(c) => (c.activate)(&mut t),
            _ => {}
        }
    }
    assert_eq!(
        cmds.try_recv().unwrap(),
        (ControlCommandIdentifiers::ListeningMode, vec![0x02])
    );
    // Checked -> turn it off (0x02).
    assert_eq!(
        cmds.try_recv().unwrap(),
        (
            ControlCommandIdentifiers::ConversationDetectConfig,
            vec![0x02]
        )
    );
}

#[test]
fn off_mode_is_offered_only_when_allowed() {
    let mut d = pods(ModelKind::AirPodsPro2, LinkState::Connected);
    d.allow_off_mode = Some(true);
    d.listening_mode = Some(0x01);
    let (t, _, _) = tray(Some(d));
    assert!(
        labels(&t.menu())
            .contains(&"radio(0): Off|Transparency|Adaptive|Noise Cancellation".to_string())
    );
}

#[test]
fn controls_are_disabled_while_the_control_channel_is_down() {
    let mut d = pods(
        ModelKind::AirPodsPro2,
        LinkState::Reconnecting { attempt: 1 },
    );
    d.conversation_awareness = Some(false);
    let (t, _, _) = tray(Some(d));
    for item in t.menu() {
        match item {
            MenuItem::RadioGroup(r) => assert!(r.options.iter().all(|o| !o.enabled)),
            MenuItem::Checkmark(c) => assert!(!c.enabled),
            _ => {}
        }
    }
}

#[test]
fn disconnected_device_has_no_controls() {
    let (t, _, _) = tray(Some(pods(ModelKind::AirPodsPro2, LinkState::Disconnected)));
    let menu = labels(&t.menu());
    assert!(
        !menu
            .iter()
            .any(|l| l.starts_with("radio") || l.contains("Conversation"))
    );
}

#[test]
fn no_device_shows_a_hint() {
    let (t, _, _) = tray(None);
    let menu = labels(&t.menu());
    assert_eq!(menu[0], "No AirPods paired");
    assert_eq!(t.tool_tip().title, "Kase");
}

#[test]
fn clicking_the_icon_or_open_item_opens_the_window() {
    let (mut t, _, mut ui) = tray(None);
    t.activate(0, 0);
    assert!(matches!(ui.try_recv(), Ok(BluetoothUIMessage::OpenWindow)));
    let open = t
        .menu()
        .into_iter()
        .find_map(|i| match i {
            MenuItem::Standard(s) if s.label == "Open Kase" => Some(s),
            _ => None,
        })
        .unwrap();
    (open.activate)(&mut t);
    assert!(matches!(ui.try_recv(), Ok(BluetoothUIMessage::OpenWindow)));
}

#[test]
fn commands_without_a_control_channel_are_dropped_quietly() {
    let (mut t, cmds, _) = tray(Some(pods(ModelKind::AirPodsPro2, LinkState::Connected)));
    drop(cmds);
    t.send(ControlCommandIdentifiers::ListeningMode, 0x02);
    t.commands = None;
    t.send(ControlCommandIdentifiers::ListeningMode, 0x02);
}

#[test]
fn icons_are_cached_and_badge_only_while_connected() {
    let (t, _, _) = tray(Some(pods(ModelKind::AirPods4, LinkState::Connected)));
    assert_eq!(t.icon_pixmap().len(), 2);
    assert_eq!(t.overlay_icon_pixmap().len(), 2);
    assert_eq!(t.cache.lock().unwrap().len(), 2);
    t.icon_pixmap();
    assert_eq!(
        t.cache.lock().unwrap().len(),
        2,
        "second call served from cache"
    );

    let (t, _, _) = tray(Some(pods(ModelKind::AirPods4, LinkState::Disconnected)));
    assert!(t.overlay_icon_pixmap().is_empty());
    assert_eq!(t.icon_name(), ICON_NAME);
    assert_eq!(t.title(), "Kase");
}

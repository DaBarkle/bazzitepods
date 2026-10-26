//! Main window: one calm screen driven entirely by the hub snapshot.
//!
//! Rules: `view` and `update` never block, never touch the disk, never panic. Device commands go
//! through `active_commands` from an async task; settings are written by a background task.

use crate::bluetooth::aacp::{AACPEvent, ControlCommandIdentifiers};
use crate::bluetooth::att::ATTHandles;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::CommandSender;
use crate::devices::enums::{DeviceData, DeviceInformation};
use crate::hub::{AdapterState, AppSnapshot, DeviceSnapshot, Hub};
use crate::ui::format::{self, Gauge, ListeningMode, PillTone};
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::settings::{
    Appearance, OVERLAY_POSITIONS, OverlayMode, OverlayPosition, Settings, SettingsStore,
};
use crate::ui::style::{self, Tokens};
use crate::ui::widgets::{self, Glyph, Ink};
use futures::Stream;
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, text, text_input, toggler,
};
use iced::{Center, Element, Fill, Length, Padding, Size, Subscription, Task, Theme, window};
use log::{debug, info, warn};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{Mutex, RwLock};

const WINDOW_SIZE: Size = Size::new(420.0, 600.0);
/// How long an optimistic control change is shown before we fall back to the device's state.
const PENDING_FOR: Duration = Duration::from_secs(4);
/// The supervisor needs a moment to find the adapter; don't flash "unavailable" at startup.
const ADAPTER_GRACE: Duration = Duration::from_secs(4);

/// Everything the UI needs from the rest of the app.
pub struct UiContext {
    pub hub: Hub,
    pub active_commands: Arc<Mutex<Option<CommandSender>>>,
    pub device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    pub settings: SettingsStore,
    /// Legacy event channel: tray "Open" requests, Nothing devices, adaptive volume state.
    pub ui_rx: UnboundedReceiver<BluetoothUIMessage>,
    pub start_minimized: bool,
    /// `--ui-demo`: no Bluetooth; controls act on the hub directly.
    pub demo: bool,
}

pub fn start_ui(ctx: UiContext) -> iced::Result {
    let UiContext {
        hub,
        active_commands,
        device_managers,
        settings,
        ui_rx,
        start_minimized,
        demo,
    } = ctx;
    let ui_rx = RxKey(Arc::new(std::sync::Mutex::new(Some(ui_rx))));

    iced::daemon(
        move || {
            App::new(
                hub.clone(),
                active_commands.clone(),
                device_managers.clone(),
                settings.clone(),
                ui_rx.clone(),
                start_minimized,
                demo,
            )
        },
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .theme(App::theme)
    .title(|_: &App, _| "BazzitePods".to_string())
    .settings(iced::Settings {
        id: Some(crate::autostart::APP_ID.to_string()),
        fonts: vec![
            include_bytes!("../../assets/font/Inter-Regular.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../assets/font/Inter-Medium.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../assets/font/Inter-SemiBold.ttf")
                .as_slice()
                .into(),
        ],
        default_font: style::INTER,
        default_text_size: 14.into(),
        ..iced::Settings::default()
    })
    .run()
}

/// Subscription identity for the hub stream.
#[derive(Clone)]
struct HubKey(Hub);

impl Hash for HubKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "librepods-hub".hash(state);
    }
}

fn hub_stream(key: &HubKey) -> impl Stream<Item = AppSnapshot> + use<> {
    let rx = key.0.subscribe();
    futures::stream::unfold((rx, true), |(mut rx, first)| async move {
        if !first {
            rx.changed().await.ok()?;
        }
        let snap = rx.borrow_and_update().clone();
        Some((snap, (rx, false)))
    })
}

/// Subscription identity for the legacy event channel (taken once).
#[derive(Clone)]
struct RxKey(Arc<std::sync::Mutex<Option<UnboundedReceiver<BluetoothUIMessage>>>>);

impl Hash for RxKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        "librepods-legacy-rx".hash(state);
    }
}

fn legacy_stream(key: &RxKey) -> impl Stream<Item = BluetoothUIMessage> + use<> {
    let rx = key.0.lock().ok().and_then(|mut g| g.take());
    futures::stream::unfold(rx, |rx| async move {
        let mut rx = rx?;
        let msg = rx.recv().await?;
        Some((msg, Some(rx)))
    })
}

#[derive(Debug, Clone)]
pub enum Message {
    Hub(AppSnapshot),
    Legacy(BluetoothUIMessage),
    WindowOpened(window::Id),
    WindowClosed(window::Id),
    SystemTheme(iced::theme::Mode),
    Tick,
    SetMode(ListeningMode),
    SetConversation(bool),
    SetAllowOff(bool),
    SetAdaptiveVolume(bool),
    SetOverlayMode(OverlayMode),
    PreviewOverlay,
    SetOverlayPosition(OverlayPosition),
    SetOverlayOpacity(f32),
    SetOverlayGlass(f32),
    CommitOverlaySliders,
    SetStartAtLogin(bool),
    SetStemControl(bool),
    SetAppearance(Appearance),
    ToggleMore,
    RenameInput(String),
    RenameSubmit,
    RenameDone(Result<(), String>),
    CommandSent(bool),
    AutostartDone(Result<(), String>),
    NothingConnected(Option<String>),
    SavedDevices(HashMap<String, DeviceData>),
    AppleMode(Option<bool>),
    CopyAppleCommand,
    TakeAudio,
    SetNothingMode(String, NothingMode),
    Ignore,
}

/// Nothing Ear noise control modes (legacy device support behind "More").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NothingMode {
    Off,
    Transparency,
    Adaptive,
    Low,
    Mid,
    High,
}

impl NothingMode {
    const ALL: [NothingMode; 6] = [
        NothingMode::Off,
        NothingMode::Transparency,
        NothingMode::Adaptive,
        NothingMode::Low,
        NothingMode::Mid,
        NothingMode::High,
    ];

    fn byte(self) -> u8 {
        match self {
            NothingMode::Off => 0x05,
            NothingMode::Transparency => 0x07,
            NothingMode::Adaptive => 0x04,
            NothingMode::Low => 0x03,
            NothingMode::Mid => 0x02,
            NothingMode::High => 0x01,
        }
    }
}

impl std::fmt::Display for NothingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            NothingMode::Off => "Off",
            NothingMode::Transparency => "Transparency",
            NothingMode::Adaptive => "Adaptive noise cancellation",
            NothingMode::Low => "Noise cancellation · Low",
            NothingMode::Mid => "Noise cancellation · Mid",
            NothingMode::High => "Noise cancellation · High",
        })
    }
}

pub struct App {
    hub: Hub,
    active_commands: Arc<Mutex<Option<CommandSender>>>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    store: SettingsStore,
    ui_rx: RxKey,
    demo: bool,

    snap: AppSnapshot,
    settings: Settings,
    window: Option<window::Id>,
    window_icon: Option<window::Icon>,
    system_dark: Option<bool>,
    dark_theme: Theme,
    light_theme: Theme,
    started: Instant,
    now: SystemTime,

    more_open: bool,
    pending_mode: Option<(u8, Instant)>,
    pending_ca: Option<(bool, Instant)>,
    /// "Use on this PC" was pressed; shows "Switching…" until the AirPods answer.
    taking_audio: Option<Instant>,
    adaptive_volume: HashMap<String, bool>,
    rename: String,
    /// Bluetooth adapter reports Apple's vendor id (DeviceID spoofing); None = unknown.
    apple_mode: Option<bool>,
    apple_copied: bool,
    rename_status: Option<String>,
    nothing: BTreeMap<String, Option<NothingMode>>,
    /// devices.json (serials, firmware), loaded off the UI thread.
    saved: HashMap<String, DeviceData>,
}

fn load_window_icon() -> Option<window::Icon> {
    let bytes = include_bytes!("../../assets/icons/io.github.dabarkle.BazzitePods.png");
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    window::icon::from_rgba(img.into_raw(), w, h).ok()
}

impl App {
    #[allow(clippy::too_many_arguments)]
    fn new(
        hub: Hub,
        active_commands: Arc<Mutex<Option<CommandSender>>>,
        device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
        store: SettingsStore,
        ui_rx: RxKey,
        start_minimized: bool,
        demo: bool,
    ) -> (Self, Task<Message>) {
        let mut app = Self {
            snap: hub.snapshot(),
            settings: store.get(),
            hub,
            active_commands,
            device_managers,
            store,
            ui_rx,
            demo,
            window: None,
            window_icon: load_window_icon(),
            system_dark: None,
            dark_theme: style::theme(true),
            light_theme: style::theme(false),
            started: Instant::now(),
            now: SystemTime::now(),
            // Demo/screenshots only: start with "More options" expanded.
            more_open: demo && std::env::var_os("LIBREPODS_UI_EXPAND").is_some(),
            pending_mode: None,
            pending_ca: None,
            taking_audio: None,
            adaptive_volume: HashMap::new(),
            rename: String::new(),
            apple_mode: demo.then_some(false),
            apple_copied: false,
            rename_status: None,
            nothing: BTreeMap::new(),
            saved: HashMap::new(),
        };
        let theme = iced::system::theme().map(Message::SystemTheme);
        let open = if start_minimized {
            Task::none()
        } else {
            app.open_window()
        };
        let saved = if demo {
            Task::none()
        } else {
            load_saved_devices()
        };
        let apple = if demo {
            Task::none()
        } else {
            load_apple_mode()
        };
        (app, Task::batch([theme, open, saved, apple]))
    }

    fn open_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window {
            return window::gain_focus(id);
        }
        // Demo/screenshots only: taller window to capture everything.
        let height = std::env::var("LIBREPODS_UI_HEIGHT")
            .ok()
            .filter(|_| self.demo)
            .and_then(|h| h.parse::<f32>().ok())
            .unwrap_or(WINDOW_SIZE.height);
        let settings = window::Settings {
            size: Size::new(WINDOW_SIZE.width, height),
            min_size: Some(Size::new(380.0, 480.0)),
            max_size: Some(Size::new(560.0, 1000.0)),
            icon: self.window_icon.clone(),
            platform_specific: window::settings::PlatformSpecific {
                application_id: crate::autostart::APP_ID.to_string(),
                ..Default::default()
            },
            ..Default::default()
        };
        let (id, open) = window::open(settings);
        self.window = Some(id);
        open.map(Message::WindowOpened)
    }

    fn primary(&self) -> Option<&DeviceSnapshot> {
        self.snap.primary()
    }

    fn send(&self, id: ControlCommandIdentifiers, value: u8) -> Task<Message> {
        if self.demo {
            self.apply_demo(id, value);
            return Task::none();
        }
        let commands = self.active_commands.clone();
        Task::perform(
            async move {
                let guard = commands.lock().await;
                match guard.as_ref() {
                    Some(tx) => tx.send((id, vec![value])).is_ok(),
                    None => false,
                }
            },
            Message::CommandSent,
        )
    }

    /// Demo mode: reflect controls straight into the hub so the UI reacts like the real thing.
    fn apply_demo(&self, id: ControlCommandIdentifiers, value: u8) {
        let Some(d) = self.primary() else { return };
        let (mac, name) = (d.mac.clone(), d.name.clone());
        self.hub.update_device(&mac, &name, |d| match id {
            ControlCommandIdentifiers::ListeningMode => d.listening_mode = Some(value),
            ControlCommandIdentifiers::ConversationDetectConfig => {
                d.conversation_awareness = Some(value == 0x01)
            }
            ControlCommandIdentifiers::AllowOffOption => d.allow_off_mode = Some(value == 0x01),
            ControlCommandIdentifiers::OwnsConnection => d.owns_audio = Some(value == 0x01),
            _ => {}
        });
    }

    fn save_settings(&mut self, f: impl FnOnce(&mut Settings)) -> Task<Message> {
        self.store.update(f);
        self.settings = self.store.get();
        let store = self.store.clone();
        Task::perform(store.persist(), |_| Message::Ignore)
    }

    /// Slider drag: update the overlay live without touching the disk (saved on release).
    fn live_overlay(&mut self, f: impl FnOnce(&mut Settings)) {
        self.store.update(f);
        self.settings = self.store.get();
        let s = &self.settings;
        crate::overlay_feed::apply_config(crate::overlay_protocol::OverlayConfig {
            mode: s.overlay_mode,
            position: s.overlay_position,
            opacity: s.overlay_opacity,
            glass: s.overlay_glass,
            ..Default::default()
        });
    }

    /// Save overlay settings, then push the new config to the running overlay.
    fn save_overlay(&mut self, f: impl FnOnce(&mut Settings)) -> Task<Message> {
        self.store.update(f);
        self.settings = self.store.get();
        let store = self.store.clone();
        Task::perform(
            async move {
                store.persist().await;
                // load_config reads the file we just wrote.
                if let Ok(cfg) = tokio::task::spawn_blocking(crate::overlay_feed::load_config).await
                {
                    crate::overlay_feed::apply_config(cfg);
                }
            },
            |_| Message::Ignore,
        )
    }

    fn expire_pending(&mut self) {
        let mode = self.primary().and_then(|d| d.listening_mode);
        let ca = self.primary().and_then(|d| d.conversation_awareness);
        if let Some((m, at)) = self.pending_mode
            && (Some(m) == mode || at.elapsed() > PENDING_FOR)
        {
            self.pending_mode = None;
        }
        if let Some((v, at)) = self.pending_ca
            && (Some(v) == ca || at.elapsed() > PENDING_FOR)
        {
            self.pending_ca = None;
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Hub(snap) => {
                self.snap = snap;
                self.now = SystemTime::now();
                self.expire_pending();
                Task::none()
            }
            Message::Legacy(msg) => self.on_legacy(msg),
            Message::WindowOpened(id) => {
                self.window = Some(id);
                Task::none()
            }
            Message::WindowClosed(id) => {
                // Closing the window keeps BazzitePods running in the tray.
                if self.window == Some(id) {
                    self.window = None;
                }
                Task::none()
            }
            Message::SystemTheme(mode) => {
                self.system_dark = match mode {
                    iced::theme::Mode::Dark => Some(true),
                    iced::theme::Mode::Light => Some(false),
                    iced::theme::Mode::None => None,
                };
                Task::none()
            }
            Message::Tick => {
                self.now = SystemTime::now();
                self.expire_pending();
                Task::none()
            }
            Message::SetMode(mode) => {
                self.pending_mode = Some((mode.byte(), Instant::now()));
                self.send(ControlCommandIdentifiers::ListeningMode, mode.byte())
            }
            Message::SetConversation(on) => {
                self.pending_ca = Some((on, Instant::now()));
                self.send(
                    ControlCommandIdentifiers::ConversationDetectConfig,
                    if on { 0x01 } else { 0x02 },
                )
            }
            Message::SetAllowOff(on) => self.send(
                ControlCommandIdentifiers::AllowOffOption,
                if on { 0x01 } else { 0x02 },
            ),
            Message::SetAdaptiveVolume(on) => {
                if let Some(mac) = self.primary().map(|d| d.mac.clone()) {
                    self.adaptive_volume.insert(mac, on);
                }
                self.send(
                    ControlCommandIdentifiers::AdaptiveVolumeConfig,
                    if on { 0x01 } else { 0x02 },
                )
            }
            Message::SetOverlayMode(m) => self.save_overlay(|s| s.overlay_mode = m),
            Message::SetOverlayPosition(p) => self.save_overlay(|s| s.overlay_position = p),
            Message::SetOverlayOpacity(v) => {
                self.live_overlay(|s| s.overlay_opacity = v);
                crate::overlay_feed::preview_opacity();
                Task::none()
            }
            Message::SetOverlayGlass(v) => {
                self.live_overlay(|s| s.overlay_glass = v);
                crate::overlay_feed::preview();
                Task::none()
            }
            Message::CommitOverlaySliders => self.save_overlay(|_| {}),
            Message::PreviewOverlay => {
                crate::overlay_feed::preview();
                Task::none()
            }
            Message::SetStemControl(on) => self.save_settings(|s| s.stem_control = on),
            Message::SetAppearance(a) => self.save_settings(|s| s.appearance = a),
            Message::SetStartAtLogin(on) => {
                let save = self.save_settings(|s| s.start_at_login = on);
                if self.demo {
                    return save;
                }
                Task::batch([
                    save,
                    Task::perform(
                        crate::autostart::ensure_autostart(on),
                        Message::AutostartDone,
                    ),
                ])
            }
            Message::AutostartDone(res) => {
                if let Err(e) = res {
                    warn!("Could not update login autostart: {e}");
                }
                Task::none()
            }
            Message::ToggleMore => {
                self.more_open = !self.more_open;
                self.apple_copied = false;
                if self.more_open && !self.demo {
                    load_apple_mode()
                } else {
                    Task::none()
                }
            }
            Message::AppleMode(on) => {
                self.apple_mode = on;
                Task::none()
            }
            Message::TakeAudio => {
                self.taking_audio = Some(Instant::now());
                self.send(ControlCommandIdentifiers::OwnsConnection, 0x01)
            }
            Message::CopyAppleCommand => {
                self.apple_copied = true;
                let cmd = if self.apple_mode == Some(true) {
                    APPLE_MODE_OFF
                } else {
                    APPLE_MODE_ON
                };
                iced::clipboard::write(cmd.to_string())
            }
            Message::RenameInput(s) => {
                self.rename = s.chars().take(32).collect();
                self.rename_status = None;
                Task::none()
            }
            Message::RenameSubmit => {
                let name = self.rename.trim().to_string();
                let Some(mac) = self.primary().map(|d| d.mac.clone()) else {
                    return Task::none();
                };
                if name.is_empty() {
                    return Task::none();
                }
                if self.demo {
                    let hub = self.hub.clone();
                    hub.update_device(&mac, &name, |d| d.name = name.clone());
                    return Task::done(Message::RenameDone(Ok(())));
                }
                let managers = self.device_managers.clone();
                Task::perform(
                    async move {
                        let aacp = managers.read().await.get(&mac).and_then(|m| m.get_aacp());
                        match aacp {
                            Some(a) => a.send_rename_packet(&name).await.map_err(|e| e.to_string()),
                            None => Err("AirPods are not connected".to_string()),
                        }
                    },
                    Message::RenameDone,
                )
            }
            Message::RenameDone(res) => {
                self.rename_status = Some(match res {
                    Ok(()) => {
                        self.rename.clear();
                        "Renamed. It may take a moment to show everywhere.".to_string()
                    }
                    Err(e) => format!("Couldn't rename: {e}"),
                });
                Task::none()
            }
            Message::CommandSent(ok) => {
                if !ok {
                    debug!("No active control channel; dropping command");
                    self.pending_mode = None;
                    self.pending_ca = None;
                }
                Task::none()
            }
            Message::NothingConnected(Some(mac)) => {
                self.nothing.entry(mac).or_insert(None);
                Task::none()
            }
            Message::NothingConnected(None) | Message::Ignore => Task::none(),
            Message::SavedDevices(map) => {
                self.saved = map;
                Task::none()
            }
            Message::SetNothingMode(mac, mode) => {
                self.nothing.insert(mac.clone(), Some(mode));
                let managers = self.device_managers.clone();
                Task::perform(
                    async move {
                        let att = managers.read().await.get(&mac).and_then(|m| m.get_att());
                        let Some(att) = att else { return };
                        let packet = [
                            0x55,
                            0x60,
                            0x01,
                            0x0F,
                            0xF0,
                            0x03,
                            0x00,
                            0x00,
                            0x01,
                            mode.byte(),
                            0x00,
                            0x00,
                            0x00,
                        ];
                        if let Err(e) = att.write(ATTHandles::NothingEverything, &packet).await {
                            warn!("Failed to set Nothing noise control on {mac}: {e}");
                        }
                    },
                    |_| Message::Ignore,
                )
            }
        }
    }

    fn on_legacy(&mut self, msg: BluetoothUIMessage) -> Task<Message> {
        match msg {
            BluetoothUIMessage::OpenWindow => {
                info!("Opening main window");
                self.open_window()
            }
            BluetoothUIMessage::DeviceConnected(mac) => {
                let managers = self.device_managers.clone();
                let nothing = Task::perform(
                    async move {
                        let guard = managers.read().await;
                        let m = guard.get(&mac)?;
                        (m.get_att().is_some() && m.get_aacp().is_none()).then_some(mac)
                    },
                    Message::NothingConnected,
                );
                Task::batch([nothing, load_saved_devices()])
            }
            BluetoothUIMessage::DeviceDisconnected(mac) => {
                self.nothing.remove(&mac);
                Task::none()
            }
            BluetoothUIMessage::AACPUIEvent(mac, AACPEvent::ControlCommand(status))
                if status.identifier == ControlCommandIdentifiers::AdaptiveVolumeConfig =>
            {
                if let Some(&b) = status.value.first() {
                    self.adaptive_volume.insert(mac, b == 0x01);
                }
                Task::none()
            }
            _ => Task::none(),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            Subscription::run_with(HubKey(self.hub.clone()), hub_stream).map(Message::Hub),
            Subscription::run_with(self.ui_rx.clone(), legacy_stream).map(Message::Legacy),
            window::close_events().map(Message::WindowClosed),
            iced::system::theme_changes().map(Message::SystemTheme),
            iced::time::every(Duration::from_secs(20)).map(|_| Message::Tick),
        ])
    }

    fn theme(&self, _id: window::Id) -> Theme {
        let dark = match self.settings.appearance {
            Appearance::System => self.system_dark.unwrap_or(true),
            Appearance::Light => false,
            Appearance::Dark => true,
        };
        if dark {
            self.dark_theme.clone()
        } else {
            self.light_theme.clone()
        }
    }

    // ---------------------------------------------------------------- view

    fn view(&self, _id: window::Id) -> Element<'_, Message> {
        let mut page = column![].spacing(14);

        match (self.snap.adapter, self.primary()) {
            (AdapterState::Off, _) => {
                page = page.push(empty_state(
                    Glyph::Bluetooth,
                    "Bluetooth is off",
                    "Turn on Bluetooth from the system tray or System Settings › Bluetooth. \
                     BazzitePods reconnects on its own.",
                ));
            }
            (AdapterState::Missing, None) => {
                page = page.push(if self.started.elapsed() < ADAPTER_GRACE {
                    empty_state(Glyph::Bluetooth, "Looking for Bluetooth…", "One moment.")
                } else {
                    empty_state(
                        Glyph::Bluetooth,
                        "Bluetooth unavailable",
                        "No Bluetooth adapter was found. Check that Bluetooth is enabled \
                         in System Settings and the adapter is plugged in.",
                    )
                });
            }
            (_, None) => {
                page = page.push(empty_state(
                    Glyph::Earbuds,
                    "No AirPods yet",
                    "Pair your AirPods in System Settings › Bluetooth. \
                     BazzitePods finds them automatically — nothing to set up here.",
                ));
            }
            (_, Some(d)) => {
                page = page.push(self.hero(d));
                if let Some(controls) = self.controls(d) {
                    page = page.push(controls);
                }
            }
        }

        page = page.push(self.settings_card());
        page = page.push(self.more_toggle());
        if self.more_open {
            page = page.push(self.more_card());
        }

        let body = scrollable(container(page).padding(Padding {
            top: 20.0,
            bottom: 24.0,
            left: 20.0,
            right: 20.0,
        }))
        .direction(scrollable::Direction::Vertical(
            scrollable::Scrollbar::new()
                .width(5)
                .scroller_width(5)
                .margin(3),
        ))
        .style(style::scroll)
        .height(Fill);

        container(body)
            .width(Fill)
            .height(Fill)
            .style(style::page)
            .into()
    }

    fn hero<'a>(&self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let (label, tone) = format::connection_label(d);
        let mut subtitle = format::model_name(d.model).to_string();
        if let (true, Some(audio)) = (d.is_connected(), d.audio.as_deref()) {
            subtitle.push_str(" · ");
            subtitle.push_str(audio);
        } else if let (false, Some(seen)) = (d.is_connected(), d.last_seen) {
            subtitle.push_str(" · ");
            subtitle.push_str(&format::last_seen_text(seen, self.now));
        }
        if d.is_connected() && !d.in_use_elsewhere() && d.other_devices > 0 {
            subtitle.push_str(&match d.other_devices {
                1 => " · Also on 1 other device".to_string(),
                n => format!(" · Also on {n} other devices"),
            });
        }

        let header = row![
            column![
                text(d.name.clone())
                    .size(21)
                    .font(style::INTER_SEMIBOLD)
                    .wrapping(text::Wrapping::WordOrGlyph),
                text(subtitle).size(13).style(dim_text),
            ]
            .spacing(3)
            .width(Fill),
            status_pill(label, tone),
        ]
        .align_y(Center)
        .spacing(12);

        let gauges = format::gauges(d, self.now);
        let size = if gauges.len() == 1 { 116.0 } else { 86.0 };
        let mut ring_row = row![].spacing(8).align_y(iced::Alignment::Start);
        for g in &gauges {
            ring_row = ring_row.push(gauge_cell(g, size));
        }

        let mut body = column![header].spacing(22);
        if d.in_use_elsewhere() {
            body = body.push(self.elsewhere_banner(d.elsewhere_name.as_deref()));
        }
        container(body.push(ring_row))
            .padding(Padding {
                top: 20.0,
                bottom: 20.0,
                left: 20.0,
                right: 20.0,
            })
            .width(Fill)
            .style(style::hero)
            .into()
    }

    /// Shown while another Apple device has the audio: the AirPods are still connected
    /// here, and one press brings the sound back to this PC.
    fn elsewhere_banner<'a>(&self, device: Option<&str>) -> Element<'a, Message> {
        let headline = match device {
            Some(name) => format!("Playing on {name}"),
            None => "Playing on another device".to_string(),
        };
        let switching = self
            .taking_audio
            .is_some_and(|t| t.elapsed() < Duration::from_secs(6));
        let label = if switching {
            "Switching…"
        } else {
            "Use on this PC"
        };
        let mut take = button(text(label).size(12).font(style::INTER_MEDIUM))
            .padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 12.0,
                right: 12.0,
            })
            .style(style::segment(true));
        if !switching {
            take = take.on_press(Message::TakeAudio);
        }
        container(
            row![
                column![
                    text(headline).size(13).font(style::INTER_SEMIBOLD),
                    text("Still connected here. Starting playback also switches back.")
                        .size(12)
                        .style(dim_text),
                ]
                .spacing(2)
                .width(Fill),
                take,
            ]
            .align_y(Center)
            .spacing(10),
        )
        .padding(12)
        .width(Fill)
        .style(style::segment_track)
        .into()
    }

    fn controls<'a>(&self, d: &'a DeviceSnapshot) -> Option<Element<'a, Message>> {
        if !d.is_connected() {
            return None;
        }
        let modes = format::listening_modes(d);
        let show_ca = format::shows_conversation_awareness(d);
        if modes.is_empty() && !show_ca {
            return None;
        }
        let enabled = format::controls_enabled(d);
        let current = self
            .pending_mode
            .map(|(m, _)| m)
            .or(d.listening_mode)
            .and_then(ListeningMode::from_byte);

        let mut col = column![].spacing(14);
        let has_modes = !modes.is_empty();
        if has_modes {
            let mut tiles = row![].spacing(8);
            for m in modes {
                tiles = tiles.push(mode_tile(m, current == Some(m), enabled));
            }
            col = col.push(column![section_label("Listening mode"), tiles].spacing(10));
        }
        if show_ca {
            if has_modes {
                col = col.push(divider());
            }
            let on = self
                .pending_ca
                .map(|(v, _)| v)
                .or(d.conversation_awareness)
                .unwrap_or(false);
            col = col.push(setting_row(
                "Conversation awareness",
                Some("Lowers media and lets voices in when you speak"),
                switch(
                    on,
                    enabled.then_some(Message::SetConversation as fn(bool) -> Message),
                ),
            ));
        }
        if !enabled {
            col = col.push(
                text("Controls return once the connection is back.")
                    .size(12)
                    .style(faint_text),
            );
        }
        Some(card(col.into()))
    }

    fn settings_card(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let overlay_on = s.overlay_mode != OverlayMode::Off;
        let mode = segmented_wide(
            &[
                ("Pop-up", OverlayMode::Auto),
                ("Compact", OverlayMode::Compact),
                ("Full", OverlayMode::Always),
                ("Off", OverlayMode::Off),
            ],
            s.overlay_mode,
            true,
            Message::SetOverlayMode,
        );
        let col = column![
            setting_block(
                "Overlay in games",
                Some(match s.overlay_mode {
                    OverlayMode::Auto => "Island opens on changes, then hides",
                    OverlayMode::Always => "Full island always on screen",
                    OverlayMode::Compact => "Slim island, opens on changes",
                    OverlayMode::Off => "Never shown",
                }),
                mode,
            ),
            divider(),
            setting_row(
                "Overlay position",
                None,
                row![
                    button(text("Preview").size(12).font(style::INTER_MEDIUM))
                        .padding(Padding {
                            top: 6.0,
                            bottom: 6.0,
                            left: 12.0,
                            right: 12.0,
                        })
                        .style(style::secondary)
                        .on_press_maybe(overlay_on.then_some(Message::PreviewOverlay)),
                    corner_picker(s.overlay_position, overlay_on),
                ]
                .spacing(10)
                .align_y(Center)
                .into(),
            ),
            divider(),
            setting_row(
                "Glass",
                Some("Island black to clear glass"),
                row![
                    slider(0.0..=1.0, s.overlay_glass, Message::SetOverlayGlass)
                        .step(0.05_f32)
                        .on_release(Message::CommitOverlaySliders)
                        .width(Length::Fixed(120.0)),
                    text(format!("{}%", (s.overlay_glass * 100.0).round() as u32))
                        .size(12)
                        .font(style::INTER_MEDIUM)
                        .width(Length::Fixed(36.0)),
                ]
                .spacing(10)
                .align_y(Center)
                .into(),
            ),
            divider(),
            setting_row(
                "Game opacity",
                Some("Over fullscreen games and video"),
                row![
                    slider(0.2..=1.0, s.overlay_opacity, Message::SetOverlayOpacity)
                        .step(0.05_f32)
                        .on_release(Message::CommitOverlaySliders)
                        .width(Length::Fixed(120.0)),
                    text(format!("{}%", (s.overlay_opacity * 100.0).round() as u32))
                        .size(12)
                        .font(style::INTER_MEDIUM)
                        .width(Length::Fixed(36.0)),
                ]
                .spacing(10)
                .align_y(Center)
                .into(),
            ),
            divider(),
            setting_row(
                "Start at login",
                Some("Runs quietly in the tray"),
                switch(
                    s.start_at_login,
                    Some(Message::SetStartAtLogin as fn(bool) -> Message)
                ),
            ),
        ]
        .spacing(14);
        card(col.into())
    }

    fn more_toggle(&self) -> Element<'_, Message> {
        button(
            row![
                text(if self.more_open {
                    "Fewer options"
                } else {
                    "More options"
                })
                .size(13)
                .font(style::INTER_MEDIUM),
                widgets::icon(Glyph::Chevron { up: self.more_open }, 14.0, Ink::Dim),
            ]
            .spacing(6)
            .align_y(Center),
        )
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 12.0,
            right: 10.0,
        })
        .style(style::link)
        .on_press(Message::ToggleMore)
        .into()
    }

    /// Opt-in Apple DeviceID ("VendorID spoofing"). The sandboxed app can't edit
    /// /etc/bluetooth/main.conf, so it hands the user a one-line command instead.
    fn apple_mode_row(&self) -> Element<'_, Message> {
        let (state, action) = match self.apple_mode {
            Some(true) => ("On", "Copy command to turn off"),
            Some(false) => ("Off", "Copy command to turn on"),
            None => ("Unknown", "Copy command to turn on"),
        };
        let button_label = if self.apple_copied {
            "Copied. Paste it in a terminal"
        } else {
            action
        };
        setting_block(
            "Apple device mode",
            Some(
                "Makes this PC introduce itself as an Apple device, so AirPods can stay paired \
                 with your iPhone and this PC and switch between them. Restarts Bluetooth \
                 for a few seconds; needs your password once.",
            ),
            row![
                text(state).size(12).font(style::INTER_SEMIBOLD).width(Fill),
                button(text(button_label).size(12).font(style::INTER_MEDIUM))
                    .padding(Padding {
                        top: 6.0,
                        bottom: 6.0,
                        left: 12.0,
                        right: 12.0,
                    })
                    .style(style::secondary)
                    .on_press(Message::CopyAppleCommand),
            ]
            .align_y(Center)
            .into(),
        )
    }

    fn more_card(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let mut col = column![setting_row(
            "Appearance",
            None,
            segmented(
                &[
                    ("System", Appearance::System),
                    ("Light", Appearance::Light),
                    ("Dark", Appearance::Dark),
                ],
                s.appearance,
                true,
                Message::SetAppearance,
            ),
        )]
        .spacing(14);

        col = col.push(divider()).push(self.apple_mode_row());
        col = col.push(divider()).push(setting_row(
            "Stem press track control",
            Some("Double-press for next track, triple-press for previous. Applies on reconnect."),
            switch(
                s.stem_control,
                Some(Message::SetStemControl as fn(bool) -> Message),
            ),
        ));

        if let Some(d) = self.primary() {
            let info = d.model.info();
            let enabled = format::controls_enabled(d);
            if info.has_anc {
                col = col.push(divider()).push(setting_row(
                    "Off listening mode",
                    Some("Adds Off to the listening modes"),
                    switch(
                        d.allow_off_mode.unwrap_or(false),
                        enabled.then_some(Message::SetAllowOff as fn(bool) -> Message),
                    ),
                ));
            }
            if info.has_adaptive_volume {
                col = col.push(divider()).push(setting_row(
                    "Personalized volume",
                    Some("Adjusts volume to your surroundings"),
                    switch(
                        self.adaptive_volume.get(&d.mac).copied().unwrap_or(false),
                        enabled.then_some(Message::SetAdaptiveVolume as fn(bool) -> Message),
                    ),
                ));
            }

            let can_rename = enabled && !self.rename.trim().is_empty();
            let mut rename_col = column![
                text("Name").size(14).font(style::INTER_MEDIUM),
                row![
                    text_input(&d.name, &self.rename)
                        .on_input(Message::RenameInput)
                        .on_submit_maybe(can_rename.then_some(Message::RenameSubmit))
                        .padding(Padding {
                            top: 8.0,
                            bottom: 8.0,
                            left: 12.0,
                            right: 12.0,
                        })
                        .size(14)
                        .style(style::input)
                        .width(Fill),
                    button(text("Rename").size(13).font(style::INTER_MEDIUM))
                        .padding(Padding {
                            top: 8.0,
                            bottom: 8.0,
                            left: 14.0,
                            right: 14.0,
                        })
                        .style(style::secondary)
                        .on_press_maybe(can_rename.then_some(Message::RenameSubmit)),
                ]
                .spacing(8)
                .align_y(Center),
            ]
            .spacing(8);
            if let Some(status) = &self.rename_status {
                rename_col = rename_col.push(text(status.clone()).size(12).style(dim_text));
            }
            col = col.push(divider()).push(rename_col);

            let mut info_col = column![section_label("About")].spacing(6);
            info_col = info_col.push(kv("Model", format::model_name(d.model).to_string()));
            if let Some(n) = &d.model_number {
                info_col = info_col.push(kv("Model number", n.clone()));
            }
            if let Some(DeviceInformation::AirPods(i)) =
                self.saved.get(&d.mac).and_then(|s| s.information.as_ref())
            {
                if !i.serial_number.is_empty() {
                    info_col = info_col.push(kv("Serial number", i.serial_number.clone()));
                }
                if !i.version1.is_empty() {
                    info_col = info_col.push(kv("Firmware", i.version1.clone()));
                }
            }
            info_col = info_col.push(kv("Address", d.mac.clone()));
            if let Some(a) = &d.audio {
                info_col = info_col.push(kv("Audio", a.clone()));
            }
            col = col.push(divider()).push(info_col);
        }

        for (mac, mode) in &self.nothing {
            let mac_c = mac.clone();
            col = col.push(divider()).push(
                column![
                    text("Nothing Ear").size(14).font(style::INTER_MEDIUM),
                    text(mac.clone()).size(12).style(faint_text),
                    pick_list(NothingMode::ALL, *mode, move |m| {
                        Message::SetNothingMode(mac_c.clone(), m)
                    })
                    .placeholder("Noise control")
                    .text_size(13)
                    .width(Fill),
                ]
                .spacing(6),
            );
        }

        card(col.into())
    }
}

// ------------------------------------------------------------------ helpers

fn load_saved_devices() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                std::fs::read_to_string(crate::utils::get_devices_path())
                    .ok()
                    .and_then(|s| serde_json::from_str::<HashMap<String, DeviceData>>(&s).ok())
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default()
        },
        Message::SavedDevices,
    )
}

/// Adds Apple's Bluetooth vendor id (0x004C) to BlueZ, keeping a backup of the config.
const APPLE_MODE_ON: &str = "sudo cp -a /etc/bluetooth/main.conf /etc/bluetooth/main.conf.bak-bazzitepods && \
sudo sed -i '/^\\[General\\]/a DeviceID = bluetooth:004C:0000:0000' /etc/bluetooth/main.conf && \
sudo systemctl restart bluetooth";
const APPLE_MODE_OFF: &str = "sudo sed -i '/^DeviceID = bluetooth:004C:/d' /etc/bluetooth/main.conf && \
sudo systemctl restart bluetooth";

/// Reads the adapter's modalias: BlueZ reports the configured DeviceID there.
fn load_apple_mode() -> Task<Message> {
    Task::perform(
        async {
            let session = bluer::Session::new().await.ok()?;
            let adapter = session.default_adapter().await.ok()?;
            let m = adapter.modalias().await.ok()??;
            Some(m.vendor == 0x004C)
        },
        Message::AppleMode,
    )
}

fn dim_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Tokens::of(theme).text_dim),
    }
}

fn faint_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Tokens::of(theme).text_faint),
    }
}

fn card(content: Element<'_, Message>) -> Element<'_, Message> {
    container(content)
        .padding(18)
        .width(Fill)
        .style(style::card)
        .into()
}

fn divider<'a>() -> Element<'a, Message> {
    container(Space::new())
        .width(Fill)
        .height(1)
        .style(style::divider)
        .into()
}

fn section_label<'a>(label: &'a str) -> Element<'a, Message> {
    text(label)
        .size(12)
        .font(style::INTER_SEMIBOLD)
        .style(dim_text)
        .into()
}

fn kv<'a>(k: &'a str, v: String) -> Element<'a, Message> {
    row![
        text(k).size(13).style(dim_text).width(Fill),
        text(v).size(13),
    ]
    .spacing(12)
    .into()
}

fn status_pill<'a>(label: &'a str, tone: PillTone) -> Element<'a, Message> {
    container(
        row![
            container(Space::new())
                .width(7)
                .height(7)
                .style(style::dot(tone)),
            text(label).size(12).font(style::INTER_MEDIUM),
        ]
        .spacing(6)
        .align_y(Center),
    )
    .padding(Padding {
        top: 5.0,
        bottom: 5.0,
        left: 10.0,
        right: 11.0,
    })
    .style(style::pill(tone))
    .into()
}

fn gauge_cell<'a>(g: &Gauge, size: f32) -> Element<'a, Message> {
    container(widgets::gauge_view(g, size))
        .center_x(Fill)
        .into()
}

fn mode_tile<'a>(m: ListeningMode, selected: bool, enabled: bool) -> Element<'a, Message> {
    let ink = if selected { Ink::OnAccent } else { Ink::Text };
    let content = column![
        widgets::icon(Glyph::Mode(m), 22.0, ink),
        text(m.label())
            .size(11.5)
            .font(style::INTER_MEDIUM)
            .align_x(Center)
            .wrapping(text::Wrapping::Word),
    ]
    .spacing(7)
    .align_x(Center)
    .width(Fill);
    button(content)
        .width(Fill)
        .height(Length::Fixed(78.0))
        .padding(Padding {
            top: 12.0,
            bottom: 8.0,
            left: 4.0,
            right: 4.0,
        })
        .style(style::tile(selected))
        .on_press_maybe(enabled.then_some(Message::SetMode(m)))
        .into()
}

/// A setting whose control needs the full width: title and caption above it.
fn setting_block<'a>(
    title: &'a str,
    caption: Option<&'a str>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut col = column![text(title).size(14).font(style::INTER_MEDIUM)].spacing(2);
    if let Some(c) = caption {
        col = col.push(text(c).size(12).style(dim_text));
    }
    column![col, control].spacing(10).into()
}

fn setting_row<'a>(
    title: &'a str,
    caption: Option<&'a str>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut left = column![text(title).size(14).font(style::INTER_MEDIUM)].spacing(2);
    if let Some(c) = caption {
        left = left.push(text(c).size(12).style(dim_text));
    }
    row![left.width(Fill), control]
        .spacing(12)
        .align_y(Center)
        .into()
}

fn switch<'a>(on: bool, msg: Option<fn(bool) -> Message>) -> Element<'a, Message> {
    toggler(on)
        .on_toggle_maybe(msg)
        .size(22)
        .spacing(0)
        .style(style::toggle)
        .into()
}

fn segmented<'a, T: Copy + PartialEq + 'a>(
    options: &[(&'a str, T)],
    selected: T,
    enabled: bool,
    msg: fn(T) -> Message,
) -> Element<'a, Message> {
    segmented_impl(options, selected, enabled, msg, false)
}

/// Like [`segmented`], stretched to the full width with equal segments.
fn segmented_wide<'a, T: Copy + PartialEq + 'a>(
    options: &[(&'a str, T)],
    selected: T,
    enabled: bool,
    msg: fn(T) -> Message,
) -> Element<'a, Message> {
    segmented_impl(options, selected, enabled, msg, true)
}

fn segmented_impl<'a, T: Copy + PartialEq + 'a>(
    options: &[(&'a str, T)],
    selected: T,
    enabled: bool,
    msg: fn(T) -> Message,
    wide: bool,
) -> Element<'a, Message> {
    let mut r = row![].spacing(2);
    for &(label, value) in options {
        let is_sel = value == selected;
        let label = text(label).size(12).font(if is_sel {
            style::INTER_SEMIBOLD
        } else {
            style::INTER_MEDIUM
        });
        let label = if wide {
            label.width(Fill).align_x(Center)
        } else {
            label
        };
        r = r.push(
            button(label)
                .width(if wide { Fill } else { Length::Shrink })
                .padding(Padding {
                    top: 5.0,
                    bottom: 5.0,
                    left: 10.0,
                    right: 10.0,
                })
                .style(style::segment(is_sel))
                .on_press_maybe(enabled.then_some(msg(value))),
        );
    }
    let track = container(r).padding(2).style(style::segment_track);
    if wide {
        track.width(Fill).into()
    } else {
        track.into()
    }
}

fn corner_picker<'a>(selected: OverlayPosition, enabled: bool) -> Element<'a, Message> {
    let spot = |p: OverlayPosition| -> Element<'a, Message> {
        button(Space::new())
            .width(14)
            .height(9)
            .padding(0)
            .style(style::corner(p == selected))
            .on_press_maybe(enabled.then_some(Message::SetOverlayPosition(p)))
            .into()
    };
    let [tl, tc, tr, bl, bc, br] = OVERLAY_POSITIONS;
    let line = |a, b, c| {
        row![
            spot(a),
            Space::new().width(Fill),
            spot(b),
            Space::new().width(Fill),
            spot(c)
        ]
        .align_y(Center)
    };
    container(
        column![
            line(tl, tc, tr),
            Space::new().height(Fill),
            line(bl, bc, br)
        ]
        .width(Fill)
        .height(Fill),
    )
    .width(76)
    .height(48)
    .padding(4)
    .style(style::screen(enabled))
    .into()
}

fn empty_state<'a>(glyph: Glyph, title: &'a str, body: &'a str) -> Element<'a, Message> {
    let disc = container(widgets::icon(glyph, 34.0, Ink::Dim))
        .center_x(Length::Fixed(72.0))
        .center_y(Length::Fixed(72.0))
        .style(style::icon_disc);
    container(
        column![
            disc,
            text(title).size(18).font(style::INTER_SEMIBOLD),
            text(body)
                .size(13)
                .align_x(Center)
                .style(dim_text)
                .width(Length::Fixed(300.0)),
        ]
        .spacing(10)
        .align_x(Center),
    )
    .padding(Padding {
        top: 34.0,
        bottom: 30.0,
        left: 20.0,
        right: 20.0,
    })
    .center_x(Fill)
    .style(style::hero)
    .into()
}

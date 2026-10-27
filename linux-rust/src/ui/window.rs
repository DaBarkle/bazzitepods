//! Main window: three calm tabs (AirPods, Island, Settings) driven entirely by the hub
//! snapshot, in the island's design language. Selections and battery rings move on springs
//! ([`crate::ui::motion`]); frames are only requested while something is moving.
//!
//! Rules: `view` and `update` never block, never touch the disk, never panic. Device commands go
//! through `active_commands` from an async task; settings are written by a background task.

use crate::bluetooth::aacp::ControlCommandIdentifiers;
use crate::bluetooth::att::ATTHandles;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::CommandSender;
use crate::devices::enums::{DeviceData, DeviceInformation};
use crate::hub::{AdapterState, AppSnapshot, DeviceSnapshot, Hub};
use crate::ui::format::{self, DeviceSetting, ListeningMode, StatusTone};
use crate::ui::messages::BluetoothUIMessage;
use crate::ui::motion::{self, Motion};
use crate::ui::settings::{
    Appearance, OVERLAY_POSITIONS, OverlayMode, OverlayPosition, OverlaySize, OverlayStyle,
    Settings, SettingsStore,
};
use crate::ui::style::{self, Tokens};
use crate::ui::widgets::{self, Glyph, Ink};
use futures::Stream;
use iced::widget::{
    Space, button, column, container, pick_list, row, scrollable, slider, stack, text, text_input,
    toggler,
};
use iced::{
    Center, Color, Element, Fill, FillPortion, Length, Padding, Size, Subscription, Task, Theme,
    window,
};
use log::{debug, info, warn};
use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::{Mutex, RwLock};

const WINDOW_SIZE: Size = Size::new(420.0, 640.0);
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
    /// An AirPods setting: (AACP control command identifier, value byte).
    SetControl(u8, u8),
    SetOverlayMode(OverlayMode),
    PreviewOverlay,
    SetOverlayPosition(OverlayPosition),
    SetOverlayOpacity(f32),
    SetOverlayGlass(f32),
    SetOverlayStyle(OverlayStyle),
    SetOverlaySize(OverlaySize),
    CommitOverlaySliders,
    SetStartAtLogin(bool),
    SetStemControl(bool),
    SetMicSwitch(bool),
    /// The "Use AirPods mic" switch: it always toggles, whatever the switch shows.
    ToggleAirPodsMic,
    SetAppearance(Appearance),
    SelectTab(Tab),
    /// A display frame while something is animating.
    Frame(Instant),
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

/// The window's three pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    AirPods,
    Island,
    Settings,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::AirPods, Tab::Island, Tab::Settings];

    fn label(self) -> &'static str {
        match self {
            Tab::AirPods => "AirPods",
            Tab::Island => "Island",
            Tab::Settings => "Settings",
        }
    }

    fn from_name(name: &str) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(name))
    }
}

/// What a spring moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Anim {
    /// Tab thumb position (tab index).
    Tab,
    /// Listening mode highlight position (index among the offered modes).
    Mode,
    Appearance,
    OverlayMode,
    OverlayStyle,
    OverlaySize,
    /// A segmented AirPods setting, by AACP control id.
    Control(u8),
    /// Battery ring fill, by gauge index.
    Ring(usize),
    /// Status island edge light.
    Glow,
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

    tab: Tab,
    motion: Motion<Anim>,
    /// Animation clock: now, as of the last update or frame.
    clock: Instant,
    /// Last status island text and tone, to pulse its glow when it changes.
    last_status: Option<(String, StatusTone)>,
    pending_mode: Option<(u8, Instant)>,
    pending_ca: Option<(bool, Instant)>,
    /// "Use on this PC" was pressed; shows "Switching…" until the AirPods answer.
    taking_audio: Option<Instant>,
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
            // Demo/screenshots only: open on another tab.
            tab: std::env::var("LIBREPODS_UI_TAB")
                .ok()
                .filter(|_| demo)
                .and_then(|t| Tab::from_name(&t))
                .unwrap_or(Tab::AirPods),
            motion: Motion::default(),
            clock: Instant::now(),
            last_status: None,
            pending_mode: None,
            pending_ca: None,
            taking_audio: None,
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
            min_size: Some(Size::new(380.0, 520.0)),
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
            other => {
                d.controls.insert(other as u8, value);
            }
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
            style: s.overlay_style,
            size: s.overlay_size,
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
        self.clock = match message {
            Message::Frame(at) => at,
            _ => Instant::now(),
        };
        let task = self.handle(message);
        self.sync_motion();
        task
    }

    /// Point every spring at what the state says now; pulse the status glow on a change.
    fn sync_motion(&mut self) {
        let now = self.clock;
        let mode = self.snap.primary().and_then(|d| self.mode_index(d));
        let m = &mut self.motion;
        let tab = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        m.target(Anim::Tab, tab as f32, None, motion::SNAPPY, now);
        let s = &self.settings;
        let style = OVERLAY_STYLES
            .iter()
            .position(|(_, v)| *v == s.overlay_style)
            .unwrap_or(0) as f32;
        let size = OVERLAY_SIZES
            .iter()
            .position(|(_, v)| *v == s.overlay_size)
            .unwrap_or(0) as f32;
        m.target(Anim::OverlayStyle, style, None, motion::SNAPPY, now);
        m.target(Anim::OverlaySize, size, None, motion::SNAPPY, now);
        m.target(
            Anim::Appearance,
            [Appearance::System, Appearance::Light, Appearance::Dark]
                .iter()
                .position(|a| *a == self.settings.appearance)
                .unwrap_or(0) as f32,
            None,
            motion::SNAPPY,
            now,
        );
        m.target(
            Anim::OverlayMode,
            OVERLAY_MODES
                .iter()
                .position(|(_, o)| *o == self.settings.overlay_mode)
                .unwrap_or(0) as f32,
            None,
            motion::SNAPPY,
            now,
        );
        let Some(d) = self.snap.primary() else {
            self.last_status = None;
            return;
        };
        if let Some(i) = mode {
            m.target(Anim::Mode, i as f32, None, motion::SNAPPY, now);
        }
        for setting in format::device_settings(d) {
            if let DeviceSetting::Choice {
                id,
                options,
                selected,
                ..
            } = setting
                && let Some(i) = options.iter().position(|(_, v)| *v == selected)
            {
                m.target(Anim::Control(id), i as f32, None, motion::SNAPPY, now);
            }
        }
        // Rings sweep in from empty the first time they are seen (window opened).
        for (i, g) in format::gauges(d, self.now).iter().enumerate() {
            let f = g.level.map_or(0.0, |l| l.min(100) as f32 / 100.0);
            m.target(Anim::Ring(i), f, Some(0.0), motion::SMOOTH, now);
        }
        let status = format::status(d, self.now);
        let lit = if status.tone == StatusTone::Idle {
            0.0
        } else {
            1.0
        };
        m.target(Anim::Glow, lit, Some(0.0), motion::SMOOTH, now);
        let key = (status.headline, status.tone);
        if self.last_status.as_ref().is_some_and(|k| *k != key) && lit > 0.0 {
            m.kick(Anim::Glow, 2.4, now);
        }
        self.last_status = Some(key);
    }

    /// Position of the current (or just requested) listening mode among those offered.
    fn mode_index(&self, d: &DeviceSnapshot) -> Option<usize> {
        let current = self
            .pending_mode
            .map(|(m, _)| m)
            .or(d.listening_mode)
            .and_then(ListeningMode::from_byte)?;
        format::listening_modes(d)
            .iter()
            .position(|m| *m == current)
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
                // Let the rings and glow play in again.
                self.motion.reset();
                self.last_status = None;
                Task::none()
            }
            Message::Frame(_) => Task::none(),
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
            Message::SetControl(id, value) => match ControlCommandIdentifiers::from_u8(id) {
                Some(id) => self.send(id, value),
                None => Task::none(),
            },
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
            Message::SetOverlayStyle(v) => {
                let task = self.save_overlay(|s| s.overlay_style = v);
                crate::overlay_feed::preview();
                task
            }
            Message::SetOverlaySize(v) => {
                let task = self.save_overlay(|s| s.overlay_size = v);
                crate::overlay_feed::preview();
                task
            }
            Message::CommitOverlaySliders => self.save_overlay(|_| {}),
            Message::PreviewOverlay => {
                crate::overlay_feed::preview();
                Task::none()
            }
            Message::SetStemControl(on) => self.save_settings(|s| s.stem_control = on),
            Message::SetMicSwitch(on) => self.save_settings(|s| s.airpods_mic_switch = on),
            Message::ToggleAirPodsMic => {
                crate::mic::request_toggle();
                Task::none()
            }
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
            Message::SelectTab(tab) => {
                if tab == self.tab {
                    return Task::none();
                }
                self.tab = tab;
                self.apple_copied = false;
                if tab == Tab::Settings && !self.demo {
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
            if self.window.is_some() && self.motion.is_animating(self.clock) {
                window::frames().map(Message::Frame)
            } else {
                Subscription::none()
            },
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
        let nav = container(self.tabs()).center_x(Fill).padding(Padding {
            top: 14.0,
            bottom: 6.0,
            left: 20.0,
            right: 20.0,
        });
        let page = match self.tab {
            Tab::AirPods => self.airpods_tab(),
            Tab::Island => self.island_tab(),
            Tab::Settings => self.settings_tab(),
        };
        let body = scrollable(container(page).padding(Padding {
            top: 12.0,
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

        container(column![nav, body])
            .width(Fill)
            .height(Fill)
            .style(style::page)
            .into()
    }

    fn anim(&self, key: Anim, fallback: f32) -> f32 {
        self.motion.value(key, fallback, self.clock)
    }

    fn tabs(&self) -> Element<'_, Message> {
        let options: Vec<(&str, Tab)> = Tab::ALL.iter().map(|t| (t.label(), *t)).collect();
        let at = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        container(slide_control(
            &options,
            self.tab,
            Some(self.anim(Anim::Tab, at as f32)),
            true,
            Message::SelectTab,
            Shape::TABS,
        ))
        .width(Length::Fixed(276.0))
        .into()
    }

    // ------------------------------------------------------------- AirPods

    fn airpods_tab(&self) -> Element<'_, Message> {
        match (self.snap.adapter, self.primary()) {
            (AdapterState::Off, _) => empty_state(
                Glyph::Bluetooth,
                "Bluetooth is off",
                "Turn on Bluetooth from the system tray or System Settings › Bluetooth. \
                 BazzitePods reconnects on its own.",
            ),
            (AdapterState::Missing, None) if self.started.elapsed() < ADAPTER_GRACE => {
                empty_state(Glyph::Bluetooth, "Looking for Bluetooth…", "One moment.")
            }
            (AdapterState::Missing, None) => empty_state(
                Glyph::Bluetooth,
                "Bluetooth unavailable",
                "No Bluetooth adapter was found. Check that Bluetooth is enabled \
                 in System Settings and the adapter is plugged in.",
            ),
            (_, None) => empty_state(
                Glyph::Earbuds,
                "No AirPods yet",
                "Pair your AirPods in System Settings › Bluetooth. \
                 BazzitePods finds them automatically — nothing to set up here.",
            ),
            (_, Some(d)) => {
                let mut page = column![self.hero(d)].spacing(0);
                if d.in_use_elsewhere() {
                    page = page.push(gap(18.0)).push(self.elsewhere_banner(d));
                }
                if let Some(controls) = self.controls(d) {
                    page = page.push(controls);
                } else if !d.is_connected() {
                    page = page.push(gap(22.0)).push(away_hint(d));
                }
                page.into()
            }
        }
    }

    fn hero<'a>(&self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let mut subtitle = format::model_name(d.model).to_string();
        if let (true, Some(audio)) = (d.is_connected(), d.audio.as_deref()) {
            subtitle.push_str(" · ");
            subtitle.push_str(audio);
        }
        if d.is_connected() && !d.in_use_elsewhere() && d.other_devices > 0 {
            subtitle.push_str(&match d.other_devices {
                1 => " · Also on 1 other device".to_string(),
                n => format!(" · Also on {n} other devices"),
            });
        }

        let gauges = format::gauges(d, self.now);
        let size = if gauges.len() == 1 { 132.0 } else { 104.0 };
        let mut rings = row![].spacing(4).align_y(iced::Alignment::Start);
        for (i, g) in gauges.iter().enumerate() {
            let fill = g.level.map(|l| l.min(100) as f32 / 100.0);
            let f = fill.map(|f| self.anim(Anim::Ring(i), f).max(0.0));
            rings =
                rings.push(container(widgets::gauge_view::<Message>(g, size, f)).center_x(Fill));
        }

        column![
            self.status_island(d),
            gap(18.0),
            text(d.name.clone())
                .size(26)
                .font(style::INTER_SEMIBOLD)
                .align_x(Center)
                .wrapping(text::Wrapping::WordOrGlyph),
            gap(4.0),
            text(subtitle).size(13).style(dim_text).align_x(Center),
            gap(20.0),
            rings,
        ]
        .align_x(Center)
        .width(Fill)
        .into()
    }

    /// The window's own little Dynamic Island: what matters right now, with an edge light
    /// that pulses when it changes.
    fn status_island<'a>(&self, d: &DeviceSnapshot) -> Element<'a, Message> {
        let s = format::status(d, self.now);
        let tone = s.tone;
        let mut line = row![
            container(Space::new())
                .width(7)
                .height(7)
                .style(style::status_dot(tone)),
            text(s.headline).size(13).font(style::INTER_SEMIBOLD),
        ]
        .spacing(8)
        .align_y(Center);
        if let Some(detail) = s.detail {
            line = line.push(
                text(format!("· {detail}"))
                    .size(13)
                    .font(style::INTER_MEDIUM)
                    .style(style::status_detail(tone)),
            );
        }
        let glow = self.anim(Anim::Glow, 1.0);
        container(line)
            .padding(Padding {
                top: 9.0,
                bottom: 9.0,
                left: 14.0,
                right: 16.0,
            })
            .style(style::status_island(tone, glow))
            .into()
    }

    /// Shown while another Apple device has the audio: the AirPods are still connected
    /// here, and one press brings the sound back to this PC.
    fn elsewhere_banner<'a>(&self, d: &DeviceSnapshot) -> Element<'a, Message> {
        let switching = self
            .taking_audio
            .is_some_and(|t| t.elapsed() < Duration::from_secs(6));
        let label = if switching {
            "Switching…"
        } else {
            "Use on this PC"
        };
        let device = d.elsewhere_name.as_deref().unwrap_or("another device");
        let take = button(text(label).size(13).font(style::INTER_SEMIBOLD))
            .padding(Padding {
                top: 7.0,
                bottom: 7.0,
                left: 14.0,
                right: 14.0,
            })
            .style(style::primary)
            .on_press_maybe((!switching).then_some(Message::TakeAudio));
        group(vec![
            row![
                column![
                    text(format!("Sound is on {device}"))
                        .size(14)
                        .font(style::INTER_MEDIUM),
                    text("Still connected here. Playing something also switches back.")
                        .size(12)
                        .style(dim_text),
                ]
                .spacing(2)
                .width(Fill),
                take,
            ]
            .align_y(Center)
            .spacing(12)
            .into(),
        ])
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

        let mut col = column![].spacing(0);
        if !modes.is_empty() {
            let selected = self.mode_index(d);
            let pos = selected.map(|i| self.anim(Anim::Mode, i as f32));
            col = col
                .push(section_label("Listening mode"))
                .push(mode_tiles(&modes, selected, pos, enabled));
        }
        if show_ca {
            let on = self
                .pending_ca
                .map(|(v, _)| v)
                .or(d.conversation_awareness)
                .unwrap_or(false);
            col = col.push(gap(10.0)).push(group(vec![setting_row(
                "Conversation awareness",
                Some("Lowers media and lets voices in when you speak"),
                switch(
                    on,
                    enabled.then_some(Message::SetConversation as fn(bool) -> Message),
                ),
            )]));
        }
        if !enabled {
            col = col.push(gap(10.0)).push(
                text("Controls return once the connection is back.")
                    .size(12)
                    .style(faint_text)
                    .width(Fill)
                    .align_x(Center),
            );
        }
        Some(col.into())
    }

    // -------------------------------------------------------------- Island

    fn island_tab(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let overlay_on = s.overlay_mode != OverlayMode::Off;
        let mode_pos = self.anim(
            Anim::OverlayMode,
            OVERLAY_MODES
                .iter()
                .position(|(_, m)| *m == s.overlay_mode)
                .unwrap_or(0) as f32,
        );

        let preview = column![
            self.island_screen(),
            row![
                text(if overlay_on {
                    "Click a spot to move the island"
                } else {
                    "The island is off"
                })
                .size(12)
                .style(dim_text)
                .width(Fill),
                button(text("Show on screen").size(13).font(style::INTER_MEDIUM))
                    .padding(Padding {
                        top: 7.0,
                        bottom: 7.0,
                        left: 14.0,
                        right: 14.0,
                    })
                    .style(style::secondary)
                    .on_press_maybe(overlay_on.then_some(Message::PreviewOverlay)),
            ]
            .align_y(Center)
            .padding(Padding {
                top: 10.0,
                left: 4.0,
                ..Padding::ZERO
            }),
        ];

        let behaviour = group(vec![setting_block(
            "In games",
            Some(match s.overlay_mode {
                OverlayMode::Auto => "Opens on changes, then hides",
                OverlayMode::Always => "Full island always on screen",
                OverlayMode::Compact => "Slim island, opens on changes",
                OverlayMode::Off => "Never shown",
            }),
            slide_control(
                &OVERLAY_MODES,
                s.overlay_mode,
                Some(mode_pos),
                true,
                Message::SetOverlayMode,
                Shape::WIDE,
            ),
        )]);

        let mut look = vec![
            setting_row(
                "Style",
                Some(match s.overlay_style {
                    OverlayStyle::Classic => "Smoked glass with a soft sheen",
                    OverlayStyle::Purist => "Flat black, light runs round the edge",
                }),
                container(slide_control(
                    &OVERLAY_STYLES,
                    s.overlay_style,
                    Some(self.anim(Anim::OverlayStyle, 0.0)),
                    overlay_on,
                    Message::SetOverlayStyle,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(160.0))
                .into(),
            ),
            setting_row(
                "Size",
                None,
                container(slide_control(
                    &OVERLAY_SIZES,
                    s.overlay_size,
                    Some(self.anim(Anim::OverlaySize, 0.0)),
                    overlay_on,
                    Message::SetOverlaySize,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(196.0))
                .into(),
            ),
        ];
        // Glass only shapes the classic island; the purist one is always solid black.
        if s.overlay_style == OverlayStyle::Classic {
            look.push(setting_row(
                "Glass",
                Some("Black to clear glass"),
                percent_slider(
                    0.0..=1.0,
                    s.overlay_glass,
                    Message::SetOverlayGlass,
                    overlay_on,
                ),
            ));
        }
        look.push(setting_row(
            "Opacity over games",
            Some("Over fullscreen games"),
            percent_slider(
                0.2..=1.0,
                s.overlay_opacity,
                Message::SetOverlayOpacity,
                overlay_on,
            ),
        ));
        let look = group(look);

        column![
            preview,
            section_label("Behaviour"),
            behaviour,
            section_label("Look"),
            look,
        ]
        .into()
    }

    /// A little screen with the island where it will appear; the other spots are buttons.
    fn island_screen(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let enabled = s.overlay_mode != OverlayMode::Off;
        let align = |p: OverlayPosition| match p {
            OverlayPosition::TopLeft | OverlayPosition::BottomLeft => {
                iced::alignment::Horizontal::Left
            }
            OverlayPosition::TopCenter | OverlayPosition::BottomCenter => {
                iced::alignment::Horizontal::Center
            }
            _ => iced::alignment::Horizontal::Right,
        };
        let spot = |p: OverlayPosition| -> Element<'_, Message> {
            let mut b = button(Space::new())
                .width(34)
                .height(11)
                .padding(0)
                .style(style::spot);
            if p != s.overlay_position {
                b = b.on_press_maybe(enabled.then_some(Message::SetOverlayPosition(p)));
            }
            container(b).width(Fill).align_x(align(p)).into()
        };
        // Each row: three spots, and over them the miniature island where it sits, free to
        // be wider than its third of the screen.
        let line = |a: OverlayPosition, b: OverlayPosition, c: OverlayPosition| {
            let mut layers = stack![row![spot(a), spot(b), spot(c)].height(Fill).align_y(Center)];
            if let Some(p) = [a, b, c].into_iter().find(|p| *p == s.overlay_position) {
                layers = layers.push(
                    container(self.mini_island(enabled))
                        .width(Fill)
                        .height(Fill)
                        .align_x(align(p))
                        .align_y(Center),
                );
            }
            layers.width(Fill).height(24)
        };
        let [tl, tc, tr, bl, bc, br] = OVERLAY_POSITIONS;
        container(
            column![
                line(tl, tc, tr),
                Space::new().height(Fill),
                line(bl, bc, br)
            ]
            .height(Fill),
        )
        .padding(10)
        .width(Fill)
        .height(150)
        .style(style::screen)
        .into()
    }

    /// The compact island in miniature, with the real readings when there are some.
    fn mini_island(&self, enabled: bool) -> Element<'_, Message> {
        let gauges = self
            .primary()
            .map(|d| format::gauges(d, self.now))
            .unwrap_or_default();
        let mut line = row![widgets::icon(Glyph::Earbuds, 11.0, Ink::Text)]
            .spacing(5)
            .align_y(Center);
        let small = |t: String, dim: bool| {
            text(t)
                .size(9.5)
                .font(style::INTER_SEMIBOLD)
                .wrapping(text::Wrapping::None)
                .style(move |_: &Theme| text::Style {
                    color: Some(Color::from_rgba(
                        1.0,
                        1.0,
                        1.0,
                        if dim { 0.55 } else { 0.92 },
                    )),
                })
        };
        for g in &gauges {
            let label = match g.label {
                "Left" => "L",
                "Right" => "R",
                "Case" => "Case",
                _ => "",
            };
            let mut item = row![].spacing(3).align_y(Center);
            if !label.is_empty() {
                item = item.push(small(label.to_string(), true));
            }
            item = item.push(small(g.text.clone(), g.dimmed));
            line = line.push(item);
        }
        button(line)
            .padding(Padding {
                top: 5.0,
                bottom: 5.0,
                left: 9.0,
                right: 10.0,
            })
            .style(style::mini_island(enabled))
            .on_press_maybe(enabled.then_some(Message::PreviewOverlay))
            .into()
    }

    // ------------------------------------------------------------ Settings

    fn settings_tab(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let appearance = [
            ("System", Appearance::System),
            ("Light", Appearance::Light),
            ("Dark", Appearance::Dark),
        ];
        let general = group(vec![
            setting_row(
                "Appearance",
                None,
                container(slide_control(
                    &appearance,
                    s.appearance,
                    Some(self.anim(Anim::Appearance, 0.0)),
                    true,
                    Message::SetAppearance,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(196.0))
                .into(),
            ),
            setting_row(
                "Start at login",
                Some("Runs quietly in the tray"),
                switch(
                    s.start_at_login,
                    Some(Message::SetStartAtLogin as fn(bool) -> Message),
                ),
            ),
        ]);

        let mut airpods: Vec<Element<'_, Message>> = Vec::new();
        let device = self.primary();
        if let Some(d) = device {
            airpods.push(self.rename_row(d));
        }
        airpods.push(setting_row(
            "Stem press track control",
            Some("Double-press for next track, triple-press for previous"),
            switch(
                s.stem_control,
                Some(Message::SetStemControl as fn(bool) -> Message),
            ),
        ));
        airpods.push(setting_row(
            "AirPods mic switch",
            Some(
                "Hold the left stem to swap between your desk mic and the AirPods mic. \
                 Hold the right stem to change noise control.",
            ),
            switch(
                s.airpods_mic_switch,
                Some(Message::SetMicSwitch as fn(bool) -> Message),
            ),
        ));
        if let Some(d) = device
            && let Some(mic) = d.mic.filter(|_| d.is_connected())
        {
            let on = mic == crate::hub::MicRoute::AirPods;
            let caption = match (&d.mic_problem, on) {
                (Some(problem), false) => problem.as_str(),
                (_, true) => "The AirPods mic is the default mic now",
                (None, false) => "Your desk mic is the default mic",
            };
            airpods.push(setting_row(
                "Use AirPods mic",
                Some(caption),
                switch(
                    on,
                    format::controls_enabled(d)
                        .then_some((|_| Message::ToggleAirPodsMic) as fn(bool) -> Message),
                ),
            ));
        }
        if let Some(d) = device {
            let enabled = format::controls_enabled(d);
            if d.model.info().has_anc {
                airpods.push(setting_row(
                    "Off listening mode",
                    Some("Adds Off to the listening modes"),
                    switch(
                        d.allow_off_mode.unwrap_or(false),
                        enabled.then_some(Message::SetAllowOff as fn(bool) -> Message),
                    ),
                ));
            }
            for setting in format::device_settings(d) {
                airpods.push(self.device_setting(setting, enabled));
            }
        }

        let mut page = column![
            general,
            section_label("AirPods"),
            group(airpods),
            section_label("This PC"),
            group(vec![self.apple_mode_row()]),
        ];

        if let Some(d) = device {
            page = page.push(section_label("About")).push(self.about(d));
        }
        for (mac, mode) in &self.nothing {
            let mac_c = mac.clone();
            page = page.push(section_label("Nothing Ear")).push(group(vec![
                column![
                    text(mac.clone()).size(12).style(faint_text),
                    pick_list(NothingMode::ALL, *mode, move |m| {
                        Message::SetNothingMode(mac_c.clone(), m)
                    })
                    .placeholder("Noise control")
                    .text_size(13)
                    .width(Fill),
                ]
                .spacing(8)
                .into(),
            ]));
        }
        page.into()
    }

    fn rename_row<'a>(&'a self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let can_rename = format::controls_enabled(d) && !self.rename.trim().is_empty();
        let mut col = column![
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
            col = col.push(text(status.clone()).size(12).style(dim_text));
        }
        col.into()
    }

    /// One of the AirPods' own settings (see [`format::device_settings`]).
    fn device_setting<'a>(&self, setting: DeviceSetting, enabled: bool) -> Element<'a, Message> {
        match setting {
            DeviceSetting::Toggle {
                id,
                title,
                caption,
                on,
            } => setting_row(
                title,
                Some(caption),
                toggler(on)
                    .on_toggle_maybe(
                        enabled.then_some(move |v| Message::SetControl(id, format::toggle_byte(v))),
                    )
                    .size(24)
                    .spacing(0)
                    .style(style::toggle)
                    .into(),
            ),
            DeviceSetting::Choice {
                id,
                title,
                caption,
                options,
                selected,
            } => {
                let options: Vec<(&str, (u8, u8))> =
                    options.iter().map(|&(label, v)| (label, (id, v))).collect();
                let at = options
                    .iter()
                    .position(|(_, (_, v))| *v == selected)
                    .map(|i| self.anim(Anim::Control(id), i as f32));
                setting_block(
                    title,
                    Some(caption),
                    slide_control(
                        &options,
                        (id, selected),
                        at,
                        enabled,
                        |(id, v)| Message::SetControl(id, v),
                        Shape::WIDE,
                    ),
                )
            }
        }
    }

    fn about<'a>(&self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let mut rows = vec![kv("Model", format::model_name(d.model).to_string())];
        if let Some(n) = &d.model_number {
            rows.push(kv("Model number", n.clone()));
        }
        if let Some(DeviceInformation::AirPods(i)) =
            self.saved.get(&d.mac).and_then(|s| s.information.as_ref())
        {
            if !i.serial_number.is_empty() {
                rows.push(kv("Serial number", i.serial_number.clone()));
            }
            if !i.version1.is_empty() {
                rows.push(kv("Firmware", i.version1.clone()));
            }
        }
        rows.push(kv("Address", d.mac.clone()));
        if let Some(a) = &d.audio {
            rows.push(kv("Audio", a.clone()));
        }
        group(rows)
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

/// Overlay modes in the order the Island tab offers them.
const OVERLAY_MODES: [(&str, OverlayMode); 4] = [
    ("Pop-up", OverlayMode::Auto),
    ("Compact", OverlayMode::Compact),
    ("Full", OverlayMode::Always),
    ("Off", OverlayMode::Off),
];

const OVERLAY_STYLES: [(&str, OverlayStyle); 2] = [
    ("Classic", OverlayStyle::Classic),
    ("Purist", OverlayStyle::Purist),
];

const OVERLAY_SIZES: [(&str, OverlaySize); 4] = [
    ("S", OverlaySize::Small),
    ("M", OverlaySize::Medium),
    ("L", OverlaySize::Large),
    ("XL", OverlaySize::ExtraLarge),
];

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

fn gap<'a>(height: f32) -> Element<'a, Message> {
    Space::new().height(height).into()
}

/// A grouped panel: rows separated by hairlines inset from the left edge, iOS style.
fn group(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    let mut col = column![];
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(
                container(
                    container(Space::new())
                        .width(Fill)
                        .height(1)
                        .style(style::divider),
                )
                .padding(Padding {
                    left: 16.0,
                    ..Padding::ZERO
                }),
            );
        }
        col = col.push(
            container(r)
                .padding(Padding {
                    top: 13.0,
                    bottom: 13.0,
                    left: 16.0,
                    right: 16.0,
                })
                .width(Fill),
        );
    }
    container(col).width(Fill).style(style::group).into()
}

fn section_label<'a>(label: &str) -> Element<'a, Message> {
    container(
        text(label.to_uppercase())
            .size(12)
            .font(style::INTER_SEMIBOLD)
            .style(dim_text),
    )
    .padding(Padding {
        top: 22.0,
        bottom: 8.0,
        left: 8.0,
        right: 0.0,
    })
    .into()
}

fn kv<'a>(k: &'a str, v: String) -> Element<'a, Message> {
    row![
        text(k).size(14).width(Fill),
        text(v).size(14).style(dim_text),
    ]
    .spacing(12)
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
        .size(24)
        .spacing(0)
        .style(style::toggle)
        .into()
}

fn percent_slider<'a>(
    range: std::ops::RangeInclusive<f32>,
    value: f32,
    msg: fn(f32) -> Message,
    enabled: bool,
) -> Element<'a, Message> {
    let pct = text(format!("{}%", (value * 100.0).round() as u32))
        .size(12)
        .font(style::INTER_MEDIUM)
        .style(if enabled { dim_text } else { faint_text })
        .align_x(iced::alignment::Horizontal::Right)
        .width(Length::Fixed(34.0));
    row![
        slider(range, value, msg)
            .step(0.05_f32)
            .on_release(Message::CommitOverlaySliders)
            .style(style::slide)
            .width(Length::Fixed(116.0)),
        pct,
    ]
    .spacing(8)
    .align_y(Center)
    .into()
}

/// Size and type of a [`slide_control`].
#[derive(Debug, Clone, Copy)]
struct Shape {
    height: f32,
    radius: f32,
    text: f32,
}

impl Shape {
    const TABS: Shape = Shape {
        height: 36.0,
        radius: 18.0,
        text: 13.0,
    };
    const WIDE: Shape = Shape {
        height: 32.0,
        radius: 10.0,
        text: 12.5,
    };
    const COMPACT: Shape = Shape {
        height: 30.0,
        radius: 10.0,
        text: 12.5,
    };
}

/// The row a sliding highlight rides in: `pos` (index, fractional while moving) of `n` equal
/// slots. Built from fill portions, so it needs no pixel widths.
fn sliding<'a>(
    n: usize,
    pos: f32,
    thumb: impl Fn(&Theme) -> container::Style + 'a,
) -> Element<'a, Message> {
    const UNIT: f32 = 1000.0;
    let max = n.saturating_sub(1) as f32;
    let left = (pos.clamp(0.0, max) * UNIT).round() as u16;
    let right = (max * UNIT) as u16 - left;
    let mut r = row![];
    if left > 0 {
        r = r.push(Space::new().width(FillPortion(left)));
    }
    r = r.push(
        container(Space::new())
            .width(FillPortion(UNIT as u16))
            .height(Fill)
            .style(thumb),
    );
    if right > 0 {
        r = r.push(Space::new().width(FillPortion(right)));
    }
    r.height(Fill).into()
}

/// Segmented control whose selection glides between options on a spring. `pos` is the
/// animated index of the selected option (None hides the highlight).
fn slide_control<'a, T: Copy + PartialEq + 'a>(
    options: &[(&'a str, T)],
    selected: T,
    pos: Option<f32>,
    enabled: bool,
    msg: fn(T) -> Message,
    shape: Shape,
) -> Element<'a, Message> {
    let inner = shape.radius - 2.0;
    let mut labels = row![];
    for &(label, value) in options {
        let is_sel = value == selected;
        labels = labels.push(
            button(
                text(label)
                    .size(shape.text)
                    .font(if is_sel {
                        style::INTER_SEMIBOLD
                    } else {
                        style::INTER_MEDIUM
                    })
                    .wrapping(text::Wrapping::None)
                    .align_x(Center)
                    .align_y(Center)
                    .width(Fill)
                    .height(Fill),
            )
            .width(Fill)
            .height(Fill)
            .padding(0)
            .style(style::segment(is_sel))
            .on_press_maybe(enabled.then_some(msg(value))),
        );
    }
    let mut layers = stack![];
    if let Some(p) = pos {
        layers = layers.push(sliding(options.len(), p, style::thumb(inner)));
    }
    layers = layers.push(labels.height(Fill));
    container(layers.width(Fill).height(Fill))
        .padding(2)
        .width(Fill)
        .height(Length::Fixed(shape.height))
        .style(style::track(shape.radius))
        .into()
}

/// The listening mode tiles, with a glowing highlight that glides to the selected mode.
fn mode_tiles<'a>(
    modes: &[ListeningMode],
    selected: Option<usize>,
    pos: Option<f32>,
    enabled: bool,
) -> Element<'a, Message> {
    let mut tiles = row![];
    for (i, &m) in modes.iter().enumerate() {
        let is_sel = selected == Some(i);
        let ink = match (enabled, is_sel) {
            (false, _) => Ink::Faint,
            (true, true) => Ink::Accent,
            (true, false) => Ink::Dim,
        };
        let content = column![
            widgets::icon(Glyph::Mode(m), 22.0, ink),
            text(m.label())
                .size(12)
                .font(if is_sel {
                    style::INTER_SEMIBOLD
                } else {
                    style::INTER_MEDIUM
                })
                .align_x(Center)
                .wrapping(text::Wrapping::Word),
        ]
        .spacing(7)
        .align_x(Center)
        .width(Fill);
        tiles = tiles.push(
            button(content)
                .width(Fill)
                .height(Fill)
                .padding(Padding {
                    top: 13.0,
                    bottom: 8.0,
                    left: 4.0,
                    right: 4.0,
                })
                .style(style::tile(is_sel))
                .on_press_maybe(enabled.then_some(Message::SetMode(m))),
        );
    }
    let mut layers = stack![];
    if let Some(p) = pos {
        layers = layers.push(sliding(modes.len(), p, style::mode_thumb(enabled)));
    }
    layers = layers.push(tiles.height(Fill));
    container(layers.width(Fill).height(Fill))
        .padding(6)
        .width(Fill)
        .height(Length::Fixed(88.0))
        .style(style::group)
        .into()
}

/// Under the rings while the AirPods are away: what the readings mean and what to do.
fn away_hint<'a>(d: &DeviceSnapshot) -> Element<'a, Message> {
    let (title, body) = if format::is_nearby(d) {
        (
            "Nearby",
            "These levels are live from the AirPods themselves. They connect as soon as \
             you start using them.",
        )
    } else if d.in_case {
        (
            "In the case",
            "Open the lid near this PC to update the levels. AirPods reconnect on their own \
             when you take them out.",
        )
    } else {
        (
            "Out of range",
            "These are the last levels seen. BazzitePods reconnects on its own when your \
             AirPods are back.",
        )
    };
    group(vec![
        column![
            text(title).size(14).font(style::INTER_MEDIUM),
            text(body).size(12).style(dim_text),
        ]
        .spacing(3)
        .into(),
    ])
}

fn empty_state<'a>(glyph: Glyph, title: &'a str, body: &'a str) -> Element<'a, Message> {
    let disc = container(widgets::icon(glyph, 34.0, Ink::Dim))
        .center_x(Length::Fixed(76.0))
        .center_y(Length::Fixed(76.0))
        .style(style::icon_disc);
    container(
        column![
            disc,
            gap(8.0),
            text(title).size(20).font(style::INTER_SEMIBOLD),
            text(body)
                .size(13)
                .align_x(Center)
                .style(dim_text)
                .width(Length::Fixed(300.0)),
        ]
        .spacing(8)
        .align_x(Center),
    )
    .padding(Padding {
        top: 64.0,
        bottom: 30.0,
        left: 20.0,
        right: 20.0,
    })
    .center_x(Fill)
    .into()
}

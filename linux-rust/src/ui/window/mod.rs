//! Main window: three calm tabs (AirPods, Island, Settings) driven entirely by the hub
//! snapshot, in the island's design language. Selections and battery rings move on springs
//! ([`crate::ui::motion`]); frames are only requested while something is moving.
//!
//! Rules: `view` and `update` never block, never touch the disk, never panic. Device commands go
//! through `active_commands` from an async task; settings are written by a background task.

use crate::bluetooth::aacp::ControlCommandIdentifiers;
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
    Space, button, column, container, scrollable, slider, stack, text, text_input, toggler,
};
use iced::{
    Center, Color, Element, Fill, FillPortion, Length, Padding, Size, Subscription, Task, Theme,
    window,
};
use log::{debug, info, warn};
use std::collections::HashMap;
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
    /// Legacy event channel: tray "Open" requests and device connect/disconnect notices.
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
    .title(|_: &App, _| "Kase".to_string())
    .settings(iced::Settings {
        id: Some(crate::autostart::APP_ID.to_string()),
        fonts: vec![
            include_bytes!("../../../assets/font/Inter-Regular.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../../assets/font/Inter-Medium.ttf")
                .as_slice()
                .into(),
            include_bytes!("../../../assets/font/Inter-SemiBold.ttf")
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
        "kase-hub".hash(state);
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
        "kase-legacy-rx".hash(state);
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
    SetReturnToPc(bool),
    SetCaseCard(bool),
    SetStayAlongside(bool),
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
    SavedDevices(HashMap<String, DeviceData>),
    AppleMode(Option<bool>),
    /// bluetoothd runs with its experimental features (None = no adapter).
    ExperimentalMode(Option<bool>),
    /// Whether the app can ask for admin rights itself (pkexec on the host).
    AppleHost(crate::apple_mode::Host),
    Diagnostics(crate::diagnostics::Diagnostics),
    /// The Apple device mode switch was flipped: show the confirm step.
    AppleToggle(bool),
    /// The "Quicker battery updates when not connected" switch was flipped: show the confirm step.
    ExperimentalToggle(bool),
    AppleCancel,
    /// "Restart Bluetooth" in the confirm step: password prompt, then restart.
    AppleConfirm,
    AppleDone(Result<bool, crate::apple_mode::Failure>),
    CopyCommand(crate::apple_mode::Setting),
    TakeAudio,
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

/// Where the Apple device mode switch is in its flow.
#[derive(Debug, Clone, PartialEq)]
enum AppleFlow {
    Idle,
    /// Explains what's about to happen; turning it on (true) or off (false).
    Confirm(bool),
    /// Password prompt open or Bluetooth restarting.
    Working(bool),
    Done(bool),
    Failed(String),
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
    apple_flow: AppleFlow,
    /// Which setting `apple_flow` belongs to.
    apple_target: crate::apple_mode::Setting,
    experimental: Option<bool>,
    /// None until probed; false shows the copy-a-command fallback.
    apple_host: Option<crate::apple_mode::Host>,
    diagnostics: Option<crate::diagnostics::Diagnostics>,
    /// The setting whose terminal command was just copied.
    apple_copied: Option<crate::apple_mode::Setting>,
    rename_status: Option<String>,
    /// devices.json (serials, firmware), loaded off the UI thread.
    saved: HashMap<String, DeviceData>,
}

fn load_window_icon() -> Option<window::Icon> {
    let bytes = include_bytes!("../../../assets/icons/io.github.dabarkle.Kase.png");
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
            tab: std::env::var("KASE_UI_TAB")
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
            apple_flow: if demo {
                demo_apple_flow()
            } else {
                AppleFlow::Idle
            },
            diagnostics: None,
            apple_host: demo.then_some(crate::apple_mode::Host {
                can_change: true,
                ..Default::default()
            }),
            apple_copied: None,
            apple_target: Default::default(),
            experimental: demo.then_some(false),
            rename_status: None,
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
            Task::batch([
                load_apple_mode(),
                Task::perform(crate::apple_mode::probe_host(), Message::AppleHost),
            ])
        };
        (app, Task::batch([theme, open, saved, apple]))
    }

    /// Shows the confirm step for a system setting, unless a change is already running.
    fn confirm_setting(&mut self, setting: crate::apple_mode::Setting, on: bool) {
        if !matches!(self.apple_flow, AppleFlow::Working(_)) {
            self.apple_target = setting;
            self.apple_flow = AppleFlow::Confirm(on);
        }
    }

    fn open_window(&mut self) -> Task<Message> {
        if let Some(id) = self.window {
            return window::gain_focus(id);
        }
        // Demo/screenshots only: taller window to capture everything.
        let height = std::env::var("KASE_UI_HEIGHT")
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
        // The overlay feed follows the store and pushes the new config.
    }

    /// Save overlay settings; the overlay feed pushes them to the running overlay.
    fn save_overlay(&mut self, f: impl FnOnce(&mut Settings)) -> Task<Message> {
        self.save_settings(f)
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
                // Closing the window keeps Kase running in the tray.
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
            Message::SetReturnToPc(on) => self.save_settings(|s| s.return_to_pc = on),
            Message::SetCaseCard(on) => self.save_settings(|s| s.case_card = on),
            Message::SetStayAlongside(on) => self.save_settings(|s| s.stay_alongside = on),
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
                self.apple_copied = None;
                if !matches!(self.apple_flow, AppleFlow::Working(_)) {
                    self.apple_flow = AppleFlow::Idle;
                }
                if tab == Tab::Settings && !self.demo {
                    Task::batch([
                        load_apple_mode(),
                        Task::perform(crate::diagnostics::collect(), Message::Diagnostics),
                    ])
                } else {
                    Task::none()
                }
            }
            Message::AppleMode(on) => {
                self.apple_mode = on;
                Task::none()
            }
            Message::ExperimentalMode(on) => {
                self.experimental = on;
                Task::none()
            }
            Message::Diagnostics(d) => {
                self.diagnostics = Some(d);
                Task::none()
            }
            Message::AppleHost(host) => {
                self.apple_host = Some(host);
                Task::none()
            }
            Message::AppleToggle(on) => {
                self.confirm_setting(crate::apple_mode::Setting::AppleMode, on);
                Task::none()
            }
            Message::ExperimentalToggle(on) => {
                self.confirm_setting(crate::apple_mode::Setting::Experimental, on);
                Task::none()
            }
            Message::AppleCancel => {
                self.apple_flow = AppleFlow::Idle;
                Task::none()
            }
            Message::AppleConfirm => {
                let AppleFlow::Confirm(on) = self.apple_flow else {
                    return Task::none();
                };
                self.apple_flow = AppleFlow::Working(on);
                if self.demo {
                    return Task::none();
                }
                Task::perform(
                    crate::apple_mode::set(self.apple_target, on),
                    Message::AppleDone,
                )
            }
            Message::AppleDone(res) => {
                self.apple_flow = match res {
                    Ok(on) => {
                        match self.apple_target {
                            crate::apple_mode::Setting::AppleMode => self.apple_mode = Some(on),
                            crate::apple_mode::Setting::Experimental => {
                                self.experimental = Some(on)
                            }
                        }
                        AppleFlow::Done(on)
                    }
                    Err(crate::apple_mode::Failure::Cancelled) => AppleFlow::Idle,
                    Err(e) => AppleFlow::Failed(e.message()),
                };
                // The LE monitor re-probes on its own after Bluetooth restarts; look again
                // once it has, so "Battery when not connected" shows the new mode.
                let later = Task::perform(
                    async {
                        tokio::time::sleep(Duration::from_secs(8)).await;
                        crate::diagnostics::collect().await
                    },
                    Message::Diagnostics,
                );
                Task::batch([load_apple_mode(), later])
            }
            Message::TakeAudio => {
                self.taking_audio = Some(Instant::now());
                self.send(ControlCommandIdentifiers::OwnsConnection, 0x01)
            }
            Message::CopyCommand(setting) => {
                self.apple_copied = Some(setting);
                let on = match setting {
                    crate::apple_mode::Setting::AppleMode => self.apple_mode,
                    crate::apple_mode::Setting::Experimental => self.experimental,
                } != Some(true);
                let cmd = crate::apple_mode::manual_command(
                    setting,
                    on,
                    self.apple_host.unwrap_or_default(),
                );
                iced::clipboard::write(cmd)
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
            Message::Ignore => Task::none(),
            Message::SavedDevices(map) => {
                self.saved = map;
                Task::none()
            }
        }
    }

    fn on_legacy(&mut self, msg: BluetoothUIMessage) -> Task<Message> {
        match msg {
            BluetoothUIMessage::OpenWindow => {
                info!("Opening main window");
                self.open_window()
            }
            BluetoothUIMessage::DeviceConnected => load_saved_devices(),
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
}

// ------------------------------------------------------------------ helpers

fn load_saved_devices() -> Task<Message> {
    Task::perform(
        async {
            tokio::task::spawn_blocking(crate::store::load_devices)
                .await
                .unwrap_or_default()
        },
        Message::SavedDevices,
    )
}

/// Reads the adapter's modalias: BlueZ reports the DeviceID it's running with there.
/// Reads what the running bluetoothd does: Apple vendor id (from the adapter's modalias)
/// and experimental features (from its D-Bus interfaces).
fn load_apple_mode() -> Task<Message> {
    Task::batch([
        Task::perform(crate::apple_mode::status(), Message::AppleMode),
        Task::perform(
            crate::apple_mode::experimental_status(),
            Message::ExperimentalMode,
        ),
    ])
}

/// Demo/screenshots only: KASE_UI_APPLE=confirm|working|done|failed.
fn demo_apple_flow() -> AppleFlow {
    match std::env::var("KASE_UI_APPLE").as_deref() {
        Ok("confirm") => AppleFlow::Confirm(true),
        Ok("working") => AppleFlow::Working(true),
        Ok("done") => AppleFlow::Done(true),
        Ok("failed") => AppleFlow::Failed(crate::apple_mode::Failure::NotAuthorized.message()),
        _ => AppleFlow::Idle,
    }
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

mod controls;
mod page_airpods;
mod page_island;
mod page_settings;

use controls::*;

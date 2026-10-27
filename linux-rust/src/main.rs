mod audio;
mod autostart;
mod battery_service;
mod bluetooth;
mod core;
mod devices;
mod disconnect_watch;
mod fullscreen_watch;
mod hub;
mod media_controller;
mod mic;
mod overlay_client;
mod overlay_feed;
mod overlay_protocol;
mod single_instance;
mod sleep_monitor;
mod ui;
mod utils;

use crate::battery_service::BatteryService;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::DeviceContext;
use crate::devices::enums::{DeviceData, DeviceType};
use crate::disconnect_watch::{DropReason, ReconnectPlan};
use crate::hub::{AdapterState, Hub};
use crate::ui::messages::BluetoothUIMessage;
use crate::utils::get_devices_path;
use bluer::{Adapter, AdapterEvent, Address, DeviceEvent, DeviceProperty};
use clap::Parser;
use futures::StreamExt;
use log::{debug, info, warn};
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep};
use tokio_util::sync::CancellationToken;

/// BlueZ service UUID advertised by AirPods (and Beats) for the AACP control channel.
const AIRPODS_UUID: uuid::Uuid = uuid::uuid!("74ec2172-0bad-4d01-8f77-997b2be0722a");

#[derive(Parser)]
struct Args {
    #[arg(long, short = 'd', help = "Enable debug logging")]
    debug: bool,
    #[arg(
        long,
        help = "Disable system tray, useful if your environment doesn't support AppIndicator or StatusNotifier"
    )]
    no_tray: bool,
    #[arg(long, help = "Start the application minimized to tray")]
    start_minimized: bool,
    #[arg(
        long,
        help = "Enable Bluetooth LE debug logging. Only use when absolutely necessary; this produces a lot of logs."
    )]
    le_debug: bool,
    #[arg(long, short = 'v', help = "Show application version and exit")]
    version: bool,
    /// Preview the UI with fake AirPods state and no Bluetooth (for screenshots/development).
    /// Scenarios: airpods4 (default), pro, max, low, disconnected, empty, btoff.
    #[arg(long, hide = true, num_args = 0..=1, default_missing_value = "airpods4")]
    ui_demo: Option<String>,
}

fn main() -> iced::Result {
    let args = Args::parse();

    if args.version {
        println!(
            "You are running BazzitePods version {}",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(());
    }

    let log_level = if args.debug { "debug" } else { "info" };
    if env::var("RUST_LOG").is_err() {
        unsafe {
            env::set_var(
                "RUST_LOG",
                log_level.to_owned()
                    + &format!(
                        ",zbus=warn,winit=warn,tracing=warn,iced_wgpu=warn,wgpu_hal=warn,wgpu_core=warn,cosmic_text=warn,naga=warn,iced_winit=warn,librepods::bluetooth::le={}",
                        if args.le_debug { "debug" } else { "info" }
                    ),
            )
        };
    }
    env_logger::init();

    let (ui_tx, ui_rx) = unbounded_channel::<BluetoothUIMessage>();
    let device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>> =
        Arc::new(RwLock::new(HashMap::new()));
    let hub = Hub::new();
    let active_commands: Arc<Mutex<Option<devices::airpods::CommandSender>>> =
        Arc::new(Mutex::new(None));
    let settings = ui::settings::SettingsStore::load();

    if let Some(scenario) = args.ui_demo.as_deref() {
        ui::demo::seed(&hub, scenario);
        return ui::window::start_ui(ui::window::UiContext {
            hub,
            active_commands,
            device_managers,
            settings: ui::settings::SettingsStore::ephemeral(settings.get()),
            ui_rx,
            start_minimized: false,
            demo: true,
        });
    }

    if let single_instance::Instance::Secondary =
        single_instance::claim(ui_tx.clone(), !args.start_minimized && !args.no_tray)
    {
        return Ok(());
    }

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to start async runtime: {e}");
            std::process::exit(1);
        }
    };

    if args.no_tray {
        info!("Running in headless mode (no GUI)");
        runtime.block_on(async_main(
            ui_tx,
            device_managers,
            hub,
            active_commands,
            args.no_tray,
        ));
        Ok(())
    } else {
        let dm = device_managers.clone();
        let no_tray = args.no_tray;
        let hub_bg = hub.clone();
        let commands_bg = active_commands.clone();
        std::thread::spawn(move || {
            runtime.block_on(async_main(ui_tx, dm, hub_bg, commands_bg, no_tray))
        });
        ui::window::start_ui(ui::window::UiContext {
            hub,
            active_commands,
            device_managers,
            settings,
            ui_rx,
            start_minimized: args.start_minimized,
            demo: false,
        })
    }
}

async fn async_main(
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    hub: Hub,
    active_commands: Arc<Mutex<Option<devices::airpods::CommandSender>>>,
    no_tray: bool,
) {
    let battery = BatteryService::spawn(hub.clone());
    overlay_feed::spawn(hub.clone());
    fullscreen_watch::spawn();

    // Zero-config: register for login autostart unless the user switched it off.
    tokio::spawn(async {
        let enabled = utils::app_setting_bool("start_at_login").unwrap_or(true);
        if let Err(e) = autostart::ensure_autostart(enabled).await {
            info!("Autostart registration skipped: {e}");
        }
    });
    if !no_tray {
        tokio::spawn(ui::tray::run_tray(
            hub.clone(),
            ui_tx.clone(),
            active_commands.clone(),
        ));
    }

    let (kick_tx, kick_rx) = unbounded_channel::<Kick>();
    sleep_monitor::spawn(kick_tx.clone());
    disconnect_watch::spawn(kick_tx.clone());

    let (ble_tx, mut ble_rx) = unbounded_channel();
    tokio::spawn(async move {
        if let Err(e) = bluetooth::le::start_le_monitor(ble_tx).await {
            warn!("LE monitor stopped: {}", e);
        }
    });
    let battery_ble = battery.clone();
    let ble_kick = kick_tx.clone();
    tokio::spawn(async move {
        // Adverts arrive about once a second; only nudge the supervisor occasionally.
        let mut last_kick: HashMap<Address, Instant> = HashMap::new();
        while let Some((mac, reading)) = ble_rx.recv().await {
            let in_use = reading.left_in_ear == Some(true)
                || reading.right_in_ear == Some(true)
                || reading.lid_open == Some(true)
                || (reading.left.is_some() && !reading.left_in_case)
                || (reading.right.is_some() && !reading.right_in_case);
            if in_use
                && last_kick
                    .get(&mac)
                    .is_none_or(|t| t.elapsed() >= Duration::from_secs(10))
            {
                last_kick.insert(mac, Instant::now());
                let _ = ble_kick.send(Kick::BleSeen(mac, in_use));
            }
            battery_ble.send(battery_service::BatteryInput::Ble(mac, reading));
        }
    });

    supervise(SupervisorCtx {
        hub,
        battery,
        ui_tx,
        device_managers,
        active_commands,
        kick_tx,
        kick_rx,
    })
    .await;
}

/// Reasons to re-check BlueZ state right away instead of waiting for the periodic sweep.
#[derive(Debug, Clone, Copy)]
pub enum Kick {
    DeviceChanged,
    Resumed,
    /// A known AirPods is advertising nearby; `in_use` when a pod is out of the case or in an ear.
    BleSeen(Address, bool),
    /// BlueZ dropped a link, and said why.
    Disconnected(Address, DropReason),
}

struct SupervisorCtx {
    hub: Hub,
    battery: BatteryService,
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    active_commands: Arc<Mutex<Option<devices::airpods::CommandSender>>>,
    kick_tx: UnboundedSender<Kick>,
    kick_rx: tokio::sync::mpsc::UnboundedReceiver<Kick>,
}

struct RunningDevice {
    cancel: CancellationToken,
    handle: JoinHandle<()>,
    is_airpods: bool,
}

/// Per-adapter bookkeeping that outlives individual device tasks.
#[derive(Default)]
struct AdapterState_ {
    running: HashMap<Address, RunningDevice>,
    /// Tasks that were cancelled but may still be cleaning up. A new task for the same device
    /// waits until the old one is gone so the two never overwrite each other's state.
    stopping: HashMap<Address, JoinHandle<()>>,
    /// AirPods whose link dropped on its own (walked out of range), and when.
    lost_link: HashMap<Address, Instant>,
    /// Last time we asked BlueZ to reconnect a device.
    last_reconnect: HashMap<Address, Instant>,
    /// AirPods worth reconnecting without waiting for their adverts: when they dropped
    /// this PC, and for how long to keep trying.
    eager: HashMap<Address, (Instant, Duration)>,
    /// Why BlueZ last dropped each device, until it connects again.
    drop_reason: HashMap<Address, DropReason>,
}

impl AdapterState_ {
    fn note_lost(&mut self, addr: Address) {
        self.lost_link.entry(addr).or_insert_with(Instant::now);
        self.plan_reconnect(addr);
    }

    /// Decide whether to keep reconnecting AirPods that dropped this PC. Runs when the drop is
    /// noticed and again when BlueZ says why, whichever comes first.
    fn plan_reconnect(&mut self, addr: Address) {
        let Some(&lost_at) = self.lost_link.get(&addr) else {
            return;
        };
        let mac = addr.to_string();
        let reason = self.drop_reason.get(&addr).copied();
        let in_case = devices::airpods::dropped_in_case(&mac);
        let for_peer = devices::airpods::dropped_for_peer(&mac);
        let window = match disconnect_watch::plan(reason, in_case, for_peer) {
            ReconnectPlan::Never => {
                info!(
                    "AirPods {} were disconnected on this PC; leaving them disconnected",
                    addr
                );
                self.lost_link.remove(&addr);
                self.eager.remove(&addr);
                return;
            }
            ReconnectPlan::Briefly => PEER_DROP_WINDOW,
            ReconnectPlan::UntilBack => CASE_DROP_WINDOW,
        };
        let entry = self.eager.entry(addr).or_insert((lost_at, Duration::ZERO));
        if entry.1 == window {
            return;
        }
        entry.1 = window;
        match (window == PEER_DROP_WINDOW, in_case) {
            (true, _) => info!("AirPods {} dropped this PC for another device", addr),
            (false, true) => info!(
                "AirPods {} went into the case; reconnecting when they come out",
                addr
            ),
            (false, false) => info!(
                "AirPods {} went out of range; reconnecting when they come back",
                addr
            ),
        }
    }
}

/// How long after losing the link we still reconnect automatically when the AirPods come back.
const AUTO_RECONNECT_WINDOW: Duration = Duration::from_secs(4 * 60 * 60);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(20);
/// Reconnecting without adverts. Out of the case, or busy with an iPhone, AirPods often
/// advertise only Find My beacons this PC can't match to them, so after a drop keep asking
/// BlueZ to connect: a failed attempt costs nothing, and connecting never takes the audio
/// from an iPhone that is playing. Never after they were disconnected here (see
/// disconnect_watch::plan), so disconnecting the AirPods here keeps them disconnected.
/// - They dropped this PC to make room for an iPhone: it can't hear the iPhone let go.
const PEER_DROP_WINDOW: Duration = Duration::from_secs(3 * 60);
/// - They went into the case or out of range: come back when they do, as on a Mac, however
///   long they were away.
const CASE_DROP_WINDOW: Duration = Duration::MAX;
/// Retry every RECONNECT_INTERVAL at first, every minute after FAST_RECONNECT_PERIOD and
/// every few minutes after SLOW_RECONNECT_PERIOD.
const FAST_RECONNECT_PERIOD: Duration = Duration::from_secs(3 * 60);
const SLOW_RECONNECT_INTERVAL: Duration = Duration::from_secs(60);
const SLOW_RECONNECT_PERIOD: Duration = Duration::from_secs(30 * 60);
const IDLE_RECONNECT_INTERVAL: Duration = Duration::from_secs(3 * 60);
/// Upper bound on waiting for a cancelled device task to finish.
const TASK_STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Outer loop: (re)acquire the BlueZ session and adapter forever. bluetoothd restarts, adapter
/// unplug/replug and rfkill all end up back here instead of crashing the app.
async fn supervise(mut ctx: SupervisorCtx) {
    let mut backoff = 1u64;
    loop {
        let adapter = match acquire_adapter().await {
            Ok(a) => a,
            Err(e) => {
                debug!("No Bluetooth adapter yet: {e}");
                ctx.hub.set_adapter(AdapterState::Missing);
                // Wait for a kick (e.g. resume) or the backoff, whichever comes first.
                tokio::select! {
                    _ = ctx.kick_rx.recv() => {}
                    _ = sleep(Duration::from_secs(backoff)) => {}
                }
                backoff = (backoff * 2).min(30);
                continue;
            }
        };
        backoff = 1;
        info!("Using Bluetooth adapter {}", adapter.name());
        run_adapter(&adapter, &mut ctx).await;
        ctx.hub.set_adapter(AdapterState::Missing);
        sleep(Duration::from_secs(2)).await;
    }
}

async fn acquire_adapter() -> bluer::Result<Adapter> {
    let session = bluer::Session::new().await?;
    let adapter = session.default_adapter().await?;
    // Make sure the adapter object is really there (default_adapter doesn't round-trip).
    adapter.is_powered().await?;
    Ok(adapter)
}

/// Cancel a device task and wait (bounded) for it to clean up.
async fn stop_task(dev: RunningDevice) {
    dev.cancel.cancel();
    let abort = dev.handle.abort_handle();
    if tokio::time::timeout(TASK_STOP_TIMEOUT, dev.handle)
        .await
        .is_err()
    {
        warn!("Device task did not stop in time; aborting it");
        abort.abort();
    }
}

/// Inner loop for one adapter: keep exactly one device task per connected AirPods.
async fn run_adapter(adapter: &Adapter, ctx: &mut SupervisorCtx) {
    let local_mac = match adapter.address().await {
        Ok(a) => a.to_string(),
        Err(e) => {
            warn!("Could not read adapter address: {e}");
            return;
        }
    };

    let mut st = AdapterState_::default();
    let mut watched: HashMap<Address, JoinHandle<()>> = HashMap::new();

    let mut adapter_events = match adapter.events().await {
        Ok(ev) => Box::pin(ev),
        Err(e) => {
            warn!("Could not subscribe to adapter events: {e}");
            return;
        }
    };

    if let Ok(addrs) = adapter.device_addresses().await {
        for addr in addrs {
            watch_device(adapter, addr, &mut watched, &ctx.kick_tx).await;
            // AirPods away when the app starts (after a reboot, say) come back like ones
            // that went into the case while it ran.
            if let Ok(device) = adapter.device(addr)
                && device.is_paired().await.unwrap_or(false)
                && !device.is_connected().await.unwrap_or(true)
                && device
                    .uuids()
                    .await
                    .ok()
                    .flatten()
                    .is_some_and(|u| u.contains(&AIRPODS_UUID))
            {
                let now = Instant::now();
                st.lost_link.insert(addr, now);
                st.eager.insert(addr, (now, CASE_DROP_WINDOW));
            }
        }
    }

    let mut sweep = tokio::time::interval(Duration::from_secs(10));
    loop {
        reconcile(adapter, &local_mac, ctx, &mut st).await;
        // Adapter vanished: stop everything and let the outer loop re-acquire.
        if adapter.is_powered().await.is_err() {
            break;
        }

        tokio::select! {
            ev = adapter_events.next() => match ev {
                // New objects appear for every nearby BLE advertiser; only paired devices
                // matter, and watch_device filters those.
                Some(AdapterEvent::DeviceAdded(addr)) => {
                    watch_device(adapter, addr, &mut watched, &ctx.kick_tx).await;
                }
                Some(AdapterEvent::DeviceRemoved(addr)) => {
                    if let Some(h) = watched.remove(&addr) { h.abort(); }
                }
                Some(AdapterEvent::PropertyChanged(_)) => {}
                None => break,
            },
            kick = ctx.kick_rx.recv() => {
                // Coalesce bursts (a connect fires several property changes at once).
                sleep(Duration::from_millis(150)).await;
                let mut kicks: Vec<Kick> = kick.into_iter().collect();
                while let Ok(k) = ctx.kick_rx.try_recv() {
                    kicks.push(k);
                }
                if kicks.iter().any(|k| matches!(k, Kick::Resumed)) {
                    info!("System resumed; restarting device sessions");
                    // Sleep often kills the L2CAP channel without a clean disconnect. Restart
                    // device tasks so they reconnect fresh instead of waiting on a dead socket.
                    for (addr, dev) in st.running.drain() {
                        if dev.is_airpods {
                            let now = Instant::now();
                            st.lost_link.insert(addr, now);
                            st.eager.insert(addr, (now, CASE_DROP_WINDOW));
                        }
                        stop_task(dev).await;
                    }
                }
                for k in kicks {
                    match k {
                        Kick::BleSeen(addr, in_use) => {
                            maybe_reconnect(adapter, addr, in_use, &mut st).await;
                        }
                        Kick::Disconnected(addr, reason) => {
                            st.drop_reason.insert(addr, reason);
                            if !st.running.contains_key(&addr) {
                                st.plan_reconnect(addr);
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ = sweep.tick() => {
                st.eager.retain(|_, (t, window)| t.elapsed() < *window);
                let due: Vec<Address> = st
                    .eager
                    .iter()
                    .filter(|(addr, (t, _))| {
                        let every = match t.elapsed() {
                            e if e < FAST_RECONNECT_PERIOD => RECONNECT_INTERVAL,
                            e if e < SLOW_RECONNECT_PERIOD => SLOW_RECONNECT_INTERVAL,
                            _ => IDLE_RECONNECT_INTERVAL,
                        };
                        st.last_reconnect.get(addr).is_none_or(|l| l.elapsed() >= every)
                    })
                    .map(|(addr, _)| *addr)
                    .collect();
                for addr in due {
                    maybe_reconnect(adapter, addr, true, &mut st).await;
                }
            }
        }
    }

    for (_, dev) in st.running.drain() {
        stop_task(dev).await;
    }
    for (_, h) in watched.drain() {
        h.abort();
    }
}

/// Bring AirPods back after they dropped the link: out of range, or dropped to make room
/// for an iPhone. BlueZ doesn't reconnect by itself, so when we see the AirPods advertising
/// (case open / pods out), or shortly after the drop, we ask BlueZ to connect. Connecting
/// doesn't take the audio from an iPhone that is playing; it only lets this PC hear when
/// the iPhone lets go. Only for AirPods that dropped off this PC, never ones the user
/// disconnected here.
async fn maybe_reconnect(adapter: &Adapter, addr: Address, in_use: bool, st: &mut AdapterState_) {
    if !in_use || st.running.contains_key(&addr) {
        return;
    }
    let Some(lost_at) = st.lost_link.get(&addr).copied() else {
        return;
    };
    if lost_at.elapsed() > AUTO_RECONNECT_WINDOW && !st.eager.contains_key(&addr) {
        st.lost_link.remove(&addr);
        return;
    }
    if st
        .last_reconnect
        .get(&addr)
        .is_some_and(|t| t.elapsed() < RECONNECT_INTERVAL)
    {
        return;
    }
    if !adapter.is_powered().await.unwrap_or(false) {
        return;
    }
    let Ok(device) = adapter.device(addr) else {
        return;
    };
    if !device.is_paired().await.unwrap_or(false) || device.is_connected().await.unwrap_or(true) {
        return;
    }
    st.last_reconnect.insert(addr, Instant::now());
    info!("Reconnecting to AirPods {}", addr);
    tokio::spawn(async move {
        match tokio::time::timeout(Duration::from_secs(15), device.connect()).await {
            Ok(Ok(())) => info!("Reconnected {}", addr),
            Ok(Err(e)) => info!("Reconnect to {} failed: {}", addr, e),
            Err(_) => info!("Reconnect to {} timed out", addr),
        }
    });
}

async fn watch_device(
    adapter: &Adapter,
    addr: Address,
    watched: &mut HashMap<Address, JoinHandle<()>>,
    kick_tx: &UnboundedSender<Kick>,
) {
    if watched.contains_key(&addr) {
        return;
    }
    let Ok(device) = adapter.device(addr) else {
        return;
    };
    // Unpaired objects are transient BLE advertisers (phones, beacons, ...). Ignoring them
    // keeps busy places (offices, trains) from triggering a reconcile per advert.
    if !device.is_paired().await.unwrap_or(false) {
        return;
    }
    let Ok(events) = device.events().await else {
        return;
    };
    let kick_tx = kick_tx.clone();
    let _ = kick_tx.send(Kick::DeviceChanged);
    watched.insert(
        addr,
        tokio::spawn(async move {
            let mut events = Box::pin(events);
            while let Some(DeviceEvent::PropertyChanged(prop)) = events.next().await {
                if matches!(
                    prop,
                    DeviceProperty::Connected(_)
                        | DeviceProperty::Paired(_)
                        | DeviceProperty::Uuids(_)
                        | DeviceProperty::ServicesResolved(_)
                ) && kick_tx.send(Kick::DeviceChanged).is_err()
                {
                    break;
                }
            }
        }),
    );
}

async fn reconcile(
    adapter: &Adapter,
    local_mac: &str,
    ctx: &SupervisorCtx,
    st: &mut AdapterState_,
) {
    let powered = adapter.is_powered().await.unwrap_or(false);
    ctx.hub.set_adapter(if powered {
        AdapterState::On
    } else {
        AdapterState::Off
    });

    // Drop tasks that ended on their own (e.g. Nothing device setup failure). An AirPods
    // task ends by itself when the AirPods drop the link, often before BlueZ reports the
    // disconnect: remember that, or they are never reconnected.
    let mut ended = Vec::new();
    st.running.retain(|addr, d| {
        let done = d.handle.is_finished();
        if done && d.is_airpods {
            ended.push(*addr);
        }
        !done
    });
    if powered {
        for addr in ended {
            st.note_lost(addr);
        }
    }
    st.stopping.retain(|_, h| !h.is_finished());

    let addrs = adapter.device_addresses().await.unwrap_or_default();
    let saved = saved_devices();
    for addr in addrs {
        let Ok(device) = adapter.device(addr) else {
            continue;
        };
        let connected = if powered {
            match device.is_connected().await {
                Ok(c) => c,
                // A transient D-Bus hiccup must not tear down a healthy session.
                Err(e) => {
                    debug!("Could not read Connected for {}: {}", addr, e);
                    continue;
                }
            }
        } else {
            false
        };
        if !connected {
            if let Some(dev) = st.running.remove(&addr) {
                info!("{} disconnected", addr);
                if dev.is_airpods && powered {
                    st.note_lost(addr);
                }
                dev.cancel.cancel();
                // The task publishes its own Disconnected state when it exits.
                st.stopping.insert(addr, dev.handle);
            }
            continue;
        }
        if st.running.contains_key(&addr) || st.stopping.contains_key(&addr) {
            // Already handled, or the previous session is still cleaning up (next pass).
            continue;
        }
        let uuids = device.uuids().await.ok().flatten().unwrap_or_default();
        let is_airpods = uuids.contains(&AIRPODS_UUID);
        let saved_type = saved.get(&addr.to_string()).map(|d| d.type_.clone());
        let name = device
            .alias()
            .await
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "AirPods".to_string());
        let cancel = CancellationToken::new();
        let handle = if is_airpods {
            info!("AirPods connected: {} ({})", name, addr);
            st.lost_link.remove(&addr);
            st.eager.remove(&addr);
            st.drop_reason.remove(&addr);
            let dctx = DeviceContext {
                hub: ctx.hub.clone(),
                battery: ctx.battery.clone(),
                ui_tx: ctx.ui_tx.clone(),
                device_managers: ctx.device_managers.clone(),
                active_commands: ctx.active_commands.clone(),
                local_mac: local_mac.to_string(),
            };
            tokio::spawn(devices::airpods::run(addr, name, dctx, cancel.clone()))
        } else if saved_type == Some(DeviceType::Nothing) {
            info!("Managed Nothing device connected: {}", addr);
            tokio::spawn(run_nothing(
                addr,
                ctx.ui_tx.clone(),
                ctx.device_managers.clone(),
                cancel.clone(),
            ))
        } else {
            continue;
        };
        st.running.insert(
            addr,
            RunningDevice {
                cancel,
                handle,
                is_airpods,
            },
        );
    }
}

async fn run_nothing(
    addr: Address,
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    cancel: CancellationToken,
) {
    let addr_s = addr.to_string();
    let Some(dev) = devices::nothing::NothingDevice::new(addr, ui_tx.clone()).await else {
        warn!("Could not set up Nothing device {}", addr_s);
        return;
    };
    device_managers
        .write()
        .await
        .entry(addr_s.clone())
        .or_insert_with(|| DeviceManagers::with_att(dev.att_manager.clone()))
        .set_att(dev.att_manager);
    let _ = ui_tx.send(BluetoothUIMessage::DeviceConnected(addr_s.clone()));
    cancel.cancelled().await;
    device_managers.write().await.remove(&addr_s);
    let _ = ui_tx.send(BluetoothUIMessage::DeviceDisconnected(addr_s));
}

fn saved_devices() -> HashMap<String, DeviceData> {
    std::fs::read_to_string(get_devices_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

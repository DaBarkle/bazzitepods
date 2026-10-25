mod audio;
mod autostart;
mod battery_service;
mod bluetooth;
mod core;
mod devices;
mod hub;
mod media_controller;
mod sleep_monitor;
mod overlay_client;
mod overlay_feed;
mod overlay_protocol;
mod ui;
mod utils;

use crate::battery_service::BatteryService;
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::DeviceContext;
use crate::devices::enums::{DeviceData, DeviceType};
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
use tokio::time::{Duration, sleep};
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
            "You are running LibrePods version {}",
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

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("Failed to start async runtime: {e}");
            std::process::exit(1);
        }
    };

    if args.no_tray {
        info!("Running in headless mode (no GUI)");
        runtime.block_on(async_main(ui_tx, device_managers, hub, active_commands, args.no_tray));
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

    // Zero-config: register for login autostart unless the user switched it off.
    tokio::spawn(async {
        let enabled = utils::app_setting_bool("start_at_login").unwrap_or(true);
        if let Err(e) = autostart::ensure_autostart(enabled).await {
            info!("Autostart registration skipped: {e}");
        }
    });
    if !no_tray {
        tokio::spawn(ui::tray::run_tray(hub.clone(), ui_tx.clone(), active_commands.clone()));
    }

    let (kick_tx, kick_rx) = unbounded_channel::<Kick>();
    sleep_monitor::spawn(kick_tx.clone());

    let (ble_tx, mut ble_rx) = unbounded_channel();
    tokio::spawn(async move {
        if let Err(e) = bluetooth::le::start_le_monitor(ble_tx).await {
            warn!("LE monitor stopped: {}", e);
        }
    });
    let battery_ble = battery.clone();
    tokio::spawn(async move {
        while let Some((mac, reading)) = ble_rx.recv().await {
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
    AdapterChanged,
    Resumed,
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
}

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

/// Inner loop for one adapter: keep exactly one device task per connected AirPods.
async fn run_adapter(adapter: &Adapter, ctx: &mut SupervisorCtx) {
    let local_mac = match adapter.address().await {
        Ok(a) => a.to_string(),
        Err(e) => {
            warn!("Could not read adapter address: {e}");
            return;
        }
    };

    let mut running: HashMap<Address, RunningDevice> = HashMap::new();
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
        }
    }

    let mut sweep = tokio::time::interval(Duration::from_secs(10));
    loop {
        reconcile(adapter, &local_mac, ctx, &mut running).await;
        // Adapter vanished: stop everything and let the outer loop re-acquire.
        if adapter.is_powered().await.is_err() {
            break;
        }

        tokio::select! {
            ev = adapter_events.next() => match ev {
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
                let mut resumed = matches!(kick, Some(Kick::Resumed));
                while let Ok(k) = ctx.kick_rx.try_recv() {
                    resumed |= matches!(k, Kick::Resumed);
                }
                if resumed {
                    info!("System resumed; restarting device sessions");
                    // Sleep often kills the L2CAP channel without a clean disconnect. Restart
                    // device tasks so they reconnect fresh instead of waiting on a dead socket.
                    for (_, dev) in running.drain() {
                        dev.cancel.cancel();
                        let _ = dev.handle.await;
                    }
                }
            }
            _ = sweep.tick() => {}
        }
    }

    for (_, dev) in running.drain() {
        dev.cancel.cancel();
        let _ = dev.handle.await;
    }
    for (_, h) in watched.drain() {
        h.abort();
    }
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
    let Ok(device) = adapter.device(addr) else { return };
    let Ok(events) = device.events().await else { return };
    let kick_tx = kick_tx.clone();
    let _ = kick_tx.send(Kick::DeviceChanged);
    watched.insert(
        addr,
        tokio::spawn(async move {
            let mut events = Box::pin(events);
            while let Some(DeviceEvent::PropertyChanged(prop)) = events.next().await {
                if matches!(
                    prop,
                    DeviceProperty::Connected(_) | DeviceProperty::Uuids(_) | DeviceProperty::ServicesResolved(_)
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
    running: &mut HashMap<Address, RunningDevice>,
) {
    let powered = adapter.is_powered().await.unwrap_or(false);
    ctx.hub.set_adapter(if powered {
        AdapterState::On
    } else {
        AdapterState::Off
    });

    // Drop tasks that ended on their own (e.g. Nothing device setup failure).
    running.retain(|_, d| !d.handle.is_finished());

    let addrs = adapter.device_addresses().await.unwrap_or_default();
    let saved = saved_devices();
    for addr in addrs {
        let Ok(device) = adapter.device(addr) else { continue };
        let connected = powered && device.is_connected().await.unwrap_or(false);
        if !connected {
            if let Some(dev) = running.remove(&addr) {
                info!("{} disconnected", addr);
                dev.cancel.cancel();
                // The task publishes its own Disconnected state when it exits.
                tokio::spawn(async move {
                    let _ = dev.handle.await;
                });
            }
            continue;
        }
        if running.contains_key(&addr) {
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
        running.insert(addr, RunningDevice { cancel, handle });
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

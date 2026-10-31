//! Device supervisor: starts the background services, then watches BlueZ and runs one
//! session per connected AirPods, reconnecting them when they come back in range.

use crate::battery_service::BatteryService;
use crate::bluetooth::adapter::{AIRPODS_UUID, airpods_adapter};
use crate::bluetooth::managers::DeviceManagers;
use crate::devices::airpods::DeviceContext;
use crate::disconnect_watch::{DropReason, ReconnectPlan};
use crate::hub::{AdapterState, Hub};
use crate::ui::messages::BluetoothUIMessage;
use crate::{
    QUIT, audio, autostart, battery_service, bluetooth, devices, disconnect_watch,
    fullscreen_watch, overlay_feed, request_quit, sleep_monitor, toplevel_watch, ui, utils,
};
use bluer::{Adapter, AdapterEvent, Address, DeviceEvent, DeviceProperty};
use futures::StreamExt;
use log::{debug, info, warn};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc::{UnboundedSender, unbounded_channel};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep};
use tokio_util::sync::CancellationToken;

pub async fn run(
    ui_tx: UnboundedSender<BluetoothUIMessage>,
    device_managers: Arc<RwLock<HashMap<String, DeviceManagers>>>,
    hub: Hub,
    active_commands: Arc<Mutex<Option<devices::airpods::CommandSender>>>,
    settings: ui::settings::SettingsStore,
    no_tray: bool,
) {
    tokio::spawn(async {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) else {
            warn!("Could not listen for SIGTERM/SIGINT");
            return;
        };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        request_quit();
    });
    // Speakers a crash or kill left muted in the middle of an audio rebuild.
    tokio::spawn(async {
        audio::guard::unmute_left_hushed(&audio::pulse::LibPulse, &audio::guard::hush_file()).await;
    });
    let battery = BatteryService::spawn(hub.clone());
    overlay_feed::spawn(hub.clone(), settings);
    fullscreen_watch::spawn();
    toplevel_watch::spawn();

    // Zero-config: register for login autostart unless the user switched it off.
    tokio::spawn(async {
        let enabled = utils::app_setting_bool("start_at_login").unwrap_or(true);
        if let Err(e) = autostart::ensure_autostart(enabled).await {
            info!("Autostart registration skipped: {e}");
            // No Background portal (Sway, Hyprland, COSMIC...): say once how to do it by hand.
            if enabled && autostart::running_in_flatpak() {
                let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
                let hint = autostart::startup_hint(&desktop);
                info!("To start Kase at login: {hint}");
                if utils::first_time("autostart") {
                    crate::overlay_feed::notify("Start Kase at login".into(), hint);
                }
            }
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
        let mut lid_open: HashMap<Address, (bool, Instant)> = HashMap::new();
        while let Some((mac, reading)) = ble_rx.recv().await {
            let in_use = reading.left_in_ear == Some(true)
                || reading.right_in_ear == Some(true)
                || reading.lid_open == Some(true)
                || (reading.left.is_some() && !reading.left_in_case)
                || (reading.right.is_some() && !reading.right_in_case);
            let lid_opened = reading.lid_open.is_some_and(|open| {
                let prev = lid_open.insert(mac, (open, Instant::now()));
                lid_just_opened(prev.map(|(was, at)| (was, at.elapsed())), open)
            });
            if ble_kick_due(in_use, lid_opened, last_kick.get(&mac).map(|t| t.elapsed())) {
                if lid_opened {
                    info!("AirPods {mac}: case lid opened; trying to connect now");
                }
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

/// Whether an advert should nudge a reconnect: when the AirPods are in use, at most every
/// 10 s, but at once when the case lid just opened. The reconnect itself still keeps
/// [`MIN_RECONNECT_GAP`] between attempts, so this can't storm.
fn ble_kick_due(in_use: bool, lid_opened: bool, since_last: Option<Duration>) -> bool {
    in_use && (lid_opened || since_last.is_none_or(|t| t >= Duration::from_secs(10)))
}

/// The lid opened since the last advert: it was shut, or the AirPods went quiet for a while
/// (a closed case stops advertising, often without saying it shut).
fn lid_just_opened(prev: Option<(bool, Duration)>, open: bool) -> bool {
    open && prev.is_none_or(|(was_open, ago)| !was_open || ago > Duration::from_secs(5))
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
    /// Someone is at the PC again: the screen was unlocked or the session stopped idling.
    UserActive,
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
    started: Instant,
}

impl RunningDevice {
    /// The AirPods dropped this PC right after connecting: another device (an iPhone) took
    /// them while the PC was setting up, and they may well come back in a few seconds.
    fn bounced(&self) -> bool {
        self.started.elapsed() < BOUNCE_LIMIT
    }
}

/// Per-adapter bookkeeping that outlives individual device tasks.
#[derive(Default)]
struct AdapterState_ {
    /// AirPods disconnected on this PC: never reconnected until they connect again by any
    /// route. Saved in `kept_path`, so restarts, reboots and suspend don't undo it.
    kept_away: HashSet<Address>,
    kept_path: PathBuf,
    /// Disconnects pressed here, saved once the adapter is still on a moment later.
    pending_keep: HashMap<Address, Instant>,
    running: HashMap<Address, RunningDevice>,
    /// Tasks that were cancelled but may still be cleaning up. A new task for the same device
    /// waits until the old one is gone so the two never overwrite each other's state.
    stopping: HashMap<Address, JoinHandle<()>>,
    /// AirPods that dropped this PC and are wanted back: since when, and when we last asked
    /// BlueZ to connect them.
    away: HashMap<Address, Away>,
    /// Why BlueZ last dropped each device, until it connects again.
    drop_reason: HashMap<Address, DropReason>,
}

#[derive(Debug, Clone, Copy)]
struct Away {
    since: Instant,
    last_try: Option<Instant>,
    /// Lost right after connecting (see [`RunningDevice::bounced`]).
    bounced: bool,
}

impl AdapterState_ {
    fn note_lost(&mut self, addr: Address, bounced: bool) {
        let new = !self.away.contains_key(&addr);
        self.away.entry(addr).or_insert(Away {
            since: Instant::now(),
            last_try: None,
            bounced,
        });
        self.plan_reconnect(addr, new);
    }

    /// AirPods that are paired here but away when the app starts (after a reboot, say), or
    /// whose session was restarted after sleep: bring them back like any other drop.
    fn want_back(&mut self, addr: Address) {
        self.away.insert(
            addr,
            Away {
                since: Instant::now(),
                last_try: None,
                bounced: false,
            },
        );
    }

    /// Decide whether to keep reconnecting AirPods that dropped this PC. Runs when the drop is
    /// noticed and again when BlueZ says why, whichever comes first.
    fn plan_reconnect(&mut self, addr: Address, log_it: bool) {
        if !self.away.contains_key(&addr) {
            return;
        }
        let reason = self.drop_reason.get(&addr).copied();
        match disconnect_watch::plan(reason) {
            ReconnectPlan::Never => {
                info!(
                    "AirPods {} were disconnected on this PC; leaving them disconnected",
                    addr
                );
                self.away.remove(&addr);
                self.pending_keep.insert(addr, Instant::now());
            }
            ReconnectPlan::UntilBack if log_it => {
                if self.away.get(&addr).is_some_and(|a| a.bounced) {
                    info!(
                        "AirPods {} dropped this PC while connecting (another device took them); trying again every {:?}",
                        addr, QUICK_RECONNECT_INTERVAL
                    );
                } else if devices::airpods::dropped_in_case(&addr.to_string()) {
                    info!(
                        "AirPods {} went into the case; reconnecting when they come out",
                        addr
                    );
                } else {
                    info!(
                        "AirPods {} left this PC (out of range or another device); reconnecting when they come back",
                        addr
                    );
                }
            }
            ReconnectPlan::UntilBack => {}
        }
    }

    /// Remember Disconnects pressed here once the adapter is confirmed still on: switching
    /// Bluetooth off also closes links as "Local", and that must not stop reconnects for good.
    fn settle_kept(&mut self, powered: bool) {
        let due: Vec<Address> = self
            .pending_keep
            .iter()
            .filter(|(_, t)| t.elapsed() >= KEEP_CONFIRM)
            .map(|(a, _)| *a)
            .collect();
        for addr in &due {
            self.pending_keep.remove(addr);
            if powered {
                info!(
                    "AirPods {addr}: remembering the Disconnect; not reconnecting them, even after a restart, until they connect again"
                );
                self.kept_away.insert(*addr);
            } else {
                info!(
                    "AirPods {addr}: Bluetooth was switched off, not a Disconnect; not remembering it"
                );
            }
        }
        if powered && !due.is_empty() {
            disconnect_watch::save_kept_away(&self.kept_path, &self.kept_away);
        }
    }

    /// The AirPods connected again (from KDE, bluetoothctl or by themselves) or were unpaired.
    fn forget_kept(&mut self, addr: Address, why: &str) {
        self.pending_keep.remove(&addr);
        if self.kept_away.remove(&addr) {
            info!("AirPods {addr} {why}; forgetting the Disconnect pressed on this PC");
            disconnect_watch::save_kept_away(&self.kept_path, &self.kept_away);
        }
    }

    /// AirPods due another background connect attempt.
    fn due(&self) -> Vec<Address> {
        // Paging competes with game traffic for the radio, so no quick retries behind a
        // fullscreen window or while nobody is at the PC.
        let at_pc = sleep_monitor::user_present() && !overlay_feed::fullscreen_in_front();
        self.away
            .iter()
            .filter(|(addr, away)| {
                let quick = quick_retry(
                    at_pc,
                    devices::airpods::dropped_in_case(&addr.to_string()),
                    away.bounced,
                    away.since.elapsed(),
                );
                !self.running.contains_key(addr)
                    && away
                        .last_try
                        .is_none_or(|t| t.elapsed() >= retry_interval(away.since.elapsed(), quick))
            })
            .map(|(addr, _)| *addr)
            .collect()
    }
}

/// Whether away AirPods get the quick (back-to-back) retries: only while someone is at the
/// PC with nothing fullscreen, and only when they went into the case or were taken by
/// another device mid-connect (for the first few minutes of that).
fn quick_retry(at_pc: bool, in_case: bool, bounced: bool, away_for: Duration) -> bool {
    at_pc && (in_case || (bounced && away_for < FAST_RECONNECT_PERIOD))
}

/// Background connect attempts while AirPods are away: every 20 s for the first 3 minutes,
/// then every minute for as long as it takes. A failed attempt costs nothing and connecting
/// never takes the audio from an iPhone that is playing, so there is no reason to give up
/// or slow down further: the AirPods come back within a minute whenever they are back.
/// `quick` (they went into the case and the user is at the PC, nothing fullscreen): page
/// back to back for the first 15 minutes, so opening the lid connects them within seconds.
fn retry_interval(away_for: Duration, quick: bool) -> Duration {
    if quick && away_for < QUICK_RECONNECT_PERIOD {
        QUICK_RECONNECT_INTERVAL
    } else if away_for < FAST_RECONNECT_PERIOD {
        RECONNECT_INTERVAL
    } else {
        SLOW_RECONNECT_INTERVAL
    }
}

/// A session shorter than this that ends on its own counts as bounced.
const BOUNCE_LIMIT: Duration = Duration::from_secs(10);
/// How long a Disconnect pressed here waits before it is saved (see [`AdapterState_::settle_kept`]).
const KEEP_CONFIRM: Duration = Duration::from_secs(3);
/// About one page timeout, so the next attempt starts as the previous one gives up.
const QUICK_RECONNECT_INTERVAL: Duration = Duration::from_secs(8);
const QUICK_RECONNECT_PERIOD: Duration = Duration::from_secs(15 * 60);
const RECONNECT_INTERVAL: Duration = Duration::from_secs(20);
const FAST_RECONNECT_PERIOD: Duration = Duration::from_secs(3 * 60);
const SLOW_RECONNECT_INTERVAL: Duration = Duration::from_secs(60);
/// The least time between two attempts, whatever asks for them (adverts, the user coming
/// back, the sweep): BlueZ refuses a connect while the previous one is still paging.
const MIN_RECONNECT_GAP: Duration = Duration::from_secs(5);
/// Upper bound on waiting for a cancelled device task to finish.
const TASK_STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Upper bound on one pass over BlueZ's devices. A D-Bus call that never answers must not
/// stop the supervisor: AirPods would then never connect again until the app restarts.
const RECONCILE_LIMIT: Duration = Duration::from_secs(30);
/// Upper bound on any single D-Bus question to BlueZ outside a reconcile pass.
const DBUS_LIMIT: Duration = Duration::from_secs(5);

/// Runs a BlueZ call with a time limit; `None` when it failed or took too long.
async fn bounded<T, E>(fut: impl std::future::Future<Output = Result<T, E>>) -> Option<T> {
    tokio::time::timeout(DBUS_LIMIT, fut).await.ok()?.ok()
}

/// Outer loop: (re)acquire the BlueZ session and adapter forever. bluetoothd restarts, adapter
/// unplug/replug and rfkill all end up back here instead of crashing the app.
async fn supervise(mut ctx: SupervisorCtx) {
    let mut backoff = 1u64;
    while !QUIT.is_cancelled() {
        let adapter = match acquire_adapter().await {
            Ok(a) => a,
            Err(e) => {
                debug!("No Bluetooth adapter yet: {e}");
                ctx.hub.set_adapter(AdapterState::Missing);
                // Wait for a kick (e.g. resume) or the backoff, whichever comes first.
                tokio::select! {
                    _ = ctx.kick_rx.recv() => {}
                    _ = sleep(Duration::from_secs(backoff)) => {}
                    _ = QUIT.cancelled() => {}
                }
                backoff = (backoff * 2).min(30);
                continue;
            }
        };
        backoff = 1;
        info!("Using Bluetooth adapter {}", adapter.name());
        run_adapter(&adapter, &mut ctx).await;
        if QUIT.is_cancelled() {
            break;
        }
        ctx.hub.set_adapter(AdapterState::Missing);
        sleep(Duration::from_secs(2)).await;
    }
}

async fn acquire_adapter() -> bluer::Result<Adapter> {
    let session = bluer::Session::new().await?;
    let adapter = airpods_adapter(&session).await?;
    // Make sure the adapter object is really there (adapter() doesn't round-trip).
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

    let kept_path = disconnect_watch::kept_away_path();
    let mut st = AdapterState_ {
        kept_away: disconnect_watch::load_kept_away(&kept_path),
        kept_path,
        ..Default::default()
    };
    let mut watched: HashMap<Address, JoinHandle<()>> = HashMap::new();

    let mut adapter_events = match adapter.events().await {
        Ok(ev) => Box::pin(ev),
        Err(e) => {
            warn!("Could not subscribe to adapter events: {e}");
            return;
        }
    };

    if let Ok(addrs) = adapter.device_addresses().await {
        let mut paired = HashSet::new();
        for addr in addrs {
            watch_device(adapter, addr, &mut watched, &ctx.kick_tx).await;
            let Ok(device) = adapter.device(addr) else {
                continue;
            };
            if bounded(device.is_paired()).await == Some(true) {
                paired.insert(addr);
            }
            // AirPods away when the app starts (after a reboot, say) come back like ones
            // that went into the case while it ran.
            if paired.contains(&addr)
                && bounded(device.is_connected()).await == Some(false)
                && bounded(device.uuids())
                    .await
                    .flatten()
                    .is_some_and(|u| u.contains(&AIRPODS_UUID))
            {
                if st.kept_away.contains(&addr) {
                    info!(
                        "AirPods {addr}: remembered Disconnect pressed on this PC; not reconnecting"
                    );
                } else {
                    st.want_back(addr);
                }
            }
        }
        // Unpaired or forgotten while Kase wasn't running.
        for addr in st.kept_away.clone() {
            if !paired.contains(&addr) {
                st.forget_kept(addr, "are no longer paired");
            }
        }
    }

    // A second adapter plugged in may be where the AirPods are paired: switch to it.
    let mut session_events = match bluer::Session::new().await {
        Ok(s) => s.events().await.ok().map(|ev| (s, Box::pin(ev))),
        Err(_) => None,
    };

    // 4 s so the 8 s quick retries land on time; attempts happen on ticks, so they stay aligned.
    let mut sweep = tokio::time::interval(Duration::from_secs(4));
    loop {
        if tokio::time::timeout(
            RECONCILE_LIMIT,
            reconcile(adapter, &local_mac, ctx, &mut st),
        )
        .await
        .is_err()
        {
            warn!("BlueZ did not answer for {RECONCILE_LIMIT:?}; trying again");
        }
        // Adapter vanished (or BlueZ stopped answering): stop everything and let the outer
        // loop re-acquire.
        match tokio::time::timeout(DBUS_LIMIT, adapter.is_powered()).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => break,
            Err(_) => {
                warn!("BlueZ stopped answering; reconnecting to it");
                break;
            }
        }

        tokio::select! {
            _ = QUIT.cancelled() => break,
            ev = adapter_events.next() => match ev {
                // New objects appear for every nearby BLE advertiser; only paired devices
                // matter, and watch_device filters those.
                Some(AdapterEvent::DeviceAdded(addr)) => {
                    watch_device(adapter, addr, &mut watched, &ctx.kick_tx).await;
                }
                Some(AdapterEvent::DeviceRemoved(addr)) => {
                    if let Some(h) = watched.remove(&addr) { h.abort(); }
                    st.forget_kept(addr, "were unpaired");
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
                    let sessions: Vec<_> = st.running.drain().collect();
                    for (addr, dev) in sessions {
                        st.want_back(addr);
                        stop_task(dev).await;
                    }
                }
                let mut everyone = false;
                for k in kicks {
                    match k {
                        Kick::BleSeen(addr, true) => {
                            maybe_reconnect(adapter, addr, &mut st).await;
                        }
                        Kick::Disconnected(addr, reason) => {
                            st.drop_reason.insert(addr, reason);
                            if !st.running.contains_key(&addr) {
                                st.plan_reconnect(addr, false);
                            }
                        }
                        Kick::UserActive | Kick::Resumed => everyone = true,
                        _ => {}
                    }
                }
                if everyone {
                    let away: Vec<Address> = st.away.keys().copied().collect();
                    for addr in away {
                        maybe_reconnect(adapter, addr, &mut st).await;
                    }
                }
            }
            Some(bluer::SessionEvent::AdapterAdded(name)) = async {
                match session_events.as_mut() {
                    Some((_, ev)) => ev.next().await,
                    None => std::future::pending().await,
                }
            } => {
                let better = match session_events.as_ref() {
                    Some((s, _)) => airpods_adapter(s).await.ok(),
                    None => None,
                };
                if better.is_some_and(|b| b.name() != adapter.name()) {
                    info!("Adapter {name} appeared and has the AirPods; switching to it");
                    break;
                }
            }
            _ = sweep.tick() => {
                if !st.pending_keep.is_empty() {
                    st.settle_kept(bounded(adapter.is_powered()).await == Some(true));
                }
                for addr in st.due() {
                    maybe_reconnect(adapter, addr, &mut st).await;
                }
            }
        }
    }

    // In parallel: on quit every session restores its own state before the app exits.
    futures::future::join_all(st.running.drain().map(|(_, dev)| stop_task(dev))).await;
    for (_, h) in watched.drain() {
        h.abort();
    }
}

/// Bring AirPods back after they dropped the link: out of range, into the case, or dropped
/// to make room for an iPhone. BlueZ doesn't reconnect by itself, so we ask it to connect
/// on a steady schedule, when the AirPods advertise nearby (case open / pods out), and when
/// the user comes back to the PC. Connecting doesn't take the audio from an iPhone that is
/// playing; it only lets this PC take over when the iPhone lets go. Only for AirPods that
/// dropped off this PC, never ones the user disconnected here.
async fn maybe_reconnect(adapter: &Adapter, addr: Address, st: &mut AdapterState_) {
    if st.running.contains_key(&addr) {
        return;
    }
    let Some(away) = st.away.get_mut(&addr) else {
        return;
    };
    if away
        .last_try
        .is_some_and(|t| t.elapsed() < MIN_RECONNECT_GAP)
    {
        return;
    }
    away.last_try = Some(Instant::now());
    if bounded(adapter.is_powered()).await != Some(true) {
        return;
    }
    let Ok(device) = adapter.device(addr) else {
        return;
    };
    if bounded(device.is_paired()).await != Some(true)
        || bounded(device.is_connected()).await != Some(false)
    {
        return;
    }
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
        if done {
            ended.push((*addr, d.bounced()));
        }
        !done
    });
    if powered {
        for (addr, bounced) in ended {
            st.note_lost(addr, bounced);
        }
    }
    st.stopping.retain(|_, h| !h.is_finished());

    let addrs = adapter.device_addresses().await.unwrap_or_default();
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
                if powered {
                    st.note_lost(addr, dev.bounced());
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
        if !uuids.contains(&AIRPODS_UUID) {
            continue;
        }
        let name = device
            .alias()
            .await
            .ok()
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "AirPods".to_string());
        let cancel = CancellationToken::new();
        info!("AirPods connected: {} ({})", name, addr);
        st.away.remove(&addr);
        st.drop_reason.remove(&addr);
        st.forget_kept(addr, "connected again");
        let dctx = DeviceContext {
            hub: ctx.hub.clone(),
            battery: ctx.battery.clone(),
            ui_tx: ctx.ui_tx.clone(),
            device_managers: ctx.device_managers.clone(),
            active_commands: ctx.active_commands.clone(),
            local_mac: local_mac.to_string(),
        };
        let handle = tokio::spawn(devices::airpods::run(addr, name, dctx, cancel.clone()));
        st.running.insert(
            addr,
            RunningDevice {
                cancel,
                handle,
                started: Instant::now(),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kept_state(name: &str) -> AdapterState_ {
        let dir = std::env::temp_dir().join(format!("kase-sup-{name}-{}", std::process::id()));
        AdapterState_ {
            kept_path: dir.join("kept_disconnected.json"),
            ..Default::default()
        }
    }

    fn settled_now(st: &mut AdapterState_) {
        for t in st.pending_keep.values_mut() {
            *t -= KEEP_CONFIRM;
        }
    }

    #[test]
    fn a_disconnect_pressed_here_is_remembered_until_they_connect_again() {
        let a: Address = "CC:22:FE:24:0C:70".parse().unwrap();
        let mut st = kept_state("keep");
        st.drop_reason.insert(a, DropReason::Local);
        st.note_lost(a, false);
        assert!(st.away.is_empty(), "no reconnects after a Disconnect here");
        st.settle_kept(true);
        assert!(
            st.kept_away.is_empty(),
            "not saved before the adapter is confirmed on"
        );
        settled_now(&mut st);
        st.settle_kept(true);
        assert_eq!(
            disconnect_watch::load_kept_away(&st.kept_path),
            HashSet::from([a])
        );
        st.forget_kept(a, "connected again");
        assert!(disconnect_watch::load_kept_away(&st.kept_path).is_empty());
        let _ = std::fs::remove_dir_all(st.kept_path.parent().unwrap());
    }

    #[test]
    fn switching_bluetooth_off_is_not_remembered_as_a_disconnect() {
        let a: Address = "CC:22:FE:24:0C:70".parse().unwrap();
        let mut st = kept_state("power");
        st.drop_reason.insert(a, DropReason::Local);
        st.note_lost(a, false);
        settled_now(&mut st);
        st.settle_kept(false);
        assert!(st.kept_away.is_empty() && st.pending_keep.is_empty());
        assert!(!st.kept_path.exists());
    }

    #[test]
    fn retry_interval_schedule() {
        let min = |m: u64| Duration::from_secs(m * 60);
        assert_eq!(retry_interval(Duration::ZERO, false), RECONNECT_INTERVAL);
        assert_eq!(retry_interval(min(5), false), SLOW_RECONNECT_INTERVAL);
        assert_eq!(
            retry_interval(Duration::ZERO, true),
            QUICK_RECONNECT_INTERVAL
        );
        assert_eq!(retry_interval(min(14), true), QUICK_RECONNECT_INTERVAL);
        assert_eq!(retry_interval(min(15), true), SLOW_RECONNECT_INTERVAL);
        assert!(QUICK_RECONNECT_INTERVAL >= MIN_RECONNECT_GAP);
    }

    #[test]
    fn quick_retries_only_at_the_pc_after_the_case_or_a_bounce() {
        let min = |m: u64| Duration::from_secs(m * 60);
        // Into the case while at the PC; not while locked, idle or behind a fullscreen game.
        assert!(quick_retry(true, true, false, min(1)));
        assert!(!quick_retry(false, true, false, min(1)));
        // Taken mid-connect by the iPhone: quick for the first 3 minutes only.
        assert!(quick_retry(true, false, true, min(2)));
        assert!(!quick_retry(true, false, true, min(4)));
        assert!(!quick_retry(false, false, true, min(1)));
        // Simply walked off (out of range): the normal schedule.
        assert!(!quick_retry(true, false, false, min(1)));
    }

    #[test]
    fn lid_opening_is_seen_even_when_the_closed_case_said_nothing() {
        let s = Duration::from_secs;
        assert!(lid_just_opened(None, true));
        assert!(lid_just_opened(Some((false, s(1))), true));
        // Still open, adverts a second apart: not a new opening.
        assert!(!lid_just_opened(Some((true, s(1))), true));
        // Open, then silence (closed case), then open again.
        assert!(lid_just_opened(Some((true, s(30))), true));
        assert!(!lid_just_opened(Some((false, s(1))), false));
    }

    #[test]
    fn lid_opening_nudges_at_once_otherwise_every_ten_seconds() {
        let s = Duration::from_secs;
        assert!(ble_kick_due(true, false, None));
        assert!(!ble_kick_due(true, false, Some(s(3))));
        assert!(ble_kick_due(true, false, Some(s(10))));
        assert!(ble_kick_due(true, true, Some(s(1))));
        assert!(!ble_kick_due(false, true, None));
    }
}

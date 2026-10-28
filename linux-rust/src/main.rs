use clap::Parser;
use kase::hub::Hub;
use kase::ui::messages::BluetoothUIMessage;
use kase::{devices, single_instance, supervisor, ui};
use log::info;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use tokio::sync::mpsc::unbounded_channel;
use tokio::sync::{Mutex, RwLock};

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
        println!("You are running Kase version {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let log_level = if args.debug { "debug" } else { "info" };
    if env::var("RUST_LOG").is_err() {
        unsafe {
            env::set_var(
                "RUST_LOG",
                log_level.to_owned()
                    + &format!(
                        ",zbus=warn,winit=warn,tracing=warn,iced_wgpu=warn,wgpu_hal=warn,wgpu_core=warn,cosmic_text=warn,naga=warn,iced_winit=warn,kase::bluetooth::le={}",
                        if args.le_debug { "debug" } else { "info" }
                    ),
            )
        };
    }
    env_logger::init();
    kase::utils::migrate_legacy_dirs();

    let (ui_tx, ui_rx) = unbounded_channel::<BluetoothUIMessage>();
    let device_managers: Arc<RwLock<HashMap<String, kase::bluetooth::managers::DeviceManagers>>> =
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
        // Nothing reads window messages without a window; don't let them pile up.
        drop(ui_rx);
        runtime.block_on(supervisor::run(
            ui_tx,
            device_managers,
            hub,
            active_commands,
            settings,
            args.no_tray,
        ));
        Ok(())
    } else {
        let dm = device_managers.clone();
        let no_tray = args.no_tray;
        let hub_bg = hub.clone();
        let commands_bg = active_commands.clone();
        let settings_bg = settings.clone();
        std::thread::spawn(move || {
            runtime.block_on(supervisor::run(
                ui_tx,
                dm,
                hub_bg,
                commands_bg,
                settings_bg,
                no_tray,
            ));
            // Returns only after a quit request; the window runs on the main thread.
            std::process::exit(0);
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

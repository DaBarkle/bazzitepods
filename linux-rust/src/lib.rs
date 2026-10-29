//! Kase: AirPods for Linux. The app's library: everything except argument parsing,
//! logging setup and choosing between the window and headless mode (`main.rs`). The
//! overlay binary uses it for the wire protocol it shares with the app.

mod apple_mode;
mod audio;
mod autostart;
mod battery_service;
pub mod bluetooth;
mod core;
pub mod devices;
mod disconnect_watch;
mod fullscreen_watch;
pub mod hub;
pub mod log_file;
mod media_controller;
mod mic;
mod overlay_client;
mod overlay_feed;
pub mod overlay_protocol;
pub mod single_instance;
mod sleep_monitor;
mod store;
pub mod supervisor;
pub mod ui;
pub mod utils;

pub use supervisor::Kick;

use log::info;
use tokio_util::sync::CancellationToken;

/// Cancelled when the user quits (tray Quit) or the session ends (SIGTERM, SIGINT).
pub(crate) static QUIT: std::sync::LazyLock<CancellationToken> =
    std::sync::LazyLock::new(Default::default);

/// Stops the device sessions cleanly (desk mic back, volume and speakers restored), then
/// exits the app.
pub fn request_quit() {
    info!("Quitting");
    QUIT.cancel();
}

/// True once [`request_quit`] was called.
pub fn quitting() -> bool {
    QUIT.is_cancelled()
}

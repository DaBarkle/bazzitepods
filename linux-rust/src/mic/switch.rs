//! The mic switch: one task per AirPods session that moves the PC between the desk mic
//! and the AirPods mic.
//!
//! ```text
//! DESK --use_airpods_mic--> remember the desk mic (on disk, for crash recovery)
//!                      -> load the virtual mic -> start the decoder -> 0x58 START
//!                      -> make it the default mic -> pause Conversation Awareness
//!                      -> move apps still on another mic -> AIRPODS
//!                         (and every 2 s while on it, for apps that open a mic later)
//! AIRPODS --use_desk_mic--> desk mic default first -> move those apps back -> 0x58 STOP
//!                      -> unload the virtual mic -> restore Conversation Awareness
//!                      -> rebuild the music output -> DESK
//! ```
//! The music output is rebuilt after every stop: on AirPods 4 ANC the right pod went
//! silent after the stream stopped (2026-10-26 test), and only a rebuild brought it back.
//! A stream that stops on its own (the AirPods went to another device) is not restarted.

use super::packets::ELD_SAMPLE_RATE;
use super::stream::{Liveness, fifo_path, make_fifo};
use super::{MicRoute, SOURCE_NAME};
use crate::audio::pulse::MicSnapshot;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use tokio::time::sleep;

/// The PC side: the sound server and the decoder.
pub trait MicHost: Send + Sync + 'static {
    fn snapshot(&self) -> impl Future<Output = Result<MicSnapshot, String>> + Send;
    /// Loads the virtual mic reading `fifo`; returns the module index.
    fn load_virtual_mic(&self, fifo: &Path) -> impl Future<Output = Result<u32, String>> + Send;
    fn unload(&self, module: u32) -> impl Future<Output = Result<(), String>> + Send;
    fn set_default(&self, source: &str) -> impl Future<Output = Result<(), String>> + Send;
    fn move_recording(
        &self,
        recording: u32,
        source: &str,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Plays the chime for switching to `to`.
    fn chime(&self, to: MicRoute) -> impl Future<Output = ()> + Send;
    /// Starts decoding `packets` into `fifo`.
    fn start_decoder(
        &self,
        packets: mpsc::Receiver<Vec<u8>>,
        fifo: &Path,
        live: Liveness,
    ) -> Result<(), String>;
}

/// The AirPods side.
pub trait MicLink: Send + Sync + 'static {
    /// Sends 0x58 START and forwards the audio packets to `packets`.
    fn start_stream(
        &self,
        packets: mpsc::Sender<Vec<u8>>,
    ) -> impl Future<Output = Result<(), String>> + Send;
    /// Sends 0x58 STOP (if the link is still up) and stops forwarding.
    fn stop_stream(&self) -> impl Future<Output = ()> + Send;
    fn conversation_awareness(&self) -> impl Future<Output = Option<bool>> + Send;
    fn set_conversation_awareness(&self, on: bool) -> impl Future<Output = ()> + Send;
    /// Mutes the speakers before the mic stream stops, which drops the music output.
    fn hush_output(&self);
    /// Rebuilds the music output (audio guard).
    fn rebuild_output(&self);
}

#[derive(Debug, Clone)]
pub enum MicRequest {
    /// The left stem hold. `allowed` says why the AirPods mic can't be used right now.
    Toggle { allowed: Result<(), &'static str> },
    /// Back to the desk mic (feature turned off, AirPods put away or taken by another device).
    Desk(&'static str),
}

/// What the switch tells the rest of the app after each change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MicReport {
    pub route: MicRoute,
    /// Why the last attempt to use the AirPods mic failed or ended, if it did.
    pub problem: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SwitchConfig {
    /// Crash recovery: what to put back if Kase dies on the AirPods mic.
    pub state_path: PathBuf,
    pub fifo: PathBuf,
    /// Apps that follow the default mic move on their own; after this, the rest are moved.
    pub settle: Duration,
    /// No audio for this long means the AirPods stopped the stream.
    pub watchdog: Duration,
    /// How often to look for apps that started recording from a real mic since.
    pub sweep: Duration,
}

impl Default for SwitchConfig {
    fn default() -> Self {
        Self {
            state_path: state_path(),
            fifo: fifo_path(),
            settle: Duration::from_millis(1500),
            watchdog: Duration::from_secs(3),
            sweep: Duration::from_secs(2),
        }
    }
}

fn state_path() -> PathBuf {
    crate::utils::get_devices_path().with_file_name("mic_switch.json")
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Saved {
    desk_source: Option<String>,
    restore_conversation_awareness: bool,
}

fn load_saved(path: &Path) -> Option<Saved> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

fn save(path: &Path, saved: &Saved) {
    let json = serde_json::to_string(saved).unwrap_or_default();
    if let Err(e) = crate::ui::settings::write_atomic(path, &json) {
        warn!("[mic] could not save the mic state: {e}");
    }
}

struct Active {
    module: u32,
    desk: Option<String>,
    /// Apps that had to be moved by hand, and the mic each came from.
    moved: Vec<(u32, String)>,
    /// Apps that refused to move; not tried again.
    refused: Vec<u32>,
    restore_ca: bool,
    live: Liveness,
}

#[derive(Clone)]
pub struct MicSwitchHandle {
    tx: mpsc::UnboundedSender<Msg>,
}

enum Msg {
    Request(MicRequest),
    Shutdown(oneshot::Sender<()>),
}

impl MicSwitchHandle {
    pub fn request(&self, req: MicRequest) {
        let _ = self.tx.send(Msg::Request(req));
    }

    /// Back to the desk mic and stop. Waits for the cleanup.
    pub async fn shutdown(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Shutdown(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

pub fn spawn<H: MicHost, L: MicLink>(
    host: H,
    link: L,
    cfg: SwitchConfig,
    report: Arc<dyn Fn(MicReport) + Send + Sync>,
) -> MicSwitchHandle {
    let (tx, rx) = mpsc::unbounded_channel();
    let switch = Switch {
        host,
        link,
        cfg,
        report,
        active: None,
    };
    tokio::spawn(switch.run(rx));
    MicSwitchHandle { tx }
}

struct Switch<H, L> {
    host: H,
    link: L,
    cfg: SwitchConfig,
    report: Arc<dyn Fn(MicReport) + Send + Sync>,
    active: Option<Active>,
}

impl<H: MicHost, L: MicLink> Switch<H, L> {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Msg>) {
        self.recover().await;
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        let mut sweep = tokio::time::interval(self.cfg.sweep);
        sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    Some(Msg::Request(req)) => self.on_request(req).await,
                    Some(Msg::Shutdown(done)) => {
                        self.use_desk_mic(None).await;
                        let _ = done.send(());
                        return;
                    }
                    None => {
                        self.use_desk_mic(None).await;
                        return;
                    }
                },
                _ = tick.tick() => {
                    let stalled = self
                        .active
                        .as_ref()
                        .is_some_and(|a| a.live.quiet_for() > self.cfg.watchdog);
                    if stalled {
                        warn!("[mic] the AirPods stopped sending mic audio; back to the desk mic");
                        self.host.chime(MicRoute::Desk).await;
                        self.use_desk_mic(Some("The AirPods stopped sending mic audio".into())).await;
                    }
                }
                _ = sweep.tick(), if self.active.is_some() => self.move_stranded().await,
            }
        }
    }

    fn report(&self, problem: Option<String>) {
        let route = if self.active.is_some() {
            MicRoute::AirPods
        } else {
            MicRoute::Desk
        };
        (self.report)(MicReport { route, problem });
    }

    async fn on_request(&mut self, req: MicRequest) {
        match req {
            MicRequest::Toggle { .. } if self.active.is_some() => {
                // Before the music output is rebuilt, which would cut it off.
                self.host.chime(MicRoute::Desk).await;
                self.use_desk_mic(None).await;
            }
            MicRequest::Toggle { allowed: Err(why) } => {
                info!("[mic] not switching to the AirPods mic: {why}");
                self.report(Some(why.to_string()));
            }
            MicRequest::Toggle { allowed: Ok(()) } => {
                if let Err(e) = self.use_airpods_mic().await {
                    warn!("[mic] could not use the AirPods mic: {e}");
                    self.report(Some(e));
                }
            }
            MicRequest::Desk(why) => {
                if self.active.is_some() {
                    info!("[mic] back to the desk mic: {why}");
                    self.use_desk_mic(None).await;
                }
            }
        }
    }

    /// Undo what a crash (or a kill) left behind: the virtual mic, the default mic and
    /// Conversation Awareness.
    async fn recover(&mut self) {
        let saved = load_saved(&self.cfg.state_path);
        let Ok(snap) = self.host.snapshot().await else {
            return;
        };
        for m in snap
            .modules
            .iter()
            .filter(|m| m.argument.contains(SOURCE_NAME))
        {
            info!("[mic] removing a virtual mic left over from an earlier run");
            let _ = self.host.unload(m.index).await;
        }
        let Some(saved) = saved else {
            return;
        };
        if let Some(desk) = saved.desk_source.as_deref()
            && snap.source(desk).is_some()
            && snap.default_source.as_deref() != Some(desk)
        {
            info!("[mic] restoring {desk} as the default mic after an unclean exit");
            let _ = self.host.set_default(desk).await;
        }
        if saved.restore_conversation_awareness {
            self.link.set_conversation_awareness(true).await;
        }
        let _ = std::fs::remove_file(&self.cfg.state_path);
        let _ = std::fs::remove_file(&self.cfg.fifo);
    }

    async fn use_airpods_mic(&mut self) -> Result<(), String> {
        let snap = self.host.snapshot().await?;
        let desk = snap
            .default_source
            .clone()
            .filter(|s| s != SOURCE_NAME && snap.source(s).is_some_and(|x| !x.is_monitor));
        let restore_ca = self.link.conversation_awareness().await == Some(true);
        save(
            &self.cfg.state_path,
            &Saved {
                desk_source: desk.clone(),
                restore_conversation_awareness: restore_ca,
            },
        );
        let fifo = self.cfg.fifo.clone();
        if let Err(e) = make_fifo(&fifo) {
            self.forget(&fifo);
            return Err(format!("could not create the mic pipe: {e}"));
        }
        let module = match self.host.load_virtual_mic(&fifo).await {
            Ok(m) => m,
            Err(e) => {
                self.forget(&fifo);
                return Err(e);
            }
        };
        let (tx, rx) = mpsc::channel(64);
        let live = Liveness::new();
        let started = match self.host.start_decoder(rx, &fifo, live.clone()) {
            Ok(()) => self.link.start_stream(tx).await,
            Err(e) => Err(e),
        };
        if let Err(e) = started {
            self.link.stop_stream().await;
            let _ = self.host.unload(module).await;
            self.forget(&fifo);
            return Err(e);
        }
        if let Err(e) = self.host.set_default(SOURCE_NAME).await {
            warn!("[mic] could not make the AirPods mic the default: {e}");
        }
        if restore_ca {
            // Talking would otherwise turn your own music down.
            self.link.set_conversation_awareness(false).await;
        }
        self.active = Some(Active {
            module,
            desk: desk.clone(),
            moved: Vec::new(),
            refused: Vec::new(),
            restore_ca,
            live,
        });
        info!(
            "[mic] AirPods mic on ({} Hz mono), desk mic was {:?}",
            ELD_SAMPLE_RATE, desk
        );
        self.report(None);
        self.host.chime(MicRoute::AirPods).await;

        // Apps that follow the default mic have moved by now; move the ones pinned to a mic.
        sleep(self.cfg.settle).await;
        self.move_stranded().await;
        Ok(())
    }

    /// Moves apps still recording from another mic to the AirPods mic, remembering where
    /// each came from: apps set to a mic by name, and apps on a virtual mic such as
    /// EasyEffects' (EasyEffects records the desk mic itself, so its mic never changes).
    async fn move_stranded(&mut self) {
        let Ok(snap) = self.host.snapshot().await else {
            return;
        };
        let Some(active) = self.active.as_ref() else {
            return;
        };
        let (mut moved, mut failed) = (Vec::new(), Vec::new());
        for (rec, source) in snap.recordings_on_mics(SOURCE_NAME) {
            if active.refused.contains(&rec) {
                continue;
            }
            let app = snap.app_of(rec).unwrap_or("an app");
            if active.moved.iter().any(|(r, _)| *r == rec) {
                // Moved once and still (or again) on a mic: something pins it there.
                // Moving it every sweep would flip it between the two mics.
                warn!("[mic] {app} (recording {rec}) stays on {source}; leaving it there");
                failed.push(rec);
                continue;
            }
            match self.host.move_recording(rec, SOURCE_NAME).await {
                Ok(()) => {
                    info!("[mic] moved {app} (recording {rec}) from {source} to the AirPods mic");
                    moved.push((rec, source));
                }
                Err(e) => {
                    warn!("[mic] could not move {app} (recording {rec}) off {source}: {e}");
                    failed.push(rec);
                }
            }
        }
        if let Some(a) = self.active.as_mut() {
            a.moved.extend(moved);
            a.refused.extend(failed);
        }
    }

    async fn use_desk_mic(&mut self, problem: Option<String>) {
        let Some(active) = self.active.take() else {
            return;
        };
        // The desk mic first, so nothing that records falls back to the AirPods' call mic.
        if let Some(desk) = active.desk.as_deref()
            && let Err(e) = self.host.set_default(desk).await
        {
            warn!("[mic] could not make {desk} the default again: {e}");
        }
        if !active.moved.is_empty()
            && let Ok(snap) = self.host.snapshot().await
        {
            let ours = snap.recordings_on(SOURCE_NAME);
            for (rec, source) in active.moved.iter().filter(|(r, _)| ours.contains(r)) {
                if let Err(e) = self.host.move_recording(*rec, source).await {
                    warn!("[mic] could not move recording {rec} back: {e}");
                }
            }
        }
        self.link.hush_output();
        self.link.stop_stream().await;
        if let Err(e) = self.host.unload(active.module).await {
            warn!("[mic] could not remove the virtual mic: {e}");
        }
        if active.restore_ca {
            self.link.set_conversation_awareness(true).await;
        }
        self.link.rebuild_output();
        self.forget(&self.cfg.fifo.clone());
        info!("[mic] desk mic on");
        self.report(problem);
    }

    fn forget(&self, fifo: &Path) {
        let _ = std::fs::remove_file(fifo);
        let _ = std::fs::remove_file(&self.cfg.state_path);
    }
}

#[cfg(test)]
#[path = "switch_tests.rs"]
mod tests;

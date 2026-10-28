//! Spawns and feeds the `kase-overlay` process (JSON lines on its stdin,
//! see `docs/ARCHITECTURE.md`, "Overlay protocol").
//!
//! Every public method only pushes onto a channel, so callers (UI thread or
//! tokio tasks) never block. A dedicated supervisor thread owns the child:
//! it writes lines, restarts the child with backoff if it crashes, replays the
//! last config and state after a restart, and gives up for good when the child
//! exits with code 3 (no layer-shell, e.g. GNOME), so the app can fall back to
//! desktop notifications (check [`OverlayClient::unsupported`]).

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use log::{debug, info, warn};

use crate::overlay_protocol::{EXIT_UNSUPPORTED, OverlayCommand, OverlayConfig, OverlayState};

pub const OVERLAY_BIN: &str = "kase-overlay";

/// At most this many (re)starts within `RESTART_WINDOW`.
const MAX_STARTS: usize = 5;
const RESTART_WINDOW: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(500);

enum Msg {
    Cmd(OverlayCommand),
    Shutdown,
}

pub struct OverlayClient {
    tx: Sender<Msg>,
    unsupported: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl OverlayClient {
    /// Starts the supervisor thread, which launches the overlay process.
    /// Never blocks and never fails; check [`Self::unsupported`] later.
    pub fn spawn(config: OverlayConfig) -> OverlayClient {
        Self::spawn_with_binary(config, find_binary())
    }

    fn spawn_with_binary(config: OverlayConfig, binary: Option<PathBuf>) -> OverlayClient {
        let (tx, rx) = mpsc::channel();
        let unsupported = Arc::new(AtomicBool::new(false));
        let sup = Supervisor {
            binary,
            config,
            state: None,
            fullscreen: false,
            child: None,
            stdin: None,
            starts: VecDeque::new(),
            next_start: Instant::now(),
            backoff: Duration::from_secs(1),
            unsupported: unsupported.clone(),
            late_retries: 0,
            quick_exits: 0,
            gave_up: false,
            started_at: None,
        };
        let thread = std::thread::Builder::new()
            .name("overlay-client".into())
            .spawn(move || sup.run(rx));
        let thread = match thread {
            Ok(t) => Some(t),
            Err(e) => {
                warn!("overlay: cannot start supervisor thread: {e}");
                unsupported.store(true, Ordering::SeqCst);
                None
            }
        };
        OverlayClient {
            tx,
            unsupported,
            thread,
        }
    }

    fn send(&self, cmd: OverlayCommand) {
        // Fails only after shutdown; nothing to do then.
        let _ = self.tx.send(Msg::Cmd(cmd));
    }

    /// Sends the latest battery state. The overlay decides itself whether the
    /// change is worth showing (in `auto` mode).
    pub fn update(&self, state: OverlayState) {
        self.send(OverlayCommand::State { state });
    }

    /// Pops the overlay up for `auto_hide_secs` (in `auto` mode).
    pub fn show(&self, reason: &str) {
        self.send(OverlayCommand::Show {
            reason: reason.to_string(),
        });
    }

    pub fn hide(&self) {
        self.send(OverlayCommand::Hide);
    }

    pub fn set_fullscreen(&self, active: bool) {
        self.send(OverlayCommand::Fullscreen { active });
    }

    pub fn set_config(&self, config: OverlayConfig) {
        self.send(OverlayCommand::Config(config));
    }

    /// True once the overlay can't run here (exit code 3: no layer-shell or no
    /// Wayland, or the binary is missing). Use desktop notifications instead.
    pub fn unsupported(&self) -> bool {
        self.unsupported.load(Ordering::SeqCst)
    }

    /// Asks the overlay to quit and stops the supervisor. Does not block.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

impl Drop for OverlayClient {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(t) = self.thread.take() {
            // The supervisor exits within ~1 s (graceful quit, then kill).
            let _ = t.join();
        }
    }
}

/// Next to our own executable first (Flatpak: /app/bin), then `$PATH`.
fn find_binary() -> Option<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(OVERLAY_BIN)));
    if let Some(p) = sibling.filter(|p| p.is_file()) {
        return Some(p);
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(OVERLAY_BIN))
            .find(|p| p.is_file())
    })
}

/// Restart policy: allowed if fewer than `MAX_STARTS` starts happened in the window.
fn may_start(starts: &mut VecDeque<Instant>, now: Instant) -> bool {
    while starts
        .front()
        .is_some_and(|t| now.duration_since(*t) > RESTART_WINDOW)
    {
        starts.pop_front();
    }
    starts.len() < MAX_STARTS
}

struct Supervisor {
    binary: Option<PathBuf>,
    config: OverlayConfig,
    state: Option<OverlayState>,
    fullscreen: bool,
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    starts: VecDeque<Instant>,
    next_start: Instant,
    backoff: Duration,
    unsupported: Arc<AtomicBool>,
    /// Retries after an "unsupported" exit. At login autostart the compositor socket may not be
    /// ready yet, which looks the same as a compositor without layer-shell.
    late_retries: u8,
    /// Crashes in a row within `QUICK_EXIT` of starting (a GPU or Vulkan setup failure).
    quick_exits: u8,
    /// No more starts: the overlay can't run here, so notifications stand in for it.
    gave_up: bool,
    started_at: Option<Instant>,
}

const LATE_RETRIES: u8 = 2;
const LATE_RETRY_DELAY: Duration = Duration::from_secs(30);
/// An overlay that dies this soon after starting never got going.
const QUICK_EXIT: Duration = Duration::from_secs(5);
/// Quick crashes in a row after which the overlay is treated as unsupported.
const MAX_QUICK_EXITS: u8 = 3;

impl Supervisor {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        let Some(binary) = self.binary.clone() else {
            warn!("overlay: {OVERLAY_BIN} not found next to the app or in PATH; overlay disabled");
            self.unsupported.store(true, Ordering::SeqCst);
            return;
        };
        loop {
            if self.gave_up {
                break;
            }
            // A late retry that stays up means the overlay works after all.
            if self.child.is_some() && self.started_at.is_some_and(|t| t.elapsed() > QUICK_EXIT) {
                self.unsupported.store(false, Ordering::SeqCst);
            }
            if self.child.is_none() && Instant::now() >= self.next_start {
                self.start(&binary);
            }
            match rx.recv_timeout(POLL) {
                Ok(Msg::Cmd(cmd)) => {
                    // Remember what a restarted child needs to catch up.
                    match &cmd {
                        OverlayCommand::Config(c) => self.config = c.clone(),
                        OverlayCommand::State { state } => self.state = Some(state.clone()),
                        OverlayCommand::Fullscreen { active } => self.fullscreen = *active,
                        _ => {}
                    }
                    self.write(&cmd);
                }
                Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.reap();
        }
        self.stop_child();
    }

    fn start(&mut self, binary: &PathBuf) {
        let now = Instant::now();
        if !may_start(&mut self.starts, now) {
            // Too many crashes: wait until the oldest start leaves the window.
            let wait = self
                .starts
                .front()
                .map(|t| RESTART_WINDOW.saturating_sub(now.duration_since(*t)))
                .unwrap_or(RESTART_WINDOW);
            warn!("overlay: crashing repeatedly, pausing restarts for {wait:?}");
            self.next_start = now + wait;
            return;
        }
        self.starts.push_back(now);
        let spawned = Command::new(binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn();
        match spawned {
            Ok(mut child) => {
                info!("overlay: started {} (pid {})", binary.display(), child.id());
                self.stdin = child.stdin.take();
                self.child = Some(child);
                self.started_at = Some(Instant::now());
                let replay = [
                    Some(OverlayCommand::Config(self.config.clone())),
                    self.state
                        .clone()
                        .map(|state| OverlayCommand::State { state }),
                    self.fullscreen
                        .then_some(OverlayCommand::Fullscreen { active: true }),
                ];
                for cmd in replay.into_iter().flatten() {
                    self.write(&cmd);
                }
            }
            Err(e) => {
                warn!("overlay: failed to start {}: {e}", binary.display());
                self.schedule_restart();
            }
        }
    }

    fn schedule_restart(&mut self) {
        self.next_start = Instant::now() + self.backoff;
        self.backoff = (self.backoff * 2).min(Duration::from_secs(30));
    }

    fn write(&mut self, cmd: &OverlayCommand) {
        let Some(stdin) = self.stdin.as_mut() else {
            return;
        };
        let line = cmd.to_line();
        if let Err(e) = stdin.write_all(line.as_bytes()).and_then(|_| stdin.flush()) {
            debug!("overlay: write failed ({e}); child probably exited");
            self.stdin = None;
            self.reap();
        }
    }

    /// Notices a dead child and decides whether to restart it.
    fn reap(&mut self) {
        let Some(child) = self.child.as_mut() else {
            return;
        };
        let status = match child.try_wait() {
            Ok(Some(status)) => status,
            Ok(None) => {
                // Still alive. A healthy run resets the backoff.
                if self
                    .starts
                    .back()
                    .is_some_and(|t| t.elapsed() > RESTART_WINDOW)
                {
                    self.backoff = Duration::from_secs(1);
                }
                return;
            }
            Err(e) => {
                warn!("overlay: cannot query child: {e}");
                return;
            }
        };
        self.child = None;
        self.stdin = None;
        if status.code() == Some(EXIT_UNSUPPORTED) {
            info!("overlay: not supported by this compositor; falling back to notifications");
            self.unsupported.store(true, Ordering::SeqCst);
            if self.late_retries < LATE_RETRIES {
                self.late_retries += 1;
                self.next_start = Instant::now() + LATE_RETRY_DELAY;
            } else {
                self.gave_up = true;
            }
            return;
        }
        if self.started_at.is_some_and(|t| t.elapsed() < QUICK_EXIT) {
            self.quick_exits += 1;
        } else {
            self.quick_exits = 0;
        }
        if self.quick_exits >= MAX_QUICK_EXITS {
            warn!(
                "overlay: crashed {} times right after starting ({status}); falling back to notifications",
                self.quick_exits
            );
            self.unsupported.store(true, Ordering::SeqCst);
            self.gave_up = true;
            return;
        }
        warn!(
            "overlay: exited ({status}); restarting in {:?}",
            self.backoff
        );
        self.schedule_restart();
    }

    fn stop_child(&mut self) {
        if self.stdin.is_some() {
            self.write(&OverlayCommand::Quit);
        }
        // Closing stdin also makes the overlay exit (EOF).
        self.stdin = None;
        if let Some(mut child) = self.child.take() {
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(20))
                    }
                    _ => {
                        let _ = child.kill();
                        let _ = child.wait();
                        break;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn script(name: &str, body: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kase-overlay-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join(name);
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn wait_for(mut f: impl FnMut() -> bool) -> bool {
        let end = Instant::now() + Duration::from_secs(5);
        while Instant::now() < end {
            if f() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    #[test]
    fn restart_window() {
        let mut q = VecDeque::new();
        let t0 = Instant::now();
        for i in 0..MAX_STARTS {
            assert!(may_start(&mut q, t0 + Duration::from_secs(i as u64)));
            q.push_back(t0 + Duration::from_secs(i as u64));
        }
        assert!(!may_start(&mut q, t0 + Duration::from_secs(10)));
        assert!(may_start(
            &mut q,
            t0 + RESTART_WINDOW + Duration::from_secs(1)
        ));
    }

    #[test]
    fn exit_code_3_marks_unsupported() {
        let bin = script("unsupported", "exit 3");
        let c = OverlayClient::spawn_with_binary(OverlayConfig::default(), Some(bin));
        assert!(wait_for(|| c.unsupported()));
        // Calls after that are harmless no-ops.
        c.show("x");
        c.update(OverlayState::default());
    }

    #[test]
    fn unsupported_gets_every_late_retry() {
        let log = std::env::temp_dir().join(format!("kase-overlay-late-{}", std::process::id()));
        let _ = std::fs::remove_file(&log);
        let bin = script("late", &format!("echo run >> '{}'; exit 3", log.display()));
        let mut sup = Supervisor {
            binary: Some(bin.clone()),
            config: OverlayConfig::default(),
            state: None,
            fullscreen: false,
            child: None,
            stdin: None,
            starts: VecDeque::new(),
            next_start: Instant::now(),
            backoff: Duration::from_secs(1),
            unsupported: Arc::new(AtomicBool::new(false)),
            late_retries: 0,
            quick_exits: 0,
            gave_up: false,
            started_at: None,
        };
        // The first run plus LATE_RETRIES retries, then no more.
        for _ in 0..=LATE_RETRIES {
            assert!(!sup.gave_up);
            sup.start(&bin);
            assert!(wait_for(|| {
                sup.reap();
                sup.child.is_none()
            }));
        }
        assert!(sup.gave_up);
        let runs = std::fs::read_to_string(&log).unwrap().lines().count();
        assert_eq!(runs, 1 + LATE_RETRIES as usize);
    }

    #[test]
    fn crash_loop_falls_back_to_notifications() {
        // E.g. no Vulkan: the overlay dies at once with a plain error.
        let bin = script("crashloop", "exit 1");
        let c = OverlayClient::spawn_with_binary(OverlayConfig::default(), Some(bin));
        let end = Instant::now() + Duration::from_secs(10);
        while !c.unsupported() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(c.unsupported());
    }

    #[test]
    fn missing_binary_marks_unsupported() {
        let c = OverlayClient::spawn_with_binary(OverlayConfig::default(), None);
        assert!(wait_for(|| c.unsupported()));
    }

    #[test]
    fn writes_lines_and_replays_after_crash() {
        let out = std::env::temp_dir().join(format!("kase-overlay-out-{}", std::process::id()));
        let _ = std::fs::remove_file(&out);
        // Each instance appends what it reads; the first one dies after 2 lines.
        let bin = script(
            "fake",
            &format!(
                "n=0; while read -r l; do echo \"$l\" >> '{}'; n=$((n+1)); [ ! -e '{}.crashed' ] && [ $n -ge 3 ] && touch '{}.crashed' && exit 1; done; exit 0",
                out.display(),
                out.display(),
                out.display()
            ),
        );
        let _ = std::fs::remove_file(format!("{}.crashed", out.display()));
        let c = OverlayClient::spawn_with_binary(OverlayConfig::default(), Some(bin));
        let state = OverlayState {
            name: "Pods".into(),
            connected: true,
            ..Default::default()
        };
        c.update(state.clone());
        c.show("connected");
        // First child: config, state, show -> crashes. Second child gets config + state replayed.
        assert!(wait_for(|| {
            std::fs::read_to_string(&out)
                .map(|s| s.lines().count() >= 5)
                .unwrap_or(false)
        }));
        let lines: Vec<OverlayCommand> = std::fs::read_to_string(&out)
            .unwrap()
            .lines()
            .filter_map(|l| OverlayCommand::parse_line(l).ok().flatten())
            .collect();
        assert_eq!(lines[0], OverlayCommand::Config(OverlayConfig::default()));
        assert_eq!(
            lines[1],
            OverlayCommand::State {
                state: state.clone()
            }
        );
        assert_eq!(
            lines[2],
            OverlayCommand::Show {
                reason: "connected".into()
            }
        );
        assert_eq!(lines[3], OverlayCommand::Config(OverlayConfig::default()));
        assert_eq!(lines[4], OverlayCommand::State { state });
        assert!(!c.unsupported());
        c.shutdown();
        drop(c);
        let s = std::fs::read_to_string(&out).unwrap();
        assert!(s.lines().last().unwrap().contains("quit"));
    }
}

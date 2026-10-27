//! From AirPods packets to the virtual mic: decode, gain, and write into the pipe that
//! PipeWire's pipe source reads.

use super::agc::{Agc, to_s16};
use super::eld::Decoder;
use super::packets::{ELD_FRAME_SAMPLES, access_units};
use log::{info, warn};
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Size of the pipe between us and PipeWire: 128 ms of audio. Writes that don't fit are
/// dropped, so a reader that falls behind costs a glitch, never growing delay.
const PIPE_BYTES: libc::c_int = 16 * 1024;

/// Where the pipe lives. Inside the Flatpak, `$XDG_RUNTIME_DIR/app/<app id>/` is the one
/// folder of ours that PipeWire, outside the sandbox, sees at the same path; only this
/// user can open it.
pub fn fifo_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    match std::env::var("FLATPAK_ID") {
        Ok(id) if !id.is_empty() => runtime.join("app").join(id).join("airpods-mic.fifo"),
        _ => runtime.join("bazzitepods-airpods-mic.fifo"),
    }
}

/// Makes a fresh pipe at `path` (owner-only).
pub fn make_fifo(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(ErrorKind::InvalidInput, e))?;
    // SAFETY: a valid C path string.
    if unsafe { libc::mkfifo(c.as_ptr(), 0o600) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// Opens the pipe for writing without ever blocking. Read+write so the open succeeds
/// before PipeWire has it open, and PipeWire never sees the writer go away.
fn open_writer(path: &Path) -> std::io::Result<File> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
    use std::os::fd::AsRawFd;
    // SAFETY: fcntl on a file descriptor we own. Failure only leaves the default size.
    unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_BYTES) };
    Ok(file)
}

/// When the last audio packet arrived, shared with the switch's watchdog.
#[derive(Clone)]
pub struct Liveness {
    start: Instant,
    last_ms: Arc<AtomicU64>,
}

impl Liveness {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
            last_ms: Arc::new(AtomicU64::new(0)),
        }
    }

    pub fn mark(&self) {
        let ms = self.start.elapsed().as_millis() as u64;
        self.last_ms.store(ms, Ordering::Relaxed);
    }

    /// Time since the last packet, or since this was created if none came yet.
    pub fn quiet_for(&self) -> Duration {
        let last = Duration::from_millis(self.last_ms.load(Ordering::Relaxed));
        self.start.elapsed().saturating_sub(last)
    }
}

/// Spawns the decode thread. It ends when the packet channel closes.
pub fn spawn(
    mut packets: mpsc::Receiver<Vec<u8>>,
    mut decoder: Decoder,
    fifo: &Path,
    live: Liveness,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    let mut out = open_writer(fifo)?;
    std::thread::Builder::new()
        .name("airpods-mic".into())
        .spawn(move || {
            let mut agc = Agc::new();
            let mut pcm: Vec<f32> = Vec::with_capacity(4 * ELD_FRAME_SAMPLES);
            let mut bytes: Vec<u8> = Vec::with_capacity(8 * ELD_FRAME_SAMPLES);
            let (mut frames, mut errors, mut dropped) = (0u64, 0u64, 0u64);
            while let Some(packet) = packets.blocking_recv() {
                live.mark();
                pcm.clear();
                for unit in access_units(&packet) {
                    match decoder.decode(unit, &mut pcm) {
                        Ok(_) => frames += 1,
                        Err(e) => {
                            if errors < 3 {
                                warn!("[mic] frame not decoded: {e}");
                            }
                            // Keep the timing: a lost frame becomes silence.
                            errors += 1;
                            pcm.resize(pcm.len() + ELD_FRAME_SAMPLES, 0.0);
                        }
                    }
                }
                agc.process(&mut pcm);
                bytes.clear();
                bytes.extend(pcm.iter().flat_map(|s| to_s16(*s).to_le_bytes()));
                match out.write(&bytes) {
                    Ok(n) if n < bytes.len() => dropped += 1,
                    Ok(_) => {}
                    Err(e) if e.kind() == ErrorKind::WouldBlock => dropped += 1,
                    Err(e) => {
                        warn!("[mic] the virtual mic's pipe broke: {e}");
                        break;
                    }
                }
                if frames > 0 && frames % 8000 == 0 {
                    info!("[mic] {frames} frames decoded, {errors} damaged, {dropped} writes dropped");
                }
            }
            info!("[mic] stream ended: {frames} frames decoded, {errors} damaged, {dropped} writes dropped");
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn pipe_never_blocks_the_writer() {
        let dir = std::env::temp_dir().join(format!("bp-mic-{}", std::process::id()));
        let path = dir.join("t.fifo");
        make_fifo(&path).unwrap();
        let mut w = open_writer(&path).unwrap();
        // Nobody reads: writes past the pipe size are refused, not waited on.
        let chunk = vec![0u8; 4096];
        let mut refused = false;
        for _ in 0..64 {
            match w.write(&chunk) {
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    refused = true;
                    break;
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(refused);
        // A reader gets what was written.
        let mut r = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&path)
            .unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(r.read(&mut buf).unwrap(), 16);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn liveness_tracks_the_last_packet() {
        let live = Liveness::new();
        std::thread::sleep(Duration::from_millis(30));
        assert!(live.quiet_for() >= Duration::from_millis(25));
        live.mark();
        assert!(live.quiet_for() < Duration::from_millis(25));
    }

    #[test]
    fn fifo_lives_in_the_flatpak_runtime_folder() {
        let p = fifo_path();
        assert!(p.to_string_lossy().ends_with(".fifo"));
    }
}

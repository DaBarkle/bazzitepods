//! A short chime when the mic changes: rising for the AirPods mic, falling for the desk mic.

use super::MicRoute;
use log::warn;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

const RATE: u32 = 48_000;
const E5: f32 = 659.25;
const B5: f32 = 987.77;

/// Two soft bell-like notes as s16le mono at `RATE`.
fn pcm(to: MicRoute) -> Vec<u8> {
    let notes = match to {
        MicRoute::AirPods => [E5, B5],
        MicRoute::Desk => [B5, E5],
    };
    let note_len = RATE as usize * 22 / 100;
    let gap = RATE as usize * 9 / 100;
    let mut out = vec![0f32; gap + note_len];
    for (n, f) in notes.iter().enumerate() {
        for i in 0..note_len {
            let t = i as f32 / RATE as f32;
            let attack = (t / 0.005).min(1.0);
            let env = attack * (-t * 14.0).exp();
            // A quiet octave on top gives it a bell tone rather than a beep.
            let w = std::f32::consts::TAU * f * t;
            out[n * gap + i] += 0.22 * env * (w.sin() + 0.25 * (2.0 * w).sin());
        }
    }
    out.iter()
        .flat_map(|s| ((s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16).to_le_bytes())
        .collect()
}

/// Plays the chime on the default output and waits for it (at most a second).
pub async fn play(to: MicRoute) {
    let child = Command::new("paplay")
        .args(["--raw", "--format=s16le", "--channels=1", "--client-name=BazzitePods"])
        .arg(format!("--rate={RATE}"))
        .arg("--property=media.role=event")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match child {
        Ok(c) => c,
        Err(e) => {
            warn!("[mic] could not play the switch chime: {e}");
            return;
        }
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(&pcm(to)).await;
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chimes_differ_and_fit() {
        let up = pcm(MicRoute::AirPods);
        let down = pcm(MicRoute::Desk);
        assert_eq!(up.len(), down.len());
        assert_ne!(up, down);
        assert!(up.len() / 2 < RATE as usize / 2, "under half a second");
        let peak = up
            .chunks(2)
            .map(|b| i16::from_le_bytes([b[0], b[1]]).unsigned_abs())
            .max()
            .unwrap();
        assert!(peak > 1000 && peak < i16::MAX as u16 / 2, "audible but soft: {peak}");
    }
}

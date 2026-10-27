//! The AirPods mic switch: hold the left stem to swap between the desk mic and the
//! AirPods' own high-quality mic, while music keeps playing in full quality.
//!
//! The AirPods mic normally means the Bluetooth headset (call) profile, which drops music
//! to phone quality. Apple devices instead ask the AirPods for a separate AAC-ELD stream
//! over the AACP control channel (message 0x58); LibrePods PR #655 worked out how, and it
//! was confirmed on AirPods 4 ANC on 2026-10-26. Here that stream is decoded and offered
//! as a virtual mic, "AirPods mic", which becomes the default while it is selected.
//!
//! - `packets`: the 0x58 messages. `eld`: the decoder (the runtime's FFmpeg).
//! - `agc`, `stream`: gain, and the pipe into PipeWire.
//! - `switch`: the state machine (which mic, crash recovery, the music rebuild).
//! - `gestures`: which stem presses are claimed, and the right-hold noise control cycle.
//!
//! The stream runs only while the AirPods mic is selected: while it runs the AirPods
//! lower the music quality themselves (measured: the PC kept sending full quality).

pub mod agc;
pub mod eld;
pub mod gestures;
pub mod packets;
pub mod stream;
pub mod switch;

use crate::audio::AudioGuardHandle;
use crate::audio::pulse::{self, DEFAULT_DEADLINE, MicSnapshot};
use crate::bluetooth::aacp::{AACPManager, ControlCommandIdentifiers};
use serde::Serialize;
use std::path::Path;
use std::sync::OnceLock;
use stream::Liveness;
use switch::{MicHost, MicLink};
use tokio::sync::{broadcast, mpsc};

/// Name of the virtual mic in PipeWire.
pub const SOURCE_NAME: &str = "bazzitepods_airpods_mic";

/// Which mic the PC is using.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum MicRoute {
    /// Whatever was the default mic before (the desk mic).
    Desk,
    AirPods,
}

/// Requests from the window and the tray ("use the AirPods mic").
pub fn request_toggle() {
    let _ = ui_requests().send(());
}

pub fn subscribe_requests() -> broadcast::Receiver<()> {
    ui_requests().subscribe()
}

fn ui_requests() -> &'static broadcast::Sender<()> {
    static TX: OnceLock<broadcast::Sender<()>> = OnceLock::new();
    TX.get_or_init(|| broadcast::channel(4).0)
}

/// The sound server and decoder, for real.
pub struct PulseMicHost;

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, pulse::PulseError> + Send + 'static,
) -> Result<T, String> {
    match tokio::task::spawn_blocking(f).await {
        Ok(r) => r.map_err(|e| e.to_string()),
        Err(e) => Err(format!("blocking task failed: {e}")),
    }
}

/// Arguments for PipeWire's pipe source. The second form avoids the quoted space in
/// the name, in case a sound server doesn't take quotes.
fn pipe_source_args(fifo: &Path) -> [String; 2] {
    let common = format!(
        "source_name={SOURCE_NAME} file={} format=s16le rate={} channels=1 channel_map=mono",
        fifo.display(),
        packets::ELD_SAMPLE_RATE
    );
    // Not a graph driver, and never picked as the default by WirePlumber on its own.
    let props = "node.driver=false priority.driver=0 priority.session=0";
    [
        format!("{common} source_properties='device.description=\"AirPods mic\" {props}'"),
        format!("{common} source_properties='device.description=AirPods_mic {props}'"),
    ]
}

impl MicHost for PulseMicHost {
    async fn snapshot(&self) -> Result<MicSnapshot, String> {
        blocking(|| pulse::mic_snapshot_sync(DEFAULT_DEADLINE)).await
    }

    async fn load_virtual_mic(&self, fifo: &Path) -> Result<u32, String> {
        let mut last = String::new();
        for args in pipe_source_args(fifo) {
            match blocking(move || {
                pulse::load_module_sync("module-pipe-source", &args, DEFAULT_DEADLINE)
            })
            .await
            {
                Ok(index) => return Ok(index),
                Err(e) => last = e,
            }
        }
        Err(format!(
            "the sound server would not create the AirPods mic ({last})"
        ))
    }

    async fn unload(&self, module: u32) -> Result<(), String> {
        blocking(move || pulse::unload_module_sync(module, DEFAULT_DEADLINE)).await
    }

    async fn set_default(&self, source: &str) -> Result<(), String> {
        let source = source.to_string();
        blocking(move || pulse::set_default_source_sync(&source, DEFAULT_DEADLINE)).await
    }

    async fn move_recording(&self, recording: u32, source: &str) -> Result<(), String> {
        let source = source.to_string();
        blocking(move || pulse::move_source_output_sync(recording, &source, DEFAULT_DEADLINE)).await
    }

    fn start_decoder(
        &self,
        packets: mpsc::Receiver<Vec<u8>>,
        fifo: &Path,
        live: Liveness,
    ) -> Result<(), String> {
        let decoder = eld::Decoder::open(&packets::ELD_ASC, packets::ELD_ASC_RATE)
            .map_err(|e| format!("the AirPods mic needs FFmpeg's AAC decoder: {e}"))?;
        stream::spawn(packets, decoder, fifo, live)
            .map(|_| ())
            .map_err(|e| format!("could not start the mic decoder: {e}"))
    }
}

/// The AirPods end, over the session's control channel.
pub struct AacpMicLink {
    pub aacp: AACPManager,
    pub audio: Option<AudioGuardHandle>,
}

impl MicLink for AacpMicLink {
    async fn start_stream(&self, packets: mpsc::Sender<Vec<u8>>) -> Result<(), String> {
        self.aacp.set_mic_sink(Some(packets)).await;
        if let Err(e) = self.aacp.send_mic_start().await {
            self.aacp.set_mic_sink(None).await;
            return Err(format!("the AirPods did not take the mic request: {e}"));
        }
        Ok(())
    }

    async fn stop_stream(&self) {
        if let Err(e) = self.aacp.send_mic_stop().await {
            log::debug!("[mic] STOP not sent: {e}");
        }
        self.aacp.set_mic_sink(None).await;
    }

    async fn conversation_awareness(&self) -> Option<bool> {
        self.aacp
            .control_value(ControlCommandIdentifiers::ConversationDetectConfig)
            .await
            .and_then(|v| v.first().copied())
            .map(|b| b == 0x01)
    }

    async fn set_conversation_awareness(&self, on: bool) {
        let value = if on { 0x01 } else { 0x02 };
        if let Err(e) = self
            .aacp
            .send_control_command(
                ControlCommandIdentifiers::ConversationDetectConfig,
                &[value],
            )
            .await
        {
            log::warn!("[mic] could not set Conversation Awareness: {e}");
        }
    }

    fn rebuild_output(&self) {
        crate::devices::airpods::rebuild_output(self.audio.as_ref());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipe_source_arguments() {
        let [quoted, plain] = pipe_source_args(Path::new("/run/user/1000/app/x/airpods-mic.fifo"));
        assert!(quoted.starts_with(
            "source_name=bazzitepods_airpods_mic file=/run/user/1000/app/x/airpods-mic.fifo format=s16le rate=64000 channels=1"
        ));
        assert!(quoted.contains("device.description=\"AirPods mic\""));
        assert!(plain.contains("device.description=AirPods_mic node.driver=false"));
        assert!(!plain.contains('"'));
    }

    #[tokio::test]
    async fn ui_requests_reach_subscribers() {
        let mut rx = subscribe_requests();
        request_toggle();
        assert!(rx.recv().await.is_ok());
    }
}

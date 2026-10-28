//! Muting the speakers while the route is rebuilt, and putting streams back afterwards.

use super::*;

impl<P: PulseApi, B: BluezApi> Guard<P, B> {
    pub(super) fn remember_default(&mut self, value: bool) {
        self.we_were_default = value;
        let mac = self.mac_str.clone();
        with_default_memory(|m| {
            if let Ok(mut m) = m.lock() {
                m.insert(mac, value);
            }
        });
    }

    /// Streams currently playing to this device's sink (to bring back after a profile toggle).
    pub(super) async fn our_streams(&self) -> Vec<u32> {
        let Ok(snap) = self.pulse.snapshot().await else {
            return Vec::new();
        };
        let Some(sink) = snap
            .card_for(&self.mac_str)
            .and_then(|c| snap.sink_for_card(c))
        else {
            return Vec::new();
        };
        snap.sink_inputs
            .iter()
            .filter(|i| i.sink == sink.index)
            .map(|i| i.index)
            .collect()
    }

    /// Mutes the other hardware outputs (speakers, HDMI) for a rebuild: while ours is
    /// gone, WirePlumber parks every stream on one of them. Outputs stay put while the
    /// streams get re-created, so unmuting them afterwards is reliable, and no app's own
    /// saved mute state is touched. Outputs already muted are left alone.
    pub(super) async fn hush(&mut self) {
        let Ok(snap) = self.pulse.snapshot().await else {
            return;
        };
        for sink in snap.sinks.iter().filter(|s| {
            s.card.is_some() && s.mac.as_deref() != Some(self.mac_str.as_str()) && !s.muted
        }) {
            match self.pulse.set_sink_mute(sink.index, true).await {
                Ok(()) => {
                    self.hushed.push(sink.index);
                    self.hushed_names.push(sink.name.clone());
                }
                Err(e) => debug!(
                    "[audio {}] mute output {} failed: {}",
                    self.mac_str, sink.name, e
                ),
            }
        }
        // shortcut: one file shared by all guards; two AirPods rebuilding at once could
        // drop the other's names. Key it by MAC if that ever happens in practice.
        if let Some(file) = self.cfg.hush_file.clone()
            && !self.hushed_names.is_empty()
            && let Ok(json) = serde_json::to_string(&self.hushed_names)
        {
            let _ =
                tokio::task::spawn_blocking(move || crate::store::write_atomic(&file, &json)).await;
        }
    }

    /// Unmutes what [`Self::hush`] muted, once nothing plays on those outputs any more:
    /// WirePlumber moves the streams back a moment after the new output appears, and
    /// unmuting before that lets the tail end play on the speakers.
    pub(super) async fn unhush(&mut self) {
        if self.hushed.is_empty() {
            self.hush_until = None;
            return;
        }
        let deadline = Instant::now() + UNHUSH_WAIT;
        loop {
            let busy = match self.pulse.snapshot().await {
                Ok(snap) => snap
                    .sinks
                    .iter()
                    .any(|s| self.hushed.contains(&s.index) && s.state == SinkState::Running),
                Err(_) => false,
            };
            if !busy {
                // A little longer for the last buffered audio on its way out.
                sleep(UNHUSH_MARGIN).await;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            sleep(self.cfg.poll).await;
        }
        self.unmute_hushed().await;
    }

    pub(super) async fn unmute_hushed(&mut self) {
        self.hush_until = None;
        for sink in std::mem::take(&mut self.hushed) {
            if let Err(e) = self.pulse.set_sink_mute(sink, false).await {
                debug!(
                    "[audio {}] unmute output {} failed: {}",
                    self.mac_str, sink, e
                );
            }
        }
        if !std::mem::take(&mut self.hushed_names).is_empty()
            && let Some(file) = &self.cfg.hush_file
        {
            let _ = tokio::fs::remove_file(file).await;
        }
    }

    /// A profile toggle recreates the sink and WirePlumber parks streams on the fallback
    /// (speakers). Move them back, and restore the default if it was ours.
    pub(super) async fn restore_streams(&mut self, streams: &[u32]) {
        let deadline = Instant::now() + self.cfg.verify_timeout;
        let (snap, sink) = loop {
            let Ok(snap) = self.pulse.snapshot().await else {
                return;
            };
            if let Some(sink) = snap
                .card_for(&self.mac_str)
                .and_then(|c| snap.sink_for_card(c))
                .cloned()
            {
                break (snap, sink);
            }
            if Instant::now() >= deadline {
                return;
            }
            sleep(self.cfg.poll).await;
        };
        if self.we_were_default
            && snap.default_sink.as_deref() != Some(sink.name.as_str())
            && let Err(e) = self.pulse.set_default_sink(&sink.name).await
        {
            debug!("[audio {}] set default sink failed: {}", self.mac_str, e);
        }
        for input in snap
            .sink_inputs
            .iter()
            .filter(|i| streams.contains(&i.index) && i.sink != sink.index)
        {
            if let Err(e) = self.pulse.move_sink_input(input.index, sink.index).await {
                debug!(
                    "[audio {}] move stream {} failed: {}",
                    self.mac_str, input.index, e
                );
            }
        }
    }

    /// Rule 6: whatever happened, never leave the card on `off`.
    pub(super) async fn ensure_not_off(&mut self) {
        let Some(card) = self.current_card().await else {
            return;
        };
        if is_off(card.active_profile.as_deref()) {
            warn!("[audio {}] card is on 'off', restoring A2DP", self.mac_str);
            let candidates = a2dp_candidates(&card);
            if let Err(e) = self.apply_profile(&candidates).await {
                warn!("[audio {}] could not restore A2DP: {:?}", self.mac_str, e);
            }
        }
    }
}

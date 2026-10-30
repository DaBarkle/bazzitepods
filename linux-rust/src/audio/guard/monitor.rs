//! The ready state: watching for stalls and lost routes once audio is working.

use super::*;

impl<P: PulseApi, B: BluezApi> Guard<P, B> {
    pub(super) async fn monitor(&mut self) {
        if !self.linked || !self.ready {
            return;
        }
        let now = Instant::now();
        let every = if self.fast_until.is_some_and(|t| now < t) {
            self.cfg.poll
        } else {
            self.cfg.monitor_interval
        };
        self.next_monitor = Some(now + every);
        let Ok(snap) = self.pulse.snapshot().await else {
            return;
        };
        let Some(card) = snap.card_for(&self.mac_str) else {
            info!(
                "[audio {}] audio card vanished while linked, re-checking",
                self.mac_str
            );
            self.lose_ready(now + self.cfg.link_debounce);
            return;
        };
        let active = card.active_profile.clone().unwrap_or_default();
        if is_off(Some(&active)) {
            info!(
                "[audio {}] card switched to 'off' externally, fixing",
                self.mac_str
            );
            self.lose_ready(now);
            return;
        }
        if std::mem::take(&mut self.rebuild_pending) && is_a2dp(&active) {
            info!(
                "[audio {}] rebuilding the output (mic stream stopped)",
                self.mac_str
            );
            self.rebuild().await;
            return;
        }
        if std::mem::take(&mut self.repair_pending) && is_a2dp(&active) {
            info!(
                "[audio {}] audio came back from another device, rebuilding the output",
                self.mac_str
            );
            self.maybe_run_ladder().await;
            return;
        }
        let Some(sink) = snap.sink_for_card(card).cloned() else {
            self.stall_since = None;
            return;
        };
        self.claim_default(&snap, &sink, true).await;
        // Healthy (also clears an earlier stall failure once audio flows again).
        let healthy = AudioHealth::Ready {
            codec: codec_for(&active, Some(&sink)),
            profile: active.clone(),
        };

        let playing = sink.state == SinkState::Running && snap.uncorked_inputs_on(sink.index) > 0;
        if !self.away && is_a2dp(&active) && self.return_to_best_codec(card, playing).await {
            self.next_monitor = Some(Instant::now() + self.cfg.poll);
            return;
        }
        if self.away
            || !is_a2dp(&active)
            || sink.state != SinkState::Running
            || snap.uncorked_inputs_on(sink.index) == 0
        {
            self.stall_since = None;
            self.set_health(healthy);
            return;
        }
        let states = match self.bluez.transport_states(self.mac).await {
            Ok(s) => s,
            Err(e) => {
                debug!("[audio {}] transport query failed: {}", self.mac_str, e);
                return;
            }
        };
        if states.iter().any(|s| s == "active") {
            self.stall_since = None;
            self.set_health(healthy);
            return;
        }
        let since = *self.stall_since.get_or_insert(now);
        debug!(
            "[audio {}] sink running but transport {:?} for {:?}",
            self.mac_str,
            states,
            now - since
        );
        let fast = self.fast_until.is_some_and(|t| now < t);
        let threshold = if fast {
            FAST_STALL.min(self.cfg.stall_threshold)
        } else {
            self.cfg.stall_threshold
        };
        if now - since >= threshold {
            self.maybe_run_ladder().await;
        } else {
            // Look again soon so detection is close to the threshold.
            self.next_monitor = Some(now + self.cfg.poll.min(self.cfg.monitor_interval));
        }
    }

    pub(super) fn lose_ready(&mut self, ensure_at: Instant) {
        self.ready = false;
        self.next_monitor = None;
        self.stall_since = None;
        if self.ensure_attempts >= self.cfg.max_ensure_attempts {
            self.set_health(AudioHealth::Failed {
                reason: "audio device keeps disappearing".into(),
            });
            return;
        }
        self.set_health(AudioHealth::Negotiating);
        self.ensure_at = Some(ensure_at);
    }
}

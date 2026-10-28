//! Recovery: the repair ladder, full rebuilds and codec changes.

use super::*;

impl<P: PulseApi, B: BluezApi> Guard<P, B> {
    pub(super) async fn maybe_run_ladder(&mut self) {
        let now = Instant::now();
        if let Some(last) = self.last_ladder
            && now - last < self.cfg.ladder_cooldown
        {
            debug!(
                "[audio {}] stall persists, recovery rate-limited",
                self.mac_str
            );
            return;
        }
        if self.ladders_this_link >= self.cfg.max_ladders_per_link {
            self.set_health(AudioHealth::Failed {
                reason: "audio keeps stalling; reconnect the AirPods".into(),
            });
            return;
        }
        self.last_ladder = Some(now);
        self.ladders_this_link += 1;
        self.stall_since = None;
        warn!(
            "[audio {}] audio stalled, starting recovery #{}",
            self.mac_str, self.ladders_this_link
        );
        let result = self.run_ladder().await;
        self.ensure_not_off().await;
        self.unhush().await;
        // A take-back reported right after this rebuild needs no second one.
        self.last_repair = Some(Instant::now());
        match result {
            Ok(Some(step)) => {
                info!("[audio {}] recovered at step {}", self.mac_str, step);
                if let Some(card) = self.current_card().await {
                    let profile = card.active_profile.unwrap_or_default();
                    self.set_health(AudioHealth::Ready {
                        codec: codec_for(&profile, None),
                        profile,
                    });
                }
            }
            Ok(None) => {
                warn!("[audio {}] recovery exhausted", self.mac_str);
                self.set_health(AudioHealth::Failed {
                    reason: "audio stalled and could not be recovered".into(),
                });
            }
            Err(Abort::Failed(reason)) => {
                self.set_health(AudioHealth::Failed { reason });
            }
            Err(Abort::Interrupted) => {}
        }
        // Monitoring continues (next_monitor is already set); rate limits bound it.
        if self.ready && self.next_monitor.is_none() {
            self.next_monitor = Some(Instant::now() + self.cfg.monitor_interval);
        }
    }

    /// One profile toggle (the first ladder step), outside the ladder's limits.
    pub(super) async fn rebuild(&mut self) {
        let Some(card) = self.current_card().await else {
            return;
        };
        let mut order = a2dp_candidates(&card);
        if let Some(current) = card.active_profile.clone().filter(|p| is_a2dp(p))
            && !order.contains(&current)
        {
            order.push(current);
        }
        if order.is_empty() {
            return;
        }
        let streams = self.our_streams().await;
        self.set_health(AudioHealth::Recovering { step: 1 });
        self.hush().await;
        if let Err(e) = self.pulse.set_card_profile(card.index, "off").await {
            warn!("[audio {}] toggle to off failed: {}", self.mac_str, e);
        }
        sleep(self.cfg.toggle_gap).await;
        let result = self.apply_profile(&order).await;
        self.restore_streams(&streams).await;
        self.ensure_not_off().await;
        // The profile is back, but the AirPods can take a few seconds more to accept
        // audio. Keep the speakers muted until they do; if they don't, recover now.
        if result.is_ok() && !self.flowing_within(self.rebuild_flow_wait()).await {
            // PipeWire locks a transport out (EIO, no acquire) for a few seconds after
            // errors, and the off toggle keeps the same AAC transport object with its
            // error count. Another codec gets a fresh transport that starts at once;
            // AAC comes back later, once that lockout is long gone.
            info!(
                "[audio {}] the AirPods didn't start the rebuilt output, switching codec",
                self.mac_str
            );
            if let Some(profile) = self.switch_codec(&streams).await {
                self.unhush().await;
                self.last_repair = Some(Instant::now());
                self.stall_since = None;
                self.set_health(AudioHealth::Ready {
                    codec: codec_for(&profile, None),
                    profile,
                });
                return;
            }
            self.maybe_run_ladder().await;
            // A rate-limited ladder leaves the speakers muted; never keep them that way.
            self.unhush().await;
            self.last_repair = Some(Instant::now());
            return;
        }
        self.unhush().await;
        self.last_repair = Some(Instant::now());
        self.stall_since = None;
        match result {
            Ok(profile) => {
                let sink = self.pulse.snapshot().await.ok().and_then(|snap| {
                    let card = snap.card_for(&self.mac_str)?;
                    snap.sink_for_card(card).cloned()
                });
                self.set_health(AudioHealth::Ready {
                    codec: codec_for(&profile, sink.as_ref()),
                    profile,
                });
            }
            Err(Abort::Failed(reason)) => self.set_health(AudioHealth::Failed { reason }),
            Err(Abort::Interrupted) => {}
        }
    }

    /// Returns `Ok(Some(step))` when recovered, `Ok(None)` when every step failed.
    pub(super) async fn run_ladder(&mut self) -> StepResult<Option<u8>> {
        let Some(card) = self.current_card().await else {
            return Err(Abort::Failed("audio device disappeared".into()));
        };
        let current = card
            .active_profile
            .clone()
            .filter(|p| is_a2dp(p))
            .unwrap_or_default();
        let candidates = a2dp_candidates(&card);
        let streams = self.our_streams().await;

        // Step 1: toggle off -> same A2DP profile. Done back-to-back without
        // interruption checks so we can never stop in between.
        self.set_health(AudioHealth::Recovering { step: 1 });
        // Best codec first (AAC), so a recovery never settles for SBC when AAC is offered
        // just because an earlier attempt fell back to it.
        let mut order = candidates.clone();
        if !current.is_empty() && !order.contains(&current) {
            order.push(current.clone());
        }
        order.retain(|p| !p.is_empty());
        if order.is_empty() {
            // Never toggle to "off" without a profile to come back to.
            return Err(Abort::Failed("the device offers no A2DP profile".into()));
        }
        self.hush().await;
        if let Err(e) = self.pulse.set_card_profile(card.index, "off").await {
            warn!("[audio {}] toggle to off failed: {}", self.mac_str, e);
        }
        sleep(self.cfg.toggle_gap).await;
        let step1 = self.apply_profile(&order).await;
        self.restore_streams(&streams).await;
        self.unhush().await;
        self.check_interrupt()?;
        if step1.is_ok() && self.stall_resolved().await {
            return Ok(Some(1));
        }
        self.check_interrupt()?;

        // Step 2: alternate codec.
        self.set_health(AudioHealth::Recovering { step: 2 });
        let active_now = self
            .current_card()
            .await
            .and_then(|c| c.active_profile)
            .unwrap_or_default();
        let alternates: Vec<String> = candidates
            .iter()
            .filter(|c| **c != active_now && **c != current)
            .cloned()
            .collect();
        if !alternates.is_empty() {
            let step2 = self.apply_profile(&alternates).await;
            self.restore_streams(&streams).await;
            self.check_interrupt()?;
            if step2.is_ok() && self.stall_resolved().await {
                self.note_codec().await;
                return Ok(Some(2));
            }
            self.check_interrupt()?;
        }

        // Step 3: reconnect the A2DP profile.
        self.set_health(AudioHealth::Recovering { step: 3 });
        if let Err(e) = self
            .bluez
            .disconnect_profile(self.mac, A2DP_SINK_UUID)
            .await
        {
            warn!("[audio {}] {}", self.mac_str, e);
        }
        self.pause(self.cfg.reconnect_gap).await?;
        if let Err(e) = self.bluez.connect_profile(self.mac, A2DP_SINK_UUID).await {
            warn!("[audio {}] {}", self.mac_str, e);
        }
        self.check_interrupt()?;
        if let Some((_, card)) = self.wait_card(self.cfg.card_wait).await?
            && self.check_profile(&card).await.is_ok()
            && {
                self.restore_streams(&streams).await;
                self.stall_resolved().await
            }
        {
            self.note_codec().await;
            return Ok(Some(3));
        }
        Ok(None)
    }

    /// Switches to another A2DP codec, which makes a fresh transport, and waits for
    /// audio. Returns the profile when it plays.
    pub(super) async fn switch_codec(&mut self, streams: &[u32]) -> Option<String> {
        let card = self.current_card().await?;
        let active = card.active_profile.clone().unwrap_or_default();
        let alternates: Vec<String> = a2dp_candidates(&card)
            .into_iter()
            .filter(|p| *p != active)
            .collect();
        let profile = self.apply_profile(&alternates).await.ok()?;
        self.restore_streams(streams).await;
        if !self.stall_resolved().await {
            return None;
        }
        self.note_codec().await;
        Some(profile)
    }

    pub(super) fn rebuild_flow_wait(&self) -> Duration {
        REBUILD_FLOW_WAIT.min(self.cfg.verify_timeout)
    }

    /// Remembers whether the output now runs on a lesser codec than the best offered.
    pub(super) async fn note_codec(&mut self) {
        let Some(card) = self.current_card().await else {
            return;
        };
        let best = a2dp_candidates(&card).into_iter().next();
        let lesser = best.is_some() && card.active_profile != best;
        self.lesser_codec_since = lesser.then(Instant::now);
    }

    /// After a fallback to a lesser codec, goes back to the best one: when nothing is
    /// playing (no gap to hear), or after [`BEST_CODEC_RETURN`] of music. If the best
    /// codec doesn't play, stays on the one that did.
    pub(super) async fn return_to_best_codec(&mut self, card: &CardInfo, playing: bool) -> bool {
        let Some(since) = self.lesser_codec_since else {
            return false;
        };
        if playing && since.elapsed() < BEST_CODEC_RETURN {
            return false;
        }
        self.lesser_codec_since = None;
        let current = card.active_profile.clone().unwrap_or_default();
        let Some(best) = a2dp_candidates(card).into_iter().next() else {
            return false;
        };
        if best == current {
            return false;
        }
        info!("[audio {}] back from {} to {}", self.mac_str, current, best);
        let streams = self.our_streams().await;
        if playing {
            self.hush().await;
        }
        let ok = self
            .apply_profile(std::slice::from_ref(&best))
            .await
            .is_ok()
            && {
                self.restore_streams(&streams).await;
                self.flowing_within(self.rebuild_flow_wait()).await
            };
        if !ok {
            warn!(
                "[audio {}] {} didn't play, staying on {}",
                self.mac_str, best, current
            );
            let _ = self.apply_profile(std::slice::from_ref(&current)).await;
            self.restore_streams(&streams).await;
        }
        self.unhush().await;
        self.stall_since = None;
        true
    }

    /// True once audio flows again (transport active) or nothing is playing.
    pub(super) async fn stall_resolved(&self) -> bool {
        self.flowing_within(self.cfg.verify_timeout).await
    }

    /// Like [`Self::stall_resolved`], waiting at most `wait`.
    pub(super) async fn flowing_within(&self, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        loop {
            if let Ok(snap) = self.pulse.snapshot().await
                && let Some(card) = snap.card_for(&self.mac_str)
                && let Some(sink) = snap.sink_for_card(card)
            {
                let playing =
                    sink.state == SinkState::Running && snap.uncorked_inputs_on(sink.index) > 0;
                if !playing {
                    return true;
                }
                if let Ok(states) = self.bluez.transport_states(self.mac).await
                    && states.iter().any(|s| s == "active")
                {
                    return true;
                }
            }
            if Instant::now() >= deadline {
                return false;
            }
            sleep(self.cfg.poll).await;
        }
    }
}

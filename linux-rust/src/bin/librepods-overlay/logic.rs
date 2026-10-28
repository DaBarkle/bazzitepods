//! Pure presentation logic: what the island shows, when, and how it moves there.
//! No iced or Wayland types here, so all of it is unit tested.

use std::time::{Duration, Instant};

use crate::island::{
    ALERT_MAX_W, Alert, Content, EXPANDED_H, EXPANDED_W_BUDS, Motion, Parts, display_name,
    elsewhere_label, glow_for, shape,
};
use crate::overlay_protocol::{
    ComponentState, OverlayCommand, OverlayConfig, OverlayMode, OverlayState, OverlayStyle,
};

/// Battery band used to detect "dropped to 20% / 10%".
fn band(level: u8) -> u8 {
    match level {
        0..=10 => 0,
        11..=20 => 1,
        _ => 2,
    }
}

fn present(c: &Option<ComponentState>) -> bool {
    c.as_ref().is_some_and(|c| c.present)
}

/// What `next` is worth showing, compared with `prev`: the full card, a one-line alert,
/// or nothing. When several things change at once the most important one wins.
pub fn event(prev: Option<&OverlayState>, next: &OverlayState) -> Option<Content> {
    let was_connected = prev.is_some_and(|p| p.connected);
    if !next.connected {
        return was_connected.then_some(Content::Alert(Alert::Disconnected));
    }
    let Some(prev) = prev.filter(|_| was_connected) else {
        return Some(Content::Expanded);
    };
    if prev.elsewhere != next.elsewhere {
        return Some(Content::Alert(if next.elsewhere {
            Alert::Elsewhere(next.elsewhere_name.clone())
        } else {
            Alert::Back
        }));
    }
    let pairs = [
        (&prev.left, &next.left),
        (&prev.right, &next.right),
        (&prev.case, &next.case),
        (&prev.headphone, &next.headphone),
    ];
    // A pod (or the case) came or went: the whole picture changed.
    if pairs.iter().any(|(a, b)| present(a) != present(b)) {
        return Some(Content::Expanded);
    }
    let mut low = Parts::default();
    let mut charging = Parts::default();
    for (i, (a, b)) in pairs.iter().enumerate() {
        let (Some(a), Some(b)) = (a, b) else { continue };
        if !b.present {
            continue;
        }
        let flag = |p: &mut Parts| match i {
            0 => p.left = true,
            1 => p.right = true,
            2 => p.case = true,
            _ => p.headphone = true,
        };
        if !b.charging && band(b.level) < band(a.level) {
            flag(&mut low);
        }
        if b.charging && !a.charging {
            flag(&mut charging);
        }
    }
    if low.any() {
        return Some(Content::Alert(Alert::Low(low)));
    }
    if charging.any() {
        return Some(Content::Alert(Alert::Charging(charging)));
    }
    // Asked for with a stem hold, so say what happened before anything else routine.
    if next.mic_failures > prev.mic_failures {
        return Some(Content::Alert(Alert::MicUnavailable));
    }
    match (prev.mic, next.mic) {
        (Some(a), Some(b)) if a != b => return Some(Content::Alert(Alert::Mic(b))),
        _ => {}
    }
    match (prev.listening_mode, next.listening_mode) {
        (Some(a), Some(b)) if a != b => Some(Content::Alert(Alert::Listening(b))),
        _ => None,
    }
}

/// Shown by a preview before any battery data has arrived.
fn placeholder() -> OverlayState {
    let unknown = || {
        Some(ComponentState {
            present: false,
            ..Default::default()
        })
    };
    OverlayState {
        name: "AirPods".into(),
        connected: true,
        left: unknown(),
        right: unknown(),
        case: unknown(),
        ..Default::default()
    }
}

/// What the iced side has to do with the layer surface after a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAction {
    None,
    /// Create the layer surface.
    Map,
    /// Destroy the layer surface (the island folded away).
    Unmap,
}

#[derive(Debug)]
pub struct Controller {
    pub config: OverlayConfig,
    pub state: Option<OverlayState>,
    /// Content popped up by an event or `show`, until the instant.
    pop: Option<(Content, Instant)>,
    /// `hide` received: stay hidden until the next show trigger.
    forced_hidden: bool,
    mapped: bool,
    last_tick: Option<Instant>,
    /// `show` with reason "preview" (settings button): visible until this instant
    /// whatever the mode or link state, with a placeholder when there is no data.
    preview_until: Option<Instant>,
    /// A fullscreen window is active on the desktop (reported by the compositor).
    fullscreen: bool,
    /// Preview started from the opacity slider: show the fullscreen look.
    opacity_preview: bool,
    /// Just disconnected: say so until this instant, then fold away.
    farewell_until: Option<Instant>,
    pub motion: Motion,
    /// Frames are still needed (springs or content transitions in flight).
    moving: bool,
}

impl Default for Controller {
    fn default() -> Self {
        Self::new(OverlayConfig::default())
    }
}

impl Controller {
    pub fn new(config: OverlayConfig) -> Self {
        Self {
            config,
            state: None,
            pop: None,
            forced_hidden: false,
            mapped: false,
            last_tick: None,
            preview_until: None,
            fullscreen: false,
            opacity_preview: false,
            farewell_until: None,
            motion: Motion::default(),
            moving: false,
        }
    }

    fn pop_up(&mut self, content: Content, now: Instant) {
        self.forced_hidden = false;
        let secs = match &content {
            Content::Alert(a) => a.duration(),
            _ => self.config.auto_hide_secs.clamp(0.5, 600.0),
        };
        let card_stays = self.config.mode == OverlayMode::Always
            || self.previewing(now)
            || content == Content::Expanded;
        if self.mapped && card_stays && self.motion.shown == Some(Content::Expanded) {
            // The card stays; its headline changes and it gives a little bounce.
            self.motion.bump();
            self.moving = true;
        }
        self.pop = Some((content, now + Duration::from_secs_f32(secs)));
    }

    /// Lights the border for `content` (purist style only; the classic island has no
    /// key line, so it would only spend frames on nothing).
    fn announce(&mut self, content: &Content, connected: bool, now: Instant) {
        if self.config.style != OverlayStyle::Purist || self.config.mode == OverlayMode::Off {
            return;
        }
        let state = self.display_state(now).unwrap_or_else(placeholder);
        if let Some(glow) = glow_for(content, &state, connected) {
            self.motion.light(glow);
            self.moving = true;
        }
        if content == &Content::Alert(Alert::MicUnavailable) {
            self.motion.shake();
        }
    }

    /// Applies one protocol command. `Quit` is handled by the caller.
    pub fn apply(&mut self, cmd: OverlayCommand, now: Instant) {
        match cmd {
            OverlayCommand::Config(cfg) => {
                let was = self.config.mode;
                self.config = cfg;
                if was == OverlayMode::Always && self.config.mode != OverlayMode::Always {
                    // Leaving `always`: keep the card up for one more period, then go.
                    if self.mapped {
                        self.pop_up(Content::Expanded, now);
                    } else {
                        self.pop = None;
                    }
                }
            }
            OverlayCommand::State { state } => {
                let ev = event(self.state.as_ref(), &state);
                let just_connected =
                    state.connected && !self.state.as_ref().is_some_and(|p| p.connected);
                if self.motion.update_levels(&state, self.mapped) {
                    self.moving = true;
                }
                if !state.connected {
                    self.pop = None;
                }
                match ev {
                    Some(Content::Alert(Alert::Disconnected)) => {
                        let secs = Alert::Disconnected.duration();
                        self.farewell_until = Some(now + Duration::from_secs_f32(secs));
                        self.state = Some(state);
                        self.announce(&Content::Alert(Alert::Disconnected), false, now);
                        return;
                    }
                    Some(c) => {
                        self.farewell_until = None;
                        self.state = Some(state);
                        self.announce(&c, just_connected, now);
                        self.pop_up(c, now);
                        return;
                    }
                    None if state.connected => self.farewell_until = None,
                    None => {}
                }
                self.state = Some(state);
            }
            OverlayCommand::Fullscreen { active } => self.fullscreen = active,
            OverlayCommand::Show { reason } if reason == "preview" || reason == "opacity" => {
                self.opacity_preview = reason == "opacity";
                self.forced_hidden = false;
                let secs = self.config.auto_hide_secs.clamp(2.0, 600.0);
                let fresh = !self.previewing(now);
                self.preview_until = Some(now + Duration::from_secs_f32(secs));
                // Show off the border light when a preview opens (not on every slider step).
                if fresh && !self.opacity_preview {
                    self.announce(&Content::Expanded, true, now);
                }
            }
            OverlayCommand::Show { .. } => {
                self.announce(&Content::Expanded, false, now);
                self.pop_up(Content::Expanded, now)
            }
            OverlayCommand::Hide => {
                self.pop = None;
                self.farewell_until = None;
                self.forced_hidden = true;
            }
            OverlayCommand::Quit => {}
        }
    }

    fn previewing(&self, now: Instant) -> bool {
        self.preview_until.is_some_and(|t| now < t)
    }

    fn popped(&self, now: Instant) -> Option<&Content> {
        self.pop.as_ref().filter(|(_, t)| now < *t).map(|(c, _)| c)
    }

    /// What to draw: the real state, or during a preview without data a placeholder
    /// (dashes), so the user can always see where the island sits.
    pub fn display_state(&self, now: Instant) -> Option<OverlayState> {
        match &self.state {
            Some(s) if s.connected || !self.previewing(now) => Some(s.clone()),
            Some(s) => Some(OverlayState {
                connected: true,
                ..s.clone()
            }),
            None if self.previewing(now) || self.mapped => Some(placeholder()),
            None => None,
        }
    }

    /// What the island should show now; None folds it away.
    pub fn target(&self, now: Instant) -> Option<Content> {
        if self.previewing(now) {
            return Some(Content::Expanded);
        }
        if self.config.mode == OverlayMode::Off || self.forced_hidden {
            return None;
        }
        if self.farewell_until.is_some_and(|t| now < t) {
            return Some(Content::Alert(Alert::Disconnected));
        }
        if !self.state.as_ref().is_some_and(|s| s.connected) {
            return None;
        }
        let popped = self.popped(now).cloned();
        match self.config.mode {
            OverlayMode::Always => Some(Content::Expanded),
            OverlayMode::Compact => Some(popped.unwrap_or(Content::Compact)),
            OverlayMode::Auto => popped,
            OverlayMode::Off => None,
        }
    }

    /// The second line of the full card: the latest news while it is fresh, otherwise
    /// where the audio is and the noise control mode.
    pub fn headline(&self, now: Instant) -> String {
        let Some(state) = self.display_state(now) else {
            return String::new();
        };
        if let Some(Content::Alert(a)) = self.popped(now) {
            return a.title(&state);
        }
        if state.elsewhere {
            return elsewhere_label(state.elsewhere_name.as_deref());
        }
        match state.listening_mode {
            Some(m) => m.label().to_string(),
            None if self.state.is_none() => "Waiting for battery".into(),
            None => "Connected".into(),
        }
    }

    /// The device's name for the card's first line.
    pub fn name(&self, now: Instant) -> String {
        self.display_state(now)
            .map(|s| display_name(&s))
            .unwrap_or_default()
    }

    /// The user's opacity applies while a fullscreen window is in front (or while
    /// they drag the slider); otherwise the island is solid.
    pub fn effective_opacity(&self) -> f32 {
        if self.fullscreen || self.opacity_preview {
            self.config.opacity.clamp(0.2, 1.0)
        } else {
            1.0
        }
    }

    /// Advances the animation to `now` and says what to do with the surface.
    pub fn tick(&mut self, now: Instant) -> SurfaceAction {
        let target = self.target(now);
        if self.pop.as_ref().is_some_and(|(_, t)| now >= *t) {
            self.pop = None;
        }
        if self.farewell_until.is_some_and(|t| now >= t) {
            self.farewell_until = None;
        }
        if self.preview_until.is_some_and(|t| now >= t) {
            self.preview_until = None;
            self.opacity_preview = false;
        }
        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default()
            // A long gap (no ticks while idle) must not skip an animation.
            .min(Duration::from_millis(50));
        self.last_tick = Some(now);

        let state = self.display_state(now).unwrap_or_else(placeholder);
        let action = match (&target, self.mapped) {
            (Some(_), false) => {
                self.mapped = true;
                self.motion.reset();
                self.motion.update_levels(&state, false);
                SurfaceAction::Map
            }
            _ => SurfaceAction::None,
        };
        let caption = self.headline(now);
        self.motion
            .set_caption(&caption, self.mapped && action == SurfaceAction::None);
        let target_shape = target.as_ref().map(|c| shape(c, &state));
        if self.motion.shown != target {
            self.motion.switch(target, target_shape);
        } else if let Some(s) = target_shape {
            self.motion.resize(s);
        }
        self.moving = self.motion.step(dt.as_secs_f32());
        if action == SurfaceAction::None && self.mapped && self.motion.gone() {
            self.mapped = false;
            self.moving = false;
            return SurfaceAction::Unmap;
        }
        action
    }

    #[cfg(test)]
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// The compositor closed our surface (e.g. its output went away).
    pub fn surface_lost(&mut self) {
        self.mapped = false;
        self.motion.reset();
    }

    /// True while frames must be produced.
    pub fn is_animating(&self, now: Instant) -> bool {
        let target = self.target(now);
        if target.is_some() != self.mapped || self.motion.shown != target {
            return true;
        }
        self.mapped && self.moving
    }

    /// The next instant at which the island changes by itself (a pop-up ends, a
    /// preview ends), so the caller can sleep until exactly then instead of polling.
    pub fn next_deadline(&self) -> Option<Instant> {
        [
            self.pop.as_ref().map(|(_, t)| *t),
            self.preview_until,
            self.farewell_until,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// Room the surface needs: the biggest island plus headroom for a spring's
    /// overshoot and the bump. It is fixed, so the surface is never resized while
    /// shown: a resize lands a frame apart from the drawing and makes the island jump.
    pub fn extent(&self) -> (f32, f32) {
        let k = self.config.size.scale();
        (
            (ALERT_MAX_W.max(EXPANDED_W_BUDS) * 1.1 + 8.0) * k,
            (EXPANDED_H * 1.1 + 6.0) * k,
        )
    }
}

#[cfg(test)]
#[path = "logic_tests.rs"]
mod tests;

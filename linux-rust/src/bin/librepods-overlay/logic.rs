//! Pure presentation logic: when the pill should be visible, and its animation.
//! No iced or Wayland types here, so all of it is unit tested.

use std::time::{Duration, Instant};

use crate::overlay_protocol::{
    ComponentState, OverlayCommand, OverlayConfig, OverlayMode, OverlayState,
};

pub const APPEAR: Duration = Duration::from_millis(240);
pub const DISAPPEAR: Duration = Duration::from_millis(320);
/// Morph between the compact widget and the full pill.
pub const MORPH: Duration = Duration::from_millis(280);

/// Battery band used to detect "crossed 20% / 10%".
fn band(level: u8) -> u8 {
    match level {
        0..=10 => 0,
        11..=20 => 1,
        _ => 2,
    }
}

/// Returns why `next` is worth popping the overlay up for, compared with `prev`.
pub fn meaningful_change(prev: Option<&OverlayState>, next: &OverlayState) -> Option<&'static str> {
    if !next.connected {
        return None;
    }
    let Some(prev) = prev.filter(|p| p.connected) else {
        return Some("connected");
    };
    if prev.elsewhere != next.elsewhere {
        return Some("route");
    }
    let pairs: [(
        &Option<ComponentState>,
        &Option<ComponentState>,
        &'static str,
    ); 4] = [
        (&prev.left, &next.left, "pod"),
        (&prev.right, &next.right, "pod"),
        (&prev.case, &next.case, "case"),
        (&prev.headphone, &next.headphone, "headphone"),
    ];
    for (a, b, what) in pairs {
        let a_present = a.as_ref().is_some_and(|c| c.present);
        let b_present = b.as_ref().is_some_and(|c| c.present);
        if a_present != b_present {
            return Some(what);
        }
        let (Some(a), Some(b)) = (a, b) else { continue };
        if !b.present {
            continue;
        }
        if a.charging != b.charging {
            return Some("charging");
        }
        if band(a.level) != band(b.level) {
            return Some("low_battery");
        }
    }
    None
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
        kind: String::new(),
        connected: true,
        left: unknown(),
        right: unknown(),
        case: unknown(),
        headphone: None,
        ..Default::default()
    }
}

/// What the iced side has to do with the layer surface after a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SurfaceAction {
    None,
    /// Create the layer surface.
    Map,
    /// Destroy the layer surface (fade-out finished).
    Unmap,
}

#[derive(Debug)]
pub struct Controller {
    pub config: OverlayConfig,
    pub state: Option<OverlayState>,
    /// Auto mode: visible until this instant.
    hide_at: Option<Instant>,
    /// `hide` received: stay hidden until the next show trigger.
    forced_hidden: bool,
    /// Linear animation progress, 0 = gone, 1 = fully shown.
    progress: f32,
    mapped: bool,
    last_tick: Option<Instant>,
    /// `show` with reason "preview" (settings button): visible until this instant
    /// whatever the mode or link state, with a placeholder when there is no data.
    preview_until: Option<Instant>,
    /// Compact mode morph, 0 = small widget, 1 = full pill (always 1 in other modes).
    expand: f32,
    /// A fullscreen window is active on the desktop (reported by the compositor).
    fullscreen: bool,
    /// Preview started from the opacity slider: show the fullscreen look.
    opacity_preview: bool,
    /// The audio just came back from another device: say so until this instant.
    back_until: Option<Instant>,
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
            hide_at: None,
            forced_hidden: false,
            progress: 0.0,
            mapped: false,
            last_tick: None,
            preview_until: None,
            expand: 1.0,
            fullscreen: false,
            opacity_preview: false,
            back_until: None,
        }
    }

    fn trigger(&mut self, now: Instant) {
        self.forced_hidden = false;
        if matches!(self.config.mode, OverlayMode::Auto | OverlayMode::Compact) {
            let secs = self.config.auto_hide_secs.clamp(0.5, 600.0);
            self.hide_at = Some(now + Duration::from_secs_f32(secs));
        }
    }

    /// Applies one protocol command. `Quit` is handled by the caller.
    pub fn apply(&mut self, cmd: OverlayCommand, now: Instant) {
        match cmd {
            OverlayCommand::Config(cfg) => {
                let was = self.config.mode;
                self.config = cfg;
                if was == OverlayMode::Always && self.config.mode != OverlayMode::Always {
                    // Leaving `always`: keep it up for one more auto period, then go.
                    if self.mapped {
                        self.trigger(now);
                    } else {
                        self.hide_at = None;
                    }
                }
            }
            OverlayCommand::State { state } => {
                let reason = meaningful_change(self.state.as_ref(), &state);
                if !state.connected {
                    self.hide_at = None;
                }
                let came_back = self.state.as_ref().is_some_and(|p| p.elsewhere)
                    && !state.elsewhere
                    && state.connected;
                if came_back {
                    let secs = self.config.auto_hide_secs.clamp(2.0, 10.0);
                    self.back_until = Some(now + Duration::from_secs_f32(secs));
                } else if state.elsewhere || !state.connected {
                    self.back_until = None;
                }
                self.state = Some(state);
                if reason.is_some() {
                    self.trigger(now);
                }
            }
            OverlayCommand::Fullscreen { active } => self.fullscreen = active,
            OverlayCommand::Show { reason } if reason == "preview" || reason == "opacity" => {
                self.opacity_preview = reason == "opacity";
                self.forced_hidden = false;
                let secs = self.config.auto_hide_secs.clamp(2.0, 600.0);
                self.preview_until = Some(now + Duration::from_secs_f32(secs));
            }
            OverlayCommand::Show { .. } => self.trigger(now),
            OverlayCommand::Hide => {
                self.hide_at = None;
                self.forced_hidden = true;
            }
            OverlayCommand::Quit => {}
        }
    }

    fn previewing(&self, now: Instant) -> bool {
        self.preview_until.is_some_and(|t| now < t)
    }

    /// What to draw: the real state, or during a preview without data a placeholder
    /// pill (dashes), so the user can always see where the overlay sits.
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

    /// The headline that replaces the device name while the audio is away or has just
    /// come back, so a switch is visible at a glance.
    pub fn route_label(&self, now: Instant) -> Option<String> {
        let state = self.state.as_ref()?;
        if state.elsewhere {
            return Some(match state.elsewhere_name.as_deref().map(str::trim) {
                Some(name) if !name.is_empty() => format!("Playing on {name}"),
                _ => "On another device".to_string(),
            });
        }
        self.back_until
            .is_some_and(|t| now < t)
            .then(|| "Back on this PC".to_string())
    }

    /// The user's opacity applies while a fullscreen window is in front (or while
    /// they drag the slider); otherwise the pill is solid.
    pub fn effective_opacity(&self) -> f32 {
        if self.fullscreen || self.opacity_preview {
            self.config.opacity.clamp(0.2, 1.0)
        } else {
            1.0
        }
    }

    pub fn wants_visible(&self, now: Instant) -> bool {
        if self.previewing(now) {
            return true;
        }
        if self.config.mode == OverlayMode::Off || self.forced_hidden {
            return false;
        }
        if !self.state.as_ref().is_some_and(|s| s.connected) {
            return false;
        }
        match self.config.mode {
            OverlayMode::Always | OverlayMode::Compact => true,
            OverlayMode::Auto => self.hide_at.is_some_and(|t| now < t),
            OverlayMode::Off => false,
        }
    }

    /// True while the full pill should show (vs. the compact widget).
    pub fn wants_expanded(&self, now: Instant) -> bool {
        self.config.mode != OverlayMode::Compact
            || self.previewing(now)
            || self.hide_at.is_some_and(|t| now < t)
    }

    /// Eased 0..1 morph from compact widget to full pill.
    pub fn expansion(&self) -> f32 {
        let t = self.expand.clamp(0.0, 1.0);
        t * t * (3.0 - 2.0 * t)
    }

    /// Advances the animation to `now` and says what to do with the surface.
    pub fn tick(&mut self, now: Instant) -> SurfaceAction {
        let target = self.wants_visible(now);
        if self.hide_at.is_some_and(|t| now >= t) {
            self.hide_at = None;
        }
        if self.preview_until.is_some_and(|t| now >= t) {
            self.preview_until = None;
            self.opacity_preview = false;
        }
        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default()
            // A long gap (no ticks while idle) must not skip the animation.
            .min(Duration::from_millis(50));
        self.last_tick = Some(now);

        let expand_target = if self.wants_expanded(now) { 1.0 } else { 0.0 };
        if target && !self.mapped {
            // Appear directly in the right form.
            self.expand = expand_target;
        } else if self.expand < expand_target {
            self.expand = (self.expand + dt.as_secs_f32() / MORPH.as_secs_f32()).min(1.0);
        } else if self.expand > expand_target {
            self.expand = (self.expand - dt.as_secs_f32() / MORPH.as_secs_f32()).max(0.0);
        }

        if target {
            if !self.mapped {
                self.mapped = true;
                self.progress = 0.0;
                return SurfaceAction::Map;
            }
            self.progress = (self.progress + dt.as_secs_f32() / APPEAR.as_secs_f32()).min(1.0);
        } else if self.mapped {
            self.progress = (self.progress - dt.as_secs_f32() / DISAPPEAR.as_secs_f32()).max(0.0);
            if self.progress <= 0.0 {
                self.mapped = false;
                return SurfaceAction::Unmap;
            }
        }
        SurfaceAction::None
    }

    #[cfg(test)]
    pub fn is_mapped(&self) -> bool {
        self.mapped
    }

    /// The compositor closed our surface (e.g. its output went away).
    pub fn surface_lost(&mut self) {
        self.mapped = false;
        self.progress = 0.0;
    }

    /// True while frames must be produced (fading in or out).
    pub fn is_animating(&self, now: Instant) -> bool {
        let target = self.wants_visible(now);
        let expand_target = if self.wants_expanded(now) { 1.0 } else { 0.0 };
        (target && (!self.mapped || self.progress < 1.0 || self.expand != expand_target))
            || (!target && self.mapped)
    }

    /// True while a timer must run to notice the auto-hide deadline.
    pub fn has_deadline(&self) -> bool {
        self.mapped && (self.hide_at.is_some() || self.preview_until.is_some())
    }

    /// Eased 0..1 value for drawing (opacity and slide).
    pub fn visual(&self) -> f32 {
        let t = self.progress.clamp(0.0, 1.0);
        // easeOutCubic
        1.0 - (1.0 - t).powi(3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay_protocol::OverlayPosition;

    fn comp(level: u8, charging: bool, present: bool) -> Option<ComponentState> {
        Some(ComponentState {
            level,
            charging,
            present,
            ..Default::default()
        })
    }

    fn buds(l: u8, r: u8, c: u8) -> OverlayState {
        OverlayState {
            name: "AirPods 4".into(),
            kind: "airpods4".into(),
            connected: true,
            left: comp(l, false, true),
            right: comp(r, false, true),
            case: comp(c, false, true),
            headphone: None,
            elsewhere: false,
            elsewhere_name: None,
        }
    }

    #[test]
    fn change_detection() {
        let a = buds(80, 75, 40);
        assert_eq!(meaningful_change(None, &a), Some("connected"));
        let mut off = a.clone();
        off.connected = false;
        assert_eq!(meaningful_change(Some(&off), &a), Some("connected"));
        assert_eq!(meaningful_change(Some(&a), &off), None);

        // Small level drift is not meaningful.
        assert_eq!(meaningful_change(Some(&a), &buds(79, 74, 40)), None);
        // Crossing 20% and 10%.
        assert_eq!(
            meaningful_change(Some(&buds(21, 75, 40)), &buds(20, 75, 40)),
            Some("low_battery")
        );
        assert_eq!(
            meaningful_change(Some(&buds(20, 75, 40)), &buds(19, 75, 40)),
            None
        );
        assert_eq!(
            meaningful_change(Some(&buds(11, 75, 40)), &buds(10, 75, 40)),
            Some("low_battery")
        );

        // Pod leaves / enters.
        let mut b = a.clone();
        b.left = comp(80, false, false);
        assert_eq!(meaningful_change(Some(&a), &b), Some("pod"));
        assert_eq!(meaningful_change(Some(&b), &a), Some("pod"));
        b.left = None;
        assert_eq!(meaningful_change(Some(&a), &b), Some("pod"));

        // Charging toggles.
        let mut c = a.clone();
        c.case = comp(40, true, true);
        assert_eq!(meaningful_change(Some(&a), &c), Some("charging"));

        // Changes on an absent component are ignored.
        let mut d = b.clone();
        d.left = Some(ComponentState {
            level: 5,
            ..Default::default()
        });
        assert_eq!(meaningful_change(Some(&b), &d), None);
    }

    fn run(c: &mut Controller, from: Instant, ms: u64) -> (Vec<SurfaceAction>, Instant) {
        let mut out = vec![];
        let mut t = from;
        for _ in 0..(ms / 16) {
            t += Duration::from_millis(16);
            let a = c.tick(t);
            if a != SurfaceAction::None {
                out.push(a);
            }
        }
        (out, t)
    }

    #[test]
    fn auto_mode_shows_then_unmaps() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        assert_eq!(c.tick(t0), SurfaceAction::None);
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t0,
        );
        assert_eq!(c.tick(t0), SurfaceAction::Map);
        assert!(c.is_animating(t0));
        let (acts, t1) = run(&mut c, t0, 1000);
        assert!(acts.is_empty());
        assert!((c.visual() - 1.0).abs() < 1e-6);
        assert!(!c.is_animating(t1));
        assert!(c.has_deadline());
        // After auto_hide_secs (4 s) + fade-out it unmaps exactly once.
        let (acts, t2) = run(&mut c, t1, 3600);
        assert_eq!(acts, vec![SurfaceAction::Unmap]);
        assert!(!c.is_mapped());
        assert!(!c.is_animating(t2));
        assert!(!c.has_deadline());
        // Insignificant update keeps it hidden.
        c.apply(
            OverlayCommand::State {
                state: buds(79, 75, 40),
            },
            t2,
        );
        assert_eq!(c.tick(t2), SurfaceAction::None);
        // `show` brings it back.
        c.apply(
            OverlayCommand::Show {
                reason: "test".into(),
            },
            t2,
        );
        assert_eq!(c.tick(t2), SurfaceAction::Map);
    }

    #[test]
    fn show_during_fade_out_reverses_without_remap() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t0,
        );
        c.tick(t0);
        let (_, t1) = run(&mut c, t0, 4100);
        assert!(c.is_mapped() && c.visual() < 1.0);
        c.apply(
            OverlayCommand::Show {
                reason: String::new(),
            },
            t1,
        );
        let (acts, _) = run(&mut c, t1, 500);
        assert!(acts.is_empty());
        assert!((c.visual() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn hide_off_and_always() {
        let t0 = Instant::now();
        let mut c = Controller::new(OverlayConfig {
            mode: OverlayMode::Always,
            position: OverlayPosition::BottomLeft,
            ..OverlayConfig::default()
        });
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t0,
        );
        assert_eq!(c.tick(t0), SurfaceAction::Map);
        let (acts, t1) = run(&mut c, t0, 10_000);
        assert!(acts.is_empty(), "always mode never hides by itself");
        assert!(!c.has_deadline());

        c.apply(OverlayCommand::Hide, t1);
        let (acts, t2) = run(&mut c, t1, 1000);
        assert_eq!(acts, vec![SurfaceAction::Unmap]);
        c.apply(
            OverlayCommand::Show {
                reason: String::new(),
            },
            t2,
        );
        assert_eq!(c.tick(t2), SurfaceAction::Map);

        // Disconnect hides even in always mode.
        let mut s = buds(80, 75, 40);
        s.connected = false;
        c.apply(OverlayCommand::State { state: s }, t2);
        let (acts, t3) = run(&mut c, t2, 1000);
        assert_eq!(acts, vec![SurfaceAction::Unmap]);

        // Off never shows.
        c.apply(
            OverlayCommand::Config(OverlayConfig {
                mode: OverlayMode::Off,
                ..OverlayConfig::default()
            }),
            t3,
        );
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t3,
        );
        c.apply(
            OverlayCommand::Show {
                reason: String::new(),
            },
            t3,
        );
        let (acts, _) = run(&mut c, t3, 1000);
        assert!(acts.is_empty());
    }

    #[test]
    fn always_to_auto_eventually_hides() {
        let t0 = Instant::now();
        let mut c = Controller::new(OverlayConfig {
            mode: OverlayMode::Always,
            ..OverlayConfig::default()
        });
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t0,
        );
        c.tick(t0);
        let (_, t1) = run(&mut c, t0, 500);
        c.apply(OverlayCommand::Config(OverlayConfig::default()), t1);
        let (acts, _) = run(&mut c, t1, 5000);
        assert_eq!(acts, vec![SurfaceAction::Unmap]);
    }

    #[test]
    fn idle_gap_does_not_skip_fade_in() {
        let t0 = Instant::now();
        let mut c = Controller::default();
        c.tick(t0);
        let later = t0 + Duration::from_secs(3600);
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            later,
        );
        assert_eq!(c.tick(later), SurfaceAction::Map);
        c.tick(later + Duration::from_secs(1));
        assert!(c.visual() < 0.9);
    }

    #[test]
    fn preview_shows_without_data_and_in_any_mode() {
        let t0 = Instant::now();
        let mut c = Controller::new(OverlayConfig {
            mode: OverlayMode::Off,
            ..OverlayConfig::default()
        });
        assert!(c.display_state(t0).is_none());
        c.apply(
            OverlayCommand::Show {
                reason: "preview".into(),
            },
            t0,
        );
        assert_eq!(c.tick(t0), SurfaceAction::Map);
        assert!(c.display_state(t0).is_some_and(|s| s.left.is_some()));
        assert!(c.has_deadline());
        let (acts, t1) = run(&mut c, t0, 5000);
        assert_eq!(acts, vec![SurfaceAction::Unmap]);
        assert!(c.display_state(t1).is_none());
    }

    #[test]
    fn compact_expands_on_change_then_collapses() {
        let t0 = Instant::now();
        let mut c = Controller::new(OverlayConfig {
            mode: OverlayMode::Compact,
            ..OverlayConfig::default()
        });
        c.apply(
            OverlayCommand::State {
                state: buds(80, 75, 40),
            },
            t0,
        );
        assert_eq!(c.tick(t0), SurfaceAction::Map);
        assert!(
            (c.expansion() - 1.0).abs() < 1e-6,
            "connect shows the full pill"
        );
        assert!(c.has_deadline());
        let (acts, t1) = run(&mut c, t0, 5000);
        assert!(acts.is_empty(), "compact never unmaps while connected");
        assert!(c.expansion() < 1e-6, "collapsed to the widget");
        assert!(!c.is_animating(t1));

        // A pod leaving expands it again.
        let mut s = buds(80, 75, 40);
        s.left.as_mut().unwrap().present = false;
        c.apply(OverlayCommand::State { state: s }, t1);
        let (acts, _) = run(&mut c, t1, 500);
        assert!(acts.is_empty());
        assert!((c.expansion() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn route_change_is_meaningful() {
        let a = buds(80, 80, 50);
        let mut b = a.clone();
        b.elsewhere = true;
        assert_eq!(meaningful_change(Some(&a), &b), Some("route"));
        assert_eq!(meaningful_change(Some(&b), &b), None);
    }

    #[test]
    fn route_label_names_the_device_then_says_back() {
        let t0 = Instant::now();
        let mut c = Controller::new(OverlayConfig::default());
        let mut s = buds(80, 80, 60);
        c.apply(OverlayCommand::State { state: s.clone() }, t0);
        assert_eq!(c.route_label(t0), None);
        s.elsewhere = true;
        s.elsewhere_name = Some("iPhone".into());
        c.apply(OverlayCommand::State { state: s.clone() }, t0);
        assert_eq!(c.route_label(t0).as_deref(), Some("Playing on iPhone"));
        s.elsewhere_name = None;
        c.apply(OverlayCommand::State { state: s.clone() }, t0);
        assert_eq!(c.route_label(t0).as_deref(), Some("On another device"));
        s.elsewhere = false;
        c.apply(OverlayCommand::State { state: s.clone() }, t0);
        assert_eq!(c.route_label(t0).as_deref(), Some("Back on this PC"));
        assert!(c.wants_visible(t0));
        assert_eq!(c.route_label(t0 + Duration::from_secs(30)), None);
    }
}

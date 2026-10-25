//! Pure presentation logic: when the pill should be visible, and its animation.
//! No iced or Wayland types here, so all of it is unit tested.

use std::time::{Duration, Instant};

use crate::overlay_protocol::{
    ComponentState, OverlayCommand, OverlayConfig, OverlayMode, OverlayState,
};

pub const APPEAR: Duration = Duration::from_millis(240);
pub const DISAPPEAR: Duration = Duration::from_millis(320);

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
        }
    }

    fn trigger(&mut self, now: Instant) {
        self.forced_hidden = false;
        if self.config.mode == OverlayMode::Auto {
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
                if was != OverlayMode::Auto && self.config.mode == OverlayMode::Auto {
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
                self.state = Some(state);
                if reason.is_some() {
                    self.trigger(now);
                }
            }
            OverlayCommand::Show { .. } => self.trigger(now),
            OverlayCommand::Hide => {
                self.hide_at = None;
                self.forced_hidden = true;
            }
            OverlayCommand::Quit => {}
        }
    }

    pub fn wants_visible(&self, now: Instant) -> bool {
        if self.config.mode == OverlayMode::Off || self.forced_hidden {
            return false;
        }
        if !self.state.as_ref().is_some_and(|s| s.connected) {
            return false;
        }
        match self.config.mode {
            OverlayMode::Always => true,
            OverlayMode::Auto => self.hide_at.is_some_and(|t| now < t),
            OverlayMode::Off => false,
        }
    }

    /// Advances the animation to `now` and says what to do with the surface.
    pub fn tick(&mut self, now: Instant) -> SurfaceAction {
        let target = self.wants_visible(now);
        if !target && self.hide_at.is_some_and(|t| now >= t) {
            self.hide_at = None;
        }
        let dt = self
            .last_tick
            .map(|t| now.saturating_duration_since(t))
            .unwrap_or_default()
            // A long gap (no ticks while idle) must not skip the animation.
            .min(Duration::from_millis(50));
        self.last_tick = Some(now);

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
        (target && (!self.mapped || self.progress < 1.0)) || (!target && self.mapped)
    }

    /// True while a timer must run to notice the auto-hide deadline.
    pub fn has_deadline(&self) -> bool {
        self.mapped && self.hide_at.is_some()
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
}

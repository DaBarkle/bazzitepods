//! What the island shows and how it moves, modelled on the iPhone's Dynamic Island:
//!
//! * **Compact**: a slim pill at rest (Compact mode), glyph leading, levels trailing.
//! * **Expanded**: the full card with a ring per battery (on connect, on `show`, in Full
//!   mode, and when a pod comes or goes).
//! * **Alerts**: a wide pill that names the one thing that just happened (charging,
//!   low battery, noise control mode, the audio moving to or from another device,
//!   disconnect), then folds back.
//!
//! Shape changes run on springs that keep their velocity when interrupted, contents
//! cross-fade with a slight scale (Apple's "content replace" transition), rings sweep
//! in, and numbers roll when a level changes. All of it is plain data here so it can be
//! unit tested; `view.rs` only draws what this says.

use crate::overlay_protocol::{ComponentState, ListeningMode, OverlayState};
use crate::spring::{Feel, Spring};

// --- Geometry (logical px) ------------------------------------------------------

pub const COMPACT_H: f32 = 36.0;
pub const COMPACT_LEAD: f32 = 40.0;
pub const COMPACT_ITEM: f32 = 52.0;
pub const COMPACT_TAIL: f32 = 8.0;
pub const EXPANDED_W_BUDS: f32 = 344.0;
pub const EXPANDED_W_HEADPHONE: f32 = 252.0;
pub const EXPANDED_H: f32 = 112.0;
pub const EXPANDED_R: f32 = 30.0;
pub const ALERT_H: f32 = 48.0;
pub const ALERT_MIN_W: f32 = 150.0;
pub const ALERT_MAX_W: f32 = 380.0;
/// Leading inset to the icon's center, and where the title starts.
pub const ALERT_ICON_X: f32 = 27.0;
pub const ALERT_TEXT_X: f32 = 50.0;
pub const ALERT_TRAIL_PAD: f32 = 22.0;
pub const ALERT_TITLE_SIZE: f32 = 15.0;
/// One trailing battery in an alert: ring, then the percentage.
pub const ALERT_LEVEL_W: f32 = 62.0;
/// The shape the island grows out of and shrinks back into, like the iPhone's sensor
/// housing.
pub const SEED_W: f32 = 96.0;
pub const SEED_H: f32 = 28.0;

// --- Timing ---------------------------------------------------------------------

/// Outgoing content fades and shrinks away this fast...
pub const CONTENT_OUT: f32 = 0.13;
/// ...and the incoming content starts after this delay...
pub const CONTENT_IN_DELAY: f32 = 0.12;
/// ...and takes this long to come in.
pub const CONTENT_IN: f32 = 0.30;
/// Rings sweep from empty to their level when a card or alert opens.
pub const REVEAL_DELAY: f32 = 0.10;
pub const REVEAL: f32 = 0.80;
/// A changed percentage rolls to its new value.
pub const ROLL: f32 = 0.36;
/// The low battery ring breathes twice.
pub const BREATHE: f32 = 1.6;

const GROW_W: Feel = Feel::new(0.48, 0.24);
const GROW_H: Feel = Feel::new(0.54, 0.20);
const SHRINK: Feel = Feel::new(0.42, 0.06);
const FADE_IN: Feel = Feel::new(0.26, 0.0);
const FADE_OUT: Feel = Feel::new(0.32, 0.0);
const PULSE: Feel = Feel::new(0.36, 0.45);
const FILL: Feel = Feel::new(0.8, 0.0);

/// Which batteries an alert is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Parts {
    pub left: bool,
    pub right: bool,
    pub case: bool,
    pub headphone: bool,
}

impl Parts {
    pub fn any(self) -> bool {
        self.left || self.right || self.case || self.headphone
    }

    pub fn count(self) -> usize {
        [self.left, self.right, self.case, self.headphone]
            .iter()
            .filter(|b| **b)
            .count()
    }

    /// What to call these parts in a sentence.
    fn noun(self) -> &'static str {
        match (self.left, self.right, self.case, self.headphone) {
            (_, _, _, true) => "",
            (true, true, false, _) => "AirPods",
            (true, false, false, _) => "Left AirPod",
            (false, true, false, _) => "Right AirPod",
            (false, false, true, _) => "Case",
            _ => "",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Alert {
    Charging(Parts),
    Low(Parts),
    /// Another device took the audio (its name when known).
    Elsewhere(Option<String>),
    /// The audio came back from another device.
    Back,
    Listening(ListeningMode),
    Disconnected,
}

impl Alert {
    pub fn title(&self, state: &OverlayState) -> String {
        match self {
            Alert::Charging(p) => match p.noun() {
                "" => "Charging".into(),
                n => format!("{n} charging"),
            },
            Alert::Low(p) => match p.noun() {
                "" => "Low battery".into(),
                n => format!("{n} low"),
            },
            Alert::Elsewhere(name) => elsewhere_label(name.as_deref()),
            Alert::Back => "Back on this PC".into(),
            Alert::Listening(m) => m.label().into(),
            Alert::Disconnected => display_name(state),
        }
    }

    /// How long the alert stays before the island folds back, in seconds.
    pub fn duration(&self) -> f32 {
        match self {
            Alert::Listening(_) => 2.2,
            Alert::Back | Alert::Disconnected => 2.6,
            Alert::Charging(_) => 3.0,
            Alert::Elsewhere(_) => 3.5,
            Alert::Low(_) => 5.0,
        }
    }

    /// The batteries shown on the trailing side, if any.
    pub fn parts(&self) -> Option<Parts> {
        match self {
            Alert::Charging(p) | Alert::Low(p) => Some(*p),
            _ => None,
        }
    }

    /// Trailing text, if any.
    pub fn trailing_text(&self) -> Option<&'static str> {
        matches!(self, Alert::Disconnected).then_some("Disconnected")
    }
}

pub fn elsewhere_label(name: Option<&str>) -> String {
    match name.map(str::trim) {
        Some(n) if !n.is_empty() => format!("Playing on {n}"),
        _ => "On another device".into(),
    }
}

pub fn display_name(state: &OverlayState) -> String {
    match state.name.trim() {
        "" => "AirPods".into(),
        n => n.into(),
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Content {
    Compact,
    Expanded,
    Alert(Alert),
}

impl Content {
    /// How long after this content appears frames are still needed (reveal, pops).
    fn settle(&self) -> f32 {
        match self {
            Content::Compact => CONTENT_IN_DELAY + CONTENT_IN,
            Content::Alert(Alert::Low(_)) => BREATHE.max(REVEAL_DELAY + REVEAL),
            _ => REVEAL_DELAY + REVEAL,
        }
    }
}

fn is_headphone(state: &OverlayState) -> bool {
    state.headphone.is_some()
}

/// Rough width of a line of text in the default sans, in logical px. Errs on the wide
/// side so the island never clips a word.
pub fn text_width(s: &str, size: f32) -> f32 {
    let em: f32 = s
        .chars()
        .map(|c| match c {
            'i' | 'l' | 'j' | '\'' | '.' | ',' | ':' | '|' | '!' => 0.27,
            'f' | 't' | 'r' | 'I' | ' ' | '(' | ')' => 0.34,
            'm' | 'w' | 'M' | 'W' => 0.86,
            '%' => 0.80,
            '0'..='9' => 0.60,
            'A'..='Z' => 0.68,
            _ if c.is_ascii() => 0.56,
            _ => 0.70,
        })
        .sum();
    (em * size * 1.04).ceil()
}

/// The island's size and corner radius for `content`.
pub fn shape(content: &Content, state: &OverlayState) -> (f32, f32, f32) {
    match content {
        Content::Compact => {
            let items = if is_headphone(state) { 1.0 } else { 3.0 };
            (
                COMPACT_LEAD + items * COMPACT_ITEM + COMPACT_TAIL,
                COMPACT_H,
                COMPACT_H / 2.0,
            )
        }
        Content::Expanded => (
            if is_headphone(state) {
                EXPANDED_W_HEADPHONE
            } else {
                EXPANDED_W_BUDS
            },
            EXPANDED_H,
            EXPANDED_R,
        ),
        Content::Alert(a) => {
            let title = text_width(&a.title(state), ALERT_TITLE_SIZE);
            let trailing = match (a.parts(), a.trailing_text()) {
                (Some(p), _) => 14.0 + p.count() as f32 * ALERT_LEVEL_W,
                (None, Some(t)) => 16.0 + text_width(t, 13.0),
                (None, None) => 0.0,
            };
            let w =
                (ALERT_TEXT_X + title + trailing + ALERT_TRAIL_PAD).clamp(ALERT_MIN_W, ALERT_MAX_W);
            (w, ALERT_H, ALERT_H / 2.0)
        }
    }
}

// --- Easing ---------------------------------------------------------------------

pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

pub fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Ease out with a small overshoot (about 4%), for icons that pop in.
pub fn ease_out_back(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let c1 = 1.2;
    let c3 = c1 + 1.0;
    1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
}

// --- Motion ---------------------------------------------------------------------

/// The displayed fill of one battery ring and its number roll.
#[derive(Debug, Clone, Copy)]
pub struct Level {
    pub fill: Spring,
    pub value: Option<u8>,
    /// The value before the last change, while it rolls away.
    pub prev: Option<u8>,
    pub change_age: f32,
}

impl Default for Level {
    fn default() -> Self {
        Self {
            fill: Spring::new(0.0, FILL, 0.0005),
            value: None,
            prev: None,
            change_age: ROLL,
        }
    }
}

impl Level {
    /// Returns true when the change starts an animation.
    fn update(&mut self, comp: Option<&ComponentState>, animate: bool) -> bool {
        let value = comp
            .filter(|c| c.present || c.stale)
            .map(|c| c.level.min(100));
        if value == self.value {
            return false;
        }
        let frac = value.map_or(0.0, |v| v as f32 / 100.0);
        if animate && self.value.is_some() && value.is_some() {
            self.prev = self.value;
            self.change_age = 0.0;
            self.fill.to(frac, FILL);
        } else {
            self.prev = None;
            self.change_age = ROLL;
            self.fill.snap(frac);
        }
        self.value = value;
        self.prev.is_some()
    }

    fn step(&mut self, dt: f32) -> bool {
        let moving = self.fill.step(dt);
        if self.change_age < ROLL {
            self.change_age = (self.change_age + dt).min(ROLL);
            if self.change_age >= ROLL {
                self.prev = None;
            }
            return true;
        }
        moving
    }

    /// 0..1 progress of the number roll (1 = settled).
    pub fn roll(&self) -> f32 {
        if self.prev.is_some() {
            smoothstep(self.change_age / ROLL)
        } else {
            1.0
        }
    }
}

/// A line of text that cross-rolls to its new wording when it changes.
#[derive(Debug, Clone, Default)]
pub struct Caption {
    pub text: String,
    pub prev: Option<String>,
    pub age: f32,
}

impl Caption {
    fn set(&mut self, text: &str, animate: bool) {
        if self.text == text {
            return;
        }
        let old = std::mem::replace(&mut self.text, text.to_string());
        if animate && !old.is_empty() {
            self.prev = Some(old);
            self.age = 0.0;
        } else {
            self.prev = None;
        }
    }

    fn step(&mut self, dt: f32) -> bool {
        if self.prev.is_none() {
            return false;
        }
        self.age += dt;
        if self.age >= ROLL {
            self.prev = None;
            return false;
        }
        true
    }

    /// 0..1 progress of the roll (1 = settled).
    pub fn roll(&self) -> f32 {
        if self.prev.is_some() {
            smoothstep(self.age / ROLL)
        } else {
            1.0
        }
    }
}

/// Index into [`Motion::levels`].
pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;
pub const CASE: usize = 2;
pub const HEADPHONE: usize = 3;

#[derive(Debug, Clone)]
pub struct Motion {
    pub w: Spring,
    pub h: Spring,
    pub r: Spring,
    /// Presence of the whole island, 0 = gone.
    pub alpha: Spring,
    /// Scale bump when something changes inside an already open card.
    pub pulse: Spring,
    /// What is coming in (None while folding away).
    pub shown: Option<Content>,
    pub shown_age: f32,
    /// What is going out, and for how long it has been leaving.
    pub outgoing: Option<(Content, f32)>,
    pub levels: [Level; 4],
    /// The card's second line (status, or the latest news).
    pub caption: Caption,
}

impl Default for Motion {
    fn default() -> Self {
        Self {
            w: Spring::new(SEED_W, SHRINK, 0.05),
            h: Spring::new(SEED_H, SHRINK, 0.05),
            r: Spring::new(SEED_H / 2.0, SHRINK, 0.02),
            alpha: Spring::new(0.0, FADE_IN, 0.002),
            pulse: Spring::new(1.0, PULSE, 0.0005),
            shown: None,
            shown_age: 0.0,
            outgoing: None,
            levels: [Level::default(); 4],
            caption: Caption::default(),
        }
    }
}

impl Motion {
    /// Starts from the seed, invisible (the surface was just created).
    pub fn reset(&mut self) {
        let levels = self.levels;
        let caption = std::mem::take(&mut self.caption);
        *self = Self::default();
        self.levels = levels;
        self.caption = caption;
    }

    /// Changes what is shown. `target` is the new content's shape, or None to fold away.
    pub fn switch(&mut self, content: Option<Content>, target: Option<(f32, f32, f32)>) {
        if self.shown == content {
            return;
        }
        if let Some(old) = self.shown.take() {
            // The content that was still fading in leaves from where it was.
            self.outgoing = Some((old, 0.0));
        }
        self.shown = content;
        self.shown_age = 0.0;
        match target {
            Some((w, h, r)) => {
                let grow = w * h > self.w.value * self.h.value;
                let (fw, fh) = if grow {
                    (GROW_W, GROW_H)
                } else {
                    (SHRINK, SHRINK)
                };
                self.w.to(w, fw);
                self.h.to(h, fh);
                self.r.to(r, fh);
                self.alpha.to(1.0, FADE_IN);
            }
            None => {
                self.w.to(SEED_W.min(self.w.target), SHRINK);
                self.h.to(SEED_H, SHRINK);
                self.r.to(SEED_H / 2.0, SHRINK);
                self.alpha.to(0.0, FADE_OUT);
            }
        }
    }

    /// The shown content's size changed (e.g. a longer title); follow it.
    pub fn resize(&mut self, (w, h, r): (f32, f32, f32)) {
        if self.shown.is_some() && (self.w.target, self.h.target, self.r.target) != (w, h, r) {
            let grow = w * h > self.w.target * self.h.target;
            let (fw, fh) = if grow {
                (GROW_W, GROW_H)
            } else {
                (SHRINK, SHRINK)
            };
            self.w.to(w, fw);
            self.h.to(h, fh);
            self.r.to(r, fh);
        }
    }

    /// A small bounce of the whole card, for news inside a card that stays open.
    pub fn bump(&mut self) {
        self.pulse.kick(0.55);
    }

    /// Sets the card's second line; it rolls over when `animate` and it changed.
    pub fn set_caption(&mut self, text: &str, animate: bool) {
        self.caption.set(text, animate);
    }

    /// Takes new battery readings. Returns true when one of them starts to animate.
    pub fn update_levels(&mut self, state: &OverlayState, animate: bool) -> bool {
        [
            self.levels[LEFT].update(state.left.as_ref(), animate),
            self.levels[RIGHT].update(state.right.as_ref(), animate),
            self.levels[CASE].update(state.case.as_ref(), animate),
            self.levels[HEADPHONE].update(state.headphone.as_ref(), animate),
        ]
        .contains(&true)
    }

    /// Advances everything by `dt` seconds. Returns true while anything still moves.
    pub fn step(&mut self, dt: f32) -> bool {
        let mut moving = false;
        for s in [
            &mut self.w,
            &mut self.h,
            &mut self.r,
            &mut self.alpha,
            &mut self.pulse,
        ] {
            moving |= s.step(dt);
        }
        for l in &mut self.levels {
            moving |= l.step(dt);
        }
        moving |= self.caption.step(dt);
        if let Some((_, age)) = &mut self.outgoing {
            *age += dt;
            if *age >= CONTENT_OUT {
                self.outgoing = None;
            } else {
                moving = true;
            }
        }
        if let Some(c) = &self.shown
            && self.shown_age < c.settle()
        {
            self.shown_age = (self.shown_age + dt).min(c.settle());
            moving = true;
        }
        moving
    }

    /// True once the island has faded away completely.
    pub fn gone(&self) -> bool {
        self.shown.is_none() && self.alpha.target == 0.0 && self.alpha.value <= 0.004
    }

    /// Opacity and scale of the incoming content.
    pub fn incoming(&self) -> (f32, f32) {
        let t = (self.shown_age - CONTENT_IN_DELAY) / CONTENT_IN;
        (smoothstep(t), 0.9 + 0.1 * ease_out_back(t))
    }

    /// Opacity and scale of the outgoing content.
    pub fn leaving(&self) -> Option<(&Content, f32, f32)> {
        self.outgoing.as_ref().map(|(c, age)| {
            let t = (age / CONTENT_OUT).clamp(0.0, 1.0);
            (c, 1.0 - t, 1.0 - 0.08 * t)
        })
    }

    /// 0..1 sweep of rings in freshly opened content.
    pub fn reveal(&self) -> f32 {
        ease_out_cubic((self.shown_age - REVEAL_DELAY) / REVEAL)
    }

    /// Current island size, never smaller than zero.
    pub fn size(&self) -> (f32, f32, f32) {
        let w = self.w.value.max(1.0);
        let h = self.h.value.max(1.0);
        (w, h, self.r.value.clamp(0.0, w.min(h) / 2.0))
    }
}

#[cfg(test)]
#[path = "island_tests.rs"]
mod tests;

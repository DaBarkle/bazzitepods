//! Visual tokens and widget styles, in the island's language: flat black, quiet grouped
//! panels, hairline edges, and colour only where something is happening.
//!
//! The dark palette matches the overlay's purist island (pure black, white text at 92% / 55%,
//! iOS green, amber, red and blue), so the window and the island read as one product.

use crate::ui::format::{StatusTone, Tone};
use iced::font::Weight;
use iced::theme::Palette;
use iced::widget::{button, container, scrollable, slider, toggler};
use iced::{Background, Border, Color, Font, Shadow, Theme, Vector};

pub const INTER: Font = Font::with_name("Inter");
pub const INTER_MEDIUM: Font = Font {
    weight: Weight::Medium,
    ..INTER
};
pub const INTER_SEMIBOLD: Font = Font {
    weight: Weight::Semibold,
    ..INTER
};

pub const GROUP_RADIUS: f32 = 20.0;
pub const TILE_RADIUS: f32 = 15.0;

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

const fn white(a: f32) -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, a)
}

const fn black(a: f32) -> Color {
    Color::from_rgba(0.0, 0.0, 0.0, a)
}

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    /// Window background.
    pub bg: Color,
    /// Grouped panels.
    pub surface: Color,
    /// Tracks of segmented controls, inputs, secondary buttons.
    pub surface_alt: Color,
    /// The selected segment riding on a track.
    pub raised: Color,
    /// Hairline edges and row separators.
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub green: Color,
    pub amber: Color,
    pub red: Color,
    /// Empty part of a battery ring or slider.
    pub track: Color,
    /// Switch track when off.
    pub switch_off: Color,
}

pub const DARK: Tokens = Tokens {
    dark: true,
    bg: rgb(0x000000),
    surface: rgb(0x0E0E10),
    surface_alt: rgb(0x1C1C1E),
    raised: rgb(0x3A3A3C),
    border: white(0.08),
    text: white(0.92),
    text_dim: white(0.55),
    text_faint: white(0.30),
    accent: rgb(0x0A84FF),
    on_accent: rgb(0xFFFFFF),
    green: rgb(0x34C759),
    amber: rgb(0xFFB340),
    red: rgb(0xFF453A),
    track: white(0.10),
    switch_off: rgb(0x39393D),
};

pub const LIGHT: Tokens = Tokens {
    dark: false,
    bg: rgb(0xF2F2F7),
    surface: rgb(0xFFFFFF),
    surface_alt: rgb(0xE9E9EE),
    raised: rgb(0xFFFFFF),
    border: black(0.07),
    text: black(0.88),
    text_dim: black(0.50),
    text_faint: black(0.28),
    accent: rgb(0x007AFF),
    on_accent: rgb(0xFFFFFF),
    green: rgb(0x28A745),
    amber: rgb(0xE08A00),
    red: rgb(0xE5342A),
    track: black(0.08),
    switch_off: rgb(0xE3E3E8),
};

impl Tokens {
    pub fn of(theme: &Theme) -> Tokens {
        if theme.extended_palette().is_dark {
            DARK
        } else {
            LIGHT
        }
    }

    pub fn tone(&self, tone: Tone, charging: bool) -> Color {
        match tone {
            Tone::Muted => self.text_faint,
            _ if charging => self.green,
            Tone::Normal => self.green,
            Tone::Low => self.amber,
            Tone::Critical => self.red,
        }
    }

    /// Glow colour of the status island; None for a quiet (idle) island.
    pub fn status(&self, tone: StatusTone) -> Option<Color> {
        match tone {
            StatusTone::Good => Some(DARK.green),
            StatusTone::Low | StatusTone::Busy => Some(DARK.amber),
            StatusTone::Critical => Some(DARK.red),
            StatusTone::Info => Some(DARK.accent),
            StatusTone::Idle => None,
        }
    }
}

pub fn theme(dark: bool) -> Theme {
    let t = if dark { DARK } else { LIGHT };
    Theme::custom(
        if dark { "Kase Dark" } else { "Kase Light" },
        Palette {
            background: t.bg,
            text: t.text,
            primary: t.accent,
            success: t.green,
            warning: t.amber,
            danger: t.red,
        },
    )
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

fn mix(base: Color, over: Color) -> Color {
    let a = over.a;
    Color::from_rgb(
        base.r * (1.0 - a) + over.r * a,
        base.g * (1.0 - a) + over.g * a,
        base.b * (1.0 - a) + over.b * a,
    )
}

fn round(radius: f32) -> Border {
    Border {
        radius: radius.into(),
        ..Default::default()
    }
}

pub fn page(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.bg)),
        text_color: Some(t.text),
        ..Default::default()
    }
}

/// A grouped panel of rows. Dark: a hairline edge on black. Light: white with a soft lift.
pub fn group(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.surface)),
        border: Border {
            color: if t.dark { t.border } else { Color::TRANSPARENT },
            width: 1.0,
            radius: GROUP_RADIUS.into(),
        },
        shadow: if t.dark {
            Shadow::default()
        } else {
            Shadow {
                color: black(0.05),
                offset: Vector::new(0.0, 1.0),
                blur_radius: 3.0,
            }
        },
        text_color: Some(t.text),
        ..Default::default()
    }
}

/// The status island: always black (it is the island), with a coloured edge light whose
/// strength is animated by the window (1 = resting glow, more while pulsing).
pub fn status_island(tone: StatusTone, strength: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = Tokens::of(theme);
        let glow = t.status(tone);
        let s = strength.max(0.0);
        let (edge, shadow) = match glow {
            Some(c) => (
                with_alpha(c, (0.55 * s).min(1.0)),
                Shadow {
                    color: with_alpha(c, (0.30 * s).min(0.8)),
                    offset: Vector::ZERO,
                    blur_radius: 14.0 + 10.0 * (s - 1.0).max(0.0),
                },
            ),
            None => (
                if t.dark {
                    white(0.10)
                } else {
                    Color::TRANSPARENT
                },
                Shadow::default(),
            ),
        };
        container::Style {
            background: Some(Background::Color(Color::BLACK)),
            border: Border {
                color: edge,
                width: 1.0,
                radius: 999.0.into(),
            },
            shadow,
            text_color: Some(white(0.92)),
            ..Default::default()
        }
    }
}

/// Status island's little light.
pub fn status_dot(tone: StatusTone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let c = Tokens::of(theme).status(tone).unwrap_or(rgb(0x8E8E93));
        container::Style {
            background: Some(Background::Color(c)),
            border: round(999.0),
            shadow: if tone == StatusTone::Idle {
                Shadow::default()
            } else {
                Shadow {
                    color: with_alpha(c, 0.8),
                    offset: Vector::ZERO,
                    blur_radius: 6.0,
                }
            },
            ..Default::default()
        }
    }
}

pub fn status_detail(tone: StatusTone) -> impl Fn(&Theme) -> iced::widget::text::Style {
    move |_theme| iced::widget::text::Style {
        color: Some(match tone {
            StatusTone::Low => DARK.amber,
            StatusTone::Critical => DARK.red,
            _ => white(0.55),
        }),
    }
}

/// Track of a segmented control (tabs, choices).
pub fn track(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| container::Style {
        background: Some(Background::Color(Tokens::of(theme).surface_alt)),
        border: round(radius),
        ..Default::default()
    }
}

/// The selected segment, drawn under the labels and slid between them by the window.
pub fn thumb(radius: f32) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = Tokens::of(theme);
        container::Style {
            background: Some(Background::Color(t.raised)),
            border: Border {
                color: if t.dark {
                    white(0.06)
                } else {
                    Color::TRANSPARENT
                },
                width: 0.5,
                radius: radius.into(),
            },
            shadow: Shadow {
                color: black(if t.dark { 0.45 } else { 0.12 }),
                offset: Vector::new(0.0, 1.0),
                blur_radius: 4.0,
            },
            ..Default::default()
        }
    }
}

/// A label sitting on a segmented track. The thumb under it shows the selection, so the
/// button itself only changes its text colour.
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = Tokens::of(theme);
        button::Style {
            background: None,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => t.text_faint,
                (true, _) => t.text,
                (false, button::Status::Hovered | button::Status::Pressed) => {
                    mix(t.text_dim, with_alpha(t.text, 0.5))
                }
                _ => t.text_dim,
            },
            border: round(999.0),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Listening mode tile; the glowing highlight slides under it (see [`mode_thumb`]).
pub fn tile(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = Tokens::of(theme);
        let hover = !selected && matches!(status, button::Status::Hovered);
        button::Style {
            background: hover.then(|| Background::Color(with_alpha(t.text, 0.04))),
            text_color: match status {
                button::Status::Disabled => t.text_faint,
                _ if selected => t.text,
                button::Status::Hovered | button::Status::Pressed => t.text,
                _ => t.text_dim,
            },
            border: round(TILE_RADIUS),
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Selected listening mode: an accent edge light, like the island's.
pub fn mode_thumb(enabled: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = Tokens::of(theme);
        let a = if enabled { 1.0 } else { 0.4 };
        container::Style {
            background: Some(Background::Color(if t.dark {
                t.surface_alt
            } else {
                t.surface
            })),
            border: Border {
                color: with_alpha(t.accent, if t.dark { 0.6 } else { 0.9 } * a),
                width: if t.dark { 1.0 } else { 1.5 },
                radius: TILE_RADIUS.into(),
            },
            shadow: Shadow {
                color: with_alpha(t.accent, if t.dark { 0.22 } else { 0.16 } * a),
                offset: Vector::new(0.0, if t.dark { 0.0 } else { 3.0 }),
                blur_radius: 16.0,
            },
            ..Default::default()
        }
    }
}

pub fn divider(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(Tokens::of(theme).border)),
        ..Default::default()
    }
}

pub fn icon_disc(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.surface_alt)),
        border: round(999.0),
        ..Default::default()
    }
}

/// The little screen on the Island tab.
pub fn screen(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Gradient(iced::Gradient::Linear(
            iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI))
                .add_stop(0.0, rgb(0x0B0F1C))
                .add_stop(0.55, rgb(0x1A2342))
                .add_stop(1.0, rgb(0x3B1F5A)),
        ))),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: 16.0.into(),
        },
        ..Default::default()
    }
}

/// An empty island spot on the little screen: a faint outline that lights up on hover.
pub fn spot(theme: &Theme, status: button::Status) -> button::Style {
    let _ = theme;
    let edge = match status {
        button::Status::Hovered | button::Status::Pressed => white(0.55),
        button::Status::Disabled => white(0.10),
        _ => white(0.22),
    };
    button::Style {
        background: Some(Background::Color(white(0.03))),
        text_color: white(0.9),
        border: Border {
            color: edge,
            width: 1.0,
            radius: 999.0.into(),
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// The miniature island on the little screen.
pub fn mini_island(enabled: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, _status| button::Style {
        background: Some(Background::Color(Color::BLACK)),
        text_color: white(0.92),
        border: Border {
            color: with_alpha(DARK.green, if enabled { 0.6 } else { 0.0 }),
            width: 0.75,
            radius: 999.0.into(),
        },
        shadow: Shadow {
            color: with_alpha(DARK.green, if enabled { 0.35 } else { 0.0 }),
            offset: Vector::ZERO,
            blur_radius: 10.0,
        },
        snap: true,
    }
}

/// Pill button: secondary actions ("Show on screen", "Rename").
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = Tokens::of(theme);
    let bg = match status {
        button::Status::Hovered => mix(t.surface_alt, with_alpha(t.text, 0.08)),
        button::Status::Pressed => mix(t.surface_alt, with_alpha(t.text, 0.14)),
        _ => t.surface_alt,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if status == button::Status::Disabled {
            t.text_faint
        } else {
            t.text
        },
        border: round(999.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Filled accent pill ("Use on this PC").
pub fn primary(theme: &Theme, status: button::Status) -> button::Style {
    let t = Tokens::of(theme);
    let bg = match status {
        button::Status::Hovered => mix(t.accent, white(0.10)),
        button::Status::Pressed => mix(t.accent, black(0.12)),
        button::Status::Disabled => with_alpha(t.accent, 0.5),
        _ => t.accent,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: t.on_accent,
        border: round(999.0),
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn toggle(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let t = Tokens::of(theme);
    let (on, disabled, hovered) = match status {
        toggler::Status::Active { is_toggled } => (is_toggled, false, false),
        toggler::Status::Hovered { is_toggled } => (is_toggled, false, true),
        toggler::Status::Disabled { is_toggled } => (is_toggled, true, false),
    };
    let track = if on { t.green } else { t.switch_off };
    let track = if hovered && !on {
        mix(track, with_alpha(t.text, 0.06))
    } else {
        track
    };
    let alpha = if disabled { 0.45 } else { 1.0 };
    toggler::Style {
        background: Background::Color(with_alpha(track, alpha)),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Background::Color(with_alpha(Color::WHITE, alpha)),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.09,
    }
}

/// Thin track, white knob: the iOS slider.
pub fn slide(theme: &Theme, status: slider::Status) -> slider::Style {
    let t = Tokens::of(theme);
    let knob = if matches!(status, slider::Status::Dragged) {
        rgb(0xF2F2F2)
    } else {
        Color::WHITE
    };
    slider::Style {
        rail: slider::Rail {
            backgrounds: (Background::Color(t.text), Background::Color(t.track)),
            width: 4.0,
            border: round(2.0),
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: 10.0 },
            background: Background::Color(knob),
            border_width: if t.dark { 0.0 } else { 0.5 },
            border_color: black(0.08),
        },
    }
}

pub fn input(
    theme: &Theme,
    status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let t = Tokens::of(theme);
    let focused = matches!(status, iced::widget::text_input::Status::Focused { .. });
    iced::widget::text_input::Style {
        background: Background::Color(t.surface_alt),
        border: Border {
            color: if focused {
                with_alpha(t.accent, 0.7)
            } else {
                Color::TRANSPARENT
            },
            width: 1.0,
            radius: 10.0.into(),
        },
        icon: t.text_dim,
        placeholder: t.text_faint,
        value: t.text,
        selection: with_alpha(t.accent, 0.35),
    }
}

pub fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = Tokens::of(theme);
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(with_alpha(t.text, 0.18)),
            border: round(4.0),
        },
    };
    let mut s = scrollable::default(theme, status);
    s.vertical_rail = rail;
    s.horizontal_rail = rail;
    s
}

/// Inset panel inside a card: a confirm step or a status note under a setting.
pub fn inset(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.surface_alt)),
        border: Border {
            radius: 12.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

//! Visual tokens and widget styles. One accent colour, soft surfaces, rounded cards.

use crate::ui::format::{PillTone, Tone};
use iced::font::Weight;
use iced::theme::Palette;
use iced::widget::{button, container, scrollable, toggler};
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

pub const CARD_RADIUS: f32 = 18.0;
pub const TILE_RADIUS: f32 = 14.0;

const fn rgb(hex: u32) -> Color {
    Color::from_rgb8((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[derive(Debug, Clone, Copy)]
pub struct Tokens {
    pub dark: bool,
    pub bg: Color,
    pub surface: Color,
    pub surface_alt: Color,
    pub border: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_faint: Color,
    pub accent: Color,
    pub on_accent: Color,
    pub green: Color,
    pub amber: Color,
    pub red: Color,
    pub track: Color,
}

pub const DARK: Tokens = Tokens {
    dark: true,
    bg: rgb(0x0E1014),
    surface: rgb(0x171A20),
    surface_alt: rgb(0x21252D),
    border: Color::from_rgba(1.0, 1.0, 1.0, 0.06),
    text: rgb(0xF3F5F8),
    text_dim: rgb(0x9BA3AF),
    text_faint: rgb(0x5D6573),
    accent: rgb(0x3D8BFF),
    on_accent: rgb(0xFFFFFF),
    green: rgb(0x34D17A),
    amber: rgb(0xFFB224),
    red: rgb(0xFF5A4E),
    track: Color::from_rgba(1.0, 1.0, 1.0, 0.08),
};

pub const LIGHT: Tokens = Tokens {
    dark: false,
    bg: rgb(0xF2F3F6),
    surface: rgb(0xFFFFFF),
    surface_alt: rgb(0xEEF0F3),
    border: Color::from_rgba(0.0, 0.0, 0.0, 0.06),
    text: rgb(0x111318),
    text_dim: rgb(0x5E6672),
    text_faint: rgb(0xA0A6B0),
    accent: rgb(0x1F6FEB),
    on_accent: rgb(0xFFFFFF),
    green: rgb(0x1EA55A),
    amber: rgb(0xD48806),
    red: rgb(0xE0362B),
    track: Color::from_rgba(0.0, 0.0, 0.0, 0.07),
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

    pub fn pill(&self, tone: PillTone) -> Color {
        match tone {
            PillTone::Good => self.green,
            PillTone::Busy => self.amber,
            PillTone::Info => self.accent,
            PillTone::Idle => self.text_dim,
        }
    }
}

pub fn theme(dark: bool) -> Theme {
    let t = if dark { DARK } else { LIGHT };
    Theme::custom(
        if dark {
            "LibrePods Dark"
        } else {
            "LibrePods Light"
        },
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

pub fn page(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.bg)),
        text_color: Some(t.text),
        ..Default::default()
    }
}

pub fn card(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.surface)),
        border: Border {
            color: t.border,
            width: 1.0,
            radius: CARD_RADIUS.into(),
        },
        shadow: Shadow {
            color: Color::from_rgba(0.0, 0.0, 0.0, if t.dark { 0.25 } else { 0.06 }),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
        text_color: Some(t.text),
        ..Default::default()
    }
}

/// Hero card: a faint accent wash over the surface.
pub fn hero(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    let wash = Color {
        a: if t.dark { 0.07 } else { 0.05 },
        ..t.accent
    };
    let mut s = card(theme);
    s.background = Some(Background::Gradient(iced::Gradient::Linear(
        iced::gradient::Linear::new(iced::Radians(std::f32::consts::PI * 0.75))
            .add_stop(0.0, mix(t.surface, wash))
            .add_stop(1.0, t.surface),
    )));
    s
}

fn mix(base: Color, over: Color) -> Color {
    let a = over.a;
    Color::from_rgb(
        base.r * (1.0 - a) + over.r * a,
        base.g * (1.0 - a) + over.g * a,
        base.b * (1.0 - a) + over.b * a,
    )
}

pub fn pill(tone: PillTone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = Tokens::of(theme);
        let c = t.pill(tone);
        container::Style {
            background: Some(Background::Color(Color { a: 0.14, ..c })),
            border: Border {
                radius: 999.0.into(),
                ..Default::default()
            },
            text_color: Some(c),
            ..Default::default()
        }
    }
}

pub fn dot(tone: PillTone) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let c = Tokens::of(theme).pill(tone);
        container::Style {
            background: Some(Background::Color(c)),
            border: Border {
                radius: 999.0.into(),
                ..Default::default()
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
        border: Border {
            radius: 999.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Listening mode tile.
pub fn tile(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = Tokens::of(theme);
        let (bg, fg) = if selected {
            (t.accent, t.on_accent)
        } else {
            (t.surface_alt, t.text)
        };
        let bg = match status {
            button::Status::Hovered if !selected => mix(bg, Color { a: 0.06, ..t.text }),
            button::Status::Pressed => mix(bg, Color { a: 0.12, ..t.text }),
            button::Status::Disabled => Color { a: 0.5, ..bg },
            _ => bg,
        };
        let fg = if status == button::Status::Disabled {
            Color { a: 0.5, ..fg }
        } else {
            fg
        };
        button::Style {
            background: Some(Background::Color(bg)),
            text_color: fg,
            border: Border {
                radius: TILE_RADIUS.into(),
                ..Default::default()
            },
            shadow: if selected {
                Shadow {
                    color: Color {
                        a: 0.35,
                        ..t.accent
                    },
                    offset: Vector::new(0.0, 3.0),
                    blur_radius: 10.0,
                }
            } else {
                Shadow::default()
            },
            snap: true,
        }
    }
}

/// Segment of a small segmented control (track drawn by [`segment_track`]).
pub fn segment(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = Tokens::of(theme);
        let bg = if selected {
            Some(Background::Color(if t.dark {
                rgb(0x343A45)
            } else {
                t.surface
            }))
        } else if status == button::Status::Hovered {
            Some(Background::Color(Color { a: 0.04, ..t.text }))
        } else {
            None
        };
        button::Style {
            background: bg,
            text_color: match (selected, status) {
                (_, button::Status::Disabled) => t.text_faint,
                (true, _) => t.text,
                _ => t.text_dim,
            },
            border: Border {
                radius: 9.0.into(),
                ..Default::default()
            },
            shadow: if selected {
                Shadow {
                    color: Color::from_rgba(0.0, 0.0, 0.0, if t.dark { 0.3 } else { 0.1 }),
                    offset: Vector::new(0.0, 1.0),
                    blur_radius: 3.0,
                }
            } else {
                Shadow::default()
            },
            snap: true,
        }
    }
}

pub fn segment_track(theme: &Theme) -> container::Style {
    let t = Tokens::of(theme);
    container::Style {
        background: Some(Background::Color(t.surface_alt)),
        border: Border {
            radius: 11.0.into(),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Mini screen of the overlay corner picker.
pub fn screen(enabled: bool) -> impl Fn(&Theme) -> container::Style {
    move |theme| {
        let t = Tokens::of(theme);
        container::Style {
            background: Some(Background::Color(t.surface_alt)),
            border: Border {
                color: if enabled { t.text_faint } else { t.border },
                width: 1.0,
                radius: 7.0.into(),
            },
            ..Default::default()
        }
    }
}

pub fn corner(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let t = Tokens::of(theme);
        let c = match (selected, status) {
            (_, button::Status::Disabled) if selected => t.text_faint,
            (_, button::Status::Disabled) => Color {
                a: 0.4,
                ..t.text_faint
            },
            (true, _) => t.accent,
            (false, button::Status::Hovered) => t.text_dim,
            (false, _) => t.text_faint,
        };
        button::Style {
            background: Some(Background::Color(
                if selected || status == button::Status::Hovered {
                    c
                } else {
                    Color { a: 0.35, ..c }
                },
            )),
            text_color: t.text,
            border: Border {
                radius: 3.0.into(),
                ..Default::default()
            },
            shadow: Shadow::default(),
            snap: true,
        }
    }
}

/// Text-only button (e.g. the "More" disclosure).
pub fn link(theme: &Theme, status: button::Status) -> button::Style {
    let t = Tokens::of(theme);
    button::Style {
        background: match status {
            button::Status::Hovered | button::Status::Pressed => {
                Some(Background::Color(Color { a: 0.05, ..t.text }))
            }
            _ => None,
        },
        text_color: t.text_dim,
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

/// Secondary filled button.
pub fn secondary(theme: &Theme, status: button::Status) -> button::Style {
    let t = Tokens::of(theme);
    let bg = match status {
        button::Status::Hovered => mix(t.surface_alt, Color { a: 0.06, ..t.text }),
        button::Status::Pressed => mix(t.surface_alt, Color { a: 0.12, ..t.text }),
        _ => t.surface_alt,
    };
    button::Style {
        background: Some(Background::Color(bg)),
        text_color: if status == button::Status::Disabled {
            t.text_faint
        } else {
            t.text
        },
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        shadow: Shadow::default(),
        snap: true,
    }
}

pub fn toggle(theme: &Theme, status: toggler::Status) -> toggler::Style {
    let t = Tokens::of(theme);
    let (on, disabled) = match status {
        toggler::Status::Active { is_toggled } | toggler::Status::Hovered { is_toggled } => {
            (is_toggled, false)
        }
        toggler::Status::Disabled { is_toggled } => (is_toggled, true),
    };
    let track = if on {
        t.accent
    } else if t.dark {
        rgb(0x3A404B)
    } else {
        rgb(0xD5D9DF)
    };
    let alpha = if disabled { 0.45 } else { 1.0 };
    toggler::Style {
        background: Background::Color(Color { a: alpha, ..track }),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Background::Color(Color {
            a: alpha,
            ..Color::WHITE
        }),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: None,
        border_radius: None,
        padding_ratio: 0.12,
    }
}

pub fn input(
    theme: &Theme,
    _status: iced::widget::text_input::Status,
) -> iced::widget::text_input::Style {
    let t = Tokens::of(theme);
    iced::widget::text_input::Style {
        background: Background::Color(t.surface_alt),
        border: Border {
            radius: 10.0.into(),
            ..Default::default()
        },
        icon: t.text_dim,
        placeholder: t.text_faint,
        value: t.text,
        selection: Color {
            a: 0.35,
            ..t.accent
        },
    }
}

pub fn scroll(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let t = Tokens::of(theme);
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(Color { a: 0.18, ..t.text }),
            border: Border {
                radius: 4.0.into(),
                ..Default::default()
            },
        },
    };
    let mut s = scrollable::default(theme, status);
    s.vertical_rail = rail;
    s.horizontal_rail = rail;
    s
}

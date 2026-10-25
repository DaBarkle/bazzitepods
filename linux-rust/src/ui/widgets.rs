//! Canvas-drawn pieces: battery ring gauges and the small line icons.

use crate::ui::format::{Gauge, ListeningMode};
use crate::ui::style::{self, Tokens};
use iced::widget::canvas::{self, Frame, Geometry, LineCap, Path, Stroke, path::Arc};
use iced::widget::{Canvas, column, container, stack, text};
use iced::{
    Color, Element, Length, Point, Radians, Rectangle, Renderer, Size, Theme, Vector, mouse,
};
use std::f32::consts::{FRAC_PI_2, PI, TAU};

/// A circular battery gauge.
#[derive(Debug, Clone, Copy)]
pub struct Ring {
    pub fraction: Option<f32>,
    pub tone: crate::ui::format::Tone,
    pub charging: bool,
    pub thickness: f32,
}

impl<Message> canvas::Program<Message> for Ring {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let t = Tokens::of(theme);
        let mut frame = Frame::new(renderer, bounds.size());
        let center = frame.center();
        let radius = bounds.width.min(bounds.height) / 2.0 - self.thickness / 2.0 - 1.0;

        frame.stroke(
            &Path::circle(center, radius),
            Stroke::default()
                .with_color(t.track)
                .with_width(self.thickness),
        );

        if let Some(f) = self.fraction {
            let f = f.clamp(0.0, 1.0);
            if f > 0.0 {
                let start = -FRAC_PI_2;
                let arc = Path::new(|b| {
                    b.arc(Arc {
                        center,
                        radius,
                        start_angle: Radians(start),
                        end_angle: Radians(start + TAU * f.max(0.004)),
                    })
                });
                let color = t.tone(self.tone, self.charging);
                let color = if self.tone == crate::ui::format::Tone::Muted {
                    Color { a: 0.55, ..color }
                } else {
                    color
                };
                frame.stroke(
                    &arc,
                    Stroke::default()
                        .with_color(color)
                        .with_width(self.thickness)
                        .with_line_cap(LineCap::Round),
                );
            }
        }
        vec![frame.into_geometry()]
    }
}

/// Small line icons drawn in the current text colour.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Glyph {
    Mode(ListeningMode),
    Bolt,
    Chevron { up: bool },
    Bluetooth,
    Earbuds,
}

/// Icon colour, resolved against the theme at draw time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Ink {
    Text,
    Dim,
    Green,
    OnAccent,
}

impl Ink {
    pub fn resolve(self, t: &Tokens) -> Color {
        match self {
            Ink::Text => t.text,
            Ink::Dim => t.text_dim,
            Ink::Green => t.green,
            Ink::OnAccent => t.on_accent,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Icon {
    pub glyph: Glyph,
    pub ink: Ink,
    pub alpha: f32,
}

fn bolt_path(cx: f32, cy: f32, h: f32) -> Path {
    // Lightning bolt inside a box of height h centred on (cx, cy).
    let s = h / 16.0;
    let p = |x: f32, y: f32| Point::new(cx + (x - 8.0) * s, cy + (y - 8.0) * s);
    Path::new(|b| {
        b.move_to(p(9.5, 0.0));
        b.line_to(p(2.5, 9.2));
        b.line_to(p(7.4, 9.2));
        b.line_to(p(6.3, 16.0));
        b.line_to(p(13.5, 6.6));
        b.line_to(p(8.6, 6.6));
        b.close();
    })
}

impl<Message> canvas::Program<Message> for Icon {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let base = self.ink.resolve(&Tokens::of(theme));
        let color = Color {
            a: base.a * self.alpha,
            ..base
        };
        let mut frame = Frame::new(renderer, bounds.size());
        let c = frame.center();
        let s = bounds.width.min(bounds.height);
        let w = (s / 12.0).max(1.4);
        let stroke = Stroke::default()
            .with_color(color)
            .with_width(w)
            .with_line_cap(LineCap::Round);
        let r = s * 0.36;
        match self.glyph {
            Glyph::Mode(ListeningMode::Off) => {
                frame.stroke(&Path::circle(c, r), stroke);
                let d = r * 0.7;
                frame.stroke(
                    &Path::line(Point::new(c.x - d, c.y + d), Point::new(c.x + d, c.y - d)),
                    stroke,
                );
            }
            Glyph::Mode(ListeningMode::Transparency) => {
                frame.fill(&Path::circle(c, r * 0.38), color);
                for i in 0..8 {
                    let a = i as f32 * TAU / 8.0;
                    let (sn, cs) = a.sin_cos();
                    frame.stroke(
                        &Path::line(
                            Point::new(c.x + cs * r * 0.72, c.y + sn * r * 0.72),
                            Point::new(c.x + cs * r * 1.12, c.y + sn * r * 1.12),
                        ),
                        stroke,
                    );
                }
            }
            Glyph::Mode(ListeningMode::Adaptive) => {
                frame.stroke(&Path::circle(c, r), stroke);
                let half = Path::new(|b| {
                    b.arc(Arc {
                        center: c,
                        radius: r,
                        start_angle: Radians(FRAC_PI_2),
                        end_angle: Radians(FRAC_PI_2 + PI),
                    });
                    b.close();
                });
                frame.fill(&half, color);
            }
            Glyph::Mode(ListeningMode::NoiseCancellation) => {
                frame.fill(&Path::circle(c, r * 0.62), color);
                frame.stroke(&Path::circle(c, r * 1.05), stroke);
            }
            Glyph::Bolt => {
                frame.fill(&bolt_path(c.x, c.y, s * 0.95), color);
            }
            Glyph::Chevron { up } => {
                let d = s * 0.22;
                let dy = if up { -d * 0.5 } else { d * 0.5 };
                let path = Path::new(|b| {
                    b.move_to(Point::new(c.x - d, c.y - dy));
                    b.line_to(Point::new(c.x, c.y + dy));
                    b.line_to(Point::new(c.x + d, c.y - dy));
                });
                frame.stroke(&path, stroke.with_line_join(canvas::LineJoin::Round));
            }
            Glyph::Bluetooth => {
                let h = s * 0.34;
                let x = c.x - s * 0.02;
                let path = Path::new(|b| {
                    b.move_to(Point::new(x - h * 0.55, c.y - h * 0.5));
                    b.line_to(Point::new(x + h * 0.55, c.y + h * 0.5));
                    b.line_to(Point::new(x, c.y + h));
                    b.line_to(Point::new(x, c.y - h));
                    b.line_to(Point::new(x + h * 0.55, c.y - h * 0.5));
                    b.line_to(Point::new(x - h * 0.55, c.y + h * 0.5));
                });
                frame.stroke(&path, stroke.with_line_join(canvas::LineJoin::Round));
            }
            Glyph::Earbuds => {
                // Same 32-unit geometry as the app icon and the overlay glyph.
                let u = s / 32.0;
                let at = |x: f32, y: f32| Point::new(c.x + (x - 16.0) * u, c.y + (y - 16.5) * u);
                for side in [1.0f32, -1.0] {
                    let x = |v: f32| 16.0 + side * (v - 16.0);
                    frame.with_save(|f| {
                        let top = at(x(25.0), 9.5);
                        f.translate(Vector::new(top.x, top.y));
                        f.rotate(side * -4f32.to_radians());
                        f.fill(
                            &Path::rounded_rectangle(
                                Point::new(-1.8 * u, 0.0),
                                Size::new(3.6 * u, 19.5 * u),
                                (1.8 * u).into(),
                            ),
                            color,
                        );
                    });
                    let ellipse = |cx: f32, cy: f32, rx: f32, ry: f32| {
                        Path::new(|b| {
                            b.ellipse(canvas::path::arc::Elliptical {
                                center: at(x(cx), cy),
                                radii: Vector::new(rx * u, ry * u),
                                rotation: (side * -20f32.to_radians()).into(),
                                start_angle: 0.0.into(),
                                end_angle: std::f32::consts::TAU.into(),
                            })
                        })
                    };
                    frame.fill(&ellipse(22.6, 8.6, 5.0, 4.6), color);
                    frame.fill(
                        &ellipse(19.4, 9.6, 1.25, 1.9),
                        Color::from_rgba(0.0, 0.0, 0.0, 0.4 * color.a),
                    );
                }
            }
        }
        vec![frame.into_geometry()]
    }
}

pub fn icon<'a, M: 'a>(glyph: Glyph, size: f32, ink: Ink) -> Element<'a, M> {
    icon_alpha(glyph, size, ink, 1.0)
}

pub fn icon_alpha<'a, M: 'a>(glyph: Glyph, size: f32, ink: Ink, alpha: f32) -> Element<'a, M> {
    Canvas::new(Icon { glyph, ink, alpha })
        .width(Length::Fixed(size))
        .height(Length::Fixed(size))
        .into()
}

/// Ring with the percentage in the middle, label (and optional caption) underneath.
pub fn gauge_view<'a, M: 'a>(g: &Gauge, size: f32) -> Element<'a, M> {
    let tone = g.tone;
    let charging = g.charging;
    let dimmed = g.dimmed;
    let ring = Canvas::new(Ring {
        fraction: g.level.map(|l| l as f32 / 100.0),
        tone,
        charging,
        thickness: (size / 12.5).round().max(4.0),
    })
    .width(Length::Fixed(size))
    .height(Length::Fixed(size));

    let pct_size = (size * 0.22).round();
    let pct = text(g.text.clone())
        .size(pct_size)
        .font(style::INTER_SEMIBOLD)
        .style(move |theme: &Theme| {
            let t = Tokens::of(theme);
            text::Style {
                color: Some(if dimmed { t.text_faint } else { t.text }),
            }
        });

    let label = text(g.label)
        .size(13)
        .font(style::INTER_MEDIUM)
        .style(move |theme: &Theme| {
            let t = Tokens::of(theme);
            text::Style {
                color: Some(if dimmed { t.text_faint } else { t.text_dim }),
            }
        });

    let mut col = column![
        {
            let mut layers = stack![
                ring,
                container(pct)
                    .center_x(Length::Fixed(size))
                    .center_y(Length::Fixed(size))
            ];
            if charging {
                // Bolt sits under the number so the percentage stays optically centred.
                layers = layers.push(
                    container(icon(Glyph::Bolt, (size * 0.16).round(), Ink::Green))
                        .width(Length::Fixed(size))
                        .height(Length::Fixed(size))
                        .align_x(iced::Center)
                        .align_y(iced::Bottom)
                        .padding(iced::Padding {
                            bottom: (size * 0.17).round(),
                            ..iced::Padding::ZERO
                        }),
                );
            }
            layers
        },
        label
    ]
    .spacing(8)
    .width(Length::Fill)
    .align_x(iced::Center);

    if let Some(c) = &g.caption {
        col = col.push(
            // Two short lines fit under a ring better than one long one.
            text(c.replacen("seen ", "seen\n", 1))
                .size(11)
                .line_height(1.25)
                .width(Length::Fill)
                .align_x(iced::Center)
                .style(|theme: &Theme| text::Style {
                    color: Some(Tokens::of(theme).text_faint),
                }),
        );
    }
    col.into()
}

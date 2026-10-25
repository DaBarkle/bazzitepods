//! Draws the battery pill with iced's canvas, all in logical pixels (iced
//! scales to the output's (fractional) scale, so it stays crisp on HiDPI).

use std::f32::consts::PI;

use iced::alignment::Vertical;
use iced::font::Weight;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, gradient, path::Arc, stroke};
use iced::widget::text::Alignment as TextAlign;
use iced::{Color, Font, Point, Rectangle, Renderer, Size, Theme, Vector, border::Radius};

use crate::overlay_protocol::{ComponentState, OverlayPosition, OverlayState};

/// Transparent room around the pill for the shadow and the slide animation.
pub const PAD: f32 = 14.0;
pub const PILL_H: f32 = 94.0;
const PILL_W_BUDS: f32 = 316.0;
const PILL_W_HEADPHONE: f32 = 232.0;
const RADIUS: f32 = 24.0;
const INSET: f32 = 18.0;
const SLIDE: f32 = 10.0;
const MINI_H: f32 = 36.0;
const MINI_ITEM_W: f32 = 50.0;
const MINI_W_BUDS: f32 = 36.0 + 3.0 * MINI_ITEM_W + 4.0;
const MINI_W_HEADPHONE: f32 = 36.0 + MINI_ITEM_W;

const GREEN: Color = Color::from_rgb(0.204, 0.820, 0.345);
const AMBER: Color = Color::from_rgb(1.0, 0.702, 0.251);
const RED: Color = Color::from_rgb(1.0, 0.271, 0.227);

fn white(a: f32) -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, a)
}

fn semibold() -> Font {
    Font {
        weight: Weight::Semibold,
        ..Font::DEFAULT
    }
}

fn medium() -> Font {
    Font {
        weight: Weight::Medium,
        ..Font::DEFAULT
    }
}

fn is_headphone(state: &OverlayState) -> bool {
    state.headphone.is_some()
}

/// Logical size of the layer surface for a given state.
pub fn surface_size(state: Option<&OverlayState>) -> (u32, u32) {
    let w = if state.is_some_and(is_headphone) {
        PILL_W_HEADPHONE
    } else {
        PILL_W_BUDS
    };
    ((w + 2.0 * PAD) as u32, (PILL_H + 2.0 * PAD) as u32)
}

enum Label {
    Letter(&'static str),
    Case,
    Headphone,
}

struct Segment<'a> {
    comp: Option<&'a ComponentState>,
    label: Label,
    title: &'static str,
}

pub struct Pill {
    pub state: OverlayState,
    /// Eased 0..1 appearance (opacity, slide, scale).
    pub visual: f32,
    /// Eased 0..1 morph: 0 = compact widget, 1 = full pill.
    pub expand: f32,
    /// User-chosen overall opacity.
    pub opacity: f32,
    /// 0 = smoked solid, 1 = clear liquid glass.
    pub glass: f32,
    pub position: OverlayPosition,
}

impl<Message> canvas::Program<Message> for Pill {
    type State = ();

    fn draw(
        &self,
        _state: &(),
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<Geometry> {
        let mut frame = Frame::new(renderer, bounds.size());
        self.paint(&mut frame, bounds.size());
        vec![frame.into_geometry()]
    }
}

impl Pill {
    fn paint(&self, frame: &mut Frame, size: Size) {
        let v = self.visual.clamp(0.0, 1.0);
        if v <= 0.0 {
            return;
        }
        // Opacity mostly thins the glass; text and icons fade far less, so they stay legible.
        let opacity = self.opacity.clamp(0.2, 1.0);
        let alpha = v * (0.8 + 0.2 * opacity);
        let body_alpha = v * opacity;
        let e = self.expand.clamp(0.0, 1.0);
        let full = Size::new(size.width - 2.0 * PAD, PILL_H.min(size.height - 2.0 * PAD));
        let mini = Size::new(
            if is_headphone(&self.state) { MINI_W_HEADPHONE } else { MINI_W_BUDS }.min(full.width),
            MINI_H,
        );
        let lerp = |a: f32, b: f32| a + (b - a) * e;
        let body_size = Size::new(lerp(mini.width, full.width), lerp(mini.height, full.height));
        let radius = lerp(MINI_H / 2.0, RADIUS);
        let full_origin = self.anchored(size, full);
        let body_origin = self.anchored(size, body_size);
        let dir = if self.position.is_top() { -1.0 } else { 1.0 };

        // Slide in from the anchored edge and grow slightly, around the body's center.
        let scale = 0.94 + 0.06 * v;
        let center = Point::new(
            body_origin.x + body_size.width / 2.0,
            body_origin.y + body_size.height / 2.0 + dir * SLIDE * (1.0 - v),
        );
        frame.translate(Vector::new(center.x, center.y));
        frame.scale(scale);
        frame.translate(Vector::new(-body_size.width / 2.0, -body_size.height / 2.0));
        self.paint_body(frame, body_size, radius, body_alpha);

        // Contents cross-fade: the widget's fades out early, the pill's fades in late.
        let mini_a = alpha * ((0.5 - e) / 0.5).clamp(0.0, 1.0);
        if mini_a > 0.0 {
            self.paint_mini(frame, body_size.height, mini_a);
        }
        let full_a = alpha * ((e - 0.55) / 0.45).clamp(0.0, 1.0);
        if full_a > 0.0 {
            frame.translate(Vector::new(
                full_origin.x - body_origin.x,
                full_origin.y - body_origin.y,
            ));
            self.paint_full(frame, full.width, full_a);
        }
    }

    /// Top-left of a `pill`-sized rect hugging the anchored edges inside the surface.
    fn anchored(&self, surface: Size, pill: Size) -> Point {
        use OverlayPosition::*;
        let x = match self.position {
            TopLeft | BottomLeft => PAD,
            TopCenter | BottomCenter => (surface.width - pill.width) / 2.0,
            TopRight | BottomRight => surface.width - PAD - pill.width,
        };
        let y = if self.position.is_top() {
            PAD
        } else {
            surface.height - PAD - pill.height
        };
        Point::new(x, y)
    }

    fn paint_body(&self, frame: &mut Frame, size: Size, radius: f32, alpha: f32) {
        let (w, h) = (size.width, size.height);
        // Soft shadow: stacked, slightly offset rounded rects.
        for i in 0..7 {
            let grow = 1.0 + i as f32 * 1.5;
            let a = 0.05 * alpha * (1.0 - i as f32 / 7.0);
            let p = Path::rounded_rectangle(
                Point::new(-grow, -grow + 3.0),
                Size::new(w + 2.0 * grow, h + 2.0 * grow),
                Radius::from(radius + grow),
            );
            frame.fill(&p, Color::from_rgba(0.0, 0.0, 0.0, a));
        }

        let g = self.glass.clamp(0.0, 1.0);
        let body = Path::rounded_rectangle(Point::ORIGIN, Size::new(w, h), Radius::from(radius));
        // Tint: dense smoked glass at 0, clear liquid glass at 1. Never below 0.36 so
        // the text stays readable over a bright game.
        frame.fill(&body, Color::from_rgba8(18, 18, 22, (0.86 - 0.5 * g) * alpha));
        // A faint cool wash, like light passing through glass.
        let wash = gradient::Linear::new(Point::ORIGIN, Point::new(w, h))
            .add_stop(0.0, Color::from_rgba(0.62, 0.74, 1.0, 0.06 * g * alpha))
            .add_stop(0.5, Color::from_rgba(1.0, 1.0, 1.0, 0.0))
            .add_stop(1.0, Color::from_rgba(1.0, 0.86, 0.95, 0.05 * g * alpha));
        frame.fill(&body, wash);
        // Specular sheen across the top half.
        let sheen = gradient::Linear::new(Point::ORIGIN, Point::new(0.0, h))
            .add_stop(0.0, white((0.07 + 0.15 * g) * alpha))
            .add_stop(0.45, white(0.02 * g * alpha))
            .add_stop(1.0, white(0.0));
        frame.fill(&body, sheen);
        // Light caught by the bottom edge (refraction).
        let caustic = gradient::Linear::new(Point::new(0.0, h * 0.6), Point::new(0.0, h))
            .add_stop(0.0, white(0.0))
            .add_stop(1.0, white(0.07 * g * alpha));
        frame.fill(&body, caustic);
        // Rim: bright at the top, fading towards the bottom, with an inner hairline.
        let border = Path::rounded_rectangle(
            Point::new(0.5, 0.5),
            Size::new(w - 1.0, h - 1.0),
            Radius::from(radius - 0.5),
        );
        let rim = gradient::Linear::new(Point::ORIGIN, Point::new(0.0, h))
            .add_stop(0.0, white((0.14 + 0.36 * g) * alpha))
            .add_stop(0.5, white((0.08 + 0.06 * g) * alpha))
            .add_stop(1.0, white((0.10 + 0.16 * g) * alpha));
        frame.stroke(
            &border,
            Stroke {
                style: canvas::Style::Gradient(rim.into()),
                width: 1.0,
                ..Stroke::default()
            },
        );
        if g > 0.0 {
            let inner = Path::rounded_rectangle(
                Point::new(1.5, 1.5),
                Size::new(w - 3.0, h - 3.0),
                Radius::from(radius - 1.5),
            );
            frame.stroke(
                &inner,
                Stroke::default()
                    .with_width(1.0)
                    .with_color(white(0.05 * g * alpha)),
            );
        }
    }

    /// Compact widget: glyph, then "L 75%  R 77%  [case] 40%" in one line.
    fn paint_mini(&self, frame: &mut Frame, h: f32, alpha: f32) {
        let cy = h / 2.0;
        let glyph = white(0.9 * alpha);
        let items: Vec<(Label, Option<&ComponentState>)> = if let Some(hp) = &self.state.headphone {
            draw_headphone_glyph(frame, Point::new(18.0, cy), 0.9, glyph);
            vec![(Label::Headphone, Some(hp))]
        } else {
            draw_buds_glyph(frame, Point::new(18.0, cy), 0.85, glyph);
            vec![
                (Label::Letter("L"), self.state.left.as_ref()),
                (Label::Letter("R"), self.state.right.as_ref()),
                (Label::Case, self.state.case.as_ref()),
            ]
        };
        let mut x = 36.0;
        for (label, comp) in items {
            let comp_shown = comp.filter(|c| c.present || c.stale);
            let dim = if comp.is_some_and(|c| c.present) { 1.0 } else { 0.45 };
            let label_c = white(0.55 * alpha * dim);
            match (&label, comp_shown.is_some_and(|c| c.charging && c.present)) {
                (_, true) => draw_bolt(frame, Point::new(x + 4.0, cy), 0.8, with_alpha(GREEN, alpha)),
                (Label::Letter(l), _) => fill_text_lifted(frame, canvas::Text {
                    content: (*l).into(),
                    position: Point::new(x + 4.0, cy),
                    color: label_c,
                    size: 11.0.into(),
                    font: semibold(),
                    align_x: TextAlign::Center,
                    align_y: Vertical::Center,
                    ..Default::default()
                }),
                (Label::Case, _) => draw_case_glyph_small(frame, Point::new(x + 5.0, cy), label_c),
                (Label::Headphone, _) => {}
            }
            let lx = match label {
                Label::Headphone => x - 2.0,
                Label::Case => x + 15.0,
                Label::Letter(_) => x + 12.0,
            };
            let (pct, color) = match comp_shown {
                Some(c) => (
                    format!("{}%", c.level.min(100)),
                    if c.present && !c.charging && c.level <= 20 {
                        with_alpha(level_color(c), alpha)
                    } else {
                        white(0.95 * alpha * dim)
                    },
                ),
                None => ("--".into(), white(0.6 * alpha * dim)),
            };
            fill_text_lifted(frame, canvas::Text {
                content: pct,
                position: Point::new(lx, cy),
                color,
                size: 13.0.into(),
                font: semibold(),
                align_y: Vertical::Center,
                ..Default::default()
            });
            x += MINI_ITEM_W;
        }
    }

    fn paint_full(&self, frame: &mut Frame, w: f32, alpha: f32) {
        // Header: glyph + name.
        let header_y = 26.0;
        if is_headphone(&self.state) {
            draw_headphone_glyph(
                frame,
                Point::new(INSET + 10.0, header_y),
                1.0,
                white(0.9 * alpha),
            );
        } else {
            draw_buds_glyph(
                frame,
                Point::new(INSET + 10.0, header_y),
                1.2,
                white(0.9 * alpha),
            );
        }
        let name_x = INSET + 28.0;
        let name = if self.state.name.trim().is_empty() {
            "AirPods".to_string()
        } else {
            truncate(self.state.name.trim(), w - name_x - INSET, 14.0)
        };
        fill_text_lifted(frame, canvas::Text {
            content: name,
            position: Point::new(name_x, header_y),
            color: white(0.95 * alpha),
            size: 14.0.into(),
            font: semibold(),
            align_y: Vertical::Center,
            ..Default::default()
        });

        // Battery segments.
        let segments: Vec<Segment> = if let Some(hp) = &self.state.headphone {
            vec![Segment {
                comp: Some(hp),
                label: Label::Headphone,
                title: "Battery",
            }]
        } else {
            vec![
                Segment {
                    comp: self.state.left.as_ref(),
                    label: Label::Letter("L"),
                    title: "Left",
                },
                Segment {
                    comp: self.state.right.as_ref(),
                    label: Label::Letter("R"),
                    title: "Right",
                },
                Segment {
                    comp: self.state.case.as_ref(),
                    label: Label::Case,
                    title: "Case",
                },
            ]
        };
        let row_y = 46.0;
        let seg_w = (w - 2.0 * INSET) / segments.len() as f32;
        for (i, seg) in segments.iter().enumerate() {
            draw_segment(
                frame,
                seg,
                Point::new(INSET + i as f32 * seg_w, row_y),
                alpha,
            );
        }
    }
}

/// Text with a soft dark halo under it, so it stays readable when the glass is
/// clear and a bright scene sits behind it. Costs two extra glyph draws per label,
/// only when the pill is redrawn; the screen behind is never read.
fn fill_text_lifted(frame: &mut Frame, text: canvas::Text) {
    let a = text.color.a;
    for (dx, dy, k) in [(0.0, 1.0, 0.42), (0.6, 0.6, 0.22), (-0.6, 0.6, 0.22)] {
        frame.fill_text(canvas::Text {
            position: Point::new(text.position.x + dx, text.position.y + dy),
            color: Color::from_rgba(0.0, 0.0, 0.0, a * k),
            ..text.clone()
        });
    }
    frame.fill_text(text);
}

/// Rough ellipsizing; the default sans averages ~0.56 em per glyph.
fn truncate(name: &str, max_w: f32, size: f32) -> String {
    let max_chars = (max_w / (size * 0.56)).floor().max(4.0) as usize;
    if name.chars().count() <= max_chars {
        return name.to_string();
    }
    let mut s: String = name.chars().take(max_chars.saturating_sub(1)).collect();
    s = s.trim_end().to_string();
    s.push('…');
    s
}

fn level_color(c: &ComponentState) -> Color {
    if c.charging {
        GREEN
    } else if c.level <= 10 {
        RED
    } else if c.level <= 20 {
        AMBER
    } else {
        white(0.92)
    }
}

fn with_alpha(c: Color, a: f32) -> Color {
    Color { a: c.a * a, ..c }
}

fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => "Just now".into(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

fn draw_segment(frame: &mut Frame, seg: &Segment, origin: Point, alpha: f32) {
    const R: f32 = 15.5;
    const RING_W: f32 = 3.2;
    let center = Point::new(origin.x + R + 1.0, origin.y + R + 1.0);
    let present = seg.comp.is_some_and(|c| c.present);
    let stale = seg.comp.is_some_and(|c| c.stale)
        || !present && seg.comp.is_some_and(|c| c.last_seen_secs.is_some());
    // Stale readings and absent components are drawn dimmed.
    let dim = if !present && !stale {
        0.38
    } else if stale {
        0.5
    } else {
        1.0
    };

    // Track.
    let track = Path::circle(center, R);
    frame.stroke(
        &track,
        Stroke::default()
            .with_width(RING_W)
            .with_color(white(0.13 * alpha)),
    );

    let comp = seg.comp.filter(|c| c.present || c.stale);
    let accent = comp.map(level_color).unwrap_or(white(0.92));

    if let Some(c) = comp {
        let frac = (c.level.min(100) as f32) / 100.0;
        if frac > 0.0 {
            let start = -PI / 2.0;
            let arc = Path::new(|b| {
                b.arc(Arc {
                    center,
                    radius: R,
                    start_angle: start.into(),
                    end_angle: (start + frac * 2.0 * PI).into(),
                })
            });
            frame.stroke(
                &arc,
                Stroke {
                    line_cap: stroke::LineCap::Round,
                    ..Stroke::default()
                        .with_width(RING_W)
                        .with_color(with_alpha(accent, alpha * dim))
                },
            );
        }
    }

    // Inner label: L / R / case glyph, or a bolt while charging.
    let label_color = white(0.78 * alpha * dim);
    if comp.is_some_and(|c| c.charging && c.present) {
        draw_bolt(frame, center, 1.0, with_alpha(GREEN, alpha));
    } else {
        match seg.label {
            Label::Letter(l) => fill_text_lifted(frame, canvas::Text {
                content: l.into(),
                position: center,
                color: label_color,
                size: 12.0.into(),
                font: semibold(),
                align_x: TextAlign::Center,
                align_y: Vertical::Center,
                ..Default::default()
            }),
            Label::Case => draw_case_glyph(frame, center, label_color),
            Label::Headphone => draw_headphone_glyph(frame, center, 0.62, label_color),
        }
    }

    // Percentage and subtitle.
    let text_x = origin.x + 2.0 * R + 11.0;
    let pct = match comp {
        Some(c) => format!("{}%", c.level.min(100)),
        None => "--".into(),
    };
    let pct_color = match comp {
        Some(c) if !c.charging && c.level <= 20 && present => with_alpha(accent, alpha),
        _ => white(0.95 * alpha * dim),
    };
    fill_text_lifted(frame, canvas::Text {
        content: pct,
        position: Point::new(text_x, center.y - 7.0),
        color: pct_color,
        size: 16.0.into(),
        font: semibold(),
        align_y: Vertical::Center,
        ..Default::default()
    });
    let subtitle = match seg.comp {
        Some(c) if !c.present || c.stale => match c.last_seen_secs {
            Some(s) => age_text(s),
            None if c.stale => "Last seen".to_string(),
            None => seg.title.to_string(),
        },
        _ => seg.title.to_string(),
    };
    fill_text_lifted(frame, canvas::Text {
        content: subtitle,
        position: Point::new(text_x, center.y + 9.0),
        color: white(0.5 * alpha * if present { 1.0 } else { 0.8 }),
        size: 11.0.into(),
        font: medium(),
        align_y: Vertical::Center,
        ..Default::default()
    });
}

/// Lightning bolt centered at `c`, about 12 px tall at scale 1.
fn draw_bolt(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let p = |x: f32, y: f32| Point::new(c.x + x * s, c.y + y * s);
    let bolt = Path::new(|b| {
        b.move_to(p(1.2, -6.5));
        b.line_to(p(-4.0, 1.0));
        b.line_to(p(-0.4, 1.0));
        b.line_to(p(-1.2, 6.5));
        b.line_to(p(4.0, -1.0));
        b.line_to(p(0.4, -1.0));
        b.close();
    });
    frame.fill(&bolt, color);
}

/// The case outline at 3/4 size, for the compact widget.
fn draw_case_glyph_small(frame: &mut Frame, c: Point, color: Color) {
    let body = Path::rounded_rectangle(
        Point::new(c.x - 5.5, c.y - 4.0),
        Size::new(11.0, 8.0),
        Radius::from(2.4),
    );
    frame.stroke(&body, Stroke::default().with_width(1.2).with_color(color));
    let lid = Path::line(
        Point::new(c.x - 4.9, c.y - 1.2),
        Point::new(c.x + 4.9, c.y - 1.2),
    );
    frame.stroke(&lid, Stroke::default().with_width(0.9).with_color(color));
}

/// Small charging-case outline.
fn draw_case_glyph(frame: &mut Frame, c: Point, color: Color) {
    let body = Path::rounded_rectangle(
        Point::new(c.x - 7.0, c.y - 5.5),
        Size::new(14.0, 11.0),
        Radius::from(3.0),
    );
    frame.stroke(&body, Stroke::default().with_width(1.4).with_color(color));
    let lid = Path::line(
        Point::new(c.x - 6.3, c.y - 1.8),
        Point::new(c.x + 6.3, c.y - 1.8),
    );
    frame.stroke(&lid, Stroke::default().with_width(1.0).with_color(color));
    frame.fill(&Path::circle(Point::new(c.x, c.y + 1.4), 0.9), color);
}

/// Two earbuds, mirrored, centered at `c`: a softly lit head with a speaker grille,
/// and a tapered stem with a microphone cap. About 16 px tall at scale 1.
fn draw_buds_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let a = color.a;
    let lit = |k: f32| Color::from_rgba(1.0, 1.0, 1.0, a * k);
    for side in [-1.0_f32, 1.0] {
        let hx = c.x + side * 4.3 * s;
        let hy = c.y - 3.4 * s;
        let head_r = 4.1 * s;

        // Stem first, so the head overlaps its top. Tapers from 2.9 to 2.3 wide.
        let top = hy + 0.8 * s;
        let bottom = c.y + 7.6 * s;
        let x_out = hx + side * 2.9 * s;
        let stem = Path::new(|b| {
            b.move_to(Point::new(x_out, top));
            b.line_to(Point::new(x_out - side * 2.9 * s, top));
            b.line_to(Point::new(x_out - side * 2.5 * s, bottom - 1.15 * s));
            b.arc_to(
                Point::new(x_out - side * 2.5 * s, bottom),
                Point::new(x_out - side * 1.4 * s, bottom),
                1.15 * s,
            );
            b.arc_to(
                Point::new(x_out - side * 0.2 * s, bottom),
                Point::new(x_out - side * 0.2 * s, bottom - 1.15 * s),
                1.15 * s,
            );
            b.close();
        });
        let stem_fill = gradient::Linear::new(Point::new(0.0, top), Point::new(0.0, bottom))
            .add_stop(0.0, lit(0.96))
            .add_stop(1.0, lit(0.78));
        frame.fill(&stem, stem_fill);
        // Microphone cap at the bottom of the stem.
        let cap = Path::rounded_rectangle(
            Point::new(x_out - side * 1.35 * s - 0.9 * s, bottom - 1.5 * s),
            Size::new(1.8 * s, 0.9 * s),
            Radius::from(0.45 * s),
        );
        frame.fill(&cap, Color::from_rgba(0.0, 0.0, 0.0, 0.28 * a));

        // Head: lit from the top-left.
        let head = Path::circle(Point::new(hx, hy), head_r);
        let head_fill = gradient::Linear::new(
            Point::new(hx - head_r, hy - head_r),
            Point::new(hx + head_r, hy + head_r),
        )
        .add_stop(0.0, lit(1.0))
        .add_stop(1.0, lit(0.8));
        frame.fill(&head, head_fill);
        // Speaker grille, facing the other bud.
        let grille = Path::circle(Point::new(hx - side * 1.6 * s, hy + 0.5 * s), 1.1 * s);
        frame.fill(&grille, Color::from_rgba(0.0, 0.0, 0.0, 0.32 * a));
    }
}

/// Over-ear headphones: band + two cups, centered at `c`.
fn draw_headphone_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let band = Path::new(|b| {
        b.arc(Arc {
            center: Point::new(c.x, c.y + 1.0 * s),
            radius: 7.5 * s,
            start_angle: PI.into(),
            end_angle: (2.0 * PI).into(),
        })
    });
    frame.stroke(
        &band,
        Stroke {
            line_cap: stroke::LineCap::Round,
            ..Stroke::default().with_width(2.0 * s).with_color(color)
        },
    );
    for side in [-1.0_f32, 1.0] {
        let cup = Path::rounded_rectangle(
            Point::new(c.x + side * 7.5 * s - 2.75 * s, c.y - 0.5 * s),
            Size::new(5.5 * s, 8.5 * s),
            Radius::from(2.2 * s),
        );
        frame.fill(&cup, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation() {
        assert_eq!(truncate("AirPods 4", 200.0, 14.0), "AirPods 4");
        let t = truncate("David's Extremely Long AirPods Pro Name", 100.0, 14.0);
        assert!(t.ends_with('…'));
        assert!(t.chars().count() <= 13);
    }

    #[test]
    fn colors_and_ages() {
        let mut c = ComponentState {
            level: 50,
            present: true,
            ..Default::default()
        };
        assert_eq!(level_color(&c), white(0.92));
        c.level = 20;
        assert_eq!(level_color(&c), AMBER);
        c.level = 10;
        assert_eq!(level_color(&c), RED);
        c.charging = true;
        assert_eq!(level_color(&c), GREEN);
        assert_eq!(age_text(30), "Just now");
        assert_eq!(age_text(125), "2m ago");
        assert_eq!(age_text(7200), "2h ago");
    }

    #[test]
    fn sizes() {
        let hp = OverlayState {
            headphone: Some(ComponentState::default()),
            ..Default::default()
        };
        assert!(surface_size(Some(&hp)).0 < surface_size(None).0);
        assert_eq!(surface_size(None).1, (PILL_H + 2.0 * PAD) as u32);
    }
}

//! Draws the island with iced's canvas, all in logical pixels (iced scales to the
//! output's (fractional) scale, so it stays crisp on HiDPI). Everything that moves is
//! decided in `island.rs` and `logic.rs`; this file only paints one frame of it.

use std::f32::consts::PI;
use std::time::Instant;

use iced::alignment::Vertical;
use iced::font::Weight;
use iced::mouse;
use iced::widget::canvas::{self, Frame, Geometry, Path, Stroke, gradient, path::Arc, stroke};
use iced::widget::text::Alignment as TextAlign;
use iced::{Color, Font, Point, Rectangle, Renderer, Size, Theme, Vector, border::Radius};

use crate::glyphs::*;
use crate::island::{
    ALERT_ICON_X, ALERT_LEVEL_W, ALERT_TEXT_X, ALERT_TITLE_SIZE, ALERT_TRAIL_PAD, Alert, BREATHE,
    CASE, COMPACT_ITEM, COMPACT_LEAD, Content, HEADPHONE, LEFT, Level, Motion, Parts, RIGHT,
    ease_out_back, shape, text_width,
};
use crate::logic::Controller;
use crate::overlay_protocol::{ComponentState, OverlayPosition, OverlayState};

/// Transparent room around the island for its shadow.
pub const PAD: f32 = 14.0;
/// How far a changing number travels while it rolls.
const ROLL_TRAVEL: f32 = 7.0;

const GREEN: Color = Color::from_rgb(0.204, 0.820, 0.345);
const AMBER: Color = Color::from_rgb(1.0, 0.702, 0.251);
const RED: Color = Color::from_rgb(1.0, 0.271, 0.227);

fn white(a: f32) -> Color {
    Color::from_rgba(1.0, 1.0, 1.0, a)
}

fn weight(w: Weight) -> Font {
    Font {
        weight: w,
        ..Font::DEFAULT
    }
}

/// Logical size of the layer surface for an island of `extent` (width, height). Kept
/// even so a centered surface never lands on a half pixel.
pub fn surface_size(extent: (f32, f32)) -> (u32, u32) {
    let even = |v: f32| {
        let v = v.ceil() as u32;
        v + v % 2
    };
    (even(extent.0 + 2.0 * PAD), even(extent.1 + 2.0 * PAD))
}

/// Top-left of a `size` rect hugging the anchored edges inside the surface.
fn anchored(position: OverlayPosition, surface: Size, size: Size) -> Point {
    use OverlayPosition::*;
    let x = match position {
        TopLeft | BottomLeft => PAD,
        TopCenter | BottomCenter => (surface.width - size.width) / 2.0,
        TopRight | BottomRight => surface.width - PAD - size.width,
    };
    let y = if position.is_top() {
        PAD
    } else {
        surface.height - PAD - size.height
    };
    Point::new(x, y)
}

pub struct Island<'a> {
    pub ctl: &'a Controller,
    pub now: Instant,
}

impl<Message> canvas::Program<Message> for Island<'_> {
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

/// Everything one piece of content needs to draw itself.
struct Ctx<'a> {
    state: &'a OverlayState,
    motion: &'a Motion,
    name: &'a str,
    /// Opacity for text and icons.
    ink: f32,
    /// Seconds since this content started coming in (large for outgoing content).
    age: f32,
    /// 0..1 ring sweep.
    reveal: f32,
    /// The island's current (unscaled) size.
    island: (f32, f32),
}

impl Island<'_> {
    fn paint(&self, frame: &mut Frame, surface: Size) {
        let ctl = self.ctl;
        let m = &ctl.motion;
        let Some(state) = ctl.display_state(self.now) else {
            return;
        };
        let presence = m.alpha.value.clamp(0.0, 1.0);
        if presence <= 0.0 {
            return;
        }
        // Opacity mostly thins the glass; text and icons fade far less, so they stay legible.
        let opacity = ctl.effective_opacity();
        let body_alpha = presence * opacity;
        let ink = presence * (0.8 + 0.2 * opacity);
        let position = ctl.config.position;

        let (w, h, r) = m.size();
        let s = m.pulse.value;
        let origin = anchored(position, surface, Size::new(w, h));
        let center = Point::new(origin.x + w / 2.0, origin.y + h / 2.0);
        let body = Rectangle::new(
            Point::new(center.x - w * s / 2.0, center.y - h * s / 2.0),
            Size::new(w * s, h * s),
        );
        // Drawn into its own layer: geometry in clipped layers is pasted ahead of the
        // frame's own, so the body must be one too or it would cover the content.
        frame.with_clip(Rectangle::with_size(surface), |f| {
            paint_body(f, body, r * s, body_alpha, ctl.config.glass)
        });

        let name = ctl.name(self.now);
        frame.with_clip(body, |f| {
            if let Some((content, a, sc)) = m.leaving() {
                let ctx = Ctx {
                    state: &state,
                    motion: m,
                    name: &name,
                    ink: ink * a,
                    age: f32::MAX,
                    reveal: 1.0,
                    island: (w, h),
                };
                paint_content(f, content, &ctx, position, surface, center, s * sc);
            }
            if let Some(content) = &m.shown {
                let (a, sc) = m.incoming();
                if a > 0.0 {
                    let ctx = Ctx {
                        state: &state,
                        motion: m,
                        name: &name,
                        ink: ink * a,
                        age: m.shown_age,
                        reveal: m.reveal(),
                        island: (w, h),
                    };
                    paint_content(f, content, &ctx, position, surface, center, s * sc);
                }
            }
        });
    }
}

/// Lays `content` out at its final size, anchored like the island, and draws it scaled
/// by `scale` around the island's center. The island's outline clips it, so content
/// is revealed as the island grows, as on the iPhone.
fn paint_content(
    f: &mut Frame,
    content: &Content,
    ctx: &Ctx,
    position: OverlayPosition,
    surface: Size,
    center: Point,
    scale: f32,
) {
    if ctx.ink <= 0.0 {
        return;
    }
    let (cw, ch, _) = shape(content, ctx.state);
    let mut origin = anchored(position, surface, Size::new(cw, ch));
    // Content smaller than the island (it is shrinking onto it) sits in the island's
    // middle; content larger than the island (it is growing) is revealed from the
    // anchored edge.
    let (w, h) = ctx.island;
    let (dx, dy) = ((w - cw).max(0.0) / 2.0, (h - ch).max(0.0) / 2.0);
    use OverlayPosition::*;
    origin.x += match position {
        TopLeft | BottomLeft => dx,
        TopRight | BottomRight => -dx,
        TopCenter | BottomCenter => 0.0,
    };
    origin.y += if position.is_top() { dy } else { -dy };
    f.with_save(|f| {
        f.translate(Vector::new(center.x, center.y));
        f.scale(scale);
        f.translate(Vector::new(origin.x - center.x, origin.y - center.y));
        match content {
            Content::Compact => paint_compact(f, cw, ch, ctx),
            Content::Expanded => paint_expanded(f, cw, ctx),
            Content::Alert(a) => paint_alert(f, a, cw, ch, ctx),
        }
    });
}

fn paint_body(frame: &mut Frame, rect: Rectangle, radius: f32, alpha: f32, glass: f32) {
    let (x, y, w, h) = (rect.x, rect.y, rect.width, rect.height);
    // Soft shadow: stacked, slightly offset rounded rects.
    for i in 0..7 {
        let grow = 1.0 + i as f32 * 1.5;
        let a = 0.05 * alpha * (1.0 - i as f32 / 7.0);
        let p = Path::rounded_rectangle(
            Point::new(x - grow, y - grow + 3.0),
            Size::new(w + 2.0 * grow, h + 2.0 * grow),
            Radius::from(radius + grow),
        );
        frame.fill(&p, Color::from_rgba(0.0, 0.0, 0.0, a));
    }

    let g = glass.clamp(0.0, 1.0);
    let at = |dx: f32, dy: f32| Point::new(x + dx, y + dy);
    let body = Path::rounded_rectangle(at(0.0, 0.0), Size::new(w, h), Radius::from(radius));
    // Glass 0 is the island's own pure black; towards 1 it clears into smoked glass.
    // Never below 0.37 opaque, so text stays readable over a bright game.
    let tint = 0.075 * g;
    frame.fill(
        &body,
        Color::from_rgba(tint, tint, tint * 1.15, (0.97 - 0.6 * g) * alpha),
    );
    // A faint cool wash, like light passing through glass.
    let wash = gradient::Linear::new(at(0.0, 0.0), at(w, h))
        .add_stop(0.0, Color::from_rgba(0.62, 0.74, 1.0, 0.06 * g * alpha))
        .add_stop(0.5, Color::from_rgba(1.0, 1.0, 1.0, 0.0))
        .add_stop(1.0, Color::from_rgba(1.0, 0.86, 0.95, 0.05 * g * alpha));
    frame.fill(&body, wash);
    // Specular sheen across the top half.
    let sheen = gradient::Linear::new(at(0.0, 0.0), at(0.0, h))
        .add_stop(0.0, white((0.04 + 0.18 * g) * alpha))
        .add_stop(0.45, white(0.02 * g * alpha))
        .add_stop(1.0, white(0.0));
    frame.fill(&body, sheen);
    // Light caught by the bottom edge (refraction).
    let caustic = gradient::Linear::new(at(0.0, h * 0.6), at(0.0, h))
        .add_stop(0.0, white(0.0))
        .add_stop(1.0, white(0.07 * g * alpha));
    frame.fill(&body, caustic);
    // Rim: bright at the top, fading towards the bottom. Kept faint on black so the
    // island reads as one object rather than a framed box.
    let border = Path::rounded_rectangle(
        at(0.5, 0.5),
        Size::new(w - 1.0, h - 1.0),
        Radius::from((radius - 0.5).max(0.0)),
    );
    let rim = gradient::Linear::new(at(0.0, 0.0), at(0.0, h))
        .add_stop(0.0, white((0.10 + 0.38 * g) * alpha))
        .add_stop(0.5, white((0.05 + 0.08 * g) * alpha))
        .add_stop(1.0, white((0.07 + 0.18 * g) * alpha));
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
            at(1.5, 1.5),
            Size::new(w - 3.0, h - 3.0),
            Radius::from((radius - 1.5).max(0.0)),
        );
        frame.stroke(
            &inner,
            Stroke::default()
                .with_width(1.0)
                .with_color(white(0.05 * g * alpha)),
        );
    }
}

/// Text with a soft dark halo under it, so it stays readable when the glass is
/// clear and a bright scene sits behind it. Costs two extra glyph draws per label,
/// only while the island is redrawn; the screen behind is never read.
fn fill_text_lifted(frame: &mut Frame, text: canvas::Text) {
    let a = text.color.a;
    if a <= 0.0 {
        return;
    }
    for (dx, dy, k) in [(0.0, 1.0, 0.42), (0.6, 0.6, 0.22), (-0.6, 0.6, 0.22)] {
        frame.fill_text(canvas::Text {
            position: Point::new(text.position.x + dx, text.position.y + dy),
            color: Color::from_rgba(0.0, 0.0, 0.0, a * k),
            ..text.clone()
        });
    }
    frame.fill_text(text);
}

fn label(content: String, at: Point, size: f32, color: Color, font: Font) -> canvas::Text {
    canvas::Text {
        content,
        position: at,
        color,
        size: size.into(),
        font,
        align_y: Vertical::Center,
        ..Default::default()
    }
}

/// Ellipsizes `s` to fit `max_w` at `size`.
fn truncate(s: &str, max_w: f32, size: f32) -> String {
    if text_width(s, size) <= max_w {
        return s.to_string();
    }
    let mut out: String = s.chars().collect();
    while !out.is_empty() && text_width(&format!("{out}…"), size) > max_w {
        out.pop();
    }
    format!("{}…", out.trim_end())
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

/// A percentage that rolls (old value up and out, new value up and in) when it changes,
/// Apple's numeric content transition.
fn draw_level_number(
    f: &mut Frame,
    level: &Level,
    at: Point,
    size: f32,
    color: Color,
    align: TextAlign,
) {
    let t = level.roll();
    let text = |s: String, y: f32, a: f32| canvas::Text {
        align_x: align,
        ..label(
            s,
            Point::new(at.x, at.y + y),
            size,
            with_alpha(color, a),
            weight(Weight::Bold),
        )
    };
    if let Some(prev) = level.prev {
        // Going down rolls downwards, going up rolls upwards.
        let dir = if level.value.unwrap_or(0) >= prev {
            -1.0
        } else {
            1.0
        };
        fill_text_lifted(f, text(format!("{prev}%"), dir * ROLL_TRAVEL * t, 1.0 - t));
        let current = level.value.map_or("--".into(), |v| format!("{v}%"));
        fill_text_lifted(f, text(current, -dir * ROLL_TRAVEL * (1.0 - t), t));
    } else {
        let current = level.value.map_or("--".into(), |v| format!("{v}%"));
        fill_text_lifted(f, text(current, 0.0, 1.0));
    }
}

/// Which motion level and component belong to each battery slot.
fn slots(state: &OverlayState) -> Vec<(usize, Option<&ComponentState>, Slot)> {
    if let Some(hp) = &state.headphone {
        vec![(HEADPHONE, Some(hp), Slot::Headphone)]
    } else {
        vec![
            (LEFT, state.left.as_ref(), Slot::Letter("L")),
            (RIGHT, state.right.as_ref(), Slot::Letter("R")),
            (CASE, state.case.as_ref(), Slot::Case),
        ]
    }
}

#[derive(Clone, Copy)]
enum Slot {
    Letter(&'static str),
    Case,
    Headphone,
}

impl Slot {
    fn title(self) -> &'static str {
        match self {
            Slot::Letter("L") => "Left",
            Slot::Letter(_) => "Right",
            Slot::Case => "Case",
            Slot::Headphone => "Battery",
        }
    }
}

// --- Compact ---------------------------------------------------------------------

/// Glyph leading, the levels trailing: "L 75%  R 77%  [case] 40%".
fn paint_compact(f: &mut Frame, _w: f32, h: f32, ctx: &Ctx) {
    let cy = h / 2.0;
    let glyph = white(0.92 * ctx.ink);
    if ctx.state.elsewhere {
        // The audio is on another device: say so where the mark usually sits.
        draw_phone_glyph(f, Point::new(21.0, cy), 0.82, glyph);
    } else if ctx.state.headphone.is_some() {
        draw_headphone_glyph(f, Point::new(20.0, cy), 0.9, glyph);
    } else {
        draw_app_glyph(f, Point::new(21.0, cy), 1.2, glyph);
    }
    for (i, (idx, comp, slot)) in slots(ctx.state).into_iter().enumerate() {
        let x = COMPACT_LEAD + i as f32 * COMPACT_ITEM;
        let present = comp.is_some_and(|c| c.present);
        let dim = if present { 1.0 } else { 0.45 };
        let shown = comp.filter(|c| c.present || c.stale);
        let label_c = white(0.55 * ctx.ink * dim);
        if shown.is_some_and(|c| c.charging && c.present) {
            draw_bolt(f, Point::new(x + 4.0, cy), 0.8, with_alpha(GREEN, ctx.ink));
        } else {
            match slot {
                Slot::Letter(l) => fill_text_lifted(
                    f,
                    canvas::Text {
                        align_x: TextAlign::Center,
                        ..label(
                            l.into(),
                            Point::new(x + 4.0, cy),
                            11.0,
                            label_c,
                            weight(Weight::Semibold),
                        )
                    },
                ),
                Slot::Case => draw_case_glyph_small(f, Point::new(x + 5.0, cy), label_c),
                Slot::Headphone => {}
            }
        }
        let tx = match slot {
            Slot::Headphone => x - 2.0,
            Slot::Case => x + 15.0,
            Slot::Letter(_) => x + 12.0,
        };
        let color = match shown {
            Some(c) if c.present && !c.charging && c.level <= 20 => level_color(c),
            _ => white(0.96 * dim),
        };
        draw_level_number(
            f,
            &ctx.motion.levels[idx],
            Point::new(tx, cy),
            13.5,
            with_alpha(color, ctx.ink),
            TextAlign::Left,
        );
    }
}

// --- Expanded --------------------------------------------------------------------

fn paint_expanded(f: &mut Frame, w: f32, ctx: &Ctx) {
    const INSET: f32 = 20.0;
    let header_y = 34.0;
    let glyph = white(0.92 * ctx.ink);
    if ctx.state.headphone.is_some() {
        draw_headphone_glyph(f, Point::new(INSET + 16.0, header_y), 1.15, glyph);
    } else {
        draw_app_glyph(f, Point::new(INSET + 16.0, header_y), 1.55, glyph);
    }
    let text_x = INSET + 40.0;
    let trailing = if ctx.state.listening_mode.is_some() {
        34.0
    } else {
        0.0
    };
    let max_w = w - text_x - INSET - trailing;
    fill_text_lifted(
        f,
        label(
            truncate(ctx.name, max_w, 15.0),
            Point::new(text_x, header_y - 8.5),
            15.0,
            white(0.97 * ctx.ink),
            weight(Weight::Semibold),
        ),
    );
    // Second line: rolls over to the news when something happens.
    let cap = &ctx.motion.caption;
    let t = cap.roll();
    let cap_y = header_y + 9.0;
    let cap_color = |a: f32| white(0.58 * ctx.ink * a);
    if let Some(prev) = &cap.prev {
        fill_text_lifted(
            f,
            label(
                truncate(prev, max_w, 12.0),
                Point::new(text_x, cap_y - ROLL_TRAVEL * t),
                12.0,
                cap_color(1.0 - t),
                weight(Weight::Medium),
            ),
        );
    }
    fill_text_lifted(
        f,
        label(
            truncate(&cap.text, max_w, 12.0),
            Point::new(text_x, cap_y + ROLL_TRAVEL * (1.0 - t)),
            12.0,
            cap_color(t),
            weight(Weight::Medium),
        ),
    );
    if let Some(mode) = ctx.state.listening_mode {
        draw_listening_glyph(
            f,
            Point::new(w - INSET - 12.0, header_y),
            0.95,
            mode,
            white(0.7 * ctx.ink),
        );
    }

    let slots = slots(ctx.state);
    let row_y = 60.0;
    let seg_w = (w - 2.0 * INSET) / slots.len() as f32;
    for (i, (idx, comp, slot)) in slots.into_iter().enumerate() {
        draw_segment(
            f,
            comp,
            slot,
            &ctx.motion.levels[idx],
            Point::new(INSET + i as f32 * seg_w, row_y),
            ctx,
        );
    }
}

fn draw_segment(
    f: &mut Frame,
    comp: Option<&ComponentState>,
    slot: Slot,
    level: &Level,
    origin: Point,
    ctx: &Ctx,
) {
    const R: f32 = 15.5;
    const RING_W: f32 = 3.4;
    let alpha = ctx.ink;
    let center = Point::new(origin.x + R + 1.0, origin.y + R + 1.0);
    let present = comp.is_some_and(|c| c.present);
    let stale = comp.is_some_and(|c| c.stale)
        || !present && comp.is_some_and(|c| c.last_seen_secs.is_some());
    // Stale readings and absent components are drawn dimmed.
    let dim = if !present && !stale {
        0.38
    } else if stale {
        0.5
    } else {
        1.0
    };

    f.stroke(
        &Path::circle(center, R),
        Stroke::default()
            .with_width(RING_W)
            .with_color(white(0.13 * alpha)),
    );
    let shown = comp.filter(|c| c.present || c.stale);
    let accent = shown.map(level_color).unwrap_or(white(0.92));
    let frac = level.fill.value.clamp(0.0, 1.0) * ctx.reveal;
    if shown.is_some() && frac > 0.001 {
        let start = -PI / 2.0;
        let arc = Path::new(|b| {
            b.arc(Arc {
                center,
                radius: R,
                start_angle: start.into(),
                end_angle: (start + frac * 2.0 * PI).into(),
            })
        });
        f.stroke(
            &arc,
            Stroke {
                line_cap: stroke::LineCap::Round,
                ..Stroke::default()
                    .with_width(RING_W)
                    .with_color(with_alpha(accent, alpha * dim))
            },
        );
    }

    // Inner label: L / R / case glyph, or a bolt while charging.
    let label_color = white(0.78 * alpha * dim);
    if shown.is_some_and(|c| c.charging && c.present) {
        let pop = 0.6 + 0.4 * ease_out_back((ctx.age - 0.15) / 0.45);
        draw_bolt(f, center, pop, with_alpha(GREEN, alpha));
    } else {
        match slot {
            Slot::Letter(l) => fill_text_lifted(
                f,
                canvas::Text {
                    align_x: TextAlign::Center,
                    ..label(
                        l.into(),
                        center,
                        12.0,
                        label_color,
                        weight(Weight::Semibold),
                    )
                },
            ),
            Slot::Case => draw_case_glyph(f, center, label_color),
            Slot::Headphone => draw_headphone_glyph(f, center, 0.62, label_color),
        }
    }

    let text_x = origin.x + 2.0 * R + 11.0;
    let pct_color = match shown {
        Some(c) if !c.charging && c.level <= 20 && present => accent,
        _ => white(0.96 * dim),
    };
    draw_level_number(
        f,
        level,
        Point::new(text_x, center.y - 7.0),
        16.5,
        with_alpha(pct_color, alpha),
        TextAlign::Left,
    );
    let subtitle = match comp {
        Some(c) if !c.present || c.stale => match c.last_seen_secs {
            Some(s) => age_text(s),
            None if c.stale => "Last seen".to_string(),
            None => slot.title().to_string(),
        },
        _ => slot.title().to_string(),
    };
    fill_text_lifted(
        f,
        label(
            subtitle,
            Point::new(text_x, center.y + 9.5),
            11.0,
            white(0.5 * alpha * if present { 1.0 } else { 0.8 }),
            weight(Weight::Medium),
        ),
    );
}

// --- Alerts ----------------------------------------------------------------------

fn paint_alert(f: &mut Frame, alert: &Alert, w: f32, h: f32, ctx: &Ctx) {
    let cy = h / 2.0;
    let icon_at = Point::new(ALERT_ICON_X, cy);
    // Icons pop in a beat after the island opens, to say what this alert is about.
    let pop = 0.55 + 0.45 * ease_out_back((ctx.age - 0.06) / 0.42);
    let ink = ctx.ink;
    match alert {
        Alert::Charging(_) => {
            f.fill(
                &Path::circle(icon_at, 13.0 * pop),
                with_alpha(GREEN, 0.18 * ink),
            );
            draw_bolt(f, icon_at, 1.25 * pop, with_alpha(GREEN, ink));
        }
        Alert::Low(parts) => {
            let lowest = lowest(ctx.state, *parts);
            let c = lowest.map_or(RED, level_color);
            let frac = lowest.map_or(0.1, |c| c.level as f32 / 100.0).max(0.12);
            draw_battery_glyph(
                f,
                icon_at,
                pop,
                frac,
                with_alpha(c, ink * breathe(ctx.age)),
                white(0.85 * ink),
            );
        }
        Alert::Elsewhere(_) => draw_phone_glyph(f, icon_at, pop, white(0.95 * ink)),
        Alert::Back => draw_monitor_glyph(f, icon_at, pop, white(0.95 * ink)),
        Alert::Listening(mode) => {
            draw_listening_glyph(f, icon_at, 1.1 * pop, *mode, white(0.95 * ink))
        }
        Alert::Mic(_) => draw_mic_glyph(f, icon_at, pop, white(0.95 * ink), false),
        Alert::MicUnavailable => draw_mic_glyph(f, icon_at, pop, white(0.6 * ink), true),
        Alert::Disconnected => {
            if ctx.state.headphone.is_some() {
                draw_headphone_glyph(f, icon_at, 1.0, white(0.55 * ink));
            } else {
                draw_app_glyph(f, icon_at, 1.3, white(0.55 * ink));
            }
        }
    }

    let trailing_w = match (alert.parts(), alert.trailing_text()) {
        (Some(p), _) => 14.0 + p.count() as f32 * ALERT_LEVEL_W,
        (None, Some(t)) => 16.0 + text_width(t, 13.0),
        (None, None) => 0.0,
    };
    let title = truncate(
        &alert.title(ctx.state),
        w - ALERT_TEXT_X - trailing_w - ALERT_TRAIL_PAD,
        ALERT_TITLE_SIZE,
    );
    fill_text_lifted(
        f,
        label(
            title,
            Point::new(ALERT_TEXT_X, cy),
            ALERT_TITLE_SIZE,
            white(0.97 * ink),
            weight(Weight::Semibold),
        ),
    );

    if let Some(text) = alert.trailing_text() {
        fill_text_lifted(
            f,
            canvas::Text {
                align_x: TextAlign::Right,
                ..label(
                    text.into(),
                    Point::new(w - ALERT_TRAIL_PAD, cy),
                    13.0,
                    white(0.55 * ink),
                    weight(Weight::Medium),
                )
            },
        );
    }
    if let Some(parts) = alert.parts() {
        let items: Vec<_> = slots(ctx.state)
            .into_iter()
            .filter(|(idx, _, _)| match *idx {
                LEFT => parts.left,
                RIGHT => parts.right,
                CASE => parts.case,
                _ => parts.headphone,
            })
            .collect();
        let n = items.len() as f32;
        let mut x = w - ALERT_TRAIL_PAD - n * ALERT_LEVEL_W + 4.0;
        for (idx, comp, slot) in items {
            draw_alert_level(
                f,
                comp,
                slot,
                &ctx.motion.levels[idx],
                Point::new(x, cy),
                alert,
                ctx,
            );
            x += ALERT_LEVEL_W;
        }
    }
}

/// Twice a soft dim and back, so a low battery draws the eye without nagging.
fn breathe(age: f32) -> f32 {
    if age >= BREATHE {
        return 1.0;
    }
    let s = (PI * age / (BREATHE / 2.0)).sin();
    1.0 - 0.45 * s * s
}

fn lowest(state: &OverlayState, parts: Parts) -> Option<&ComponentState> {
    [
        (parts.left, &state.left),
        (parts.right, &state.right),
        (parts.case, &state.case),
        (parts.headphone, &state.headphone),
    ]
    .into_iter()
    .filter_map(|(on, c)| on.then_some(c.as_ref()).flatten())
    .min_by_key(|c| c.level)
}

/// A small ring with L / R / case inside, then the percentage.
fn draw_alert_level(
    f: &mut Frame,
    comp: Option<&ComponentState>,
    slot: Slot,
    level: &Level,
    left_center: Point,
    alert: &Alert,
    ctx: &Ctx,
) {
    const R: f32 = 10.0;
    let ink = ctx.ink;
    let center = Point::new(left_center.x + R, left_center.y);
    let accent = comp.map_or(white(0.9), level_color);
    let breath = if matches!(alert, Alert::Low(_)) {
        breathe(ctx.age)
    } else {
        1.0
    };
    f.stroke(
        &Path::circle(center, R),
        Stroke::default()
            .with_width(2.6)
            .with_color(white(0.14 * ink)),
    );
    let frac = level.fill.value.clamp(0.0, 1.0) * ctx.reveal;
    if frac > 0.001 {
        let start = -PI / 2.0;
        let arc = Path::new(|b| {
            b.arc(Arc {
                center,
                radius: R,
                start_angle: start.into(),
                end_angle: (start + frac * 2.0 * PI).into(),
            })
        });
        f.stroke(
            &arc,
            Stroke {
                line_cap: stroke::LineCap::Round,
                ..Stroke::default()
                    .with_width(2.6)
                    .with_color(with_alpha(accent, ink * breath))
            },
        );
    }
    let inner = white(0.8 * ink);
    match slot {
        Slot::Letter(l) => fill_text_lifted(
            f,
            canvas::Text {
                align_x: TextAlign::Center,
                ..label(l.into(), center, 9.5, inner, weight(Weight::Bold))
            },
        ),
        Slot::Case => draw_case_glyph_small(f, center, inner),
        Slot::Headphone => draw_headphone_glyph(f, center, 0.45, inner),
    }
    draw_level_number(
        f,
        level,
        Point::new(center.x + R + 7.0, center.y),
        14.5,
        with_alpha(accent, ink),
        TextAlign::Left,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncation() {
        assert_eq!(truncate("AirPods 4", 200.0, 14.0), "AirPods 4");
        let t = truncate("David's Extremely Long AirPods Pro Name", 100.0, 14.0);
        assert!(t.ends_with('…'));
        assert!(text_width(&t, 14.0) <= 100.0);
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
    fn surface_sizes_are_even_and_padded() {
        let (w, h) = surface_size((197.3, 36.0));
        assert_eq!(w % 2, 0);
        assert_eq!(h % 2, 0);
        assert!(w as f32 >= 197.3 + 2.0 * PAD);
        assert_eq!(h, (36.0 + 2.0 * PAD) as u32);
    }

    #[test]
    fn breathing_is_bounded() {
        assert_eq!(breathe(0.0), 1.0);
        assert!(breathe(BREATHE / 4.0) < 0.6);
        assert_eq!(breathe(BREATHE + 1.0), 1.0);
    }

    #[test]
    fn anchoring_keeps_the_island_on_its_edge() {
        let surface = Size::new(400.0, 140.0);
        let small = Size::new(100.0, 36.0);
        let p = anchored(OverlayPosition::TopCenter, surface, small);
        assert_eq!(p, Point::new(150.0, PAD));
        let p = anchored(OverlayPosition::BottomRight, surface, small);
        assert_eq!(p, Point::new(400.0 - PAD - 100.0, 140.0 - PAD - 36.0));
    }
}

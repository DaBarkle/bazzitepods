//! Small vector glyphs drawn on the island, in logical px around a center point.

use std::f32::consts::PI;

use iced::widget::canvas::{self, Frame, Path, Stroke, gradient, path::Arc, stroke};
use iced::{Color, Point, Size, border::Radius};

use kase::overlay_protocol::ListeningMode;

/// Lightning bolt centered at `c`, about 12 px tall at scale 1.
pub fn draw_bolt(frame: &mut Frame, c: Point, s: f32, color: Color) {
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
pub fn draw_case_glyph_small(frame: &mut Frame, c: Point, color: Color) {
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
pub fn draw_case_glyph(frame: &mut Frame, c: Point, color: Color) {
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

/// The app's mark: an AirPods case (lid, seam, status light) in a 32-unit box centered at
/// `c`, the same geometry as `assets/icons/` and the tray icon.
pub fn draw_app_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let u = s * 0.5;
    let a = color.a;
    let at = |x: f32, y: f32| Point::new(c.x + (x - 16.0) * u, c.y + (y - 16.0) * u);
    let shade = gradient::Linear::new(at(0.0, 6.0), at(0.0, 26.0))
        .add_stop(0.0, Color::from_rgba(1.0, 1.0, 1.0, a))
        .add_stop(1.0, Color::from_rgba(0.85, 0.87, 0.9, a));
    let (lid, body) = case_paths(at, u);
    for path in [&lid, &body] {
        frame.fill(
            path,
            canvas::Fill {
                style: canvas::Style::Gradient(shade.into()),
                rule: canvas::fill::Rule::EvenOdd,
            },
        );
    }
}

/// Lid and body of the case; the body carries the status light as an even-odd hole.
fn case_paths(at: impl Fn(f32, f32) -> Point, u: f32) -> (Path, Path) {
    let lid = Path::new(|b| {
        b.rounded_rectangle(
            at(4.0, 6.0),
            Size::new(24.0 * u, 6.3 * u),
            Radius {
                top_left: 6.3 * u,
                top_right: 6.3 * u,
                bottom_right: 0.0,
                bottom_left: 0.0,
            },
        )
    });
    let body = Path::new(|b| {
        b.rounded_rectangle(
            at(4.0, 14.1),
            Size::new(24.0 * u, 11.9 * u),
            Radius {
                top_left: 0.0,
                top_right: 0.0,
                bottom_right: 7.0 * u,
                bottom_left: 7.0 * u,
            },
        );
        b.circle(at(16.0, 19.6), 1.7 * u);
    });
    (lid, body)
}

/// Over-ear headphones: band + two cups, centered at `c`.
pub fn draw_headphone_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
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

fn line_stroke(width: f32, color: Color) -> Stroke<'static> {
    Stroke {
        line_cap: stroke::LineCap::Round,
        line_join: stroke::LineJoin::Round,
        ..Stroke::default().with_width(width).with_color(color)
    }
}

/// A phone, for "Playing on iPhone".
pub fn draw_phone_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let body = Path::rounded_rectangle(
        Point::new(c.x - 5.5 * s, c.y - 9.0 * s),
        Size::new(11.0 * s, 18.0 * s),
        Radius::from(2.8 * s),
    );
    frame.stroke(&body, line_stroke(1.6 * s, color));
    let island = Path::rounded_rectangle(
        Point::new(c.x - 2.2 * s, c.y - 6.6 * s),
        Size::new(4.4 * s, 1.6 * s),
        Radius::from(0.8 * s),
    );
    frame.fill(&island, color);
}

/// A desktop monitor, for "Back on this PC".
pub fn draw_monitor_glyph(frame: &mut Frame, c: Point, s: f32, color: Color) {
    let screen = Path::rounded_rectangle(
        Point::new(c.x - 9.0 * s, c.y - 7.5 * s),
        Size::new(18.0 * s, 12.5 * s),
        Radius::from(2.2 * s),
    );
    frame.stroke(&screen, line_stroke(1.6 * s, color));
    let stand = Path::new(|b| {
        b.move_to(Point::new(c.x, c.y + 5.0 * s));
        b.line_to(Point::new(c.x, c.y + 8.0 * s));
        b.move_to(Point::new(c.x - 4.0 * s, c.y + 8.5 * s));
        b.line_to(Point::new(c.x + 4.0 * s, c.y + 8.5 * s));
    });
    frame.stroke(&stand, line_stroke(1.6 * s, color));
}

/// A microphone, for the mic switch; `crossed` adds a slash (unavailable).
pub fn draw_mic_glyph(frame: &mut Frame, c: Point, s: f32, color: Color, crossed: bool) {
    let capsule = Path::rounded_rectangle(
        Point::new(c.x - 3.6 * s, c.y - 9.0 * s),
        Size::new(7.2 * s, 12.0 * s),
        Radius::from(3.6 * s),
    );
    frame.fill(&capsule, color);
    let cradle = Path::new(|b| {
        b.arc(Arc {
            center: Point::new(c.x, c.y - 1.5 * s),
            radius: 6.4 * s,
            start_angle: 0.0.into(),
            end_angle: PI.into(),
        });
        b.move_to(Point::new(c.x, c.y + 4.9 * s));
        b.line_to(Point::new(c.x, c.y + 8.5 * s));
        b.move_to(Point::new(c.x - 3.6 * s, c.y + 8.8 * s));
        b.line_to(Point::new(c.x + 3.6 * s, c.y + 8.8 * s));
    });
    frame.stroke(&cradle, line_stroke(1.6 * s, color));
    if crossed {
        let slash = Path::line(
            Point::new(c.x - 7.5 * s, c.y + 7.5 * s),
            Point::new(c.x + 7.5 * s, c.y - 9.0 * s),
        );
        frame.stroke(&slash, line_stroke(1.8 * s, color));
    }
}

/// A horizontal battery filled to `frac`, for low battery alerts.
pub fn draw_battery_glyph(frame: &mut Frame, c: Point, s: f32, frac: f32, fill: Color, rim: Color) {
    let (w, h) = (20.0 * s, 10.5 * s);
    let body = Path::rounded_rectangle(
        Point::new(c.x - w / 2.0 - 1.0 * s, c.y - h / 2.0),
        Size::new(w, h),
        Radius::from(3.0 * s),
    );
    frame.stroke(&body, line_stroke(1.4 * s, rim));
    let nub = Path::rounded_rectangle(
        Point::new(c.x + w / 2.0, c.y - 2.0 * s),
        Size::new(1.8 * s, 4.0 * s),
        Radius::from(0.9 * s),
    );
    frame.fill(&nub, rim);
    let inner_w = (w - 4.4 * s) * frac.clamp(0.0, 1.0);
    if inner_w > 0.5 {
        let level = Path::rounded_rectangle(
            Point::new(c.x - w / 2.0 + 1.2 * s, c.y - h / 2.0 + 2.2 * s),
            Size::new(inner_w.max(2.4 * s), h - 4.4 * s),
            Radius::from(1.4 * s),
        );
        frame.fill(&level, fill);
    }
}

/// The noise control mode's symbol, in an 18 px circle.
pub fn draw_listening_glyph(
    frame: &mut Frame,
    c: Point,
    s: f32,
    mode: ListeningMode,
    color: Color,
) {
    let r = 8.5 * s;
    match mode {
        ListeningMode::NoiseCancellation => {
            // Sound kept out: a solid core inside a closed ring.
            frame.stroke(&Path::circle(c, r), line_stroke(1.6 * s, color));
            frame.fill(&Path::circle(c, 4.6 * s), color);
        }
        ListeningMode::Transparency => {
            // Sound let in: an open, dotted ring around a small core.
            for i in 0..10 {
                let a = i as f32 / 10.0 * 2.0 * PI;
                let p = Point::new(c.x + r * a.cos(), c.y + r * a.sin());
                frame.fill(&Path::circle(p, 1.15 * s), color);
            }
            frame.fill(&Path::circle(c, 3.2 * s), color);
        }
        ListeningMode::Adaptive => {
            // Both at once: half solid, half open.
            frame.stroke(&Path::circle(c, r), line_stroke(1.6 * s, color));
            let half = Path::new(|b| {
                b.arc(Arc {
                    center: c,
                    radius: 4.8 * s,
                    start_angle: (PI / 2.0).into(),
                    end_angle: (3.0 * PI / 2.0).into(),
                });
                b.close();
            });
            frame.fill(&half, color);
            frame.stroke(&Path::circle(c, 4.8 * s), line_stroke(1.2 * s, color));
        }
        ListeningMode::Off => {
            frame.stroke(&Path::circle(c, r), line_stroke(1.6 * s, color));
            let slash = Path::line(
                Point::new(c.x - 5.6 * s, c.y + 5.6 * s),
                Point::new(c.x + 5.6 * s, c.y - 5.6 * s),
            );
            frame.stroke(&slash, line_stroke(1.6 * s, color));
        }
    }
}

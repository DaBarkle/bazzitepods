//! Small building blocks shared by the tabs: grouped rows, switches, sliders, segmented
//! controls and text styles.

use super::*;
use iced::widget::{column, row};

pub(super) fn dim_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Tokens::of(theme).text_dim),
    }
}

pub(super) fn red_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Tokens::of(theme).red),
    }
}

pub(super) fn faint_text(theme: &Theme) -> text::Style {
    text::Style {
        color: Some(Tokens::of(theme).text_faint),
    }
}

pub(super) fn gap<'a>(height: f32) -> Element<'a, Message> {
    Space::new().height(height).into()
}

/// A grouped panel: rows separated by hairlines inset from the left edge, iOS style.
pub(super) fn group(rows: Vec<Element<'_, Message>>) -> Element<'_, Message> {
    let mut col = column![];
    for (i, r) in rows.into_iter().enumerate() {
        if i > 0 {
            col = col.push(
                container(
                    container(Space::new())
                        .width(Fill)
                        .height(1)
                        .style(style::divider),
                )
                .padding(Padding {
                    left: 16.0,
                    ..Padding::ZERO
                }),
            );
        }
        col = col.push(
            container(r)
                .padding(Padding {
                    top: 13.0,
                    bottom: 13.0,
                    left: 16.0,
                    right: 16.0,
                })
                .width(Fill),
        );
    }
    container(col).width(Fill).style(style::group).into()
}

pub(super) fn section_label<'a>(label: &str) -> Element<'a, Message> {
    container(
        text(label.to_uppercase())
            .size(12)
            .font(style::INTER_SEMIBOLD)
            .style(dim_text),
    )
    .padding(Padding {
        top: 22.0,
        bottom: 8.0,
        left: 8.0,
        right: 0.0,
    })
    .into()
}

pub(super) fn kv<'a>(k: &'a str, v: String) -> Element<'a, Message> {
    row![
        text(k).size(14).width(Fill),
        text(v).size(14).style(dim_text),
    ]
    .spacing(12)
    .into()
}

/// A setting whose control needs the full width: title and caption above it.
/// The confirm step under the Apple device mode switch.
pub(super) fn apple_confirm<'a>(
    setting: crate::apple_mode::Setting,
    on: bool,
) -> Element<'a, Message> {
    use crate::apple_mode::Setting;
    let (title, body, action) = match (setting, on) {
        (Setting::AppleMode, true) => (
            "Turn on Apple device mode?",
            "Bluetooth will restart, so your AirPods and other Bluetooth devices disconnect \
             for a few seconds and reconnect on their own. Your system will ask for your \
             password once.",
            "Restart Bluetooth",
        ),
        (Setting::AppleMode, false) => (
            "Turn off Apple device mode?",
            "Your original Bluetooth settings come back. Bluetooth restarts, so devices \
             disconnect for a few seconds, and your system will ask for your password.",
            "Turn Off",
        ),
        (Setting::Experimental, true) => (
            "Turn on experimental Bluetooth?",
            "Bluetooth's experimental features let Kase watch for your AirPods' battery \
             while they're away. They're widely used, but marked experimental by Linux. \
             Bluetooth restarts, so devices disconnect for a few seconds, and your system \
             will ask for your password once.",
            "Restart Bluetooth",
        ),
        (Setting::Experimental, false) => (
            "Turn off experimental Bluetooth?",
            "Bluetooth goes back to its default features. It restarts, so devices disconnect \
             for a few seconds, and your system will ask for your password.",
            "Turn Off",
        ),
    };
    let pad = Padding {
        top: 7.0,
        bottom: 7.0,
        left: 14.0,
        right: 14.0,
    };
    container(
        column![
            text(title).size(13).font(style::INTER_SEMIBOLD),
            text(body).size(12).style(dim_text),
            row![
                Space::new().width(Fill),
                button(text("Cancel").size(12).font(style::INTER_MEDIUM))
                    .padding(pad)
                    .style(style::secondary)
                    .on_press(Message::AppleCancel),
                button(text(action).size(12).font(style::INTER_SEMIBOLD))
                    .padding(pad)
                    .style(style::primary)
                    .on_press(Message::AppleConfirm),
            ]
            .spacing(8)
            .align_y(Center),
        ]
        .spacing(8),
    )
    .padding(14)
    .width(Fill)
    .style(style::inset)
    .into()
}

/// A one-line status under the Apple device mode switch.
pub(super) fn apple_note<'a>(
    msg: &'a str,
    style: fn(&Theme) -> text::Style,
) -> Element<'a, Message> {
    container(text(msg).size(12).style(style))
        .padding(Padding {
            top: 10.0,
            bottom: 10.0,
            left: 14.0,
            right: 14.0,
        })
        .width(Fill)
        .style(style::inset)
        .into()
}

pub(super) fn setting_block<'a>(
    title: &'a str,
    caption: Option<&'a str>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut col = column![text(title).size(14).font(style::INTER_MEDIUM)].spacing(2);
    if let Some(c) = caption {
        col = col.push(text(c).size(12).style(dim_text));
    }
    column![col, control].spacing(10).into()
}

pub(super) fn setting_row<'a>(
    title: &'a str,
    caption: Option<&'a str>,
    control: Element<'a, Message>,
) -> Element<'a, Message> {
    let mut left = column![text(title).size(14).font(style::INTER_MEDIUM)].spacing(2);
    if let Some(c) = caption {
        left = left.push(text(c).size(12).style(dim_text));
    }
    row![left.width(Fill), control]
        .spacing(12)
        .align_y(Center)
        .into()
}

pub(super) fn switch<'a>(on: bool, msg: Option<fn(bool) -> Message>) -> Element<'a, Message> {
    toggler(on)
        .on_toggle_maybe(msg)
        .size(24)
        .spacing(0)
        .style(style::toggle)
        .into()
}

pub(super) fn percent_slider<'a>(
    range: std::ops::RangeInclusive<f32>,
    value: f32,
    msg: fn(f32) -> Message,
    enabled: bool,
) -> Element<'a, Message> {
    let pct = text(format!("{}%", (value * 100.0).round() as u32))
        .size(12)
        .font(style::INTER_MEDIUM)
        .style(if enabled { dim_text } else { faint_text })
        .align_x(iced::alignment::Horizontal::Right)
        .width(Length::Fixed(34.0));
    row![
        slider(range, value, msg)
            .step(0.05_f32)
            .on_release(Message::CommitOverlaySliders)
            .style(style::slide)
            .width(Length::Fixed(116.0)),
        pct,
    ]
    .spacing(8)
    .align_y(Center)
    .into()
}

/// Size and type of a [`slide_control`].
#[derive(Debug, Clone, Copy)]
pub(super) struct Shape {
    height: f32,
    radius: f32,
    text: f32,
}

impl Shape {
    pub(super) const TABS: Shape = Shape {
        height: 36.0,
        radius: 18.0,
        text: 13.0,
    };
    pub(super) const WIDE: Shape = Shape {
        height: 32.0,
        radius: 10.0,
        text: 12.5,
    };
    pub(super) const COMPACT: Shape = Shape {
        height: 30.0,
        radius: 10.0,
        text: 12.5,
    };
}

/// The row a sliding highlight rides in: `pos` (index, fractional while moving) of `n` equal
/// slots. Built from fill portions, so it needs no pixel widths.
pub(super) fn sliding<'a>(
    n: usize,
    pos: f32,
    thumb: impl Fn(&Theme) -> container::Style + 'a,
) -> Element<'a, Message> {
    const UNIT: f32 = 1000.0;
    let max = n.saturating_sub(1) as f32;
    let left = (pos.clamp(0.0, max) * UNIT).round() as u16;
    let right = (max * UNIT) as u16 - left;
    let mut r = row![];
    if left > 0 {
        r = r.push(Space::new().width(FillPortion(left)));
    }
    r = r.push(
        container(Space::new())
            .width(FillPortion(UNIT as u16))
            .height(Fill)
            .style(thumb),
    );
    if right > 0 {
        r = r.push(Space::new().width(FillPortion(right)));
    }
    r.height(Fill).into()
}

/// Segmented control whose selection glides between options on a spring. `pos` is the
/// animated index of the selected option (None hides the highlight).
pub(super) fn slide_control<'a, T: Copy + PartialEq + 'a>(
    options: &[(&'a str, T)],
    selected: T,
    pos: Option<f32>,
    enabled: bool,
    msg: fn(T) -> Message,
    shape: Shape,
) -> Element<'a, Message> {
    let inner = shape.radius - 2.0;
    let mut labels = row![];
    for &(label, value) in options {
        let is_sel = value == selected;
        labels = labels.push(
            button(
                text(label)
                    .size(shape.text)
                    .font(if is_sel {
                        style::INTER_SEMIBOLD
                    } else {
                        style::INTER_MEDIUM
                    })
                    .wrapping(text::Wrapping::None)
                    .align_x(Center)
                    .align_y(Center)
                    .width(Fill)
                    .height(Fill),
            )
            .width(Fill)
            .height(Fill)
            .padding(0)
            .style(style::segment(is_sel))
            .on_press_maybe(enabled.then_some(msg(value))),
        );
    }
    let mut layers = stack![];
    if let Some(p) = pos {
        layers = layers.push(sliding(options.len(), p, style::thumb(inner)));
    }
    layers = layers.push(labels.height(Fill));
    container(layers.width(Fill).height(Fill))
        .padding(2)
        .width(Fill)
        .height(Length::Fixed(shape.height))
        .style(style::track(shape.radius))
        .into()
}

/// The listening mode tiles, with a glowing highlight that glides to the selected mode.
pub(super) fn mode_tiles<'a>(
    modes: &[ListeningMode],
    selected: Option<usize>,
    pos: Option<f32>,
    enabled: bool,
) -> Element<'a, Message> {
    let mut tiles = row![];
    for (i, &m) in modes.iter().enumerate() {
        let is_sel = selected == Some(i);
        let ink = match (enabled, is_sel) {
            (false, _) => Ink::Faint,
            (true, true) => Ink::Accent,
            (true, false) => Ink::Dim,
        };
        let content = column![
            widgets::icon(Glyph::Mode(m), 22.0, ink),
            text(m.label())
                .size(12)
                .font(if is_sel {
                    style::INTER_SEMIBOLD
                } else {
                    style::INTER_MEDIUM
                })
                .align_x(Center)
                .wrapping(text::Wrapping::Word),
        ]
        .spacing(7)
        .align_x(Center)
        .width(Fill);
        tiles = tiles.push(
            button(content)
                .width(Fill)
                .height(Fill)
                .padding(Padding {
                    top: 13.0,
                    bottom: 8.0,
                    left: 4.0,
                    right: 4.0,
                })
                .style(style::tile(is_sel))
                .on_press_maybe(enabled.then_some(Message::SetMode(m))),
        );
    }
    let mut layers = stack![];
    if let Some(p) = pos {
        layers = layers.push(sliding(modes.len(), p, style::mode_thumb(enabled)));
    }
    layers = layers.push(tiles.height(Fill));
    container(layers.width(Fill).height(Fill))
        .padding(6)
        .width(Fill)
        .height(Length::Fixed(88.0))
        .style(style::group)
        .into()
}

/// Under the rings while the AirPods are away: what the readings mean and what to do.
pub(super) fn away_hint<'a>(d: &DeviceSnapshot) -> Element<'a, Message> {
    let (title, body) = if format::is_nearby(d) {
        (
            "Nearby",
            "These levels are live from the AirPods themselves. They connect as soon as \
             you start using them.",
        )
    } else if d.in_case {
        (
            "In the case",
            "Open the lid near this PC to update the levels. AirPods reconnect on their own \
             when you take them out.",
        )
    } else {
        (
            "Out of range",
            "These are the last levels seen. Kase reconnects on its own when your \
             AirPods are back.",
        )
    };
    group(vec![
        column![
            text(title).size(14).font(style::INTER_MEDIUM),
            text(body).size(12).style(dim_text),
        ]
        .spacing(3)
        .into(),
    ])
}

pub(super) fn empty_state<'a>(glyph: Glyph, title: &'a str, body: &'a str) -> Element<'a, Message> {
    let disc = container(widgets::icon(glyph, 34.0, Ink::Dim))
        .center_x(Length::Fixed(76.0))
        .center_y(Length::Fixed(76.0))
        .style(style::icon_disc);
    container(
        column![
            disc,
            gap(8.0),
            text(title).size(20).font(style::INTER_SEMIBOLD),
            text(body)
                .size(13)
                .align_x(Center)
                .style(dim_text)
                .width(Length::Fixed(300.0)),
        ]
        .spacing(8)
        .align_x(Center),
    )
    .padding(Padding {
        top: 64.0,
        bottom: 30.0,
        left: 20.0,
        right: 20.0,
    })
    .center_x(Fill)
    .into()
}

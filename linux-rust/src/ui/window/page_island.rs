//! The Island tab: where and how the overlay appears.

use super::*;
use iced::widget::{column, row};

impl App {
    pub(super) fn island_tab(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let overlay_on = s.overlay_mode != OverlayMode::Off;
        let mode_pos = self.anim(
            Anim::OverlayMode,
            OVERLAY_MODES
                .iter()
                .position(|(_, m)| *m == s.overlay_mode)
                .unwrap_or(0) as f32,
        );

        let preview = column![
            self.island_screen(),
            row![
                text(if overlay_on {
                    "Click a spot to move the island"
                } else {
                    "The island is off"
                })
                .size(12)
                .style(dim_text)
                .width(Fill),
                button(text("Show on screen").size(13).font(style::INTER_MEDIUM))
                    .padding(Padding {
                        top: 7.0,
                        bottom: 7.0,
                        left: 14.0,
                        right: 14.0,
                    })
                    .style(style::secondary)
                    .on_press_maybe(overlay_on.then_some(Message::PreviewOverlay)),
            ]
            .align_y(Center)
            .padding(Padding {
                top: 10.0,
                left: 4.0,
                ..Padding::ZERO
            }),
        ];

        let behaviour = group(vec![
            setting_block(
                "In games",
                Some(match s.overlay_mode {
                    OverlayMode::Auto => "Opens on changes, then hides",
                    OverlayMode::Always => "Full island always on screen",
                    OverlayMode::Compact => "Slim island, opens on changes",
                    OverlayMode::Off => "Never shown",
                }),
                slide_control(
                    &OVERLAY_MODES,
                    s.overlay_mode,
                    Some(mode_pos),
                    true,
                    Message::SetOverlayMode,
                    Shape::WIDE,
                ),
            ),
            setting_row(
                "Show a card when I open the case",
                Some("Both AirPods and the case, for a few seconds. Never over games."),
                switch(
                    s.case_card,
                    overlay_on.then_some(Message::SetCaseCard as fn(bool) -> Message),
                ),
            ),
        ]);

        let mut look = vec![
            setting_row(
                "Style",
                Some(match s.overlay_style {
                    OverlayStyle::Classic => "Smoked glass with a soft sheen",
                    OverlayStyle::Purist => "Flat black, light runs round the edge",
                }),
                container(slide_control(
                    &OVERLAY_STYLES,
                    s.overlay_style,
                    Some(self.anim(Anim::OverlayStyle, 0.0)),
                    overlay_on,
                    Message::SetOverlayStyle,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(160.0))
                .into(),
            ),
            setting_row(
                "Size",
                None,
                container(slide_control(
                    &OVERLAY_SIZES,
                    s.overlay_size,
                    Some(self.anim(Anim::OverlaySize, 0.0)),
                    overlay_on,
                    Message::SetOverlaySize,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(196.0))
                .into(),
            ),
        ];
        // Glass only shapes the classic island; the purist one is always solid black.
        if s.overlay_style == OverlayStyle::Classic {
            look.push(setting_row(
                "Glass",
                Some("Black to clear glass"),
                percent_slider(
                    0.0..=1.0,
                    s.overlay_glass,
                    Message::SetOverlayGlass,
                    overlay_on,
                ),
            ));
        }
        look.push(setting_row(
            "Opacity over games",
            Some("Over fullscreen games"),
            percent_slider(
                0.2..=1.0,
                s.overlay_opacity,
                Message::SetOverlayOpacity,
                overlay_on,
            ),
        ));
        let look = group(look);

        column![
            preview,
            section_label("Behaviour"),
            behaviour,
            section_label("Look"),
            look,
        ]
        .into()
    }

    /// A little screen with the island where it will appear; the other spots are buttons.
    pub(super) fn island_screen(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let enabled = s.overlay_mode != OverlayMode::Off;
        let align = |p: OverlayPosition| match p {
            OverlayPosition::TopLeft | OverlayPosition::BottomLeft => {
                iced::alignment::Horizontal::Left
            }
            OverlayPosition::TopCenter | OverlayPosition::BottomCenter => {
                iced::alignment::Horizontal::Center
            }
            _ => iced::alignment::Horizontal::Right,
        };
        let spot = |p: OverlayPosition| -> Element<'_, Message> {
            let mut b = button(Space::new())
                .width(34)
                .height(11)
                .padding(0)
                .style(style::spot);
            if p != s.overlay_position {
                b = b.on_press_maybe(enabled.then_some(Message::SetOverlayPosition(p)));
            }
            container(b).width(Fill).align_x(align(p)).into()
        };
        // Each row: three spots, and over them the miniature island where it sits, free to
        // be wider than its third of the screen.
        let line = |a: OverlayPosition, b: OverlayPosition, c: OverlayPosition| {
            let mut layers = stack![row![spot(a), spot(b), spot(c)].height(Fill).align_y(Center)];
            if let Some(p) = [a, b, c].into_iter().find(|p| *p == s.overlay_position) {
                layers = layers.push(
                    container(self.mini_island(enabled))
                        .width(Fill)
                        .height(Fill)
                        .align_x(align(p))
                        .align_y(Center),
                );
            }
            layers.width(Fill).height(24)
        };
        let [tl, tc, tr, bl, bc, br] = OVERLAY_POSITIONS;
        container(
            column![
                line(tl, tc, tr),
                Space::new().height(Fill),
                line(bl, bc, br)
            ]
            .height(Fill),
        )
        .padding(10)
        .width(Fill)
        .height(150)
        .style(style::screen)
        .into()
    }

    /// The compact island in miniature, with the real readings when there are some.
    pub(super) fn mini_island(&self, enabled: bool) -> Element<'_, Message> {
        let gauges = self
            .primary()
            .map(|d| format::gauges(d, self.now))
            .unwrap_or_default();
        let mut line = row![widgets::icon(Glyph::Earbuds, 11.0, Ink::Text)]
            .spacing(5)
            .align_y(Center);
        let small = |t: String, dim: bool| {
            text(t)
                .size(9.5)
                .font(style::INTER_SEMIBOLD)
                .wrapping(text::Wrapping::None)
                .style(move |_: &Theme| text::Style {
                    color: Some(Color::from_rgba(
                        1.0,
                        1.0,
                        1.0,
                        if dim { 0.55 } else { 0.92 },
                    )),
                })
        };
        for g in &gauges {
            let label = match g.label {
                "Left" => "L",
                "Right" => "R",
                "Case" => "Case",
                _ => "",
            };
            let mut item = row![].spacing(3).align_y(Center);
            if !label.is_empty() {
                item = item.push(small(label.to_string(), true));
            }
            item = item.push(small(g.text.clone(), g.dimmed));
            line = line.push(item);
        }
        button(line)
            .padding(Padding {
                top: 5.0,
                bottom: 5.0,
                left: 9.0,
                right: 10.0,
            })
            .style(style::mini_island(enabled))
            .on_press_maybe(enabled.then_some(Message::PreviewOverlay))
            .into()
    }

    // ------------------------------------------------------------ Settings
}

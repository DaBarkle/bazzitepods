//! The AirPods tab: battery hero, status island and listening mode controls.

use super::*;
use iced::widget::{column, row};

impl App {
    pub(super) fn airpods_tab(&self) -> Element<'_, Message> {
        match (self.snap.adapter, self.primary()) {
            (AdapterState::Off, _) => empty_state(
                Glyph::Bluetooth,
                "Bluetooth is off",
                "Turn on Bluetooth from the system tray or System Settings › Bluetooth. \
                 Kase reconnects on its own.",
            ),
            (AdapterState::Missing, None) if self.started.elapsed() < ADAPTER_GRACE => {
                empty_state(Glyph::Bluetooth, "Looking for Bluetooth…", "One moment.")
            }
            (AdapterState::Missing, None) => empty_state(
                Glyph::Bluetooth,
                "Bluetooth unavailable",
                "No Bluetooth adapter was found. Check that Bluetooth is enabled \
                 in System Settings and the adapter is plugged in.",
            ),
            (_, None) => empty_state(
                Glyph::Earbuds,
                "No AirPods yet",
                "Pair your AirPods in System Settings › Bluetooth. \
                 Kase finds them automatically — nothing to set up here.",
            ),
            (_, Some(d)) => {
                let mut page = column![self.hero(d)].spacing(0);
                if d.in_use_elsewhere() {
                    page = page.push(gap(18.0)).push(self.elsewhere_banner(d));
                }
                if let Some(controls) = self.controls(d) {
                    page = page.push(controls);
                } else if !d.is_connected() {
                    page = page.push(gap(22.0)).push(away_hint(d));
                }
                page.into()
            }
        }
    }

    pub(super) fn hero<'a>(&self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let mut subtitle = format::model_name(d.model).to_string();
        if let (true, Some(audio)) = (d.is_connected(), d.audio.as_deref()) {
            subtitle.push_str(" · ");
            subtitle.push_str(audio);
        }
        if d.is_connected() && !d.in_use_elsewhere() && d.other_devices > 0 {
            subtitle.push_str(&match d.other_devices {
                1 => " · Also on 1 other device".to_string(),
                n => format!(" · Also on {n} other devices"),
            });
        }

        let gauges = format::gauges(d, self.now);
        let size = if gauges.len() == 1 { 132.0 } else { 104.0 };
        let mut rings = row![].spacing(4).align_y(iced::Alignment::Start);
        for (i, g) in gauges.iter().enumerate() {
            let fill = g.level.map(|l| l.min(100) as f32 / 100.0);
            let f = fill.map(|f| self.anim(Anim::Ring(i), f).max(0.0));
            rings =
                rings.push(container(widgets::gauge_view::<Message>(g, size, f)).center_x(Fill));
        }

        column![
            self.status_island(d),
            gap(18.0),
            text(d.name.clone())
                .size(26)
                .font(style::INTER_SEMIBOLD)
                .align_x(Center)
                .wrapping(text::Wrapping::WordOrGlyph),
            gap(4.0),
            text(subtitle).size(13).style(dim_text).align_x(Center),
            gap(20.0),
            rings,
        ]
        .align_x(Center)
        .width(Fill)
        .into()
    }

    /// The window's own little Dynamic Island: what matters right now, with an edge light
    /// that pulses when it changes.
    pub(super) fn status_island<'a>(&self, d: &DeviceSnapshot) -> Element<'a, Message> {
        let s = format::status(d, self.now);
        let tone = s.tone;
        let mut line = row![
            container(Space::new())
                .width(7)
                .height(7)
                .style(style::status_dot(tone)),
            text(s.headline).size(13).font(style::INTER_SEMIBOLD),
        ]
        .spacing(8)
        .align_y(Center);
        if let Some(detail) = s.detail {
            line = line.push(
                text(format!("· {detail}"))
                    .size(13)
                    .font(style::INTER_MEDIUM)
                    .style(style::status_detail(tone)),
            );
        }
        let glow = self.anim(Anim::Glow, 1.0);
        container(line)
            .padding(Padding {
                top: 9.0,
                bottom: 9.0,
                left: 14.0,
                right: 16.0,
            })
            .style(style::status_island(tone, glow))
            .into()
    }

    /// Shown while another Apple device has the audio: the AirPods are still connected
    /// here, and one press brings the sound back to this PC.
    pub(super) fn elsewhere_banner<'a>(&self, d: &DeviceSnapshot) -> Element<'a, Message> {
        let switching = self
            .taking_audio
            .is_some_and(|t| t.elapsed() < Duration::from_secs(6));
        let label = if switching {
            "Switching…"
        } else {
            "Use on this PC"
        };
        let device = d.elsewhere_name.as_deref().unwrap_or("another device");
        let take = button(text(label).size(13).font(style::INTER_SEMIBOLD))
            .padding(Padding {
                top: 7.0,
                bottom: 7.0,
                left: 14.0,
                right: 14.0,
            })
            .style(style::primary)
            .on_press_maybe((!switching).then_some(Message::TakeAudio));
        group(vec![
            row![
                column![
                    text(format!("Sound is on {device}"))
                        .size(14)
                        .font(style::INTER_MEDIUM),
                    text("Still connected here. Playing something also switches back.")
                        .size(12)
                        .style(dim_text),
                ]
                .spacing(2)
                .width(Fill),
                take,
            ]
            .align_y(Center)
            .spacing(12)
            .into(),
        ])
    }

    pub(super) fn controls<'a>(&self, d: &'a DeviceSnapshot) -> Option<Element<'a, Message>> {
        if !d.is_connected() {
            return None;
        }
        let modes = format::listening_modes(d);
        let show_ca = format::shows_conversation_awareness(d);
        if modes.is_empty() && !show_ca {
            return None;
        }
        let enabled = format::controls_enabled(d);

        let mut col = column![].spacing(0);
        if !modes.is_empty() {
            let selected = self.mode_index(d);
            let pos = selected.map(|i| self.anim(Anim::Mode, i as f32));
            col = col
                .push(section_label("Listening mode"))
                .push(mode_tiles(&modes, selected, pos, enabled));
        }
        if show_ca {
            let on = self
                .pending_ca
                .map(|(v, _)| v)
                .or(d.conversation_awareness)
                .unwrap_or(false);
            col = col.push(gap(10.0)).push(group(vec![setting_row(
                "Conversation awareness",
                Some("Lowers media and lets voices in when you speak"),
                switch(
                    on,
                    enabled.then_some(Message::SetConversation as fn(bool) -> Message),
                ),
            )]));
        }
        if !enabled {
            col = col.push(gap(10.0)).push(
                text("Controls return once the connection is back.")
                    .size(12)
                    .style(faint_text)
                    .width(Fill)
                    .align_x(Center),
            );
        }
        Some(col.into())
    }

    // -------------------------------------------------------------- Island
}

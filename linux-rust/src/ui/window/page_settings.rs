//! The Settings tab: app, AirPods and Apple device mode settings.

use super::*;
use crate::apple_mode::Setting;
use iced::widget::{column, row};

/// One Diagnostics line: a name, its value and an optional explanation underneath.
fn diag_row<'a>(name: &'a str, value: String, note: Option<String>) -> Element<'a, Message> {
    let mut col = column![kv(name, value)].spacing(2);
    if let Some(n) = note {
        col = col.push(text(n).size(12).style(dim_text));
    }
    col.into()
}

fn diagnostics<'a>(
    diag: &crate::diagnostics::Diagnostics,
    device: Option<&DeviceSnapshot>,
    experimental: Element<'a, Message>,
) -> Element<'a, Message> {
    use crate::diagnostics::{codec_note, le_note};
    let adapter = match &diag.adapter {
        Some((name, true)) => format!("{name}, on"),
        Some((name, false)) => format!("{name}, off"),
        None => "None found".into(),
    };
    let mut rows = vec![diag_row(
        "Bluetooth adapter",
        adapter,
        diag.chip_warning.map(str::to_string),
    )];
    if let Some(blocked) = diag.blocked {
        rows.push(diag_row(
            "Blocked (rfkill)",
            if blocked { "Yes" } else { "No" }.into(),
            blocked.then(|| "Airplane mode or a hardware switch has Bluetooth turned off.".into()),
        ));
    }
    let codec = codec_note(
        device
            .filter(|d| d.is_connected())
            .and_then(|d| d.audio.as_deref()),
    );
    let (value, note) = match codec.split_once(". ") {
        Some((v, n)) => (v.to_string(), Some(n.to_string())),
        None => (codec, None),
    };
    rows.push(diag_row("Audio codec", value, note));
    if let Some(le) = diag.le {
        let text = le_note(le);
        let (value, note) = match text.split_once(": ") {
            Some((v, n)) => (v.to_string(), Some(n.to_string())),
            None => (text.to_string(), None),
        };
        rows.push(diag_row("Battery when not connected", value, note));
    }
    rows.push(experimental);
    group(rows)
}

impl App {
    pub(super) fn settings_tab(&self) -> Element<'_, Message> {
        let s = &self.settings;
        let appearance = [
            ("System", Appearance::System),
            ("Light", Appearance::Light),
            ("Dark", Appearance::Dark),
        ];
        let general = group(vec![
            setting_row(
                "Appearance",
                None,
                container(slide_control(
                    &appearance,
                    s.appearance,
                    Some(self.anim(Anim::Appearance, 0.0)),
                    true,
                    Message::SetAppearance,
                    Shape::COMPACT,
                ))
                .width(Length::Fixed(196.0))
                .into(),
            ),
            setting_row(
                "Start at login",
                Some("Runs quietly in the tray"),
                switch(
                    s.start_at_login,
                    Some(Message::SetStartAtLogin as fn(bool) -> Message),
                ),
            ),
        ]);

        let mut airpods: Vec<Element<'_, Message>> = Vec::new();
        let device = self.primary();
        if let Some(d) = device {
            airpods.push(self.rename_row(d));
        }
        airpods.push(setting_row(
            "Bring AirPods back to this PC",
            Some("When your iPhone stops playing, the AirPods come back here by themselves"),
            switch(
                s.return_to_pc,
                Some(Message::SetReturnToPc as fn(bool) -> Message),
            ),
        ));
        airpods.push(setting_row(
            "Stem press track control",
            Some("Double-press for next track, triple-press for previous"),
            switch(
                s.stem_control,
                Some(Message::SetStemControl as fn(bool) -> Message),
            ),
        ));
        airpods.push(setting_row(
            "AirPods mic switch",
            Some(
                "Hold the left stem to swap between your desk mic and the AirPods mic. \
                 Hold the right stem to change noise control.",
            ),
            switch(
                s.airpods_mic_switch,
                Some(Message::SetMicSwitch as fn(bool) -> Message),
            ),
        ));
        if let Some(d) = device
            && let Some(mic) = d.mic.filter(|_| d.is_connected())
        {
            let on = mic == crate::hub::MicRoute::AirPods;
            let caption = match (&d.mic_problem, on) {
                (Some(problem), false) => problem.as_str(),
                (_, true) => "The AirPods mic is the default mic now",
                (None, false) => "Your desk mic is the default mic",
            };
            airpods.push(setting_row(
                "Use AirPods mic",
                Some(caption),
                switch(
                    on,
                    format::controls_enabled(d)
                        .then_some((|_| Message::ToggleAirPodsMic) as fn(bool) -> Message),
                ),
            ));
        }
        if let Some(d) = device {
            let enabled = format::controls_enabled(d);
            if d.model.info().has_anc {
                airpods.push(setting_row(
                    "Off listening mode",
                    Some("Adds Off to the listening modes"),
                    switch(
                        d.allow_off_mode.unwrap_or(false),
                        enabled.then_some(Message::SetAllowOff as fn(bool) -> Message),
                    ),
                ));
            }
            for setting in format::device_settings(d) {
                airpods.push(self.device_setting(setting, enabled));
            }
        }

        let mut page = column![
            general,
            section_label("AirPods"),
            group(airpods),
            section_label("This PC"),
            group(vec![self.system_setting_row(Setting::AppleMode)]),
        ];
        if let Some(diag) = &self.diagnostics {
            page = page.push(section_label("Diagnostics")).push(diagnostics(
                diag,
                device,
                self.system_setting_row(Setting::Experimental),
            ));
        }

        if let Some(d) = device {
            page = page.push(section_label("About")).push(self.about(d));
        }
        page.into()
    }

    pub(super) fn rename_row<'a>(&'a self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let can_rename = format::controls_enabled(d) && !self.rename.trim().is_empty();
        let mut col = column![
            text("Name").size(14).font(style::INTER_MEDIUM),
            row![
                text_input(&d.name, &self.rename)
                    .on_input(Message::RenameInput)
                    .on_submit_maybe(can_rename.then_some(Message::RenameSubmit))
                    .padding(Padding {
                        top: 8.0,
                        bottom: 8.0,
                        left: 12.0,
                        right: 12.0,
                    })
                    .size(14)
                    .style(style::input)
                    .width(Fill),
                button(text("Rename").size(13).font(style::INTER_MEDIUM))
                    .padding(Padding {
                        top: 8.0,
                        bottom: 8.0,
                        left: 14.0,
                        right: 14.0,
                    })
                    .style(style::secondary)
                    .on_press_maybe(can_rename.then_some(Message::RenameSubmit)),
            ]
            .spacing(8)
            .align_y(Center),
        ]
        .spacing(8);
        if let Some(status) = &self.rename_status {
            col = col.push(text(status.clone()).size(12).style(dim_text));
        }
        col.into()
    }

    /// One of the AirPods' own settings (see [`format::device_settings`]).
    pub(super) fn device_setting<'a>(
        &self,
        setting: DeviceSetting,
        enabled: bool,
    ) -> Element<'a, Message> {
        match setting {
            DeviceSetting::Toggle {
                id,
                title,
                caption,
                on,
            } => setting_row(
                title,
                Some(caption),
                toggler(on)
                    .on_toggle_maybe(
                        enabled.then_some(move |v| Message::SetControl(id, format::toggle_byte(v))),
                    )
                    .size(24)
                    .spacing(0)
                    .style(style::toggle)
                    .into(),
            ),
            DeviceSetting::Choice {
                id,
                title,
                caption,
                options,
                selected,
            } => {
                let options: Vec<(&str, (u8, u8))> =
                    options.iter().map(|&(label, v)| (label, (id, v))).collect();
                let at = options
                    .iter()
                    .position(|(_, (_, v))| *v == selected)
                    .map(|i| self.anim(Anim::Control(id), i as f32));
                setting_block(
                    title,
                    Some(caption),
                    slide_control(
                        &options,
                        (id, selected),
                        at,
                        enabled,
                        |(id, v)| Message::SetControl(id, v),
                        Shape::WIDE,
                    ),
                )
            }
        }
    }

    pub(super) fn about<'a>(&self, d: &'a DeviceSnapshot) -> Element<'a, Message> {
        let mut rows = vec![kv("Model", format::model_name(d.model).to_string())];
        if let Some(n) = &d.model_number {
            rows.push(kv("Model number", n.clone()));
        }
        if let Some(DeviceInformation::AirPods(i)) =
            self.saved.get(&d.mac).and_then(|s| s.information.as_ref())
        {
            if !i.serial_number.is_empty() {
                rows.push(kv("Serial number", i.serial_number.clone()));
            }
            if !i.version1.is_empty() {
                rows.push(kv("Firmware", i.version1.clone()));
            }
        }
        rows.push(kv("Address", d.mac.clone()));
        if let Some(a) = &d.audio {
            rows.push(kv("Audio", a.clone()));
        }
        group(rows)
    }

    /// A system Bluetooth setting (Apple device mode, experimental features): a switch, a
    /// short confirm step that warns about the Bluetooth restart, the system password
    /// prompt, then the result. Falls back to copying a terminal command when the app can't
    /// ask for admin rights. One change runs at a time, so both rows share one flow.
    pub(super) fn system_setting_row(&self, setting: Setting) -> Element<'_, Message> {
        let t = SettingTexts::of(setting);
        let state = self.setting_state(setting);
        let can_change = self.apple_host.is_none_or(|h| h.can_change);
        let flow = if self.apple_target == setting {
            &self.apple_flow
        } else {
            &AppleFlow::Idle
        };
        let busy = matches!(self.apple_flow, AppleFlow::Working(_));
        let shown_on = match flow {
            AppleFlow::Confirm(on) | AppleFlow::Working(on) => *on,
            _ => state.unwrap_or(false),
        };
        let toggle = (can_change && !busy && state.is_some()).then_some(match setting {
            Setting::AppleMode => Message::AppleToggle as fn(bool) -> Message,
            Setting::Experimental => Message::ExperimentalToggle as fn(bool) -> Message,
        });
        let header = setting_row(t.title, Some(t.caption), switch(shown_on, toggle));

        let detail: Option<Element<'_, Message>> = match flow {
            _ if !can_change => Some(self.manual_fallback(setting)),
            AppleFlow::Idle if state.is_none() => Some(
                text("Turn on Bluetooth to change this.")
                    .size(12)
                    .style(faint_text)
                    .into(),
            ),
            AppleFlow::Idle => None,
            AppleFlow::Confirm(on) => Some(apple_confirm(setting, *on)),
            AppleFlow::Working(_) => Some(apple_note(
                "Enter your password in the system prompt. Bluetooth restarts right after, \
                 so your AirPods will drop for a few seconds.",
                dim_text,
            )),
            AppleFlow::Done(true) => Some(apple_note(t.done_on, dim_text)),
            AppleFlow::Done(false) => Some(apple_note(t.done_off, dim_text)),
            AppleFlow::Failed(msg) => Some(apple_note(msg.as_str(), red_text)),
        };
        match detail {
            Some(d) => column![header, d].spacing(10).into(),
            None => header,
        }
    }

    fn setting_state(&self, setting: Setting) -> Option<bool> {
        match setting {
            Setting::AppleMode => self.apple_mode,
            Setting::Experimental => self.experimental,
        }
    }

    /// No pkexec, NixOS, or the sandbox can't start host commands: hand over a command.
    fn manual_fallback(&self, setting: Setting) -> Element<'_, Message> {
        let action = if self.setting_state(setting) == Some(true) {
            "Copy command to turn off"
        } else {
            "Copy command to turn on"
        };
        let label = if self.apple_copied == Some(setting) {
            "Copied. Paste it in a terminal"
        } else {
            action
        };
        row![
            text(
                if self
                    .apple_host
                    .is_some_and(|h| h.os == crate::apple_mode::HostOs::NixOs)
                {
                    "On NixOS this is set in configuration.nix. Add this line, then run sudo \
                     nixos-rebuild switch."
                } else {
                    "This build can't ask for your password, so it needs one terminal command."
                },
            )
            .size(12)
            .style(dim_text)
            .width(Fill),
            button(text(label).size(12).font(style::INTER_MEDIUM))
                .padding(Padding {
                    top: 6.0,
                    bottom: 6.0,
                    left: 12.0,
                    right: 12.0,
                })
                .style(style::secondary)
                .on_press(Message::CopyCommand(setting)),
        ]
        .spacing(12)
        .align_y(Center)
        .into()
    }
}

/// What each system setting's row says.
struct SettingTexts {
    title: &'static str,
    caption: &'static str,
    done_on: &'static str,
    done_off: &'static str,
}

impl SettingTexts {
    fn of(setting: Setting) -> Self {
        match setting {
            Setting::AppleMode => SettingTexts {
                title: "Apple device mode",
                caption: "Lets your AirPods switch between this PC and your iPhone or iPad \
                          the way they do with a Mac, by having this PC introduce itself \
                          as an Apple device.",
                done_on: "Apple device mode is on. Your AirPods will reconnect on their own.",
                done_off: "Apple device mode is off and your original Bluetooth settings are \
                           back.",
            },
            Setting::Experimental => SettingTexts {
                title: "Quicker battery updates when not connected",
                caption: "Shows your AirPods and case battery sooner when they aren't \
                          connected to this PC, for example while you're using them with your \
                          iPhone, or when you open the case nearby. Uses a newer Linux \
                          Bluetooth feature that's still labelled experimental. Bluetooth \
                          restarts for a few seconds when you change this.",
                done_on: "Quicker battery updates are on. They start within a minute.",
                done_off: "Quicker battery updates are off. Battery updates as before.",
            },
        }
    }
}

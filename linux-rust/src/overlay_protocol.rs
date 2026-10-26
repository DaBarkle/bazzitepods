//! Wire protocol between the main app and the `librepods-overlay` process.
//!
//! The parent writes one JSON object per line on the overlay's stdin (see
//! `docs/ARCHITECTURE.md`, "Overlay protocol"). This file is compiled into both
//! binaries (the overlay includes it with `#[path]`), so it must stay free of
//! dependencies on the rest of the crate.

// Each binary uses a different subset of this shared file.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};

/// Exit code of the overlay when the compositor has no `zwlr_layer_shell_v1`
/// (GNOME/Mutter) or there is no Wayland display at all. The client then stops
/// retrying and the app falls back to desktop notifications.
pub const EXIT_UNSUPPORTED: i32 = 3;

/// Where on the screen the pill sits.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum OverlayPosition {
    TopLeft,
    TopCenter,
    #[default]
    TopRight,
    BottomLeft,
    BottomCenter,
    BottomRight,
}

impl OverlayPosition {
    pub fn is_top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopCenter | Self::TopRight)
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum OverlayMode {
    /// Show briefly on `show` and on meaningful state changes, then unmap.
    #[default]
    Auto,
    /// Stay visible while connected.
    Always,
    /// Stay visible while connected as a small widget; expand to the full pill for
    /// `auto_hide_secs` on `show` and on meaningful state changes.
    Compact,
    /// Never show.
    Off,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct OverlayConfig {
    pub position: OverlayPosition,
    /// Distance from the anchored screen edges, in logical pixels.
    pub margin: i32,
    /// How long the pill stays up in `auto` mode, in seconds.
    pub auto_hide_secs: f32,
    pub mode: OverlayMode,
    /// Overall opacity of the pill, 0.2..=1.0 (clamped when drawn).
    pub opacity: f32,
    /// Material, 0 = smoked solid .. 1 = clear liquid glass.
    pub glass: f32,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self {
            position: OverlayPosition::TopRight,
            margin: 16,
            auto_hide_secs: 4.0,
            mode: OverlayMode::Auto,
            opacity: 1.0,
            glass: 0.7,
        }
    }
}

/// One battery component (left pod, right pod, case, or a headphone's single battery).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct ComponentState {
    /// 0..=100. Values above 100 are clamped when drawn.
    pub level: u8,
    pub charging: bool,
    /// False when the component is not reporting (pod out of range, case closed
    /// and away). Drawn dimmed with "--", never as 0%.
    pub present: bool,
    /// True when the value is an old reading (e.g. case after both pods left it).
    pub stale: bool,
    /// Optional age of a stale reading, in seconds, for a "2m ago" hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_seen_secs: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Default)]
#[serde(default)]
pub struct OverlayState {
    pub name: String,
    /// Model key, e.g. "airpods4", "airpods_pro2", "airpods_max". Only used for the glyph.
    pub kind: String,
    pub connected: bool,
    pub left: Option<ComponentState>,
    pub right: Option<ComponentState>,
    pub case: Option<ComponentState>,
    /// Set for over-ear headphones (AirPods Max); then left/right/case are ignored.
    pub headphone: Option<ComponentState>,
    /// Connected here, but another Apple device has the audio.
    pub elsewhere: bool,
    /// The name of the device that has the audio while `elsewhere`, e.g. "iPhone".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub elsewhere_name: Option<String>,
}

/// One line of the protocol.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum OverlayCommand {
    Config(OverlayConfig),
    State {
        state: OverlayState,
    },
    Show {
        #[serde(default)]
        reason: String,
    },
    Hide,
    /// Whether a fullscreen window (game, video) is in front. The pill then uses the
    /// configured opacity; otherwise it is drawn solid.
    Fullscreen {
        active: bool,
    },
    Quit,
}

impl OverlayCommand {
    /// Parses one protocol line. Blank lines yield `Ok(None)`.
    pub fn parse_line(line: &str) -> Result<Option<Self>, serde_json::Error> {
        let line = line.trim();
        if line.is_empty() {
            return Ok(None);
        }
        serde_json::from_str(line).map(Some)
    }

    /// Serializes to a single line terminated by `\n`.
    pub fn to_line(&self) -> String {
        // Serializing these plain data types cannot fail; fall back to a no-op line anyway.
        let mut s = serde_json::to_string(self).unwrap_or_else(|_| String::from("{}"));
        s.push('\n');
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_architecture_examples() {
        let c = OverlayCommand::parse_line(
            r#"{"type":"config","position":"top-right","margin":16,"auto_hide_secs":4,"mode":"auto"}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(c, OverlayCommand::Config(OverlayConfig::default()));

        let s = OverlayCommand::parse_line(
            r#"{"type":"state","state":{ "name":"David's AirPods","kind":"airpods4","connected":true,
  "left":{"level":80,"charging":false,"present":true,"stale":false},
  "right":{"level":75,"charging":false,"present":true,"stale":false},
  "case":{"level":40,"charging":true,"present":true,"stale":false},
  "headphone":null }}"#
                .replace('\n', " ")
                .as_str(),
        )
        .unwrap()
        .unwrap();
        let OverlayCommand::State { state } = s else {
            panic!("not a state")
        };
        assert_eq!(state.name, "David's AirPods");
        assert_eq!(state.left.as_ref().unwrap().level, 80);
        assert!(state.case.as_ref().unwrap().charging);
        assert!(state.headphone.is_none());

        assert_eq!(
            OverlayCommand::parse_line(r#"{"type":"show","reason":"connected"}"#).unwrap(),
            Some(OverlayCommand::Show {
                reason: "connected".into()
            })
        );
        assert_eq!(
            OverlayCommand::parse_line(r#"{"type":"hide"}"#).unwrap(),
            Some(OverlayCommand::Hide)
        );
        assert_eq!(
            OverlayCommand::parse_line(r#"{"type":"quit"}"#).unwrap(),
            Some(OverlayCommand::Quit)
        );
    }

    #[test]
    fn tolerant_parsing() {
        assert_eq!(OverlayCommand::parse_line("   ").unwrap(), None);
        assert!(OverlayCommand::parse_line("not json").is_err());
        assert!(OverlayCommand::parse_line(r#"{"type":"bogus"}"#).is_err());
        // Missing fields fall back to defaults.
        let c = OverlayCommand::parse_line(r#"{"type":"config","mode":"always"}"#)
            .unwrap()
            .unwrap();
        assert_eq!(
            c,
            OverlayCommand::Config(OverlayConfig {
                mode: OverlayMode::Always,
                ..OverlayConfig::default()
            })
        );
        let c = OverlayCommand::parse_line(r#"{"type":"config","position":"bottom-center"}"#)
            .unwrap()
            .unwrap();
        let OverlayCommand::Config(cfg) = c else {
            panic!()
        };
        assert_eq!(cfg.position, OverlayPosition::BottomCenter);
        // Show without a reason, unknown extra fields ignored.
        assert_eq!(
            OverlayCommand::parse_line(r#"{"type":"show","extra":1}"#).unwrap(),
            Some(OverlayCommand::Show {
                reason: String::new()
            })
        );
    }

    #[test]
    fn roundtrip() {
        let cmds = [
            OverlayCommand::Config(OverlayConfig::default()),
            OverlayCommand::State {
                state: OverlayState {
                    name: "AirPods Max".into(),
                    kind: "airpods_max".into(),
                    connected: true,
                    headphone: Some(ComponentState {
                        level: 55,
                        present: true,
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            },
            OverlayCommand::Show { reason: "x".into() },
            OverlayCommand::Hide,
            OverlayCommand::Quit,
        ];
        for c in cmds {
            let line = c.to_line();
            assert!(line.ends_with('\n'));
            assert_eq!(line.matches('\n').count(), 1);
            assert_eq!(OverlayCommand::parse_line(&line).unwrap(), Some(c));
        }
    }
}

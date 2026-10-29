//! Apple device mode: make BlueZ report Apple's Bluetooth vendor id (0x004C), so AirPods
//! treat this PC like an Apple device (multi-device switching, Apple-only features).
//!
//! The setting lives in `/etc/bluetooth/main.conf` (`DeviceID = bluetooth:004C:0000:0000`)
//! and only takes effect after bluetoothd restarts, so it needs root. The app never runs
//! as root: it hands a small fixed script to `pkexec`, which shows the desktop's own
//! password prompt. Inside Flatpak the script is started on the host through
//! `flatpak-spawn --host` (needs `--talk-name=org.freedesktop.Flatpak`); it still runs as
//! the user until polkit grants admin rights, and asks for the password every time.
//!
//! Nothing user-controlled ever reaches the script: it is a compile-time constant and its
//! only argument is the literal `on` or `off`.
//!
//! The current state is read from the adapter's modalias, which BlueZ fills from the
//! DeviceID it is actually running with, so it reflects reality rather than the file.

use log::{info, warn};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Runs as root under pkexec, with `$1` = `on` | `off`. Idempotent both ways.
///
/// * on: backs up main.conf once, drops any earlier Kase line, comments out a
///   DeviceID the user set themselves (marked so `off` can restore it), then adds ours
///   under `[General]`.
/// * off: removes our line and restores anything `on` commented out.
///
/// Then bluetoothd is restarted so the change applies (connected devices drop briefly),
/// through systemd, OpenRC or runit. Only POSIX sed and awk: no GNU-only addresses.
const SCRIPT: &str = r#"set -eu
f=/etc/bluetooth/main.conf
ours='DeviceID = bluetooth:004C:0000:0000'
mark='#kase-disabled# '
old_mark='#bazzitepods-disabled# '
if [ ! -f "$f" ]; then
  mkdir -p /etc/bluetooth
  printf '[General]\n' > "$f"
fi
case "${1:-}" in
  on)
    [ -e "$f.bak-kase" ] || [ -e "$f.bak-bazzitepods" ] || cp -a "$f" "$f.bak-kase"
    sed -i '/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*bluetooth:004C:/d' "$f"
    sed -i "s|^\([[:space:]]*DeviceID[[:space:]]*=\)|$mark\1|" "$f"
    if grep -q '^[[:space:]]*\[General\]' "$f"; then
      awk -v l="$ours" '{ print } !d && /^[[:space:]]*\[General\]/ { print l; d = 1 }' "$f" > "$f.kase-tmp"
      cat "$f.kase-tmp" > "$f"
      rm -f "$f.kase-tmp"
    else
      printf '\n[General]\n%s\n' "$ours" >> "$f"
    fi
    ;;
  off)
    sed -i '/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*bluetooth:004C:/d' "$f"
    sed -i "s|^$mark||; s|^$old_mark||" "$f"
    ;;
  *)
    exit 2
    ;;
esac
command -v restorecon >/dev/null 2>&1 && restorecon "$f" || true
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
  systemctl restart bluetooth.service
elif command -v rc-service >/dev/null 2>&1; then
  rc-service bluetooth restart
elif command -v sv >/dev/null 2>&1; then
  sv restart bluetoothd
else
  echo "edited $f; restart Bluetooth (or your PC) to apply it" >&2
  exit 3
fi
"#;

/// The host system, as far as Apple device mode cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HostOs {
    /// /etc is generated from configuration.nix, so main.conf can't be edited.
    NixOs,
    /// The default `deck` account has no password, so pkexec can't succeed until it has one.
    SteamOs,
    #[default]
    Other,
}

/// What the app found out about the host when Settings opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Host {
    pub os: HostOs,
    pub systemd: bool,
    /// pkexec exists and (inside Flatpak) host commands are allowed, and the OS lets us edit
    /// main.conf. When false, Settings offers a command to copy instead.
    pub can_change: bool,
}

/// Reads `ID` and `ID_LIKE` from an os-release file.
pub fn parse_os(os_release: &str) -> HostOs {
    let mut ids = String::new();
    for line in os_release.lines() {
        if let Some(v) = line
            .strip_prefix("ID=")
            .or_else(|| line.strip_prefix("ID_LIKE="))
        {
            ids.push(' ');
            ids.push_str(v.trim_matches(|c| c == '"' || c == '\''));
        }
    }
    let has = |id: &str| ids.split_whitespace().any(|w| w == id);
    if has("nixos") {
        HostOs::NixOs
    } else if has("steamos") {
        HostOs::SteamOs
    } else {
        HostOs::Other
    }
}

/// The command Settings copies when the app can't make the change itself.
pub fn manual_command(on: bool, host: Host) -> String {
    const CONF: &str = "/etc/bluetooth/main.conf";
    if host.os == HostOs::NixOs {
        return if on {
            "hardware.bluetooth.settings.General.DeviceID = \"bluetooth:004C:0000:0000\";".into()
        } else {
            "# remove hardware.bluetooth.settings.General.DeviceID from configuration.nix, then: sudo nixos-rebuild switch".into()
        };
    }
    let restart = if host.systemd {
        "sudo systemctl restart bluetooth"
    } else {
        "sudo rc-service bluetooth restart  # or: sudo sv restart bluetoothd"
    };
    if on {
        format!(
            "sudo cp -a {CONF} {CONF}.bak-kase && \
sudo sed -i '/^\\[General\\]/a DeviceID = bluetooth:004C:0000:0000' {CONF} && \
{restart}"
        )
    } else {
        format!("sudo sed -i '/^DeviceID = bluetooth:004C:/d' {CONF} && {restart}")
    }
}

/// Inside Flatpak the host's os-release is at /run/host; /etc is the runtime's.
fn host_os() -> HostOs {
    ["/run/host/os-release", "/etc/os-release"]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| parse_os(&s))
        .unwrap_or_default()
}

/// Why a change didn't happen. Each maps to one plain sentence in Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// The password prompt was closed.
    Cancelled,
    /// Wrong password three times, or the account isn't an administrator.
    NotAuthorized,
    /// The change ran but Bluetooth didn't come back reporting the new state.
    NotApplied,
    /// Anything else (no pkexec, the script failed...).
    Other(String),
}

impl Failure {
    pub fn message(&self) -> String {
        match self {
            Failure::Cancelled => "Cancelled. Nothing was changed.".into(),
            Failure::NotAuthorized => {
                "Your password wasn't accepted, or this account can't make system changes.".into()
            }
            Failure::NotApplied => {
                "Bluetooth didn't pick up the change. Try again, or restart your PC.".into()
            }
            Failure::Other(e) => format!("Couldn't change it: {e}"),
        }
    }
}

fn in_flatpak() -> bool {
    crate::autostart::running_in_flatpak()
}

/// Builds the command, wrapped in `flatpak-spawn --host` inside the sandbox.
fn host_command(program: &str, args: &[&str]) -> Command {
    let mut cmd = if in_flatpak() {
        let mut c = Command::new("flatpak-spawn");
        c.arg("--host").arg(program);
        c
    } else {
        Command::new(program)
    };
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    cmd
}

async fn host_has(check: &str) -> bool {
    let probe = host_command("sh", &["-c", check]).status();
    match tokio::time::timeout(Duration::from_secs(5), probe).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}

/// Probes the host: its OS, whether it runs systemd, and whether the app can ask for admin
/// rights on its own (pkexec exists on the host and, inside Flatpak, we're allowed to start
/// host commands).
pub async fn probe_host() -> Host {
    let os = host_os();
    Host {
        os,
        systemd: host_has("[ -d /run/systemd/system ]").await,
        can_change: os != HostOs::NixOs && host_has("command -v pkexec >/dev/null").await,
    }
}

/// True when the running bluetoothd reports Apple's vendor id; None if there's no adapter.
pub async fn status() -> Option<bool> {
    let session = bluer::Session::new().await.ok()?;
    let adapter = crate::bluetooth::adapter::airpods_adapter(&session)
        .await
        .ok()?;
    let m = adapter.modalias().await.ok()??;
    Some(m.vendor == 0x004C)
}

/// Turns Apple device mode on or off. Shows the system password prompt, restarts
/// Bluetooth, then waits for the adapter to come back and confirms the new state.
pub async fn set(on: bool) -> Result<bool, Failure> {
    let arg = if on { "on" } else { "off" };
    info!("Apple device mode: asking pkexec to turn it {arg}");
    let out = host_command("pkexec", &["/bin/sh", "-c", SCRIPT, "kase-apple-mode", arg])
        .output()
        .await
        .map_err(|e| Failure::Other(e.to_string()))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        warn!(
            "Apple device mode: helper exited {:?}: {stderr}",
            out.status.code()
        );
        return Err(match out.status.code() {
            // pkexec: 126 = dialog dismissed, 127 = not authorized (or couldn't authenticate).
            Some(126) => Failure::Cancelled,
            Some(127) if stderr.contains("authentication agent") || stderr.contains("polkit") => {
                Failure::Other(
                    "no password prompt (polkit agent) is running on this desktop. Start one, \
                     or run the change in a terminal with sudo"
                        .into(),
                )
            }
            Some(127) if host_os() == HostOs::SteamOs => Failure::Other(
                "SteamOS needs a password for system changes. Set one with passwd in Konsole, \
                 then try again"
                    .into(),
            ),
            Some(127) => Failure::NotAuthorized,
            Some(3) => Failure::Other(
                "the setting was saved, but Bluetooth couldn't be restarted. Restart your PC to apply it"
                    .into(),
            ),
            _ if stderr.is_empty() => Failure::Other("the helper failed".into()),
            _ => Failure::Other(stderr.lines().last().unwrap_or_default().to_string()),
        });
    }

    // bluetoothd takes a moment to come back and re-register the adapter.
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if status().await == Some(on) {
            info!("Apple device mode is now {arg}");
            return Ok(on);
        }
    }
    Err(Failure::NotApplied)
}

#[cfg(test)]
mod tests {
    use super::SCRIPT;
    use std::process::Command;

    /// Runs the root script's file edits against a temp copy (no systemctl, no /etc).
    fn run(conf: &str, arg: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "kase-apple-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("main.conf");
        std::fs::write(&f, conf).unwrap();
        let script = SCRIPT
            .replace("f=/etc/bluetooth/main.conf", &format!("f={}", f.display()))
            .replace("mkdir -p /etc/bluetooth", "true")
            .replace(
                "command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]",
                "true",
            )
            .replace("systemctl restart bluetooth.service", "true")
            .replace("restorecon \"$f\"", "true");
        let ok = Command::new("sh")
            .args(["-c", &script, "test", arg])
            .status()
            .unwrap()
            .success();
        assert!(ok, "script failed for {arg}");
        let out = std::fs::read_to_string(&f).unwrap();
        std::fs::remove_dir_all(&dir).ok();
        out
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    }

    use super::{Host, HostOs, manual_command, parse_os};

    #[test]
    fn os_release_is_recognised() {
        assert_eq!(parse_os("NAME=NixOS\nID=nixos\n"), HostOs::NixOs);
        assert_eq!(parse_os("ID=steamos\nID_LIKE=arch\n"), HostOs::SteamOs);
        assert_eq!(
            parse_os("ID=\"bazzite\"\nID_LIKE=\"fedora\"\n"),
            HostOs::Other
        );
        assert_eq!(parse_os(""), HostOs::Other);
    }

    #[test]
    fn manual_command_fits_the_host() {
        let nix = Host {
            os: HostOs::NixOs,
            ..Default::default()
        };
        assert_eq!(
            manual_command(true, nix),
            "hardware.bluetooth.settings.General.DeviceID = \"bluetooth:004C:0000:0000\";"
        );
        let systemd = Host {
            systemd: true,
            ..Default::default()
        };
        assert!(manual_command(true, systemd).ends_with("sudo systemctl restart bluetooth"));
        assert!(manual_command(false, systemd).starts_with("sudo sed -i '/^DeviceID"));
        assert!(manual_command(false, Host::default()).contains("rc-service bluetooth restart"));
    }

    const FEDORA: &str = "[General]\n#Name = BlueZ\n#DeviceID = bluetooth:1234:5678:abcd\n\n[Policy]\nAutoEnable=true\n";

    #[test]
    fn on_adds_one_line_under_general() {
        let out = run(FEDORA, "on");
        assert_eq!(
            out,
            "[General]\nDeviceID = bluetooth:004C:0000:0000\n#Name = BlueZ\n#DeviceID = bluetooth:1234:5678:abcd\n\n[Policy]\nAutoEnable=true\n"
        );
    }

    #[test]
    fn on_is_idempotent_and_off_restores() {
        let once = run(FEDORA, "on");
        let twice = run(&once, "on");
        assert_eq!(once, twice);
        assert_eq!(run(&twice, "off"), FEDORA);
    }

    #[test]
    fn user_device_id_is_parked_and_restored() {
        let conf = "[General]\nDeviceID = bluetooth:1d6b:0246:0540\n";
        let on = run(conf, "on");
        assert_eq!(
            on,
            "[General]\nDeviceID = bluetooth:004C:0000:0000\n#kase-disabled# DeviceID = bluetooth:1d6b:0246:0540\n"
        );
        assert_eq!(run(&on, "off"), conf);
    }

    #[test]
    fn a_device_id_parked_by_an_older_build_is_restored() {
        let conf = "[General]\nDeviceID = bluetooth:004C:0000:0000\n#bazzitepods-disabled# DeviceID = bluetooth:1d6b:0246:0540\n";
        assert_eq!(
            run(conf, "off"),
            "[General]\nDeviceID = bluetooth:1d6b:0246:0540\n"
        );
    }

    #[test]
    fn readme_command_line_is_replaced_not_duplicated() {
        let conf = "[General]\nDeviceID = bluetooth:004C:0000:0000\n";
        assert_eq!(run(conf, "on"), conf);
        assert_eq!(run(conf, "off"), "[General]\n");
    }

    #[test]
    fn missing_general_section_is_added() {
        let out = run("[Policy]\nAutoEnable=true\n", "on");
        assert_eq!(
            out,
            "[Policy]\nAutoEnable=true\n\n[General]\nDeviceID = bluetooth:004C:0000:0000\n"
        );
    }

    #[test]
    fn only_first_general_section_gets_the_line() {
        let out = run("[General]\n[General]\n", "on");
        assert_eq!(
            out,
            "[General]\nDeviceID = bluetooth:004C:0000:0000\n[General]\n"
        );
    }
}

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
/// Then bluetoothd is restarted so the change applies (connected devices drop briefly).
const SCRIPT: &str = r#"set -eu
f=/etc/bluetooth/main.conf
ours='DeviceID = bluetooth:004C:0000:0000'
mark='#bazzitepods-disabled# '
if [ ! -f "$f" ]; then
  mkdir -p /etc/bluetooth
  printf '[General]\n' > "$f"
fi
case "${1:-}" in
  on)
    [ -e "$f.bak-bazzitepods" ] || cp -a "$f" "$f.bak-bazzitepods"
    sed -i '/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*bluetooth:004C:/d' "$f"
    sed -i "s|^\([[:space:]]*DeviceID[[:space:]]*=\)|$mark\1|" "$f"
    if grep -q '^[[:space:]]*\[General\]' "$f"; then
      sed -i "0,/^[[:space:]]*\[General\]/{//a $ours
}" "$f"
    else
      printf '\n[General]\n%s\n' "$ours" >> "$f"
    fi
    ;;
  off)
    sed -i '/^[[:space:]]*DeviceID[[:space:]]*=[[:space:]]*bluetooth:004C:/d' "$f"
    sed -i "s|^$mark||" "$f"
    ;;
  *)
    exit 2
    ;;
esac
command -v restorecon >/dev/null 2>&1 && restorecon "$f" || true
systemctl restart bluetooth.service
"#;

/// Shown in Settings when the app can't ask for admin rights itself (see [`can_change`]).
pub const MANUAL_ON: &str = "sudo cp -a /etc/bluetooth/main.conf /etc/bluetooth/main.conf.bak-bazzitepods && \
sudo sed -i '/^\\[General\\]/a DeviceID = bluetooth:004C:0000:0000' /etc/bluetooth/main.conf && \
sudo systemctl restart bluetooth";
pub const MANUAL_OFF: &str = "sudo sed -i '/^DeviceID = bluetooth:004C:/d' /etc/bluetooth/main.conf && \
sudo systemctl restart bluetooth";

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

/// Whether the app can ask for admin rights on its own: pkexec exists on the host and,
/// inside Flatpak, we're allowed to start host commands.
pub async fn can_change() -> bool {
    let probe = host_command("sh", &["-c", "command -v pkexec >/dev/null"]).status();
    match tokio::time::timeout(Duration::from_secs(5), probe).await {
        Ok(Ok(status)) => status.success(),
        _ => false,
    }
}

/// True when the running bluetoothd reports Apple's vendor id; None if there's no adapter.
pub async fn status() -> Option<bool> {
    let session = bluer::Session::new().await.ok()?;
    let adapter = session.default_adapter().await.ok()?;
    let m = adapter.modalias().await.ok()??;
    Some(m.vendor == 0x004C)
}

/// Turns Apple device mode on or off. Shows the system password prompt, restarts
/// Bluetooth, then waits for the adapter to come back and confirms the new state.
pub async fn set(on: bool) -> Result<bool, Failure> {
    let arg = if on { "on" } else { "off" };
    info!("Apple device mode: asking pkexec to turn it {arg}");
    let out = host_command(
        "pkexec",
        &["/bin/sh", "-c", SCRIPT, "bazzitepods-apple-mode", arg],
    )
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
            Some(127) if stderr.contains("authentication agent") => {
                Failure::Other("no password prompt is available on this desktop".into())
            }
            Some(127) => Failure::NotAuthorized,
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
            "bazzitepods-apple-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("main.conf");
        std::fs::write(&f, conf).unwrap();
        let script = SCRIPT
            .replace("f=/etc/bluetooth/main.conf", &format!("f={}", f.display()))
            .replace("mkdir -p /etc/bluetooth", "true")
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
            "[General]\nDeviceID = bluetooth:004C:0000:0000\n#bazzitepods-disabled# DeviceID = bluetooth:1d6b:0246:0540\n"
        );
        assert_eq!(run(&on, "off"), conf);
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

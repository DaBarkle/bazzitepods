# LibrePods for Bazzite: architecture

Goal: a zero-config AirPods companion for Bazzite (Fedora Atomic, KDE Plasma 6 Wayland by default),
shipped as a Flatpak. It autostarts, finds AirPods on its own, shows accurate left/right/case battery,
and keeps audio working across range loss. No user input should ever be required.

## Hard rules

1. **Never touch game processes.** No Vulkan/OpenGL layers, no LD_PRELOAD, no ptrace, no reading other
   processes' memory or files. The overlay is a separate process that draws a normal compositor surface
   (wlr-layer-shell on KWin). This keeps anti-cheat (EAC, BattlEye, VAC) risk at zero.
2. **No panics on runtime data.** Release builds use `panic = "abort"`, so any `unwrap()`/`expect()` on
   Bluetooth packets, D-Bus replies, files or channels kills the app. Use `?`, `let ... else`, or log and continue.
3. **No blocking on the tokio runtime.** libpulse and blocking D-Bus go through `spawn_blocking` with timeouts.
4. **No shelling out** (`bluetoothctl`, `systemctl`, `pactl`): none exist inside the Flatpak sandbox.
5. **Never change the user's Bluetooth power state** (no `set_powered(true)`).
6. **Never leave a bluez card on the `off` profile.** WirePlumber persists it and audio stays dead on reconnect.

## Modules (src/)

| Module | Owner | Purpose |
|---|---|---|
| `core/battery.rs` | battery | Pure battery model: merges AACP + BLE readings per component, source arbitration, stale handling. Unit tested. |
| `core/models.rs` | battery | AirPods model table (model numbers, BLE product IDs) -> `ModelInfo` with capabilities. |
| `core/hub.rs` | lifecycle | `AppSnapshot` published on a `tokio::sync::watch` channel. UI, tray and overlay only read from it. |
| `bluetooth/aacp.rs` | battery | AACP L2CAP protocol. Parsing must be bounds-checked. |
| `bluetooth/le.rs` | battery | BLE proximity adverts -> `BleBatteryReading`. No auto-connect via `bluetoothctl`. |
| `audio/` | audio | `AudioGuard`: per-device state machine that keeps A2DP alive (reconnect bug fix). |
| `media_controller.rs` | audio | MPRIS pause/resume, conversational awareness volume. Never sets the card to `off`. |
| `devices/airpods.rs`, `main.rs` | lifecycle | Device supervisor: one task per device, cancellation on disconnect, AACP retry/backoff, sleep/resume, adapter hotplug. |
| `overlay_client.rs` + `src/bin/librepods-overlay.rs` | overlay | Spawns and feeds the overlay process over stdin (JSON lines). |
| `autostart.rs` | packaging | Requests autostart through the XDG Background portal. |
| `ui/` | ui | Main window and tray, reading `AppSnapshot`. |

## Data flow

```
BlueZ (D-Bus) ──> supervisor (main.rs) ──spawns──> AirPodsDevice task (per MAC)
                                                  ├─ AACPManager (L2CAP PSM 0x1001) ─┐
BLE adverts ──> le.rs ──BleBatteryReading────────────────────────────────────────────┤
                                                  ├─ AudioGuard (libpulse + BlueZ)  ─┤
                                                  ▼                                   ▼
                                        core::hub  watch<AppSnapshot>  <── BatteryTracker
                                          │            │             │
                                       tray (ksni)   UI (iced)   overlay_client ──stdin──> librepods-overlay (layer-shell)
```

## Battery rules (core/battery.rs)

* AACP is authoritative while the AACP link is up. BLE fills in only when AACP is down or has not
  reported a component for more than 60 s.
* A component reported as `Disconnected` (or missing) keeps its last level but is marked `present = false`.
  The UI shows it dimmed, never "0%".
* Level `0x7F`/`0xFF`/>100 is invalid in BLE and is ignored (keeps the last value).
* The case only reports while at least one pod is in it. When both pods are out, the case keeps its last
  value and is marked `stale = true` with the time it was last seen.
* Left/right come from explicit AACP component IDs; BLE uses the primary-left flip bit.

## Overlay protocol (stdin JSON lines, one object per line)

```json
{"type":"config","position":"top-center","margin":12,"auto_hide_secs":4,"mode":"auto"}
{"type":"state","state":{ "name":"David's AirPods","kind":"airpods4","connected":true,
  "left":{"level":80,"charging":false,"present":true,"stale":false},
  "right":{"level":75,"charging":false,"present":true,"stale":false},
  "case":{"level":40,"charging":true,"present":true,"stale":false},
  "headphone":null, "listening_mode":"noise_cancellation" }}
{"type":"show","reason":"connected"}
{"type":"hide"}
{"type":"quit"}
```

`mode`: `auto` (Pop-up: open on changes, then fold away), `compact` (the slim island stays;
it opens on changes), `always` (Full: the card stays), `off`.

`listening_mode` (optional): `off`, `noise_cancellation`, `transparency`, `adaptive`.

What the island shows (`src/bin/librepods-overlay/logic.rs`, `event`): connecting or a pod
coming or going opens the card; the audio moving to or from another device, a battery
dropping to 20% or 10%, charging starting, and a noise control change each show a one-line
alert; disconnecting shows a short goodbye. Shapes move on damped springs
(`spring.rs`, SwiftUI's duration/bounce mapping) and contents cross-fade (`island.rs`).
The overlay redraws only while something moves, paced by the compositor's frame callbacks,
and sleeps on a single timer until the next deadline otherwise.
Exit code 3 from the overlay means no layer-shell (e.g. GNOME); the client then falls back to desktop notifications.

# Kase: architecture

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
| `mic/` | audio | The AirPods mic switch: the AACP 0x58 AAC-ELD mic stream, its FFmpeg decoder (loaded at run time), the virtual "AirPods mic" and the desk/AirPods state machine. See "AirPods mic switch" below. |
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
  "headphone":null, "listening_mode":"noise_cancellation", "mic":"desk" }}
{"type":"show","reason":"connected"}
{"type":"hide"}
{"type":"quit"}
```

`mode`: `auto` (Pop-up: open on changes, then fold away), `compact` (the slim island stays;
it opens on changes), `always` (Full: the card stays), `off`.

`listening_mode` (optional): `off`, `noise_cancellation`, `transparency`, `adaptive`.

`mic` (optional, only while the AirPods mic switch is on): `desk` or `airpods`.
`mic_failures` (optional, left out when 0) counts AirPods mic failures; each new one shows
an *AirPods Mic Unavailable* alert.

What the island shows (`src/bin/librepods-overlay/logic.rs`, `event`): connecting or a pod
coming or going opens the card; the audio moving to or from another device, a battery
dropping to 20% or 10%, charging starting, a mic switch and a noise control change each show a one-line
alert; disconnecting shows a short goodbye. Shapes move on damped springs
(`spring.rs`, SwiftUI's duration/bounce mapping) and contents cross-fade (`island.rs`).
The overlay redraws only while something moves, paced by the compositor's frame callbacks,
and sleeps on a single timer until the next deadline otherwise.
Exit code 3 from the overlay means no layer-shell (e.g. GNOME); the client then falls back to desktop notifications.

## AirPods mic switch (mic/)

Off by default ("AirPods mic switch" in More options). When on, holding the left stem swaps
the PC's mic between the desk mic and the AirPods, and holding the right stem cycles noise
control (ANC models). The presses are claimed with control 0x39 (`gestures::stem_claims`)
and released again when the setting is turned off. AirPods 4 ignore a claim sent during
the connection handshake, so it is sent again 6 s after connecting, after taking the AirPods
back from another device, and whenever they report a different claim (at most every 10 s).

* **Why not the headset profile:** it drops music to phone quality. Apple devices instead
  ask for a separate AAC-ELD mic stream over AACP (opcode 0x58, START/STOP in
  `mic/packets.rs`), found in LibrePods PR #655. Music stays on A2DP AAC.
* **Decode:** `mic/eld.rs` opens the runtime's FFmpeg (`libavcodec.so.58`-`62`) with
  `dlopen` and decodes with its native `aac` decoder; the stream is 64 kHz mono, 480-sample
  frames. The decoder context needs a sample rate set (48 kHz, as the ASC states) or FFmpeg
  rejects every ELD frame. Gain control (`agc.rs`), then s16 into a FIFO (`stream.rs`).
* **Virtual mic:** pipewire-pulse `module-pipe-source` named `bazzitepods_airpods_mic`
  ("AirPods mic"), reading that FIFO from the app's runtime directory.
* **Switching (`switch.rs`):** the desk mic is remembered on disk (`mic_switch.json`, for
  crash recovery), and the virtual mic becomes the default. After 1.5 s, and every 2 s
  after that, every app still recording from another mic is moved to it: apps set to a mic
  by name, and apps on a virtual mic such as EasyEffects' (EasyEffects records the desk mic
  natively, so its own input never shows up as a recording). An app found back on a mic after
being moved is left there rather than moved every sweep. The pulse connection sets
`media.category=Manager`, which the Flatpak needs to change the default mic and move
streams. A stem hold plays a short chime (`chime.rs`, synthesized, played with the
runtime's `paplay`, role `event`): rising for the AirPods mic, falling for the desk mic,
the latter before the output is rebuilt. Going back reverses it in
  order: desk mic default, move those apps back, STOP, unload, restore Conversation Awareness (paused while the mic runs), then
  `AudioGuardHandle::rebuild`.
* **The rebuild is required:** after STOP the right pod goes silent while the transport
  still looks healthy (measured 2026-10-27). One off -> A2DP toggle fixes it.
* **The stream runs only while the AirPods mic is selected,** because the AirPods lower the
  music quality themselves while they record (the PC kept sending full quality).
* **Back to the desk mic on its own** when the AirPods go to another device, a call starts,
  both pods leave the ears, the link drops, or no mic audio arrives for 3 s. It never
  restarts the stream by itself.

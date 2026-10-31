# Testing on real hardware

Everything below runs as your normal user. Nothing needs sudo (or rpm-ostree on image-based systems such as Bazzite).

## 1. Install

Download `Kase.flatpak` from the latest successful **Flatpak (Kase)** run on `main`
(Actions tab → run → Artifacts), unzip it, then:

```bash
curl -fsSL https://raw.githubusercontent.com/DaBarkle/kase/main/linux-rust/packaging/install.sh \
  | bash -s -- ~/Downloads/Kase.flatpak
```

Once a tagged release exists, the same command without the file path installs the latest release.

Uninstall: append `--uninstall` instead of the file path.

**Upgrading from 0.6.** Before installing, note which AirPods are saved and your Island
settings. After installing, log out and back in, then check that:

1. Kase starts on its own at login (the old autostart entry still works).
2. The AirPods, the Island settings and the mic switch setting are all as they were.
3. `ls ~/.var/app/io.github.dabarkle.Kase/data ~/.var/app/io.github.dabarkle.Kase/config`
   shows `kase` folders and no `librepods` ones.

## 2. Smoke test (2 minutes)

1. Connect the AirPods in System Settings → Bluetooth as usual. The tray icon should turn into
   an earbud with a battery badge within ~5 s.
2. Click the tray icon. The window shows Left, Right and Case. Put one pod in the case: that
   pod shows charging and the case shows a value; take both out: the case keeps its last
   value marked "last seen".
3. Settings → Overlay → Preview: the island opens into its card at the chosen position over
   everything for ~4 s, then folds away (Compact: folds into the slim island).
4. Start a fullscreen game (Steam, desktop mode). Take a pod out and back in: the island
   should open over the game and fold back. It must never take mouse clicks or keyboard focus.
5. Change the noise control mode (press and hold a stem, or use the window): a short
   *Noise Cancellation* / *Transparency* alert appears. Put the case on a charger: *Case
   charging* with a green ring.
6. With the island at rest, `top -p $(pgrep -f kase-overlay)` shows 0.0% CPU.

## 3. The walk-away bug

1. Play music to the AirPods (YouTube in a browser is fine).
2. Walk out of range for at least 5 minutes. Music should pause rather than switch to speakers.
3. Walk back and put the AirPods in. Within ~20 s they should reconnect on their own and the
   music should come back through the AirPods when you press play.

If audio stays silent, run this and send the output:

```bash
flatpak run --command=sh io.github.dabarkle.Kase -c 'RUST_LOG=kase=debug kase' 2>&1 | tee ~/kase-debug.log
pactl list cards short; wpctl status
grep bluez ~/.local/state/wireplumber/default-profile
```

## 4. Suspend / resume

Suspend the PC with the AirPods connected, wake it, put the AirPods in. Battery and audio
should come back without touching anything.

## 5. Case battery and multi-device

1. Put one pod in the case and open the lid. The case level shows within a few seconds,
   and is still shown (dimmed) after you take the pod out again.
2. With the AirPods playing on the PC, start something on your iPhone. The window shows
   **On another device** and the overlay pops up. Press **Use on this PC**: the audio
   comes back to the PC within a few seconds.

## 6. Switching with an iPhone

Wear the AirPods with both the PC and the iPhone nearby. Every decision about who gets the
AirPods is one `route:` line in the log, with the reason.

| Do this | Expected |
| --- | --- |
| Play a game on the PC; get a notification on the iPhone | Back on the PC about 3 s after the ding |
| Sit in a Discord voice channel; get a notification | Back about 3 s after the ding |
| Play a video on the iPhone, then stop it (nothing playing on the PC) | Back on the PC about 5 s after stopping |
| Same with **Bring AirPods back to this PC** off | Stays on the iPhone until you press play on the PC |
| Take a phone call while gaming | Stays on the phone for the whole call, then comes back |
| Leave with the AirPods on the iPhone for hours; come home and open the case | Connected to the PC within a minute, sooner if you unlock the PC |
| Let the iPhone take the AirPods, wait for them to reconnect to the PC, then press play on the PC | Back on the PC straight away; the log never says `skipping takeover` while you wear them |
| Press Disconnect on the PC | Stays disconnected, also after restarting Kase, a reboot or suspend |
| Then connect from the Bluetooth settings | Connects; from then on Kase reconnects them as usual again |

## 7. AirPods mic switch (after every runtime or WirePlumber update)

The mic switch depends on the sound server letting Kase act as a mixer, a rule WirePlumber
can change between versions, and on FFmpeg's libavcodec in the Flatpak runtime. Re-test it
whenever the Freedesktop runtime or WirePlumber is updated:

1. Settings → turn on **AirPods mic switch**, AirPods in.
2. Hold the left stem: rising chime, the island shows **AirPods Mic**, and Discord (and
   EasyEffects' mic) follow.
3. Hold it again: falling chime, the desk mic is back.
4. If it fails, look for `access denied` or `libavcodec` in the log.

## 8. Case and reconnect scenarios

What Kase does in each situation, and where. `sup` is `src/supervisor.rs`, `guard` is
`src/audio/guard/`. Each decision is one line in the log.

| Scenario | Expected | Handled by |
| --- | --- | --- |
| Pods into the case, lid closed, reopened within 5 s | Reconnects within a few seconds. The case card shows both pods and the case | Advert on the lid-open edge knocks at once (`sup` `ble_kick_due`); the 8 s retry covers a missed advert (`quick_retry`) |
| Same, reopened after a minute or more | Reconnects within about 8 s of opening, for 15 minutes after the drop; then within a minute | `quick_retry` + `retry_interval` (8 s for 15 min, then 20 s / 60 s) |
| Lid opened right after using the iPhone, iPhone grabs them first | The PC's connect may be reset by the AirPods; it keeps knocking every 8 s and connects once the iPhone lets go | Short session counts as bounced (`RunningDevice::bounced`, `quick_retry`, 3 min) |
| One pod in the case, one in an ear | Stays connected; if it drops, it counts as in use (normal schedule), and the sound goes to the AirPods | `put_away` (in the case and none in an ear) |
| Case closed after an iPhone hand-back, no ear reports reached the PC | Still counts as into the case, from the AirPods' adverts | `devices::airpods::note_put_away` fed by adverts (`battery_service.rs`) |
| Adverts present (Quicker battery updates on, advertisement monitor) | Lid-open knock, case battery while away | `bluetooth/le.rs` `run_monitor` |
| Adverts absent (switch off: discovery only while away; or none reach the adapter) | Falls back to timed knocks; nothing else changes | `run_discovery`, `quick_retry` (ear reports only) |
| Screen locked or idle | No 8 s knocks; the normal schedule, plus a knock the moment you come back | `sleep_monitor::user_present`, `Kick::UserActive` |
| Fullscreen game in front | No 8 s knocks (radio time stays with the game) | `overlay_feed::fullscreen_in_front` |
| Suspend and resume | Sessions restart and the AirPods are wanted back | `Kick::Resumed`, `want_back` |
| 15 minutes in the case | The 8 s knocks stop; 60 s from then on | `QUICK_RECONNECT_PERIOD` |
| Disconnect pressed on the PC | Stays disconnected: no knocks, lid-open nudges, 8 s retries or stay alongside. Remembered across a Kase restart, a reboot and suspend | `disconnect_watch::plan` (`Local` → `Never`); saved to `kept_disconnected.json` by `sup` `settle_kept` once the adapter is still on 3 s later |
| Bluetooth switched off on the PC | Not remembered as a Disconnect (it also drops links as `Local`) | `sup` `settle_kept` (adapter off → not saved) |
| Connected again after a Disconnect (KDE, `bluetoothctl`, or the AirPods dialling in) | Accepted, and the remembered Disconnect is cleared | `sup` `reconcile` → `forget_kept` |
| Unpaired or forgotten after a Disconnect | The remembered Disconnect is cleared, also if it happened while Kase was off | `AdapterEvent::DeviceRemoved` and the startup check → `forget_kept` |
| Out of range | Normal schedule (20 s for 3 min, then 60 s); a knock as soon as an advert is heard again | `retry_interval`, `ble_kick_due` |
| Stay alongside after a case reconnect | Priority list sent once the iPhone shows up, the player first; at most 8 per connection | `devices/airpods/alongside.rs` |
| Reconnect from an iPhone hand-back with pods in the ears | No case card | Overlay only pops the card when the pods are in the case (`kase-overlay` `logic.rs`) |
| AirPods connect with pods out | The sound goes to the AirPods, unless you picked another output during this connection, another device is playing on them, or the default is a virtual output | `guard` `decide_sink` / `claim_default` |

Kase never blocks a connection. An incoming one from the AirPods while a Disconnect is
remembered counts as the user's choice: it connects and the Disconnect is forgotten. This
matches 0.7.0's intent, which was that Kase doesn't pull back AirPods you disconnected,
not that it refuses them.

## Logs

Kase writes its log to `~/.var/app/io.github.dabarkle.Kase/.local/state/kase/kase.log`
(the previous run's is `kase.log.1` once it grows past 5 MB). For more detail, run with
`RUST_LOG=kase=debug` as above.

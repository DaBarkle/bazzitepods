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
| Press Disconnect on the PC | Stays disconnected |

## 7. AirPods mic switch (after every runtime or WirePlumber update)

The mic switch depends on the sound server letting Kase act as a mixer, a rule WirePlumber
can change between versions, and on FFmpeg's libavcodec in the Flatpak runtime. Re-test it
whenever the Freedesktop runtime or WirePlumber is updated:

1. Settings → turn on **AirPods mic switch**, AirPods in.
2. Hold the left stem: rising chime, the island shows **AirPods Mic**, and Discord (and
   EasyEffects' mic) follow.
3. Hold it again: falling chime, the desk mic is back.
4. If it fails, look for `access denied` or `libavcodec` in the log.

## Logs

Kase writes its log to `~/.var/app/io.github.dabarkle.Kase/.local/state/kase/kase.log`
(the previous run's is `kase.log.1` once it grows past 5 MB). For more detail, run with
`RUST_LOG=kase=debug` as above.

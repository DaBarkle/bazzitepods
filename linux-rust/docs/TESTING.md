# Testing on real hardware (Bazzite)

Everything below runs as your normal user. Nothing needs sudo or rpm-ostree.

## 1. Install

Download `Kase.flatpak` from the latest successful **Flatpak (Kase)** run on the
`bazzite` branch (Actions tab → run → Artifacts), unzip it, then:

```bash
curl -fsSL https://raw.githubusercontent.com/DaBarkle/bazzitepods/main/linux-rust/packaging/install-bazzite.sh \
  | bash -s -- ~/Downloads/Kase.flatpak
```

Once a tagged release exists, the same command without the file path installs the latest release.

Uninstall: append `--uninstall` instead of the file path.

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
6. With the island at rest, `top -p $(pgrep -f librepods-overlay)` shows 0.0% CPU.

## 3. The walk-away bug

1. Play music to the AirPods (YouTube in a browser is fine).
2. Walk out of range for at least 5 minutes. Music should pause rather than switch to speakers.
3. Walk back and put the AirPods in. Within ~20 s they should reconnect on their own and the
   music should come back through the AirPods when you press play.

If audio stays silent, run this and send the output:

```bash
flatpak run --command=sh io.github.dabarkle.Kase -c 'RUST_LOG=librepods=debug librepods' 2>&1 | tee ~/librepods-debug.log
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

## Logs

`journalctl --user -b | grep -i librepods` or run with `RUST_LOG=librepods=debug` as above.

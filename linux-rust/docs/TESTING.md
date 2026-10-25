# Testing on real hardware (Bazzite)

Everything below runs as your normal user. Nothing needs sudo or rpm-ostree.

## 1. Install

Download `BazzitePods.flatpak` from the latest successful **Flatpak (BazzitePods)** run on the
`bazzite` branch (Actions tab → run → Artifacts), unzip it, then:

```bash
curl -fsSL https://raw.githubusercontent.com/DaBarkle/bazzitepods/bazzite/linux-rust/packaging/install-bazzite.sh \
  | bash -s -- ~/Downloads/BazzitePods.flatpak
```

Once a tagged release exists, the same command without the file path installs the latest release.

Uninstall: append `--uninstall` instead of the file path.

## 2. Smoke test (2 minutes)

1. Connect the AirPods in System Settings → Bluetooth as usual. The tray icon should turn into
   an earbud with a battery badge within ~5 s.
2. Click the tray icon. The window shows Left, Right and Case. Put one pod in the case: that
   pod shows charging and the case shows a value; take both out: the case keeps its last
   value marked "last seen".
3. Settings → Overlay → Preview: a pill appears top-right over everything for ~4 s and vanishes.
4. Start a fullscreen game (Steam, desktop mode). Take a pod out and back in: the pill should
   appear briefly over the game and disappear. It must never take mouse clicks or keyboard focus.

## 3. The walk-away bug

1. Play music to the AirPods (YouTube in a browser is fine).
2. Walk out of range for at least 5 minutes. Music should pause rather than switch to speakers.
3. Walk back and put the AirPods in. Within ~20 s they should reconnect on their own and the
   music should come back through the AirPods when you press play.

If audio stays silent, run this and send the output:

```bash
flatpak run --command=sh io.github.dabarkle.BazzitePods -c 'RUST_LOG=librepods=debug librepods' 2>&1 | tee ~/librepods-debug.log
pactl list cards short; wpctl status
grep bluez ~/.local/state/wireplumber/default-profile
```

## 4. Suspend / resume

Suspend the PC with the AirPods connected, wake it, put the AirPods in. Battery and audio
should come back without touching anything.

## Logs

`journalctl --user -b | grep -i librepods` or run with `RUST_LOG=librepods=debug` as above.

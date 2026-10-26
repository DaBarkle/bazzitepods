#!/usr/bin/env python3
"""BazzitePods AirPods mic feasibility test (research only, nothing is installed).

Checks, on real hardware, what the planned "roam mic" feature depends on:
  1. Whether the AirPods answer Apple's high-quality mic command (AACP 0x58) and
     stream AAC-ELD audio while music keeps playing over A2DP.
  2. What happens to the music output when that stream starts and stops.
  3. How stem holds (left, right, both) are reported once the hold is claimed.
  4. Whether the hold claim survives a reconnect, and that it can be undone.
  5. The audio setup (default mic, what EasyEffects records from).

It stops BazzitePods while it runs and starts it again at the end, and puts the
stem settings back as they were. It never touches games: it refuses to run while
one is open. Results go to ~/bazzitepods-mictest/. Python standard library only.
"""

import datetime
import glob
import json
import os
import re
import shutil
import signal
import socket
import struct
import subprocess
import sys
import tarfile
import threading
import time

APP_ID = "io.github.dabarkle.BazzitePods"
PSM = 0x1001
HEADER = bytes([0x04, 0x00, 0x04, 0x00])
HANDSHAKE = bytes([0x00, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02, 0x00] + [0x00] * 8)
FEATURE_FLAGS = HEADER + bytes([0x4D, 0x00, 0xFF, 0, 0, 0, 0, 0, 0, 0])
NOTIFICATIONS = HEADER + bytes([0x0F, 0x00, 0xFF, 0xFF, 0xFF, 0xFF])
# From LibrePods PR #655 (bluetooth/aacp.rs): start/stop the hi-res mic stream.
MIC_START = bytes([0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x09, 0x00,
                   0x00, 0x01, 0x82, 0x00, 0x00, 0x00, 0x04, 0x96, 0x00])
MIC_STOP = bytes([0x04, 0x00, 0x04, 0x00, 0x58, 0x00, 0x00, 0x00, 0x02, 0x00, 0x03, 0x01])
STEM_CONFIG = 0x39  # raw gestures bitmask: single 1, double 2, triple 4, long 8

OUT = os.path.expanduser("~/bazzitepods-mictest/run-" + datetime.datetime.now().strftime("%Y%m%d-%H%M%S"))
T0 = time.monotonic()
# Totals across reconnects.
STATS = {"mic_sdus": 0, "mic_bytes": 0, "stem_events": []}


def ts():
    return f"{time.monotonic() - T0:9.3f}"


def say(msg=""):
    print(msg, flush=True)
    note(msg)


def note(msg):
    with open(os.path.join(OUT, "notes.log"), "a") as f:
        f.write(f"{ts()} {msg}\n")


def run(cmd, timeout=10):
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
        return p.stdout + p.stderr
    except Exception as e:  # missing tool, timeout
        return f"<{cmd[0]} failed: {e}>"


def save(name, text):
    with open(os.path.join(OUT, name), "w") as f:
        f.write(text)


def ask(question):
    while True:
        a = input(f"\n>>> {question} [y/n] ").strip().lower()
        if a in ("y", "n"):
            note(f"ASK {question} -> {a}")
            return a == "y"


def wait_enter(prompt):
    input(f"\n>>> {prompt}\n    Press Enter when ready... ")
    note(f"ENTER {prompt}")


# ---------------------------------------------------------------- safety checks

GAME_PATTERNS = [r"SteamLaunch", r"steam-launch-wrapper", r"\bgamescope\b", r"wine(server|64)?\b",
                 r"\.exe\b", r"proton\b", r"pressure-vessel"]


def running_games():
    out = run(["ps", "-eo", "pid,args"])
    hits = []
    for line in out.splitlines()[1:]:
        if "mic-test.py" in line:
            continue
        if any(re.search(p, line) for p in GAME_PATTERNS):
            hits.append(line.strip()[:160])
    return hits


def airpods_mac():
    out = run(["bluetoothctl", "devices", "Connected"])
    for line in out.splitlines():
        m = re.match(r"Device ([0-9A-F:]{17}) (.*)", line.strip())
        if m and "airpods" in m.group(2).lower():
            return m.group(1), m.group(2)
    return None, None


# ------------------------------------------------------------- audio watching

def bluez_card(mac):
    return "bluez_card." + mac.replace(":", "_")


def card_profile(mac):
    out = run(["pactl", "list", "cards"])
    block = out.split("Name: " + bluez_card(mac))
    if len(block) < 2:
        return None
    m = re.search(r"Active Profile: (\S+)", block[1])
    return m.group(1) if m else None


def a2dp_profiles(mac):
    out = run(["pactl", "list", "cards"])
    block = out.split("Name: " + bluez_card(mac))
    if len(block) < 2:
        return []
    profiles = re.findall(r"^\s+(a2dp-sink[\w-]*):", block[1].split("Active Profile")[0], re.M)
    order = ["a2dp-sink-aac", "a2dp-sink-aptx_hd", "a2dp-sink-aptx", "a2dp-sink", "a2dp-sink-sbc_xq", "a2dp-sink-sbc"]
    return sorted(set(profiles), key=lambda p: order.index(p) if p in order else 99)


def sink_state(mac):
    out = run(["pactl", "list", "sinks", "short"])
    key = mac.replace(":", "_")
    for line in out.splitlines():
        if key in line:
            return line.split("\t")[-1].strip()
    return "none"


class AudioWatcher(threading.Thread):
    def __init__(self, mac):
        super().__init__(daemon=True)
        self.mac, self.stop = mac, threading.Event()
        self.last = None

    def run(self):
        with open(os.path.join(OUT, "audio_state.log"), "w") as f:
            while not self.stop.is_set():
                state = (card_profile(self.mac), sink_state(self.mac))
                if state != self.last:
                    f.write(f"{ts()} profile={state[0]} sink={state[1]}\n")
                    f.flush()
                    self.last = state
                time.sleep(0.25)


def reset_a2dp(mac, why):
    """The PR's fix: card profile off, then back to the best A2DP profile."""
    profiles = a2dp_profiles(mac)
    if not profiles:
        note(f"reset_a2dp({why}): no a2dp profile found")
        return
    note(f"reset_a2dp({why}): off -> {profiles[0]}")
    run(["pactl", "set-card-profile", bluez_card(mac), "off"])
    time.sleep(0.4)
    run(["pactl", "set-card-profile", bluez_card(mac), profiles[0]])


def ensure_a2dp(mac):
    prof = card_profile(mac)
    if prof is None or prof.startswith("a2dp"):
        return
    profiles = a2dp_profiles(mac)
    if profiles:
        note(f"ensure_a2dp: {prof} -> {profiles[0]}")
        run(["pactl", "set-card-profile", bluez_card(mac), profiles[0]])


# ----------------------------------------------------------------- AACP link

class Link:
    def __init__(self, mac):
        self.mac = mac
        self.sock = None
        self.reader = None
        self.lock = threading.Lock()
        self.pkt_log = open(os.path.join(OUT, "packets.log"), "a")
        self.sdu_file = open(os.path.join(OUT, "mic_sdus.bin"), "ab")

    def connect(self):
        self.sock = socket.socket(socket.AF_BLUETOOTH, socket.SOCK_SEQPACKET, socket.BTPROTO_L2CAP)
        self.sock.settimeout(10)
        self.sock.connect((self.mac, PSM))
        self.sock.settimeout(0.5)
        self.alive = True
        self.reader = threading.Thread(target=self._read, daemon=True)
        self.reader.start()
        self.send(HANDSHAKE, "handshake")
        time.sleep(0.3)
        self.send(FEATURE_FLAGS, "feature flags")
        time.sleep(0.3)
        self.send(NOTIFICATIONS, "notification request")
        time.sleep(1.5)

    def close(self):
        self.alive = False
        if self.reader:
            self.reader.join(timeout=2)
        try:
            self.sock.close()
        except Exception:
            pass

    def send(self, data, label):
        self.pkt_log.write(f"{ts()} TX {label}: {data.hex()}\n")
        self.pkt_log.flush()
        self.sock.send(data)

    def control(self, ident, value, label):
        v = bytes([ident]) + bytes(value[:4]).ljust(4, b"\x00")
        self.send(HEADER + bytes([0x09, 0x00]) + v, label)

    def _read(self):
        while self.alive:
            try:
                data = self.sock.recv(4096)
            except socket.timeout:
                continue
            except OSError as e:
                self.pkt_log.write(f"{ts()} RX error {e}\n")
                break
            if not data:
                break
            is_audio = (len(data) >= 8 and data[0] == 0x04 and data[2] == 0x04
                        and data[4:6] == b"\x58\x00" and data[6:8] == b"\x01\x00")
            if is_audio:
                with self.lock:
                    STATS["mic_sdus"] += 1
                    STATS["mic_bytes"] += len(data)
                    n = STATS["mic_sdus"]
                self.sdu_file.write(struct.pack("<dI", time.monotonic() - T0, len(data)) + data)
                if n <= 20 or n % 200 == 0:
                    self.pkt_log.write(f"{ts()} RX audio#{n} len={len(data)} head={data[:40].hex()}\n")
                continue
            if len(data) >= 5 and data[4] == 0x19:
                with self.lock:
                    STATS["stem_events"].append((round(time.monotonic() - T0, 3), data.hex()))
                print(f"    (stem event: {data.hex()})", flush=True)
            self.pkt_log.write(f"{ts()} RX {data.hex()}\n")
            self.pkt_log.flush()


# ------------------------------------------------------------------ the test

def stem_control_setting():
    for path in glob.glob(os.path.expanduser(f"~/.var/app/{APP_ID}/config/**/*.json"), recursive=True):
        try:
            with open(path) as f:
                data = json.load(f)
        except Exception:
            continue
        if isinstance(data, dict) and "stem_control" in data:
            return bool(data["stem_control"]), path
    return False, None


def bazzitepods_running():
    return APP_ID in run(["flatpak", "ps", "--columns=application"])


def restart_bazzitepods():
    if bazzitepods_running():
        return
    subprocess.Popen(["flatpak", "run", APP_ID, "--start-minimized"], stdout=subprocess.DEVNULL,
                     stderr=subprocess.DEVNULL, start_new_session=True)


def snapshot(prefix):
    save(f"{prefix}_pactl_info.txt", run(["pactl", "info"]))
    save(f"{prefix}_cards.txt", run(["pactl", "list", "cards"]))
    save(f"{prefix}_sources.txt", run(["pactl", "list", "sources", "short"]))
    save(f"{prefix}_source_outputs.txt", run(["pactl", "list", "source-outputs"]))
    save(f"{prefix}_wpctl.txt", run(["wpctl", "status"]))


def main():
    os.makedirs(OUT, exist_ok=True)
    say("BazzitePods AirPods mic test")
    say(f"Results folder: {OUT}\n")

    games = running_games()
    if games:
        say("A game (or Wine/Proton) seems to be running, so this test will not start:")
        for g in games:
            say("   " + g)
        say("Close it and run the test again.")
        return 1

    mac, name = airpods_mac()
    if not mac:
        say("No connected AirPods found. Put them in your ears, connect them, and run again.")
        return 1
    say(f"AirPods: {name} ({mac})")
    stem_on, settings_path = stem_control_setting()
    note(f"stem_control={stem_on} settings={settings_path}")
    was_running = bazzitepods_running()
    note(f"bazzitepods_running={was_running}")

    snapshot("before")
    save("easyeffects.txt", "\n".join([
        run(["flatpak", "list", "--columns=application"]),
        run(["gsettings", "get", "com.github.wwmm.easyeffects.streaminputs", "use-default-input-device"]),
        run(["flatpak", "run", "--command=gsettings", "com.github.wwmm.easyeffects", "get",
             "com.github.wwmm.easyeffects.streaminputs", "use-default-input-device"], timeout=20),
        run(["pactl", "get-default-source"]),
    ]))

    wait_enter("Put both AirPods in your ears and start some music (Plex or Brave) playing on them.")

    link = None
    watcher = AudioWatcher(mac)
    restore_claim = 0x06 if stem_on else 0x00
    try:
        if was_running:
            say("Pausing BazzitePods for the test (it comes back at the end)...")
            run(["flatpak", "kill", APP_ID])
            time.sleep(2)
        watcher.start()
        link = Link(mac)
        link.connect()
        say("Connected to the AirPods control channel.")

        # ---- 1. high-quality mic stream
        wait_enter("TEST 1 (mic): after Enter, talk normally for about 12 seconds "
                   "(count slowly, or read something aloud). Keep the music playing.")
        link.send(MIC_START, "MIC START")
        t_start = time.monotonic()
        time.sleep(4)
        say(f"    mic packets so far: {STATS['mic_sdus']}")
        playing = ask("Is the music still playing in your AirPods right now?")
        if not playing:
            reset_a2dp(mac, "after start")
            time.sleep(2.5)
            playing = ask("After a short reset: is the music playing again?")
        while time.monotonic() - t_start < 14:
            time.sleep(0.2)
        link.send(MIC_STOP, "MIC STOP")
        time.sleep(1.5)
        say(f"    mic packets received: {STATS['mic_sdus']} ({STATS['mic_bytes']} bytes)")
        playing = ask("Mic stopped. Is the music still playing?")
        if not playing:
            reset_a2dp(mac, "after stop")
            time.sleep(2.5)
            ask("After a short reset: is the music playing again?")

        # ---- 2. stem holds
        link.control(STEM_CONFIG, [0x08], "claim long press")
        time.sleep(0.5)
        say("\nTEST 2 (stems): the press-and-hold now goes to the PC instead of the AirPods.")
        wait_enter("Hold the LEFT stem for about 1 second, let go, then press Enter.")
        wait_enter("Hold the RIGHT stem for about 1 second, let go, then press Enter.")
        wait_enter("Hold BOTH stems at the same time for about 1 second, let go, then press Enter.")
        wait_enter("Once more: hold BOTH stems together, let go, then press Enter.")
        ask("During those holds, did the noise mode change (ANC/Transparency chime)?")

        # ---- 3. does the claim survive a reconnect?
        link.close()
        time.sleep(1.5)
        link = Link(mac)
        link.connect()
        wait_enter("TEST 3: reconnected without re-claiming. Hold the LEFT stem for 1 second, "
                   "let go, then press Enter.")
        ask("Did the noise mode change this time?")
        link.control(STEM_CONFIG, [restore_claim], "restore stem config")
        time.sleep(0.5)
        wait_enter("Stem settings restored. Hold the LEFT stem once more, let go, then press Enter.")
        ask("Did the noise mode change this time?")
    except KeyboardInterrupt:
        say("\nStopped early.")
    except Exception as e:
        say(f"\nThe test hit an error: {e!r}")
    finally:
        if link and link.sock:
            try:
                link.send(MIC_STOP, "MIC STOP (cleanup)")
                link.control(STEM_CONFIG, [restore_claim], "restore stem config (cleanup)")
                time.sleep(0.5)
            except Exception:
                pass
            link.close()
        ensure_a2dp(mac)
        watcher.stop.set()
        time.sleep(0.5)
        snapshot("after")
        with open(os.path.join(OUT, "summary.json"), "w") as f:
            json.dump({"airpods": name, "mac": mac, "stem_control": stem_on, **STATS}, f, indent=1)
        if was_running:
            restart_bazzitepods()
            say("BazzitePods started again.")
        archive = OUT + ".tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(OUT, arcname=os.path.basename(OUT))
        say(f"\nDone. Results saved to {archive}")
        say("Tell Claude in the thread that the test is finished.")
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    sys.exit(main())

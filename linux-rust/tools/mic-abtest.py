#!/usr/bin/env python3
"""Kase music-quality A/B test for the AirPods mic stream (research only).

Plays your music through the AirPods while the high-quality mic stream (AACP 0x58)
is switched off, on, off, on, and logs what PipeWire's AAC encoder does in each
phase. PipeWire lowers the AAC bitrate (by a third, every half second, down to
64 kbps) whenever the Bluetooth link can't keep up, and only logs that at debug
level, so WirePlumber's log level is raised to debug for the test and restored
afterwards.

Needs mic-test.py in the same folder. Stops Kase while it runs and starts it
again at the end. Refuses to run while a game is open. Results go to
~/kase-mictest/abtest-*/.
"""

import importlib.util
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("mictest", os.path.join(HERE, "mic-test.py"))
mt = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mt)

mt.OUT = mt.OUT.replace("/run-", "/abtest-")
PHASE_SECONDS = 25


def journal_since(since):
    return mt.run(["journalctl", "--user", "--since", since, "-o", "short-precise", "--no-pager"], timeout=30)


def main():
    os.makedirs(mt.OUT, exist_ok=True)
    mt.say("Kase music-quality A/B test")
    mt.say(f"Results folder: {mt.OUT}\n")

    games = mt.running_games()
    if games:
        mt.say("A game (or Wine/Proton) seems to be running, so this test will not start:")
        for g in games:
            mt.say("   " + g)
        return 1
    mac, name = mt.airpods_mac()
    if not mac:
        mt.say("No connected AirPods found. Put them in your ears, connect them, and run again.")
        return 1
    mt.say(f"AirPods: {name} ({mac})")
    was_running = mt.kase_running()
    mt.snapshot("before")

    mt.wait_enter("Put both AirPods in your ears and start some music you know well "
                  "(Plex or Brave). Keep it playing for the whole test, and stay quiet.")

    since = time.strftime("%Y-%m-%d %H:%M:%S")
    mt.note(f"journal since {since}")
    link = None
    watcher = mt.AudioWatcher(mac)
    try:
        if was_running:
            mt.say("Pausing Kase for the test (it comes back at the end)...")
            mt.run(["flatpak", "kill", mt.APP_ID])
            time.sleep(2)
        mt.note("wpctl set-log-level D: " + mt.run(["wpctl", "set-log-level", "D"]))
        watcher.start()
        link = mt.Link(mac)
        link.connect()

        phases = [("OFF", False), ("ON", True), ("OFF", False), ("ON", True)]
        for i, (label, on) in enumerate(phases, 1):
            if on:
                link.send(mt.MIC_START, f"MIC START (phase {i})")
            else:
                if i > 1:
                    link.send(mt.MIC_STOP, f"MIC STOP (phase {i})")
            mt.say(f"\nPhase {i} of {len(phases)}: mic {label}. Just listen for {PHASE_SECONDS} seconds...")
            mt.note(f"PHASE {i} mic {label} begins")
            time.sleep(PHASE_SECONDS)
            mt.note(f"PHASE {i} mic {label} ends")
            if not mt.ask(f"Phase {i}: is the music still playing?"):
                mt.reset_a2dp(mac, f"phase {i}")
                time.sleep(2.5)
                mt.ask("After a short reset: is the music playing again?")
            if i > 1:
                mt.ask(f"Phase {i}: did it sound worse than phase {i - 1}?")
        link.send(mt.MIC_STOP, "MIC STOP (end)")
        time.sleep(2)
        if not mt.ask("Mic off for good now. Is the music still playing?"):
            mt.reset_a2dp(mac, "end")
            time.sleep(2.5)
            mt.ask("After a short reset: is the music playing again?")
    except KeyboardInterrupt:
        mt.say("\nStopped early.")
    except Exception as e:
        mt.say(f"\nThe test hit an error: {e!r}")
    finally:
        if link and link.sock:
            try:
                link.send(mt.MIC_STOP, "MIC STOP (cleanup)")
                time.sleep(0.5)
            except Exception:
                pass
            link.close()
        mt.note("wpctl set-log-level -: " + mt.run(["wpctl", "set-log-level", "-"]))
        mt.ensure_a2dp(mac)
        watcher.stop.set()
        time.sleep(0.5)
        mt.snapshot("after")
        mt.save("journal.txt", journal_since(since))
        mt.save("bitrate.txt", "\n".join(
            l for l in journal_since(since).splitlines()
            if any(k in l for k in ("bitpool", "AAC:", "bitrate", "fail flush", "transport"))))
        if was_running:
            mt.restart_kase()
            mt.say("Kase started again.")
        archive = mt.OUT + ".tar.gz"
        import tarfile
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(mt.OUT, arcname=os.path.basename(mt.OUT))
        mt.say(f"\nDone. Results saved to {archive}")
        mt.say("Tell Claude in the thread that the test is finished.")
    return 0


if __name__ == "__main__":
    sys.exit(main())

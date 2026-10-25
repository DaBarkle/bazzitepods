# Changelog

All notable changes to BazzitePods are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.2.2] - 2026-10-25

### Added
- The overlay names the device that took the AirPods ("Playing on iPhone") and says
  "Back on this PC" when they return. The window's banner names the device too. ([#6])

### Fixed
- Pressing play on the PC while the iPhone is still playing no longer pauses the PC
  again: reports from before the switch are ignored for a moment. ([#6])
- A phone call on the iPhone pauses the PC right away, and the PC never takes the AirPods
  back during a call. ([#6])
- Notification sounds on the PC no longer count as playback. ([#6])
- Sound now plays straight away when the AirPods come back from the iPhone. Before, the
  AirPods could connect with no sound until the output was changed by hand. Voice calls
  in apps like Discord move back to the AirPods with it. ([#6])
- While the iPhone plays, it keeps the AirPods. The PC no longer takes them back when an
  app here keeps its audio open after pausing (Plex does this), or when its own sound
  check sees the AirPods' output go quiet, or when a player it just paused (Brave)
  briefly reports playing again. ([#6])
- When the iPhone stops playing, the AirPods come back to the PC by themselves if media
  is still playing here, including media that was already playing before the iPhone
  connected or BazzitePods started. ([#6])
- Choosing the AirPods in the iPhone's Control Center is handled like any other switch:
  the PC takes them back about a second after the iPhone lets go. ([#6])
- Audio no longer plays through the speakers for a moment while the AirPods' output is
  rebuilt after a switch. The speakers are muted for that second.
  ([#6])
- Switching back from an iPhone now works with media apps that do not publish an MPRIS
  player, such as the Plex desktop app. The AirPods return to the PC about a second after
  playback starts. ([#5])

### Changed
- Playback is now also detected from the system's audio streams. Background streams that
  never stop, such as Discord's idle voice connections and EasyEffects' processing stream,
  are ignored. ([#5])
- A failed send right after the AirPods disconnect is now logged at debug level instead of
  as an error. ([#5])

## [0.2.1] - 2026-10-25

### Fixed
- The PC takes the AirPods back about a second after playback starts, instead of 7 to 11
  seconds. ([#4])
- iPhone notifications no longer take the AirPods while the PC is playing. The message
  that tells other Apple devices the PC is playing was malformed and has been corrected.
  ([#3], [#4])
- Short status messages from the iPhone no longer pause playback on the PC. ([#4])
- If a notification does take the AirPods, the PC gets them back as soon as the iPhone
  lets go. ([#3])

## [0.2.0] - 2026-10-25

First release of BazzitePods, a zero-config AirPods app for Bazzite and other Flatpak
desktops, built on [LibrePods](https://github.com/librepods-org/librepods).

### Added
- Battery levels for the left pod, right pod and case, with charging state.
- Game overlay with Compact, Pop-up, Full and Off modes, a liquid-glass look and
  translucency over fullscreen games. It uses no injection and no screen capture.
- Switching between devices: **On another device** status and a **Use on this PC** button.
- Noise control, ear detection, conversation awareness and renaming.
- One-line installer, autostart at login and a tray icon.

### Fixed
- Audio no longer stays silent after the AirPods reconnect ("walked away and came back").

[0.2.2]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/DaBarkle/bazzitepods/releases/tag/v0.2.0
[#3]: https://github.com/DaBarkle/bazzitepods/pull/3
[#4]: https://github.com/DaBarkle/bazzitepods/pull/4
[#5]: https://github.com/DaBarkle/bazzitepods/pull/5
[#6]: https://github.com/DaBarkle/bazzitepods/pull/6

# Changelog

All notable changes to Kase (formerly BazzitePods) are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed
- **Clearer wording for the battery setting.** The switch added in 0.8.2 is now called
  **Quicker battery updates when not connected**, with a plain explanation of what it does,
  and the Diagnostics row reads **Battery when not connected**: *Quick updates* or
  *Standard (updates less often)*. The technical names stay in the log. ([#41])

## [0.8.2] - 2026-10-30

### Added
- **Faster battery while away** switch in Settings → Diagnostics (off by default). It turns
  on Bluetooth's experimental features (`Experimental = true` in
  `/etc/bluetooth/main.conf`) so Kase can use the advertisement monitor instead of fallback
  scanning, with the same password prompt, Bluetooth restart and distribution handling
  (NixOS, SteamOS, no systemd) as Apple device mode. The switch shows what the running
  Bluetooth service really does, the two settings stay independent, and turning it off
  removes only its own line. ([#39])

## [0.8.1] - 2026-10-30

### Added
- **64-bit ARM builds.** Releases include `Kase-aarch64.flatpak` (Raspberry Pi 5, Asahi
  Linux, ARM laptops) next to `Kase-x86_64.flatpak`, and the installer picks the right
  one. `Kase.flatpak` stays the x86_64 bundle, so older install commands keep working. ([#37])

### Changed
- The installer scripts are shellchecked in CI on every pull request. ([#37])

## [0.8.0] - 2026-10-29

### Fixed
- **PCs with two Bluetooth adapters.** Kase now uses the adapter your AirPods are paired
  on, not just the default one, and switches when that adapter is plugged in later. ([#32])
- **Apple device mode beyond Fedora.** On NixOS, Settings gives you the
  `hardware.bluetooth.settings` line to add instead of editing a generated file; systems
  without systemd restart Bluetooth through OpenRC or runit; SteamOS explains that the
  account needs a password first (`passwd`); a missing password prompt (polkit agent) gets
  a clear message. The script no longer relies on GNU-only `sed`. ([#32])
- **Audio on PulseAudio as well as PipeWire.** The audio guard understands PulseAudio's
  profile names (`a2dp_sink`, `headset_head_unit`), and the log says which sound server is
  running. ([#33])
- **Fewer false "something is playing" readings.** JamesDSP, NoiseTorch, loopbacks, filter
  chains and monitor streams no longer count as playback, and Zoom, TeamSpeak, Mumble,
  Slack, Teams, Element, Signal, Telegram, Skype and Webex count as calls, like Discord.
  Streams are recognised by their program and node names, not just the name they show.
  ([#33])
- **The AirPods mic on newer FFmpeg.** libavcodec 63 is accepted, an unknown version says
  so clearly in the log, and a sound server that refuses Kase's mixer rights is logged as
  such instead of a bare failure. ([#33])
- **Coming back to a KDE desktop brings the AirPods back.** KDE never marks the session idle
  in logind, so Kase now also listens for the screen saver ending (KDE and GNOME). ([#34])
- **Fullscreen transparency on Plasma 5.** The KWin script works with Plasma 5's window
  API as well as Plasma 6's. ([#34])
- **The island survives GPU trouble.** If it keeps crashing on start (NVIDIA without a
  matching Flatpak driver, no Vulkan), it tries once more drawing in software before
  falling back to notifications. ([#34])
- **The tray appears as soon as the panel does**, instead of up to 30 seconds later, and
  GNOME users are told once to turn on the AppIndicator extension. ([#34])

### Added
- **Fullscreen transparency on Sway, Hyprland and niri**, from the compositor's window list
  (wlr foreign-toplevel management). ([#34])
- **Start at login on Sway, Hyprland, niri and COSMIC.** Where the desktop can't register
  Kase itself, it tells you once, in a notification and the log, the exact line to add. ([#34])
- **Diagnostics** in Settings: which Bluetooth adapter is in use and whether it's on or
  blocked, AAC or SBC-only (with why), whether battery-while-away uses the advertisement
  monitor or fallback scanning, and a warning for Bluetooth chips known to struggle with
  AirPods (Realtek RTL8761B). ([#32])

### Changed
- **One installer for every distribution.** `install.sh` replaces `install-bazzite.sh`
  (the old link still works). If Flatpak is missing it prints the exact command for your
  distribution (apt, dnf, pacman, zypper, or the NixOS option), and it stops with a clear
  message on CPUs without a Kase build. ([#31])
- **AirPods for Linux.** The README, app metadata and docs are no longer Bazzite-only: a
  compatibility table per desktop (Plasma 5 and 6, GNOME, X11, Sway and Hyprland), AAC
  availability per distribution, a troubleshooting section for NVIDIA, older GPUs and
  dual-GPU laptops, and build packages for Fedora, Arch and Ubuntu. ([#31])
- The installer scripts are checked with shellcheck (`just lint-packaging`). ([#31])
- **The last "BazzitePods" names are gone.** The virtual mic is now `kase_airpods_mic`, and
  Apple device mode keeps its backup at `main.conf.bak-kase`. Leftovers from older builds
  (the old virtual mic, pipe and parked DeviceID line) are still found, cleaned up and
  undone. ([#35])

## [0.7.0] - 2026-10-29

### Fixed
- **AirPods come back after a day away.** When the iPhone took them, Kase tried for only
  3 minutes and gave up after 4 hours, so after work they didn't reconnect until you
  restarted Kase. It now keeps trying (every minute) until they're back. ([#30])
- **A phone notification no longer steals the sound while you game.** A notification only
  borrows the AirPods; they're back on the PC about 3 seconds later whenever something
  plays here, games included, or you're in a Discord call. ([#30])
- **No tug of war.** If you move the AirPods back to the iPhone, or the two keep swapping,
  Kase stops taking them back for two minutes. ([#30])
- **Quitting puts your audio back.** Tray Quit and logging out now restore the desk mic,
  a volume lowered by a conversation, and speakers muted mid-repair before Kase exits.
  After a crash, the next start unmutes anything left muted. ([#24])
- **Settings survive a power cut.** Saved files are flushed to disk before they replace
  the old ones, so a sudden power-off can no longer wipe your settings or saved AirPods. ([#24])
- **The island matches the Settings page.** On a fresh install it drew at full opacity
  over games while the slider showed 60%; it now follows your settings live. ([#24])
- **Disconnecting from KDE stays disconnected** even after Bluetooth or D-Bus restarts. ([#24])
- **Audio keeps recovering** when the AirPods' sound card drops out several times in a
  row, and two audio repairs no longer run at once after a quick reconnect. ([#24])
- **The case no longer shows 0%** after an out-of-range battery reading, an unknown
  ear reading no longer pauses your music, and a dead connection now reconnects. ([#24])
- **Conversation volume** no longer silences the AirPods when it can't read the volume,
  and is restored if the link drops mid-conversation. ([#24])
- If the island crashes straight away (for example, no Vulkan), Kase now falls back to
  notifications instead of restarting it forever. ([#24])

### Added
- **Your PC is home.** A few seconds after your iPhone stops playing, the AirPods come back
  to the PC by themselves, even from another room. Turn off **Bring AirPods back to this
  PC** in Settings to keep the Mac behaviour. ([#30])
- **Reconnects the moment you're back.** Unlocking the PC, or using it after it went idle,
  looks for the AirPods straight away. ([#30])
- **A log file** at `~/.var/app/io.github.dabarkle.Kase/.local/state/kase/kase.log`, with
  one line explaining each decision about who gets the AirPods. ([#30])

### Changed
- **A leaner, tidier codebase.** Everything Kase doesn't use from LibrePods is gone:
  the Android app, the old C++ Linux app, the Magisk module, Nix files and Nothing Ear
  support. The Rust code is now a library with smaller, focused modules, and CI also
  checks formatting and lints. ([#25], [#26])
- **The programs are now `kase` and `kase-overlay`,** and their data and config folders
  are `kase`. Your existing settings and saved AirPods move across by themselves on first
  start, and autostart entries from 0.6 keep working. ([#27])

## [0.6.0] - 2026-10-28

### Changed
- **BazzitePods is now Kase**, "AirPods for KDE Plasma". The new name says who the app is
  for without tying it to one distro: it works on any KDE Plasma desktop, Bazzite
  included. The app id is now `io.github.dabarkle.Kase`, the release bundle is
  `Kase.flatpak`, and the repository is now `DaBarkle/kase` (old links redirect). ([#23])
- **Updating moves you across by itself.** Run the install command as usual: it copies
  your settings, saved AirPods and Flatpak overrides from BazzitePods to Kase, then
  removes BazzitePods and its old autostart entry. Apple device mode is a system setting
  and stays as it is. ([#23])
- **A new README,** rewritten from scratch with new screenshots and animations of the
  current app. ([#23])

### Notes
- Nothing else about the app changes in this release: same features, same settings.

## [0.5.0] - 2026-10-28

### Added
- **Purist island style, now the default.** The iPhone's Dynamic Island as it really is: solid flat black with a hairline edge, tinted to match the alert. When
  something happens, light runs round the border in that event's colour: green for
  charging, a slow amber or red breath for low battery, blue when the audio moves to or
  from another device, orange when you swap to the AirPods mic, and a flowing spectrum when
  the AirPods connect. Each glow lasts under two seconds, then the island is still again
  and draws nothing. ([#20])
- **Island size.** Small, Medium, Large or Extra large scales the whole island, for big or
  high-resolution screens. ([#20])
- In the purist style the island gives a quick head shake when the AirPods mic can't be
  used, and its outline moves like liquid as it stretches. ([#20])
- **A sound when the mic swaps.** Holding the left stem now plays a soft rising chime when
  the AirPods mic turns on and a falling one when the desk mic comes back, so you know which
  mic you're on without looking. ([#21])

### Changed
- **Redesigned app window:** pure black like the island, three tabs (AirPods, Island,
  Settings), a glowing status island, battery rings and selections that move on springs,
  and a preview screen for placing the overlay. ([#19])
- **Apple device mode is now one switch.** Flip it in the Settings tab, confirm, and enter
  your password in the system prompt; BazzitePods makes the change and restarts Bluetooth
  for you, then shows when it's on. Turning it off restores your original Bluetooth
  settings. No terminal needed. If the app can't ask for your password (no `pkexec`), it
  still offers the command to copy. ([#18])

### Fixed
- **The left stem hold now swaps the mic on AirPods 4.** The AirPods ignored the request to
  hand stem holds to the PC when it was sent while connecting, so holding the stem changed
  noise control instead. BazzitePods now sends it again once the AirPods have settled, after
  taking them back from another device, and whenever the AirPods reset it. ([#21])
- **The AirPods mic sends sound.** Its audio wasn't decoded with the FFmpeg in the Flatpak,
  so apps recorded silence. ([#21])
- **The swap works inside the Flatpak.** The sound server refused to let BazzitePods change
  the default mic or move apps between mics; it now identifies itself as an audio manager,
  like EasyEffects and Discord do. This also fixes moving music back to the AirPods after a
  reconnect. ([#21])
- **Discord now moves to the AirPods mic.** Swapping to the AirPods mic only moved apps
  that were recording from your default mic, so apps set to a mic by name, or using
  EasyEffects' mic (*Easy Effects Source*), stayed on the desk mic. Every app recording from
  a mic is now moved to the AirPods mic, and back to the mic it was using when you swap
  back. ([#17])
- **Apps that open the mic after the swap move too.** Discord only opens the mic when you
  join a voice channel; while the AirPods mic is on, such apps are now moved across within
  two seconds. ([#17])
- An app that keeps going back to the desk mic is left there instead of being moved back
  and forth every two seconds. ([#21])
- **Swapping back to the desk mic no longer drops your sound onto the speakers.** The
  AirPods cut their music connection when their mic stops; the speakers are now muted
  before that happens and stay muted until the AirPods play again, and if they don't
  start playing, BazzitePods repairs the connection straight away instead of after a few
  seconds of silence. ([#22])
- **The switch chime is heard even when system notification sounds are muted.** ([#22])
- **Music no longer gets stuck on a lower-quality codec.** When the AirPods refused to
  restart AAC after a mic swap, the repair fell back to SBC-XQ and stayed there, even
  across reconnects. A refused restart now switches codec once to get the sound back
  within about two seconds (instead of a second, slower reset), and BazzitePods
  returns to AAC as soon as nothing is playing. ([#22])
- **Battery and connection status can't silently stop updating any more.** After a
  reconnect, the AirPods' control connection could stop responding while music kept
  playing, so the app showed them disconnected with no battery. BazzitePods now notices
  when that connection stalls and reopens it on its own within about 20 seconds, and
  sending to the AirPods no longer blocks their battery and ear reports. ([#22])
- The speakers are never left muted if BazzitePods stops in the middle of a switch,
  for example when the PC goes to sleep. ([#22])

### Notes
- *Purist* is the new default. If you had never changed the overlay's look, it switches to
  Purist; pick *Classic* in the Island tab to get the glass island back exactly as before.
  The Glass slider only applies to Classic; *Opacity over games* applies to both.
- The purist look is drawn by a small GPU shader on the overlay's own surface. It runs only
  while the island moves, so a still island still costs nothing, and it never touches games.
- Apps on EasyEffects' mic skip EasyEffects while the AirPods mic is in use (EasyEffects
  keeps processing the desk mic). The AirPods do their own noise reduction.

## [0.4.0] - 2026-10-27

### Added
- **Use your AirPods as the PC's mic without losing music quality.** Turn on *AirPods mic
  switch* in More options, then hold the **left stem** to swap between your desk mic and the
  AirPods' mic, and hold it again to swap back. Music keeps playing in full quality instead
  of dropping to phone-call sound, because the AirPods send their mic over a separate
  high-quality stream, the way they do for an iPhone or Mac. ([#15])
- **Hold the right stem to change noise control** (models with noise cancellation), cycling
  through the modes enabled for the AirPods. ([#15])
- The overlay shows *Desk Mic* or *AirPods Mic* when you swap, and *AirPods Mic Unavailable*
  if the AirPods mic stops. The window and tray have a *Use AirPods mic* switch too. ([#15])

### Changed
- Apps follow the swap on their own: the AirPods mic becomes the default while selected,
  and apps that were set to the desk mic (Discord, for example) are moved across and back.
- Turning stem control on or off now applies at once instead of on the next reconnect.

### Notes
- The PC goes back to the desk mic on its own when the AirPods go to another device, a call
  starts, both AirPods come out of your ears, or the AirPods stop sending mic audio.
- While the AirPods mic is in use, the AirPods themselves lower the music quality slightly.
  This happens inside the AirPods (the PC keeps sending full quality), which is why the
  stream only runs while the AirPods mic is selected.
- While the switch is on, holding a stem is taken by BazzitePods on every device, so on an
  iPhone a hold won't change noise control until the switch is turned off.

## [0.3.2] - 2026-10-27

### Fixed
- **AirPods reconnect when you come home.** If you walked out of range wearing them, the PC
  only reconnected when it happened to recognise their Bluetooth adverts, and stopped
  trying after 4 hours, so after a workday they never came back. Now they are reconnected
  however long you were away, the same as after putting them in the case. ([#16])
- Disconnecting the AirPods on this PC (from the KDE Bluetooth menu, for example) now keeps
  them disconnected for sure: BlueZ says why each link dropped, and the app only stays away
  when the drop was yours. ([#16])

## [0.3.1] - 2026-10-27

### Added
- **More of your AirPods' settings**, read from the AirPods themselves: automatic ear
  detection, Adaptive Audio strength, noise cancellation with one AirPod, which AirPod's
  microphone to use, and whether to connect to this PC automatically. Each appears only when
  your model reports it. ([#12])
- **"In case".** With both pods put away in a closed case, the window and tray say *In case*
  instead of *Not connected*, and the case's last reading carries the hint *open the lid to
  update*. ([#12])

### Fixed
- **Charging now shows when the AirPods are away from this PC.** Readings broadcast by the
  AirPods (for example pods charging in an open case nearby) were greyed out and their
  charging bolt hidden whenever the AirPods weren't connected here. Fresh readings are now
  shown live, charging included. ([#12])
- When the AirPods disconnect, their last levels turn into *last seen* at once instead of
  looking live for up to a minute. ([#12])
- **Personalized Volume shows its real state.** The switch used to start Off after every
  launch whatever the AirPods were set to. ([#12])

## [0.3.0] - 2026-10-27

### Added
- **The overlay is now an island**, modelled on the iPhone's Dynamic Island. It opens into a
  card with a ring for each pod and the case, rests as a slim island with all three levels,
  and turns into a short alert that names what just happened: *Case charging*, *Right AirPod
  low*, *Playing on iPhone*, *Back on this PC* and *Disconnected*. ([#11])
- **Noise control alerts.** Changing between Noise Cancellation, Transparency, Adaptive Audio
  and Off (on the AirPods or in the app) shows the new mode, like on an iPhone. ([#11])
- **Spring animations throughout.** The island grows out of a small pill with a slight
  bounce and folds back without one; contents cross-fade with a gentle scale; rings sweep in
  when the card opens; percentages roll to their new value; in Full mode, news rolls into
  the card's second line with a small bounce. A change that arrives mid-animation carries
  the motion on instead of restarting it. ([#11])

### Changed
- The overlay now sits at the **top center** by default, where the Dynamic Island lives. A
  position you already chose is kept. ([#11])
- **Glass at 0% is now the island's pure black** (was dark smoked glass). ([#11])
- The overlay no longer pops up when charging stops or a level climbs back out of the low
  range; it alerts when charging starts and when a battery drops to 20% or 10%. ([#11])
- **Lower resource use in games.** The overlay redraws only while it moves, in step with the
  compositor's frames (so it is as smooth as your monitor's refresh rate). At rest it draws
  nothing and wakes up only when it has something to do, instead of checking five times a
  second. ([#11])

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
- The PC reconnects to the AirPods by itself after they drop it: within about 20 seconds
  when they made room for an iPhone (so the PC can take them back when the iPhone lets
  go), and when they come out of the case after being put away, as a Mac does, however
  long they were away (also after a reboot or sleep). Before,
  the drop often went unnoticed and the AirPods stayed off the PC until connected by
  hand. ([#6])
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

[0.8.2]: https://github.com/DaBarkle/kase/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/DaBarkle/kase/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/DaBarkle/kase/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/DaBarkle/kase/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/DaBarkle/kase/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/DaBarkle/kase/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/DaBarkle/kase/compare/v0.2.2...v0.4.0
[0.3.0]: https://github.com/DaBarkle/kase/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/DaBarkle/kase/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/DaBarkle/kase/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/DaBarkle/kase/releases/tag/v0.2.0
[#3]: https://github.com/DaBarkle/kase/pull/3
[#4]: https://github.com/DaBarkle/kase/pull/4
[#5]: https://github.com/DaBarkle/kase/pull/5
[#6]: https://github.com/DaBarkle/kase/pull/6
[#21]: https://github.com/DaBarkle/kase/pull/21
[#18]: https://github.com/DaBarkle/kase/pull/18
[#22]: https://github.com/DaBarkle/kase/pull/22
[#17]: https://github.com/DaBarkle/kase/pull/17
[#16]: https://github.com/DaBarkle/kase/pull/16
[#19]: https://github.com/DaBarkle/kase/pull/19
[#20]: https://github.com/DaBarkle/kase/pull/20
[#15]: https://github.com/DaBarkle/kase/pull/15
[#12]: https://github.com/DaBarkle/kase/pull/12
[#11]: https://github.com/DaBarkle/kase/pull/11
[#23]: https://github.com/DaBarkle/kase/pull/23
[#24]: https://github.com/DaBarkle/kase/pull/24
[#25]: https://github.com/DaBarkle/kase/pull/25
[#26]: https://github.com/DaBarkle/kase/pull/26
[#27]: https://github.com/DaBarkle/kase/pull/27
[#30]: https://github.com/DaBarkle/kase/pull/30
[#31]: https://github.com/DaBarkle/kase/pull/31
[#32]: https://github.com/DaBarkle/kase/pull/32
[#33]: https://github.com/DaBarkle/kase/pull/33
[#34]: https://github.com/DaBarkle/kase/pull/34
[#35]: https://github.com/DaBarkle/kase/pull/35
[#37]: https://github.com/DaBarkle/kase/pull/37
[#39]: https://github.com/DaBarkle/kase/pull/39
[#41]: https://github.com/DaBarkle/kase/pull/41

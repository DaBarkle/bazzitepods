# Changelog

All notable changes to BazzitePods are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.5.0]: https://github.com/DaBarkle/bazzitepods/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.2...v0.4.0
[0.3.0]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/DaBarkle/bazzitepods/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/DaBarkle/bazzitepods/releases/tag/v0.2.0
[#3]: https://github.com/DaBarkle/bazzitepods/pull/3
[#4]: https://github.com/DaBarkle/bazzitepods/pull/4
[#5]: https://github.com/DaBarkle/bazzitepods/pull/5
[#6]: https://github.com/DaBarkle/bazzitepods/pull/6
[#21]: https://github.com/DaBarkle/bazzitepods/pull/21
[#18]: https://github.com/DaBarkle/bazzitepods/pull/18
[#17]: https://github.com/DaBarkle/bazzitepods/pull/17
[#16]: https://github.com/DaBarkle/bazzitepods/pull/16
[#19]: https://github.com/DaBarkle/bazzitepods/pull/19
[#20]: https://github.com/DaBarkle/bazzitepods/pull/20
[#15]: https://github.com/DaBarkle/bazzitepods/pull/15
[#12]: https://github.com/DaBarkle/bazzitepods/pull/12
[#11]: https://github.com/DaBarkle/bazzitepods/pull/11

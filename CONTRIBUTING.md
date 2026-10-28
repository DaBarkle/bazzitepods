# Contributing to Kase

Thanks for helping improve Kase. Please read the [Code of Conduct](CODE_OF_CONDUCT.md) first.

## Issues

Search the [existing issues](https://github.com/DaBarkle/kase/issues) before opening a new
one. For a bug, say which AirPods you have, your distribution and desktop, the Kase version,
and attach a debug log if you can:
`flatpak run io.github.dabarkle.Kase --debug 2>&1 | tee ~/kase-debug.log`.

## Making changes

The app lives in [`linux-rust/`](linux-rust/). Read
[`ARCHITECTURE.md`](linux-rust/docs/ARCHITECTURE.md) before you start: its hard rules (never
touch games, never break the audio route) apply to every change.

```bash
git clone https://github.com/DaBarkle/kase.git
cd kase/linux-rust
cargo test --all-targets --locked   # needs dbus, libpulse, xkbcommon, wayland and libavcodec headers
just flatpak                        # build and install the Flatpak locally
```

- Add tests next to the module you change (`*_tests.rs`). CI fails if line coverage drops
  below the floor in [`tests.yml`](.github/workflows/tests.yml).
- Run `cargo fmt` and `cargo clippy --all-targets` before pushing.
- After changing `Cargo.lock`, run `just flatpak-sources` so the offline Flatpak build still
  works.
- Hardware behaviour (Bluetooth, audio, the overlay) can't be covered by CI; say in the pull
  request what you tested on real AirPods, following [`TESTING.md`](linux-rust/docs/TESTING.md).

Protocol notes inherited from LibrePods are in
[`linux-rust/docs/protocol/`](linux-rust/docs/protocol/).

## Pull requests

Open the pull request against `main` and describe what changed and why. The "Run tests" and
"Build Flatpak bundle" checks must pass before it can merge.

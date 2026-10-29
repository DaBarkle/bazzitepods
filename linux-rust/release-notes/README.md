# Release notes

One file per release, named after its tag (`v0.7.0.md`). The first line is the release
title (`# Kase 0.7.0`); the rest is the body of the GitHub release.

To publish a release, open a pull request that:

1. bumps `version` in `linux-rust/Cargo.toml` (and `Cargo.lock`),
2. adds a `<release>` entry to `flatpak/io.github.dabarkle.Kase.metainfo.xml`,
3. moves the `[Unreleased]` section of `CHANGELOG.md` under the new version, and
4. adds the notes here as `vX.Y.Z.md`.

When it merges, the Release workflow (`.github/workflows/release.yml`) builds `Kase.flatpak`
and publishes the tag and the release with these notes and the bundle attached. Nobody has
to create the release by hand.

# Changelog

Notable changes to Splitlane are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.3] - 2026-09-28

### Fixed

- Codex and Claude Code no longer show as running before their first
  message. Codex 0.158 animates its idle prompt and repaints on focus, and
  that output was read as work; for agents whose state Splitlane reads from
  their own records, terminal output no longer counts as running.

### Added

- Every package now ships `THIRD_PARTY_LICENSES.md`, the license texts of
  all Rust crates compiled into Splitlane. The macOS app bundle also carries
  `LICENSE`.

### Documentation

- The README and `THIRD_PARTY_NOTICES.md` credit Zed Industries for GPUI and
  list the code adapted from Zed's crates, with their licenses.

## [0.1.2] - 2026-09-28

### Security

- `"ai_unrestricted": true` in `splitlane.json` no longer opens `send` and
  `key` by itself. The file is reloaded while Splitlane runs and agents can
  edit it, so an agent could grant itself write access to other panes. The
  key is now a request: Free access opens only when you turn it on in
  Settings -> Agents, once per launch. Setting the key to `false` still
  closes access at once. If you had Free access on, Splitlane tells you after
  the upgrade and one click in Settings turns it back on.

## [0.1.1] - 2026-09-28

### Fixed

- A freshly started Codex, or any agent without hooks, no longer shows as
  running with a Stop button until its first message. It now settles to
  idle once its terminal goes quiet.
- The main window has a title, so Linux taskbars and Alt-Tab show
  "Splitlane" instead of "Unnamed Window".

### Documentation

- The Install section and `keys/README.md` explain how to verify downloads,
  including the minisign public key, and cover the tarball, rpm key import
  for zypper, and the minimum glibc version.

## [0.1.0] - 2026-09-27

First release with builds: a signed and notarized macOS app for Apple
Silicon, and signed `.deb`/`.rpm` packages, an AppImage and a tarball for
Linux on x86_64 and aarch64. Windows is built from source for now. See
[Install](README.md#install).

[Unreleased]: https://github.com/ivkan/splitlane/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/ivkan/splitlane/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ivkan/splitlane/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ivkan/splitlane/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ivkan/splitlane/releases/tag/v0.1.0

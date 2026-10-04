# Changelog

Notable changes to Splitlane are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.4] - 2026-10-04

### Changed

- A worktree Splitlane created is now removed when its project closes, as
  the "Remove it when the project closes" choice always said. Until now it
  never was. It is removed only when it holds nothing but committed files:
  an ignored file, an untracked file or an edited `.env` keeps it, because
  git would delete those without asking. The branch is never deleted.
- `workspace.close` and `workspace.select` require `index`. Without one,
  `workspace.close` used to close whichever project was active.
- A parameter that names a target (`surface_id`, `to_surface_id`, `index`)
  must be a JSON number. A string such as `"42"` used to be read as "not
  given", and the write went to the first pane of the active project; it is
  now refused with `-32602`.

### Added

- Splitlane asks the release feed for a newer version every four hours
  while it stays open, not only at startup.
- The macOS `.dmg` is itself signed, notarized and stapled. Before, only the
  app inside it was.
- `surface.send_text` and `surface.send_keystroke` answer `-32003` when the
  pane did not take the input - its process has exited, or its input queue
  is full - instead of `sent: true`.

### Fixed

- The git commands Splitlane runs in the background no longer run programs
  named in a repository's own configuration: a file-system monitor, a text
  converter, an external diff or a content filter. Opening a folder is not
  enough to execute what its `.git/config` says.
- On Linux, starting the app with no display server prints an error and
  exits instead of hanging.
- The error for an unreachable instance no longer repeats the program name,
  and `--help` no longer lists a flag meant for the update test harness.

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

[Unreleased]: https://github.com/ivkan/splitlane/compare/v0.1.4...HEAD
[0.1.4]: https://github.com/ivkan/splitlane/compare/v0.1.3...v0.1.4
[0.1.3]: https://github.com/ivkan/splitlane/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/ivkan/splitlane/compare/v0.1.1...v0.1.2
[0.1.1]: https://github.com/ivkan/splitlane/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/ivkan/splitlane/releases/tag/v0.1.0

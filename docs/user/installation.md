# Installation

Splitlane has no release builds. You build it from source. The short version
is in the root [README](../../README.md#build-from-source). This page adds the
per-platform details: what to install first, how to get a proper app on macOS,
where your settings end up, and how to put the `splitlane` command on `PATH`.

## Rust

Install Rust with [rustup](https://rustup.rs). You don't need to pick a
version: [`rust-toolchain.toml`](../../rust-toolchain.toml) pins the toolchain
(with `rustfmt` and `clippy`), and rustup installs it the first time you run
`cargo` in the checkout.

## Platform prerequisites

### macOS

CI builds and tests on Apple Silicon (`aarch64-apple-darwin`). The minimum
macOS version is 13 (Ventura), the `LSMinimumSystemVersion` in the bundle's
`Info.plist`.

GPUI compiles its Metal shaders during the build by running `xcrun metal`, so
the Metal compiler has to be installed. It ships with Xcode, not with the
Command Line Tools alone. Check that it's there:

```bash
xcode-select -p
xcrun --find metal
```

If `xcrun --find metal` fails, install Xcode (or add its Metal toolchain
component) and point `xcode-select` at it.

### Linux

CI builds on x86_64 and aarch64. You need the Vulkan, Wayland and X11
development libraries. This is the list CI installs on Ubuntu, and it works on
Debian too:

```bash
sudo apt-get install libvulkan-dev libwayland-dev libxkbcommon-dev \
  libxkbcommon-x11-dev libx11-dev libxcb1-dev libxcb-render0-dev \
  libxcb-shape0-dev libxcb-xfixes0-dev libfontconfig1-dev libfreetype6-dev
```

On other distributions, install the equivalent development packages.

To run the app you also need a GPU driver. GPUI uses Vulkan when it can and
can fall back to OpenGL. The default Linux terminal backend (libghostty) links
a prebuilt archive that is committed to the repository, so the build doesn't
download or compile Zig.

### Windows

CI builds `x86_64-pc-windows-msvc`. Install Visual Studio Build Tools with the
C++ workload (MSVC). Windows-specific limitations are covered in
[Windows notes](../WINDOWS.md).

## Build and run

```bash
git clone https://github.com/ivkan/splitlane.git
cd splitlane
cargo run --release -p splitlane-app
```

The binary is `target/release/splitlane` (`splitlane.exe` on Windows). The
first build takes several minutes, because GPUI and its dependencies are
compiled from source.

A plain `cargo run` (no `--release`) gives you a debug build. Debug builds keep
their own config and state, separate from release builds (see
[Where settings live](#where-settings-live)).

If the libghostty backend won't link on your machine, `--no-default-features`
builds with only the Alacritty backend.

## A macOS app bundle

A bare binary works, but on macOS some features need a real `.app` bundle:

- **Notifications.** macOS delivers notifications only to apps that have a
  bundle. When the binary runs outside one, no notification can ever be
  shown, and Settings says so.
- **Folder permission prompts.** The bundle includes the sentences macOS
  shows when a pane opens Desktop, Documents, Downloads or a removable or
  network volume.

`scripts/bundle-macos.sh` builds the bundle from a release binary:

```bash
cargo build --release -p splitlane-app
scripts/bundle-macos.sh --version 0.8.2 --arch aarch64 --target-dir target/release
```

The script writes `dist/Splitlane.app` with the binary, `Info.plist` (the
`--version` value goes in there) and the icon. It then ad-hoc signs the bundle
with the bundle identifier `io.github.ivkan.splitlane`.

Pass `--target-dir target/release` after a plain `cargo build --release`.
Without it, the script looks in `target/aarch64-apple-darwin/release`, which
may contain an older binary. If a newer binary exists elsewhere, the script
prints a warning.

To check the signature:

```bash
codesign -dv dist/Splitlane.app
```

The `Identifier` line should say `io.github.ivkan.splitlane`. If you replace an
installed bundle with a fresh build, macOS may ask again for folder permissions
you already granted. Ad-hoc signatures change with every build.

## Put `splitlane` on PATH

One binary is both the app and the command-line tool (`splitlane up`,
`splitlane mcp install` and the rest, see [Scripting](scripting.md)). Link it
into a directory on your `PATH`.

Linux or macOS, from the checkout:

```bash
mkdir -p ~/.local/bin
ln -sf "$PWD/target/release/splitlane" ~/.local/bin/splitlane
```

macOS, using the bundle:

```bash
ln -sf "$PWD/dist/Splitlane.app/Contents/MacOS/splitlane" ~/.local/bin/splitlane
```

Windows: add the folder that contains `splitlane.exe` to your user `PATH`, then
open a new terminal.

A release binary talks to the running release app, and a debug binary talks to
the running debug app. Each build profile has its own IPC socket.

## Verify

```bash
splitlane --version
```

This prints `splitlane <version>` and exits without opening a window.
`splitlane --help` lists the options and subcommands.

## Where settings live

Release builds use the `splitlane` directory. Debug builds use `splitlane-dev`,
so a from-source debug run never reads or overwrites a release build's config
or layout.

| | Release | Debug |
| --- | --- | --- |
| Config, Linux | `~/.config/splitlane/splitlane.json` | `~/.config/splitlane-dev/splitlane.json` |
| Config, macOS | `~/Library/Application Support/splitlane/splitlane.json` | `~/Library/Application Support/splitlane-dev/splitlane.json` |
| Config, Windows | `%APPDATA%\splitlane\splitlane.json` | `%APPDATA%\splitlane-dev\splitlane.json` |
| Presets | `presets/` next to `splitlane.json` | same, under `splitlane-dev` |
| Saved layout | `session.json` in the platform cache directory, under `splitlane/` | `session-dev.json`, under `splitlane-dev/` |
| IPC socket | `splitlane.sock` (Windows: `\\.\pipe\splitlane`) | `splitlane-dev.sock` (Windows: `\\.\pipe\splitlane-dev`) |

On Linux, `$XDG_CONFIG_HOME` replaces `~/.config` when it is set. The cache
directory is `~/.cache` on Linux, `~/Library/Caches` on macOS and
`%LOCALAPPDATA%` on Windows.

So if a debug build starts with none of your projects, nothing is broken. It is
reading its own, empty, state. Every config key is documented in the
[configuration schema](configuration/schema.md).

## Turn off update checks for your own build

At startup Splitlane asks the GitHub releases feed whether a newer version
exists. The updater can't tell a binary you built from an official one, so
accepting an offered update would replace your build. To stop the check
entirely, add this to `splitlane.json`:

```json
{
  "check_for_updates": false
}
```

## Two copies on Linux

Splitlane notices when one Linux machine has two copies installed: a system
package at `/usr/bin/splitlane` and a user-local copy under
`~/.local/splitlane.app/`. It shows a one-time notice that links to this page.
The two copies share one config directory and can drift apart in version.
Remove the one you don't use.

## Something went wrong

See [Troubleshooting](troubleshooting.md). For a problem it doesn't cover, open
an [issue](https://github.com/ivkan/splitlane/issues).

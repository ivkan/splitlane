# Troubleshooting

Find your symptom in the table, confirm it, then try the first fix. If nothing
here matches, [collect logs](#logs) and open an issue.

| Symptom | Platform | Confirm | First fix |
| --- | --- | --- | --- |
| Crash or GPU error at launch | Linux | `RUST_LOG=info splitlane` shows no selected GPU adapter | Install a Vulkan (or OpenGL) driver for your GPU. |
| Blank window or startup failure under Wayland | Linux | It works with `WAYLAND_DISPLAY= splitlane` | Fix the GPU driver, or run through XWayland for now. |
| "Creating DirectX devices" error at launch | Windows | GPU driver date | Update the GPU driver. |
| Build fails compiling Metal shaders | macOS | `xcrun --find metal` fails | Install Xcode with its Metal toolchain. |
| Config change has no effect | All | Validate `splitlane.json` | Fix the JSON syntax, or edit the file your build actually reads. |
| Shortcut does nothing | All | Settings → Shortcuts | Use a registered action name and a key string that parses. |
| Theme resets or won't change | All | Look for `theme_mode` in `splitlane.json` | Use a bundled theme name, and pick it by name rather than System. |
| macOS asks for folder access "for Splitlane" | macOS | The prompt appears when an agent or command runs | Allow or deny. The request comes from a process in a pane. |
| No macOS notifications | macOS | Settings → Notifications → macOS permission | Run Splitlane from an `.app` bundle. |

## Launch and rendering

### Splitlane fails at launch with a GPU error

Splitlane draws through GPUI. That means Metal on macOS, Direct3D 11 on Windows,
and Vulkan or OpenGL on Linux.

**Linux.** Start from a terminal with logging on:

```bash
RUST_LOG=info splitlane
```

GPUI logs `Found N GPU adapter(s)`, lists each one, then logs
`Selected GPU adapter`. If there are no adapters, or it fails right after
selecting one, the problem is the graphics stack. Install your distribution's
Vulkan loader and the Mesa or vendor Vulkan driver. On a hybrid-GPU machine you
can pick the adapter yourself: set `ZED_DEVICE_ID` to the adapter's 4-digit
hexadecimal PCI device id, as shown in that log.

```bash
ZED_DEVICE_ID=0x1234 RUST_LOG=info splitlane
```

**Windows.** Splitlane needs a GPU and driver that can create a Direct3D 11
device at feature level 10.1 or higher. If startup fails with
`Creating DirectX devices`, update the graphics driver from Windows Update or
from the GPU vendor. Include your GPU and driver version in the report:

```powershell
Get-CimInstance Win32_VideoController |
  Select-Object Name, DriverVersion, DriverDate
```

[Windows notes](../WINDOWS.md) lists known problems in the Windows graphics
path, including Remote Desktop.

**macOS.** Splitlane needs macOS 13 (Ventura) or later.

### The window is blank, or startup fails, under Wayland

When `WAYLAND_DISPLAY` is set, GPUI uses Wayland. Otherwise it uses X11 via
`DISPLAY`. To check whether the problem is specific to Wayland, clear the
variable so Splitlane runs through XWayland:

```bash
WAYLAND_DISPLAY= splitlane
```

If that works, the Vulkan driver can't present to your compositor. Update Mesa
or the vendor driver. On NVIDIA, make sure the kernel module matches the
running kernel.

## Configuration

### My `splitlane.json` changes are ignored

**Check that you're editing the right file.** Release and debug builds read
different directories:

| Platform | Release build | Debug build (`cargo run`) |
| --- | --- | --- |
| Linux | `~/.config/splitlane/splitlane.json` | `~/.config/splitlane-dev/splitlane.json` |
| macOS | `~/Library/Application Support/splitlane/splitlane.json` | `~/Library/Application Support/splitlane-dev/splitlane.json` |
| Windows | `%APPDATA%\splitlane\splitlane.json` | `%APPDATA%\splitlane-dev\splitlane.json` |

**Check that the file is valid JSON.** Splitlane doesn't show parse errors in
the window or print them to the log, so check the file yourself:

```bash
python3 -m json.tool ~/.config/splitlane/splitlane.json
```

```powershell
Get-Content $env:APPDATA\splitlane\splitlane.json -Raw | ConvertFrom-Json | Out-Null
```

Here is what happens to a bad file:

- **At startup**, if the file has invalid JSON, isn't a JSON object, or is
  larger than 1 MiB, Splitlane ignores it and uses the defaults.
- **While running**, Splitlane watches the file and reloads it about 300 ms
  after you save. If the new contents don't parse, it keeps the last config
  that did.
- **One bad value** (a wrong type for a key) drops only that key. The rest of
  the file still applies.
- **Unknown keys** are ignored. The
  [JSON Schema](configuration/schema.md) lets your editor flag them.
- **Settings won't save** while the file has invalid JSON. It refuses to
  overwrite a file it can't read, and logs
  `config: invalid JSON at <path>; refusing to overwrite`.

`window_decorations` and `window_backdrop` are read only when the window opens.
Restart Splitlane after changing either one.

### A shortcut does nothing

Open **Settings → Shortcuts**. It shows the binding every action actually has,
including your overrides, and marks actions without a key as "Unassigned".

Overrides go in `shortcuts`. Each entry maps a key string to an action name,
or to `"none"` to remove a default binding:

```json
{
  "shortcuts": {
    "alt-g": "layout_grid",
    "secondary-shift-w": "none"
  }
}
```

- Action names are the `snake_case` names in the
  [keybindings reference](keybindings.md). An unknown name is skipped and logs
  `shortcuts: unknown action '<name>'`.
- `+` and `-` both work as separators. `secondary` means `cmd` on macOS and
  `ctrl` on Linux and Windows. A key string that doesn't parse logs
  `shortcuts: invalid keystroke`.
- A key belongs to one action. If you bind a key that a default already uses,
  your binding replaces the default.
- Some actions only work in a certain place, such as a terminal. Terminal
  actions do nothing while focus is on the rail or in Settings.
- Shortcut changes apply when the file is saved. No restart needed.

Those warnings go to the log, so start Splitlane from a terminal to see them.

### My theme won't change, or keeps switching back

Three themes are bundled: `Harbor Dark` (the default), `Harbor Light` and
`One Dark`. The `theme` value isn't case-sensitive. An unknown name falls back
to Harbor Dark and logs `Unknown theme '<name>', using default`.

If `theme_mode` is `"system"`, Splitlane follows the OS appearance. It writes
`Harbor Light` or `Harbor Dark` into `theme` at launch and whenever the
appearance changes, which overwrites a hand-edited `theme`. To keep one theme,
pick it by name under **Settings → Appearance → Theme** instead of System, or
set `theme_mode` to `"light"` or `"dark"`.

Theme changes apply without a restart. Splitlane watches the config directory.
If the file watcher can't start (some network or sandboxed filesystems), it
checks the file every 500 ms instead.

More in [Themes](themes.md).

## macOS permission prompts

### macOS says Splitlane wants to access Desktop, Documents or Downloads

Usually it isn't Splitlane's own code asking. Every pane runs a shell or an
agent as a child process, and macOS attributes a child's file access to the
app that launched it. When an agent searches your Documents folder, the prompt
names Splitlane. Splitlane itself reads the project folders you open, the
agent CLIs' session folders (such as `~/.claude` and `~/.codex`) and its own
config and cache. None of those are protected folders.

So the prompt is a real question about something running in one of your
panes. Allow it if you expected that access, otherwise deny it. An app bundle
built with `scripts/bundle-macos.sh` shows a one-sentence explanation in the
prompt (see [Installation](installation.md#a-macos-app-bundle)).

If macOS asks again for a folder you already allowed, check whether you just
replaced the app with a new build. A locally built bundle has an ad-hoc
signature, and that signature changes with every build.

### A "Choose Application" panel appears over Splitlane

The panel that asks "Where is *something*?" and lists every installed
application appears when a process asks macOS to open an application by name.
Splitlane never does that. It only opens files and folders by path. Some
program in one of your panes made the request, and macOS attributed it to
Splitlane. Cancel the panel, then look at what was running in your panes.

### No notifications on macOS

macOS delivers notifications only to an app running from a `.app` bundle. A
binary started from `target/release` or with `cargo run` can never show one.
**Settings → Notifications** shows the macOS permission state and says when
there is no bundle. Build a bundle as described in
[Installation](installation.md#a-macos-app-bundle). If the state says
"denied", change it in **System Settings → Notifications → Splitlane**.

## Logs

Splitlane logs to standard error. By default it prints only warnings and
errors. `RUST_LOG` sets the level, and the value replaces the default filter:

```bash
RUST_LOG=info splitlane                  # startup diagnostics: GPU, IPC, session restore
RUST_LOG=debug splitlane                 # everything, very verbose
RUST_LOG=warn,splitlane::agent_state=debug splitlane   # why an agent shows the status it does
RUST_LOG=warn,splitlane::terminal::backend=info splitlane  # which terminal backend each pane uses
```

On macOS, to capture logs from a bundle, start the binary inside it from a
terminal:

```bash
RUST_LOG=info dist/Splitlane.app/Contents/MacOS/splitlane
```

On Windows (PowerShell):

```powershell
$env:RUST_LOG = "info"
$env:RUST_BACKTRACE = "1"
.\target\release\splitlane.exe
```

If Splitlane crashes, set `RUST_BACKTRACE=1` and include the backtrace.

## What to put in an issue

Use the matching template:

- [Bug report](https://github.com/ivkan/splitlane/issues/new?template=bug_report.md)
  for Linux or macOS.
- [Windows bug report](https://github.com/ivkan/splitlane/issues/new?template=windows-bug-report.md)
  for Windows-only problems.
- [aarch64 bug report](https://github.com/ivkan/splitlane/issues/new?template=aarch64-bug-report.md)
  for problems that happen only on ARM64 Linux.

Include:

- the output of `splitlane --version`, and the commit you built (`git rev-parse HEAD`);
- OS version, architecture, and on Linux whether you run Wayland or X11;
- whether it was a release or a debug build;
- steps to reproduce, and whether it still happens after you move
  `splitlane.json` out of the way;
- the log from `RUST_LOG=info`, and a backtrace if it crashed.

Before you paste logs, read them. They can include paths from your machine.

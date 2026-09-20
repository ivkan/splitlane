# Splitlane on Windows

Notes for building and running Splitlane on Windows: what is supported, what
works differently from Linux and macOS, and known problems in the libraries
Splitlane depends on. Build instructions are in
[Installation](user/installation.md). General problems are covered in
[Troubleshooting](user/troubleshooting.md).

## Supported versions

| OS | Architecture | Status |
| --- | --- | --- |
| Windows 10, version 1809 (build 17763) or later | x86_64 | Supported |
| Windows 11 | x86_64 | Supported |
| Windows 10 before 1809 | any | Not supported. ConPTY, which every terminal pane needs, first shipped in 1809. |
| Windows 10 / 11 | ARM64 | Not built or tested in CI. The libghostty terminal backend is only available for `x86_64-pc-windows-msvc`. |
| Windows Server | x86_64 | Untested |

Splitlane renders with Direct3D 11 through GPUI, so the GPU driver has to
support feature level 10.1 or higher.

## Building

Follow [Installation](user/installation.md). On Windows you need Visual Studio
Build Tools with the C++ workload (MSVC). CI builds the
`x86_64-pc-windows-msvc` target. Run the `cargo` commands from PowerShell, not
Git Bash: Git Bash puts a coreutils `link.exe` ahead of the MSVC linker on
`PATH`, and build scripts then fail to link.

## Default shell

If `default_shell` isn't set, Splitlane uses PowerShell 7 (`pwsh`), then
Windows PowerShell 5.1, and falls back to `cmd.exe` (`%ComSpec%`) only when
neither is installed. A bare `"default_shell": "bash"` resolves to Git for
Windows' Bash before the WSL `bash.exe` launcher in `System32`.

## Known limitations

- **Panes running `cmd.exe` don't report their working directory.**
  Splitlane tracks a pane's current directory from the OSC 7 sequence the
  shell prints at each prompt. It adds that hook to PowerShell 5.1, PowerShell
  7, Bash, Zsh and Fish automatically, but `cmd.exe` has no per-prompt hook to
  add it to. Windows also has no fallback that reads a shell's working
  directory from the OS, as Linux and macOS do. So Splitlane doesn't know
  where a `cmd.exe` pane currently is, and features that need that (such as
  reopening a closed shell in the same directory) can't use it. Use PowerShell
  or Git Bash instead.

- **WSL shells start as interactive non-login shells.** When `default_shell` is
  `wsl.exe` and shell integration is on, Splitlane starts your default Linux
  shell (Bash, Zsh or Fish) and loads its interactive startup file (`.bashrc`,
  `.zshrc` or `config.fish`). Login-only files such as `.profile`,
  `.bash_profile` and `.zprofile` are not read. Put what your terminals need in
  the interactive file, or set `"shell_integration": false` to launch `wsl.exe`
  unmodified.

- **Dev-server labels can be missing.** Splitlane finds listening ports with
  `GetExtendedTcpTable` and names a known dev server (Vite, Next.js and others)
  from the owning process's command line. If Windows won't let Splitlane read
  that process's command line, the port shows as a plain port number.

- **Shift+Enter is sent as Alt+Enter.** ConPTY doesn't pass through the key
  encoding that distinguishes Shift+Enter, so Splitlane sends `ESC CR` instead.
  Programs receive it as Alt+Enter, which CLIs such as Codex also accept as a
  new line.

## Known upstream problems

These are bugs in Splitlane's dependencies (GPUI, alacritty, Windows ConPTY).
The fixes belong upstream.

### IME composition crash

- **Upstream:** [zed-industries/zed#12563](https://github.com/zed-industries/zed/issues/12563)
- Some CJK input sequences in a Windows IME can hit an assertion in GPUI's
  Windows IME handler and close the window. If it happens to you, report the
  exact sequence with the Windows bug template. An IME that commits the whole
  token at once, instead of on every keystroke, avoids it.

### Ctrl+C under ConPTY

- **Upstream:** [alacritty/alacritty#3075](https://github.com/alacritty/alacritty/issues/3075)
- ConPTY doesn't cleanly separate "Ctrl+C was pressed in this terminal" from
  "interrupt the foreground process". In some shell setups Ctrl+C reaches
  processes you didn't expect.

### Remote Desktop

- **Upstream:** [zed-industries/zed#26692](https://github.com/zed-industries/zed/issues/26692)
- Starting Splitlane inside a Remote Desktop session can fail to create the
  Direct3D device: the window renders incorrectly, or the process exits. Run it
  in a local session.

### Launching from a devcontainer or WSL2

- **Upstream:** [zed-industries/zed#49072](https://github.com/zed-industries/zed/issues/49072)
- Starting the app from inside a VS Code devcontainer or a WSL2 shell can
  freeze it before the first frame. Start it from a normal Windows shell or
  from Explorer. Using WSL as the shell *inside* a pane is fine.

### Old GPU drivers

- **Upstream:** [zed-industries/zed#28683](https://github.com/zed-industries/zed/issues/28683)
- With an old driver, GPUI can't create a Direct3D device and Splitlane exits
  at startup with `Creating DirectX devices` in the error. Update the driver
  through Windows Update or from the GPU vendor. If that doesn't fix it, attach
  your GPU details (see below) to a report.

## Reporting a Windows bug

Use the
[Windows bug report](https://github.com/ivkan/splitlane/issues/new?template=windows-bug-report.md)
template. Launch Splitlane from PowerShell with logging and backtraces on, and
paste the output from around the time of the bug:

```powershell
$env:RUST_LOG = "info"
$env:RUST_BACKTRACE = "1"
.\target\release\splitlane.exe
```

For graphics problems, include your GPU and driver:

```powershell
Get-CimInstance Win32_VideoController |
  Select-Object Name, DriverVersion, DriverDate
```

If the same bug also happens on Linux or macOS, use the general
[bug report](https://github.com/ivkan/splitlane/issues/new?template=bug_report.md)
instead.

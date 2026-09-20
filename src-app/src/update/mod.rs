//! In-app self-update flow + install-method detection + GitHub release polling.
//!
//! Current on-the-wire update strategies:
//!
//! - **AppImage** - handed off to `appimageupdatetool` for a zsync delta
//!   update in place (see [`linux::appimage::run_update`]). Preferred on any
//!   `InstallMethod::AppImage` install.
//! - **Linux tar.gz** - downloaded, minisign-verified, hardened-extracted,
//!   and atomically swapped under `$HOME/.local/splitlane.app/`.
//! - **Linux system packages** - delegated to the native package manager
//!   through `pkexec`.
//! - **macOS DMG** - minisign-verified, Gatekeeper-checked, copied and swapped
//!   in place inside `/Applications` or `~/Applications`.
//! - **Windows MSI** - minisign-verified, Authenticode-checked, staged, then
//!   installed by a detached relay after the GUI exits.
//!
//! Both strategies eventually call GPUI's `cx.set_restart_path(path) +
//! cx.restart()` - the "launcher pattern" where GPUI spawns a detached
//! `bash` script that waits for our PID to exit (via `kill -0` polling) and
//! then execs the new binary. Safe for Wayland/GPU apps because the current
//! process runs its Drops cleanly before the new one opens a fresh
//! compositor/GPU connection.
//!
//! State lives in `SplitlaneApp::self_update_status`. The title bar reads it
//! each render to flip the pill label between `available / Downloading… /
//! Installing…`. Errors are reported via a toast.
//!
//! Module layout:
//! - [`error`] - `UpdateError`, `IntegrityMismatch`, `classify`, `is_disk_full`
//! - [`checker`] - GitHub release polling + asset picking
//! - [`install_method`] - install source detection (AppImage / TarGz / SystemPackage / Unknown)
//! - [`linux`] - platform-specific update runners (AppImage zsync, tar.gz atomic swap)
//! - [`macos`] - DMG update runner
//! - [`windows`] - MSI update runner

pub mod checker;
pub mod error;
pub mod install_method;
pub mod linux;
pub mod macos;
// Minisign detached-signature verification -
// the independent root of trust shared by every installer path.
pub mod signature;
pub(crate) mod verified_download;
pub mod windows;

// Install-method hygiene migrations. Linux-only by construction
// (the only crossover this cleans up is tar.gz → rpm/deb, which has no
// equivalent on macOS or Windows).
#[cfg(target_os = "linux")]
pub mod migrations;

// Ergonomic re-export: callers use `crate::update::UpdateError` without
// reaching into `update::error::UpdateError`. `IntegrityMismatch` stays
// accessible via `update::error::IntegrityMismatch` (only constructed inside
// `update/linux/targz.rs`, not re-exported to avoid a dead `pub use`).
pub use error::UpdateError;

use std::path::PathBuf;

#[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
use anyhow::Context;
use anyhow::Result;

/// Rendering-facing state of the self-update flow.
#[derive(Clone, Debug, Default)]
pub enum SelfUpdateStatus {
    /// No update operation in flight - the title bar shows `v{x} available`.
    #[default]
    Idle,
    Downloading,
    /// Intermediate "installer is running" state for native package-manager
    /// or MSI relay paths that hand off to an external installer.
    Installing,
    /// The new binary has been downloaded, verified and swapped into place;
    /// `cx.set_restart_path()` has already been called. The pill switches
    /// to a "Restart for vX" affordance whose click handler only invokes
    /// `cx.restart()` - no I/O, no waiting, no analytics flush. This
    /// mirrors Zed's auto-update split (download/install in background →
    /// "Restart to Update" CTA) so the click-to-restart latency is ~0
    /// regardless of network speed or disk throughput.
    ReadyToRestart,
    /// Structured classification of the last failure. The toast
    /// renderer picks its copy per variant; the pill shows "Update failed"
    /// and remains clickable so the user can retry.
    Errored(#[allow(dead_code)] UpdateError),
}

impl SelfUpdateStatus {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            SelfUpdateStatus::Downloading | SelfUpdateStatus::Installing
        )
    }
}

/// Resolve the expected install location of the splitlane binary. The
/// installer writes here; callers pass this path to `cx.set_restart_path()` so
/// GPUI's relaunch script execs the freshly installed binary.
///
/// Per-OS semantics:
///
/// - **Linux / BSD**: `~/.local/bin/splitlane`, retained for legacy callers.
///   The active tar.gz updater returns its own restart path instead.
/// - **macOS**: unused by the DMG updater. The dispatcher passes
///   `InstallMethod::AppBundle { bundle_path }` directly to the macOS install
///   flow, which returns the promoted `.app` bundle path for GPUI's `open`
///   based restart.
/// - **Windows**: `%ProgramFiles%\Splitlane\splitlane.exe`.
///   Extension comes from `std::env::consts::EXE_EXTENSION` rather than
///   a literal `"exe"` so the helper is cross-target clean; the MSI
///   installer targets `%ProgramFiles%\Splitlane\`; the shipping WiX package is
///   machine-wide.
#[allow(dead_code)]
pub fn installed_binary_path() -> Result<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Ok(PathBuf::from(
            "/Applications/Splitlane.app/Contents/MacOS/splitlane",
        ))
    }
    #[cfg(target_os = "windows")]
    {
        let program_files = std::env::var_os("ProgramFiles")
            .context("ProgramFiles environment variable is not set")?;
        let mut exe = PathBuf::from(program_files)
            .join("Splitlane")
            .join("splitlane");
        // `EXE_EXTENSION` = "exe" on windows, "" elsewhere - keeps this
        // cross-target clean and avoids hardcoding the literal suffix.
        if !std::env::consts::EXE_EXTENSION.is_empty() {
            exe.set_extension(std::env::consts::EXE_EXTENSION);
        }
        Ok(exe)
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = std::env::var_os("HOME").context("HOME environment variable is not set")?;
        Ok(PathBuf::from(home).join(".local/bin/splitlane"))
    }
}

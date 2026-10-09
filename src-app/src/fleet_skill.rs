//! Keeping the fleet skill where the agents on this machine read skills.
//!
//! The skill is the text that teaches an agent the `splitlane` commands for
//! opening and running other sessions. The app writes it at every start and
//! takes it out when the setting is turned off; the installer itself, and
//! what it will not overwrite, is `splitlane_mcp_install::skill`.
//!
//! Both run on a thread of their own: they touch files under the home
//! directory, which may be slow or on a network, and nothing in the window
//! waits for the answer.

use splitlane_mcp_install::skill::{
    InstallOutcome, UninstallOutcome, default_places, install_everywhere, uninstall_everywhere,
};

/// What the app does at start.
///
/// A debug build writes nothing: it keeps its config, session and socket
/// apart from an installed release, and a skill directory is shared with it.
/// A build with an unfinished `SKILL.md` would otherwise hand that text to
/// every agent on the developer's machine.
pub(crate) fn keep_installed_at_start(enabled: bool) {
    if cfg!(debug_assertions) {
        log::info!("fleet skill: not installed by a debug build");
        return;
    }
    if !enabled {
        log::info!("fleet skill: not installed (fleet_skill: false)");
        return;
    }
    apply(true);
}

/// Install the skill, or take it out, off the calling thread.
pub(crate) fn apply(enabled: bool) {
    let spawned = std::thread::Builder::new()
        .name("fleet-skill".into())
        .spawn(move || {
            let places = default_places();
            if enabled {
                for (id, outcome) in install_everywhere(&places) {
                    match outcome {
                        InstallOutcome::Installed => log::info!("fleet skill: installed for {id}"),
                        InstallOutcome::Updated => log::info!("fleet skill: updated for {id}"),
                        InstallOutcome::LeftModified => log::info!(
                            "fleet skill: the copy for {id} was edited by hand and is left alone"
                        ),
                        InstallOutcome::Error(error) => {
                            log::warn!("fleet skill: could not install for {id}: {error}");
                        }
                        InstallOutcome::AlreadyCurrent | InstallOutcome::NotDetected => {}
                    }
                }
            } else {
                for (id, outcome) in uninstall_everywhere(&places) {
                    match outcome {
                        UninstallOutcome::Removed => log::info!("fleet skill: removed for {id}"),
                        UninstallOutcome::LeftModified => log::info!(
                            "fleet skill: the copy for {id} was edited by hand and is left alone"
                        ),
                        UninstallOutcome::Error(error) => {
                            log::warn!("fleet skill: could not remove for {id}: {error}");
                        }
                        UninstallOutcome::SetAside(_)
                        | UninstallOutcome::NothingToRemove
                        | UninstallOutcome::NotDetected => {}
                    }
                }
            }
        });
    if let Err(error) = spawned {
        log::warn!("fleet skill: could not start the install thread: {error}");
    }
}

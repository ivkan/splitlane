//! `splitlane skill install | status | uninstall` - put the fleet skill where
//! the agents on this machine look for skills, and take it out again.
//!
//! The skill is the text that teaches an agent the `splitlane` commands. Its
//! source is `skills/splitlane-fleet/SKILL.md` in the repository, built into
//! this crate, so the copy an install writes always matches the binary that
//! wrote it.
//!
//! The app keeps it installed: every start writes it wherever an agent is,
//! unless `fleet_skill` is `false` in `splitlane.json`. It used to be a
//! command the person had to know about and run, on the reasoning that a
//! skill changes what an agent does and so is theirs to switch on. Nobody
//! runs a command they have not heard of, and an agent that was never told
//! the commands cannot open a session however it is asked, so the feature
//! was off for everyone who had not read the documentation. What keeps the
//! default safe is the skill's own first rule: it is used only when the
//! person asks for sessions in Splitlane.
//!
//! Three places are known: Claude Code's skills directory, Codex's, and the
//! one several agents share. Each is written only when the directory above
//! it already exists - a skills directory for an agent that is not installed
//! would be litter.
//!
//! **A file a person edited is not overwritten.** The installed copy ends
//! with a line carrying a fingerprint of the text above it. A file whose text
//! still matches its own fingerprint is ours, as written by this or an
//! earlier version, and is replaced freely; one that does not match was
//! edited by hand, and is left alone unless `--force` is given. A file with
//! no such line at all was not put there by this command and is treated the
//! same way. With `--force` the edited file is not destroyed either: it is
//! set aside beside the skill under a name no earlier backup has.
//!
//! Exit codes are those of `splitlane mcp`: `0` done (including "nothing
//! detected"), `1` when a place could not be written or was left alone
//! because it was edited, `2` for a usage error.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;

/// The directory name of the skill, which is also its published name.
pub const SKILL_NAME: &str = "splitlane-fleet";

/// The skill as shipped.
const SKILL_TEXT: &str = include_str!("../../../skills/splitlane-fleet/SKILL.md");

/// What the last line of an installed copy starts with.
const MARKER_PREFIX: &str = "<!-- managed by splitlane skill install; fingerprint ";
const MARKER_SUFFIX: &str = " -->";

const USAGE: &str = "\
splitlane skill - install the Splitlane fleet skill for your CLI agents

Usage:
  splitlane skill install [--force]     Write the skill for every detected agent
  splitlane skill uninstall [--force]   Remove it again
  splitlane skill status                Report where it is installed

A copy that was edited by hand is left alone unless --force is given, and
is then kept beside the skill as SKILL.md.bak.

The app installs the skill each time it starts. To keep it out, turn off
Settings -> Agents -> Fleet skill, or set \"fleet_skill\": false in
splitlane.json.";

/// One place a skill can be installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillPlace {
    /// `claude-code`, `codex` or `agents`.
    pub id: &'static str,
    /// The agent's own directory. The place counts as detected when it
    /// exists.
    pub root: PathBuf,
}

impl SkillPlace {
    /// Where the skill's file goes under this place.
    pub fn file(&self) -> PathBuf {
        self.root.join("skills").join(SKILL_NAME).join("SKILL.md")
    }
}

/// The three places, from the home directory and the two variables that move
/// an agent's directory. `None` values are simply left out.
pub fn places_from(
    home: Option<PathBuf>,
    claude_config_dir: Option<OsString>,
    codex_home: Option<OsString>,
) -> Vec<SkillPlace> {
    let set = |value: Option<OsString>| value.filter(|dir| !dir.is_empty()).map(PathBuf::from);
    let mut places = Vec::new();
    if let Some(root) = set(claude_config_dir).or_else(|| home.as_ref().map(|h| h.join(".claude")))
    {
        places.push(SkillPlace {
            id: "claude-code",
            root,
        });
    }
    if let Some(root) = set(codex_home).or_else(|| home.as_ref().map(|h| h.join(".codex"))) {
        places.push(SkillPlace { id: "codex", root });
    }
    if let Some(home) = home {
        places.push(SkillPlace {
            id: "agents",
            root: home.join(".agents"),
        });
    }
    places
}

/// The places on this machine.
pub fn default_places() -> Vec<SkillPlace> {
    places_from(
        dirs::home_dir(),
        std::env::var_os("CLAUDE_CONFIG_DIR"),
        std::env::var_os("CODEX_HOME"),
    )
}

/// What is at one place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillState {
    /// The agent's directory is not there.
    NotDetected,
    NotInstalled,
    /// The copy this version would write.
    Installed,
    /// A copy written by another version and not edited since.
    Stale,
    /// Edited by hand, or not written by this command at all.
    Modified,
}

impl SkillState {
    pub fn word(self) -> &'static str {
        match self {
            SkillState::NotDetected => "not detected",
            SkillState::NotInstalled => "not installed",
            SkillState::Installed => "installed",
            SkillState::Stale => "stale",
            SkillState::Modified => "modified",
        }
    }
}

/// 64-bit FNV-1a. A fingerprint for telling an edited file from an unedited
/// one, not a defence against anybody: whoever can edit the file can edit
/// the line that carries this too.
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// The file as installed: the skill, then the line that says whose it is.
pub fn installed_text() -> String {
    let body = if SKILL_TEXT.ends_with('\n') {
        SKILL_TEXT.to_string()
    } else {
        format!("{SKILL_TEXT}\n")
    };
    let mark = fingerprint(&body);
    format!("{body}{MARKER_PREFIX}{mark}{MARKER_SUFFIX}\n")
}

/// Split an installed file into the text and the fingerprint its last line
/// carries. `None` when the last line is not ours.
fn split_marker(contents: &str) -> Option<(&str, &str)> {
    let trimmed = contents.strip_suffix('\n').unwrap_or(contents);
    let line_start = trimmed.rfind('\n').map_or(0, |at| at + 1);
    let mark = trimmed[line_start..]
        .strip_prefix(MARKER_PREFIX)?
        .strip_suffix(MARKER_SUFFIX)?;
    Some((&trimmed[..line_start], mark))
}

/// What the contents of a skill file are, to this version.
fn state_of_contents(contents: &str) -> SkillState {
    if contents == installed_text() {
        return SkillState::Installed;
    }
    match split_marker(contents) {
        Some((body, mark)) if fingerprint(body) == mark => SkillState::Stale,
        _ => SkillState::Modified,
    }
}

/// What is at `place`. A file that cannot be read counts as edited: the one
/// thing not to do with it is overwrite it.
pub fn state_at(place: &SkillPlace) -> SkillState {
    if !place.root.is_dir() {
        return SkillState::NotDetected;
    }
    let file = place.file();
    if !file.exists() {
        return SkillState::NotInstalled;
    }
    match std::fs::read_to_string(&file) {
        Ok(contents) => state_of_contents(&contents),
        Err(_) => SkillState::Modified,
    }
}

/// How installing at one place went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed,
    Updated,
    AlreadyCurrent,
    NotDetected,
    /// Edited by hand and `--force` was not given. Nothing was written.
    LeftModified,
    Error(String),
}

/// Set an edited file aside under a name nothing else has: `SKILL.md.bak`,
/// then `SKILL.md.bak.1` and so on. Moved, not copied, so the caller writes
/// or leaves nothing in its place as it chooses.
///
/// Never over an existing backup. An earlier one can be the only copy of
/// what a person wrote, and a fixed name would let the second forced
/// install destroy what the first one saved.
fn set_aside(file: &std::path::Path) -> std::io::Result<PathBuf> {
    let name = |n: u32| {
        let mut name = file.as_os_str().to_owned();
        name.push(if n == 0 {
            ".bak".to_string()
        } else {
            format!(".bak.{n}")
        });
        PathBuf::from(name)
    };
    let free = (0..)
        .map(name)
        .find(|candidate| !candidate.exists())
        .unwrap_or_else(|| name(0));
    std::fs::rename(file, &free)?;
    Ok(free)
}

/// Write the skill at `place`.
///
/// A copy of ours, current or from another version, is replaced with no
/// backup: there is nothing in it a person wrote. An edited one is replaced
/// only with `force`, and is set aside first.
pub fn install_at(place: &SkillPlace, force: bool) -> InstallOutcome {
    let before = state_at(place);
    match before {
        SkillState::NotDetected => return InstallOutcome::NotDetected,
        SkillState::Installed => return InstallOutcome::AlreadyCurrent,
        SkillState::Modified if !force => return InstallOutcome::LeftModified,
        SkillState::NotInstalled | SkillState::Stale | SkillState::Modified => {}
    }
    let file = place.file();
    let written = crate::io::with_config_lock(&file, || {
        if before == SkillState::Modified {
            set_aside(&file)?;
        }
        crate::io::write_atomic(&file, installed_text().as_bytes())
    });
    match written {
        Ok(()) if before == SkillState::NotInstalled => InstallOutcome::Installed,
        Ok(()) => InstallOutcome::Updated,
        Err(error) => InstallOutcome::Error(format!("{error:#}")),
    }
}

/// How removing from one place went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallOutcome {
    Removed,
    /// An edited copy was taken out with `--force`, and kept under this
    /// name.
    SetAside(PathBuf),
    NothingToRemove,
    NotDetected,
    /// Edited by hand and `--force` was not given. Nothing was removed.
    LeftModified,
    Error(String),
}

/// Remove the skill from `place`: its file, and its directory when that
/// leaves it empty. A copy of ours is deleted. An edited one, taken out
/// with `force`, is set aside instead - the agent stops reading it, and
/// what the person wrote is still there. A directory that holds such a file
/// stays.
pub fn uninstall_at(place: &SkillPlace, force: bool) -> UninstallOutcome {
    let before = state_at(place);
    match before {
        SkillState::NotDetected => return UninstallOutcome::NotDetected,
        SkillState::NotInstalled => return UninstallOutcome::NothingToRemove,
        SkillState::Modified if !force => return UninstallOutcome::LeftModified,
        SkillState::Installed | SkillState::Stale | SkillState::Modified => {}
    }
    let file = place.file();
    if before == SkillState::Modified {
        return match set_aside(&file) {
            Ok(kept) => UninstallOutcome::SetAside(kept),
            Err(error) => {
                UninstallOutcome::Error(format!("set aside {} failed: {error}", file.display()))
            }
        };
    }
    if let Err(error) = std::fs::remove_file(&file) {
        return UninstallOutcome::Error(format!("remove {} failed: {error}", file.display()));
    }
    if let Some(dir) = file.parent() {
        // Fails on a directory that is not empty, which is the point.
        let _ = std::fs::remove_dir(dir);
    }
    UninstallOutcome::Removed
}

/// Write the skill at every place, never over an edited copy. What the app
/// does at start and when the setting is turned on.
pub fn install_everywhere(places: &[SkillPlace]) -> Vec<(&'static str, InstallOutcome)> {
    places
        .iter()
        .map(|place| (place.id, install_at(place, false)))
        .collect()
}

/// Remove the skill from every place, leaving an edited copy where it is.
/// What the app does when the setting is turned off.
pub fn uninstall_everywhere(places: &[SkillPlace]) -> Vec<(&'static str, UninstallOutcome)> {
    places
        .iter()
        .map(|place| (place.id, uninstall_at(place, false)))
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Command {
    Install,
    Uninstall,
    Status,
}

/// Entry point. `args` is everything after `splitlane skill`.
#[must_use]
pub fn run_skill_cli(args: &[String]) -> i32 {
    run_with(
        args,
        &default_places(),
        &mut std::io::stdout(),
        &mut std::io::stderr(),
    )
}

/// Testable core: the places and the output sinks are handed in.
fn run_with(
    args: &[String],
    places: &[SkillPlace],
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> i32 {
    let command = match args.first().map(String::as_str) {
        Some("install") => Command::Install,
        Some("uninstall") => Command::Uninstall,
        Some("status") => Command::Status,
        _ => {
            let _ = writeln!(err, "{USAGE}");
            return 2;
        }
    };
    let force = match &args[1..] {
        [] => false,
        [flag] if flag == "--force" && command != Command::Status => true,
        [other, ..] => {
            let _ = writeln!(err, "unexpected argument `{other}`\n\n{USAGE}");
            return 2;
        }
    };
    if places.iter().all(|place| !place.root.is_dir()) {
        let _ = writeln!(
            out,
            "No agent directory found (looked for {}) - nothing to do.",
            places
                .iter()
                .map(|place| place.root.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        return 0;
    }
    let mut failed = false;
    for place in places {
        let file = place.file().display().to_string();
        let id = place.id;
        match command {
            Command::Status => {
                let state = state_at(place);
                let _ = match state {
                    SkillState::NotDetected | SkillState::NotInstalled => {
                        writeln!(out, "{id}: {}", state.word())
                    }
                    SkillState::Installed => writeln!(out, "{id}: installed ({file})"),
                    SkillState::Stale => writeln!(
                        out,
                        "{id}: stale ({file}) - written by another version; \
                         re-run `splitlane skill install`"
                    ),
                    SkillState::Modified => writeln!(
                        out,
                        "{id}: modified ({file}) - edited by hand; \
                         `splitlane skill install --force` replaces it"
                    ),
                };
            }
            Command::Install => {
                let _ = match install_at(place, force) {
                    InstallOutcome::Installed => writeln!(out, "{id}: installed ({file})"),
                    InstallOutcome::Updated => writeln!(out, "{id}: updated ({file})"),
                    InstallOutcome::AlreadyCurrent => writeln!(out, "{id}: already up to date"),
                    InstallOutcome::NotDetected => writeln!(out, "{id}: skipped (not detected)"),
                    InstallOutcome::LeftModified => {
                        failed = true;
                        writeln!(
                            out,
                            "{id}: left alone ({file}) - edited by hand; \
                             --force replaces it and keeps a .bak"
                        )
                    }
                    InstallOutcome::Error(error) => {
                        failed = true;
                        writeln!(out, "{id}: error - {error}")
                    }
                };
            }
            Command::Uninstall => {
                let _ = match uninstall_at(place, force) {
                    UninstallOutcome::Removed => writeln!(out, "{id}: removed"),
                    UninstallOutcome::SetAside(kept) => {
                        writeln!(out, "{id}: removed (kept as {})", kept.display())
                    }
                    UninstallOutcome::NothingToRemove => {
                        writeln!(out, "{id}: not installed (nothing to remove)")
                    }
                    UninstallOutcome::NotDetected => {
                        writeln!(out, "{id}: not detected (nothing to remove)")
                    }
                    UninstallOutcome::LeftModified => {
                        failed = true;
                        writeln!(
                            out,
                            "{id}: left alone ({file}) - edited by hand; \
                             --force removes it and keeps a .bak"
                        )
                    }
                    UninstallOutcome::Error(error) => {
                        failed = true;
                        writeln!(out, "{id}: error - {error}")
                    }
                };
            }
        }
    }
    if command == Command::Uninstall {
        let _ = writeln!(
            out,
            "Splitlane installs it again at its next start unless \"fleet_skill\" is false in \
             splitlane.json (Settings -> Agents -> Fleet skill)."
        );
    }
    i32::from(failed)
}

/// Where the skill would be read from in the repository, for the test that
/// keeps the built-in text and the file from drifting apart.
#[cfg(test)]
fn repository_skill() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../skills")
        .join(SKILL_NAME)
        .join("SKILL.md")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A home with Claude Code and Codex in it, and no shared directory.
    fn home() -> (tempfile::TempDir, Vec<SkillPlace>) {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".claude")).unwrap();
        std::fs::create_dir(dir.path().join(".codex")).unwrap();
        let places = places_from(Some(dir.path().to_path_buf()), None, None);
        (dir, places)
    }

    fn run(args: &[&str], places: &[SkillPlace]) -> (i32, String, String) {
        let args: Vec<String> = args.iter().map(|arg| (*arg).to_string()).collect();
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run_with(&args, places, &mut out, &mut err);
        (
            code,
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    #[test]
    fn the_three_places_follow_the_variables_that_move_them() {
        let home = PathBuf::from("/home/u");
        let places = places_from(Some(home.clone()), None, None);
        let roots: Vec<_> = places.iter().map(|p| (p.id, p.root.clone())).collect();
        assert_eq!(
            roots,
            [
                ("claude-code", home.join(".claude")),
                ("codex", home.join(".codex")),
                ("agents", home.join(".agents")),
            ]
        );
        assert_eq!(
            places[0].file(),
            home.join(".claude/skills/splitlane-fleet/SKILL.md")
        );
        let moved = places_from(
            Some(home),
            Some(OsString::from("/work/claude")),
            Some(OsString::from("/work/codex")),
        );
        assert_eq!(moved[0].root, PathBuf::from("/work/claude"));
        assert_eq!(moved[1].root, PathBuf::from("/work/codex"));
        // An empty variable is an unset one.
        let empty = places_from(Some(PathBuf::from("/h")), Some(OsString::new()), None);
        assert_eq!(empty[0].root, PathBuf::from("/h/.claude"));
        // With no home there is still what the variables name.
        let homeless = places_from(None, Some(OsString::from("/work/claude")), None);
        assert_eq!(homeless.len(), 1);
    }

    /// The copy an install writes is the repository's file, then one line.
    #[test]
    fn what_is_installed_is_the_skill_in_the_repository() {
        let source = std::fs::read_to_string(repository_skill()).unwrap();
        let installed = installed_text();
        let (body, mark) = split_marker(&installed).expect("the last line is the marker");
        assert_eq!(body, source);
        assert_eq!(mark, fingerprint(&source));
        // The front matter has to stay the first thing in the file.
        assert!(installed.starts_with("---\nname: splitlane-fleet\n"));
    }

    #[test]
    fn install_writes_where_an_agent_is_and_nowhere_else() {
        let (dir, places) = home();
        let (code, out, _) = run(&["install"], &places);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("claude-code: installed"), "{out}");
        assert!(out.contains("codex: installed"), "{out}");
        assert!(out.contains("agents: skipped (not detected)"), "{out}");
        assert_eq!(
            std::fs::read_to_string(places[0].file()).unwrap(),
            installed_text()
        );
        assert!(!dir.path().join(".agents").exists());
        assert_eq!(state_at(&places[0]), SkillState::Installed);
        assert_eq!(state_at(&places[2]), SkillState::NotDetected);
    }

    #[test]
    fn installing_twice_changes_nothing() {
        let (_dir, places) = home();
        run(&["install"], &places);
        let file = places[0].file();
        let written = std::fs::metadata(&file).unwrap().modified().unwrap();
        let (code, out, _) = run(&["install"], &places);
        assert_eq!(code, 0);
        assert!(out.contains("claude-code: already up to date"), "{out}");
        assert_eq!(
            std::fs::metadata(&file).unwrap().modified().unwrap(),
            written
        );
        assert!(!file.parent().unwrap().join("SKILL.md.bak").exists());
    }

    /// A copy an earlier version wrote, untouched since, is replaced without
    /// being asked: it still matches the fingerprint it was written with.
    #[test]
    fn a_copy_from_another_version_is_stale_and_is_updated() {
        let (_dir, places) = home();
        let file = places[0].file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let old = "---\nname: splitlane-fleet\n---\nan older text\n";
        std::fs::write(
            &file,
            format!("{old}{MARKER_PREFIX}{}{MARKER_SUFFIX}\n", fingerprint(old)),
        )
        .unwrap();
        assert_eq!(state_at(&places[0]), SkillState::Stale);
        let (code, out, _) = run(&["status"], &places);
        assert_eq!(code, 0);
        assert!(out.contains("claude-code: stale"), "{out}");
        let (code, out, _) = run(&["install"], &places);
        assert_eq!(code, 0);
        assert!(out.contains("claude-code: updated"), "{out}");
        assert_eq!(state_at(&places[0]), SkillState::Installed);
        // Nothing a person wrote was in it, so nothing is kept of it.
        assert!(!file.parent().unwrap().join("SKILL.md.bak").exists());
    }

    /// What a forced install saved is still there after the next update,
    /// and after a second forced install: no backup is written over one.
    #[test]
    fn a_backup_is_never_written_over() {
        let (_dir, places) = home();
        let file = places[0].file();
        let dir = file.parent().unwrap().to_path_buf();
        run(&["install"], &places);
        std::fs::write(&file, "first edit\n").unwrap();
        run(&["install", "--force"], &places);
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.bak")).unwrap(),
            "first edit\n"
        );

        // An update of an unedited copy leaves the backup as it is.
        let old = "an older text\n";
        std::fs::write(
            &file,
            format!("{old}{MARKER_PREFIX}{}{MARKER_SUFFIX}\n", fingerprint(old)),
        )
        .unwrap();
        run(&["install"], &places);
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.bak")).unwrap(),
            "first edit\n"
        );

        // A second edit forced over goes beside the first.
        std::fs::write(&file, "second edit\n").unwrap();
        run(&["install", "--force"], &places);
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.bak")).unwrap(),
            "first edit\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("SKILL.md.bak.1")).unwrap(),
            "second edit\n"
        );
        assert_eq!(state_at(&places[0]), SkillState::Installed);
    }

    /// The reason for the fingerprint: a person's edits are not written over.
    #[test]
    fn a_copy_edited_by_hand_is_left_alone_without_force() {
        let (_dir, places) = home();
        run(&["install"], &places);
        let file = places[0].file();
        let edited = installed_text().replace("# Splitlane fleet", "# My own rules");
        assert_ne!(edited, installed_text());
        std::fs::write(&file, &edited).unwrap();
        assert_eq!(state_at(&places[0]), SkillState::Modified);

        let (code, out, _) = run(&["install"], &places);
        assert_eq!(code, 1, "an edited copy left alone is not success: {out}");
        assert!(out.contains("claude-code: left alone"), "{out}");
        assert!(out.contains("codex: already up to date"), "{out}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), edited);

        let (code, out, _) = run(&["uninstall"], &places);
        assert_eq!(code, 1);
        assert!(out.contains("claude-code: left alone"), "{out}");
        assert!(file.exists());

        // With --force it is replaced, and what the person wrote is kept
        // beside it.
        let (code, out, _) = run(&["install", "--force"], &places);
        assert_eq!(code, 0, "{out}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), installed_text());
        let backup = file.parent().unwrap().join("SKILL.md.bak");
        assert_eq!(std::fs::read_to_string(backup).unwrap(), edited);
    }

    /// A file at that path that this command never wrote is somebody's.
    #[test]
    fn a_file_without_the_marker_counts_as_edited() {
        let (_dir, places) = home();
        let file = places[1].file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "---\nname: splitlane-fleet\n---\nmine\n").unwrap();
        assert_eq!(state_at(&places[1]), SkillState::Modified);
        assert_eq!(install_at(&places[1], false), InstallOutcome::LeftModified);
    }

    #[test]
    fn uninstall_removes_the_file_and_its_empty_directory() {
        let (dir, places) = home();
        run(&["install"], &places);
        let (code, out, _) = run(&["uninstall"], &places);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("claude-code: removed"), "{out}");
        assert!(out.contains("agents: not detected"), "{out}");
        assert!(!places[0].file().parent().unwrap().exists());
        // The agent's own skills directory is not ours to remove.
        assert!(dir.path().join(".claude/skills").is_dir());
        let (_, out, _) = run(&["uninstall"], &places);
        assert!(out.contains("claude-code: not installed"), "{out}");
    }

    /// A backup beside the file can be the only copy of what a person wrote.
    #[test]
    fn uninstall_leaves_a_backup_where_it_is() {
        let (_dir, places) = home();
        run(&["install"], &places);
        let file = places[0].file();
        std::fs::write(&file, "edited\n").unwrap();
        run(&["install", "--force"], &places);
        run(&["uninstall"], &places);
        assert!(!file.exists());
        assert!(file.parent().unwrap().join("SKILL.md.bak").exists());
    }

    /// Forcing an edited copy out takes it away from the agent and keeps it
    /// for the person.
    #[test]
    fn a_forced_uninstall_keeps_what_was_edited() {
        let (_dir, places) = home();
        run(&["install"], &places);
        let file = places[0].file();
        std::fs::write(&file, "my own rules\n").unwrap();
        let (code, out, _) = run(&["uninstall", "--force"], &places);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains("claude-code: removed (kept as "), "{out}");
        assert!(!file.exists());
        assert_eq!(
            std::fs::read_to_string(file.parent().unwrap().join("SKILL.md.bak")).unwrap(),
            "my own rules\n"
        );
        assert_eq!(state_at(&places[0]), SkillState::NotInstalled);
    }

    #[test]
    fn keeping_it_installed_spares_an_edited_copy_and_an_absent_agent() {
        let (_home, places) = home();
        let claude = &places[0];
        std::fs::create_dir_all(claude.file().parent().unwrap()).unwrap();
        std::fs::write(claude.file(), "my own notes\n").unwrap();

        let outcomes = install_everywhere(&places);

        assert_eq!(outcomes[0], ("claude-code", InstallOutcome::LeftModified));
        assert_eq!(outcomes[1], ("codex", InstallOutcome::Installed));
        assert_eq!(outcomes[2], ("agents", InstallOutcome::NotDetected));
        assert_eq!(
            std::fs::read_to_string(claude.file()).unwrap(),
            "my own notes\n"
        );

        let removed = uninstall_everywhere(&places);
        assert_eq!(removed[0], ("claude-code", UninstallOutcome::LeftModified));
        assert_eq!(removed[1], ("codex", UninstallOutcome::Removed));
        assert!(claude.file().exists());
    }

    #[test]
    fn status_writes_nothing() {
        let (dir, places) = home();
        let (code, out, _) = run(&["status"], &places);
        assert_eq!(code, 0);
        assert!(out.contains("claude-code: not installed"), "{out}");
        assert!(out.contains("agents: not detected"), "{out}");
        assert!(!dir.path().join(".claude/skills").exists());
    }

    #[test]
    fn a_machine_with_no_agent_is_not_an_error() {
        let dir = tempfile::TempDir::new().unwrap();
        let places = places_from(Some(dir.path().to_path_buf()), None, None);
        let (code, out, _) = run(&["install"], &places);
        assert_eq!(code, 0);
        assert!(out.contains("nothing to do"), "{out}");
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
    }

    #[test]
    fn anything_else_is_a_usage_error() {
        let (_dir, places) = home();
        for args in [
            &[][..],
            &["upgrade"],
            &["install", "--now"],
            &["status", "--force"],
            &["install", "--force", "again"],
        ] {
            let (code, out, err) = run(args, &places);
            assert_eq!(code, 2, "{args:?}");
            assert!(out.is_empty(), "{args:?}");
            assert!(err.contains("Usage:"), "{args:?}");
        }
    }
}

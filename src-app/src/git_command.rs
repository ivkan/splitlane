//! The one place git is started from.
//!
//! Git reads its behaviour from the repository it is pointed at, and a
//! repository's own configuration can name programs to run: a file-system
//! monitor on `git status`, a text converter or an external diff on
//! `git diff`, a content filter on either. That is fine for a command the
//! person asked for. It is not fine for the commands Splitlane runs on its
//! own, in the background, in every folder that is merely open in the rail -
//! opening a folder must not be enough to run what its `.git/config` says.
//!
//! So every call states which of the two it is:
//!
//! - [`GitProfile::Probe`] - a read Splitlane makes for itself (status, diff,
//!   branch and worktree listings). Runs none of the repository's programs
//!   and never writes the index.
//! - [`GitProfile::UserAction`] - something the person asked for (creating a
//!   worktree, switching a branch). Hooks and filters run as they would from
//!   their own shell; a post-checkout hook is part of what they asked for.
//!
//! Filter drivers are neutralized only when the repository itself defines
//! them. A driver from the person's own global configuration - Git LFS after
//! `git lfs install` - is theirs, and turning it off would report every
//! tracked large file as modified.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use splitlane_process::{BoundedOutput, ProcError};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GitProfile {
    Probe,
    UserAction,
}

/// Variables that point git at a repository other than the one in `dir`.
/// They are set inside a hook or a `rebase --exec`, and a `splitlane up`
/// started from there would otherwise act on the wrong repository.
const INHERITED_REPOSITORY_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
];

const PROBE_CONFIG: &[&str] = &[
    "core.fsmonitor=false",
    "diff.external=",
    // `git diff` refreshes the index as a side effect; with the lock refused
    // below it would only warn, so do not try.
    "diff.autoRefreshIndex=false",
];

/// `diff`, `show` and `log` take these; a text converter is chosen by an attribute
/// in the tree and no configuration key turns all of them off at once.
const PROBE_DIFF_SUBCOMMANDS: &[&str] = &["diff", "show", "log"];
const PROBE_DIFF_FLAGS: &[&str] = &["--no-ext-diff", "--no-textconv"];

/// The subcommands that compare file contents with the index, which is when
/// a clean filter runs.
const FILTERING_SUBCOMMANDS: &[&str] = &["status", "diff"];

const FILTER_DRIVER_KEYS: &str = r"^filter\..*\.(clean|smudge|process|required)$";
const REPOSITORY_CONFIG_SCOPES: &[&str] = &["local", "worktree"];
const NEUTRALIZED_FILTER_SETTINGS: &[(&str, &str)] = &[
    ("clean", ""),
    ("smudge", ""),
    ("process", ""),
    ("required", "false"),
];
const FILTER_QUERY_DEADLINE: Duration = Duration::from_secs(5);
const FILTER_QUERY_STDOUT_CAP: u64 = 64 * 1024;

/// Run `git <args>` in `dir` under a deadline and an output cap.
pub(crate) fn run(
    profile: GitProfile,
    dir: &Path,
    args: &[&str],
    deadline: Duration,
    stdout_cap: u64,
) -> Result<BoundedOutput, ProcError> {
    // Everything below recognizes the subcommand by its place. An option in
    // front of it would run a probe with none of its protections, quietly.
    if args.first().is_none_or(|sub| sub.starts_with('-')) {
        return Err(ProcError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "git arguments must start with the subcommand",
        )));
    }
    let mut command = command(profile, dir, args);
    if profile == GitProfile::Probe
        && args
            .first()
            .is_some_and(|sub| FILTERING_SUBCOMMANDS.contains(sub))
    {
        neutralize_repository_filters(&mut command, dir)?;
    }
    splitlane_process::run_with_timeout(command, deadline, stdout_cap)
}

/// `args` starts with the subcommand: global options are this function's to
/// add, so a caller cannot put one in front and hide the subcommand from it.
fn command(profile: GitProfile, dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command.current_dir(dir);
    for key in INHERITED_REPOSITORY_ENV {
        command.env_remove(key);
    }
    // A credential or helper prompt has no terminal to appear on and would
    // hold the background task until the deadline.
    command.env("GIT_TERMINAL_PROMPT", "0");
    if profile == GitProfile::Probe {
        // A probe that takes `index.lock` fails the agent's own `git add`
        // running at the same moment, and an index write is what fires a
        // post-index-change hook.
        command.env("GIT_OPTIONAL_LOCKS", "0");
        command.env_remove("GIT_EXTERNAL_DIFF");
        for setting in PROBE_CONFIG {
            command.arg("-c").arg(setting);
        }
    }
    let mut args = args.iter();
    if let Some(subcommand) = args.next() {
        command.arg(subcommand);
        if profile == GitProfile::Probe && PROBE_DIFF_SUBCOMMANDS.contains(subcommand) {
            command.args(PROBE_DIFF_FLAGS);
        }
    }
    command.args(args);
    command
}

/// Blank every filter driver the repository defines, for this one command.
///
/// Fails the command when the drivers cannot be listed: a probe that cannot
/// tell whether it is about to run a repository's program does not run.
fn neutralize_repository_filters(command: &mut Command, dir: &Path) -> Result<(), ProcError> {
    let query = self::command(
        GitProfile::Probe,
        dir,
        &[
            "config",
            "--show-scope",
            "--name-only",
            "-z",
            "--get-regexp",
            FILTER_DRIVER_KEYS,
        ],
    );
    // Exit status 1 is "no such key", the ordinary answer; its listing is
    // empty. Outside a repository the listing is empty too.
    let output =
        splitlane_process::run_with_timeout(query, FILTER_QUERY_DEADLINE, FILTER_QUERY_STDOUT_CAP)?;
    if output.stdout_truncated {
        return Err(ProcError::Spawn(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the repository's filter drivers could not be listed in full",
        )));
    }
    let drivers = repository_filter_drivers(&output.stdout);
    if drivers.is_empty() {
        return Ok(());
    }
    // Environment rather than `-c`: it reaches the git processes this one
    // starts for submodules, and it appends to what the person's own
    // environment already configures instead of replacing it.
    let inherited = std::env::var("GIT_CONFIG_COUNT")
        .ok()
        .and_then(|count| count.parse::<usize>().ok())
        .unwrap_or(0);
    let mut count = inherited;
    for driver in &drivers {
        for (setting, value) in NEUTRALIZED_FILTER_SETTINGS {
            command.env(
                format!("GIT_CONFIG_KEY_{count}"),
                format!("filter.{driver}.{setting}"),
            );
            command.env(format!("GIT_CONFIG_VALUE_{count}"), value);
            count += 1;
        }
    }
    command.env("GIT_CONFIG_COUNT", count.to_string());
    Ok(())
}

/// The driver names in a `config --show-scope --name-only -z` listing that
/// the repository itself defines. Each record is `scope NUL key NUL`.
fn repository_filter_drivers(listing: &[u8]) -> BTreeSet<String> {
    let mut drivers = BTreeSet::new();
    let mut fields = listing.split(|byte| *byte == 0);
    while let (Some(scope), Some(key)) = (fields.next(), fields.next()) {
        if !REPOSITORY_CONFIG_SCOPES
            .iter()
            .any(|wanted| wanted.as_bytes() == scope)
        {
            continue;
        }
        let key = String::from_utf8_lossy(key);
        if let Some((driver, _)) = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
        {
            drivers.insert(driver.to_string());
        }
    }
    drivers
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    const DEADLINE: Duration = Duration::from_secs(20);
    const CAP: u64 = 1024 * 1024;

    fn user(dir: &Path, args: &[&str]) {
        let out = run(GitProfile::UserAction, dir, args, DEADLINE, CAP).expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with one committed file, and a second directory beside it
    /// where the programs under test leave a file when they run.
    fn repo(name: &str) -> (PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "splitlane-git-command-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        let repo = root.join("repo");
        let traces = root.join("traces");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::create_dir_all(&traces).expect("traces dir");
        user(&repo, &["init", "-q"]);
        user(&repo, &["config", "user.email", "test@example.com"]);
        user(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("file");
        user(&repo, &["add", "."]);
        user(&repo, &["commit", "-q", "-m", "init"]);
        (repo, traces)
    }

    /// A shell command that leaves `name` in `traces` and then behaves as
    /// `then` says. Git runs configured programs through `sh` on every
    /// platform, Windows included, so one spelling serves all three.
    fn leaves(traces: &Path, name: &str, then: &str) -> String {
        let trace = traces.join(name).to_string_lossy().replace('\\', "/");
        format!("sh -c 'echo ran > \"{trace}\"; {then}'")
    }

    #[test]
    fn a_probe_does_not_run_the_repositorys_file_monitor() {
        let (repo, traces) = repo("fsmonitor");
        let monitor = leaves(&traces, "fsmonitor", "exit 1");
        user(&repo, &["config", "core.fsmonitor", &monitor]);

        let out = run(
            GitProfile::Probe,
            &repo,
            &["status", "--porcelain"],
            DEADLINE,
            CAP,
        )
        .expect("status runs");
        assert!(out.status.success());
        assert!(!traces.join("fsmonitor").exists());

        // The same repository, asked the way a person's shell asks it: the
        // monitor runs, so the assertion above is about the profile and not
        // about a monitor that never worked.
        let _ = run(
            GitProfile::UserAction,
            &repo,
            &["status", "--porcelain"],
            DEADLINE,
            CAP,
        );
        assert!(traces.join("fsmonitor").exists());
        let _ = std::fs::remove_dir_all(repo.parent().expect("root"));
    }

    #[test]
    fn a_probe_does_not_run_a_text_converter_or_an_external_diff() {
        let (repo, traces) = repo("textconv");
        std::fs::write(repo.join(".gitattributes"), "*.txt diff=shown\n").expect("attributes");
        let converter = leaves(&traces, "textconv", "cat \"$0\"");
        user(&repo, &["config", "diff.shown.textconv", &converter]);
        let external = leaves(&traces, "external", "exit 0");
        user(&repo, &["config", "diff.external", &external]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("edit");

        let out = run(
            GitProfile::Probe,
            &repo,
            &["diff", "HEAD", "--"],
            DEADLINE,
            CAP,
        )
        .expect("diff runs");
        assert!(out.status.success());
        assert!(String::from_utf8_lossy(&out.stdout).contains("+two"));
        assert!(!traces.join("textconv").exists());
        assert!(!traces.join("external").exists());
        let _ = std::fs::remove_dir_all(repo.parent().expect("root"));
    }

    #[test]
    fn a_probe_does_not_run_a_filter_the_repository_defines() {
        let (repo, traces) = repo("filter");
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=planted\n").expect("attributes");
        let clean = leaves(&traces, "clean", "cat");
        user(&repo, &["config", "filter.planted.clean", &clean]);
        // The same length as what is committed, so git cannot settle the
        // comparison from the file's size and has to read it - which is when
        // the filter runs.
        std::fs::write(repo.join("a.txt"), "two\n").expect("edit");

        for args in [&["status", "--porcelain"][..], &["diff", "HEAD", "--"][..]] {
            let out = run(GitProfile::Probe, &repo, args, DEADLINE, CAP).expect("probe runs");
            assert!(out.status.success(), "{args:?}");
            assert!(!traces.join("clean").exists(), "{args:?} ran the filter");
        }

        let _ = run(
            GitProfile::UserAction,
            &repo,
            &["diff", "HEAD", "--"],
            DEADLINE,
            CAP,
        );
        assert!(traces.join("clean").exists());
        let _ = std::fs::remove_dir_all(repo.parent().expect("root"));
    }

    #[test]
    fn a_probe_does_not_write_the_index() {
        let (repo, traces) = repo("index");
        let hooks = repo.join(".git").join("hooks");
        std::fs::create_dir_all(&hooks).expect("hooks dir");
        let hook = hooks.join("post-index-change");
        let trace = traces.join("hook").to_string_lossy().replace('\\', "/");
        std::fs::write(&hook, format!("#!/bin/sh\necho ran > \"{trace}\"\n")).expect("hook");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755))
                .expect("executable hook");
        }
        // Rewritten with the same content: the index entry's timestamp is
        // stale, which is what makes `git status` want to write it back.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(repo.join("a.txt"), "one\n").expect("touch");

        let out = run(
            GitProfile::Probe,
            &repo,
            &["status", "--porcelain"],
            DEADLINE,
            CAP,
        )
        .expect("status runs");
        assert!(out.status.success());
        assert!(!traces.join("hook").exists());
        let _ = std::fs::remove_dir_all(repo.parent().expect("root"));
    }

    #[test]
    fn only_the_repositorys_own_filter_drivers_are_named() {
        let listing = b"global\0filter.lfs.clean\0local\0filter.planted.clean\0\
                        local\0filter.planted.required\0worktree\0filter.a.b.process\0\
                        system\0filter.sys.smudge\0";
        let drivers = repository_filter_drivers(listing);
        assert_eq!(
            drivers.into_iter().collect::<Vec<_>>(),
            vec!["a.b".to_string(), "planted".to_string()]
        );
        assert!(repository_filter_drivers(b"").is_empty());
    }

    #[test]
    fn an_option_in_front_of_the_subcommand_is_refused() {
        for args in [&["-c", "core.fsmonitor=true", "status"][..], &[][..]] {
            assert!(run(GitProfile::Probe, Path::new("."), args, DEADLINE, CAP).is_err());
        }
    }

    #[test]
    fn a_user_action_keeps_the_repositorys_configuration() {
        let command = command(GitProfile::UserAction, Path::new("."), &["switch", "main"]);
        let args: Vec<_> = command.get_args().collect();
        assert_eq!(args, ["switch", "main"]);
        assert!(
            !command
                .get_envs()
                .any(|(key, _)| key == "GIT_OPTIONAL_LOCKS")
        );
    }

    #[test]
    fn a_probe_diff_carries_its_flags_after_the_subcommand() {
        let command = command(GitProfile::Probe, Path::new("."), &["diff", "HEAD", "--"]);
        let args: Vec<String> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let at = args.iter().position(|arg| arg == "diff").expect("diff");
        assert_eq!(
            &args[at..],
            ["diff", "--no-ext-diff", "--no-textconv", "HEAD", "--"]
        );
    }
}

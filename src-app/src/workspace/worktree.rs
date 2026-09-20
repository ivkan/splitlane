//! Git worktree-per-agent management.
//!
//! `splitlane up` panes can declare `worktree = "branch"`: the CLI process
//! creates (or reuses) a git worktree in a SIBLING directory of the repo -
//! `<repo>.worktrees/<branch-slug>`, or `<branch-slug>-<hash>` on slug
//! collision - copies the top-level gitignored `.env*` files, optionally runs
//! a `setup` command, and the pane spawns with the worktree as its cwd. The
//! app side records ownership ([`ManagedWorktree`]) so closing the workspace
//! tears the worktree down - IF it is clean.
//!
//! Invariants:
//! - a branch is NEVER deleted, only the worktree directory;
//! - a worktree with uncommitted changes is NEVER removed;
//! - only worktrees Splitlane created (tracked in `managed_worktrees`) are
//!   ever torn down - a pre-existing worktree pointed at by `cwd` is not ours;
//! - every git invocation is a subprocess with argv (no shell interpolation)
//!   under [`splitlane_process::run_with_timeout`], and on the app side it runs
//!   off the render thread (`smol::unblock`).
//!
//! Sibling (not in-repo) placement keeps recursive file watchers - including
//! Splitlane's own diff watcher - from descending into N extra checkouts.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Wall-clock bound for plumbing git calls (list/status/remove/prune).
const GIT_DEADLINE: Duration = Duration::from_secs(10);
/// `worktree add` checks out a full tree - give it more room on big repos.
const ADD_DEADLINE: Duration = Duration::from_secs(120);
const STDOUT_CAP: u64 = 256 * 1024;
const OWNER_MARKER_FILE: &str = ".splitlane-worktree";

/// Teardown policy for a managed worktree. `Auto` removes the
/// worktree at workspace close when it has no uncommitted changes; `Keep`
/// opts out entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TeardownPolicy {
    #[default]
    Auto,
    Keep,
}

impl TeardownPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            TeardownPolicy::Auto => "auto",
            TeardownPolicy::Keep => "keep",
        }
    }
}

/// A worktree Splitlane created for a pane and therefore owns the lifecycle of.
/// Carried by `Workspace`, persisted in `session.json` (so a crash does not
/// orphan the ownership record), torn down at workspace close.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedWorktree {
    /// Worktree checkout directory (`<repo>.worktrees/<slug>`).
    pub path: PathBuf,
    /// Main repository root (where `git worktree …` commands run).
    pub repo_root: PathBuf,
    /// Branch checked out in the worktree. Recorded for diagnostics only -
    /// teardown never touches the branch.
    pub branch: String,
    pub teardown: TeardownPolicy,
}

pub fn owner_marker_path(worktree_path: &Path) -> PathBuf {
    worktree_path.join(OWNER_MARKER_FILE)
}

pub fn has_owner_marker(worktree_path: &Path) -> bool {
    owner_marker_path(worktree_path).is_file()
}

fn write_owner_marker(worktree_path: &Path, repo_root: &Path, branch: &str) -> Result<(), String> {
    let marker = owner_marker_path(worktree_path);
    let contents = format!(
        "owner=splitlane\nrepo_root={}\nbranch={}\n",
        repo_root.display(),
        branch
    );
    std::fs::write(&marker, contents)
        .map_err(|e| format!("cannot write owner marker {}: {e}", marker.display()))
}

/// Rehydrate a persisted or IPC-provided ownership record. The record is only
/// accepted when it matches Splitlane's deterministic worktree directory and the
/// on-disk worktree carries Splitlane's owner marker.
pub fn managed_worktree_from_record(
    path_raw: &str,
    repo_root_raw: &str,
    branch_raw: &str,
    teardown_raw: &str,
) -> Option<ManagedWorktree> {
    let path = PathBuf::from(path_raw);
    let repo_root = PathBuf::from(repo_root_raw);
    if !path.is_absolute() || !repo_root.is_absolute() {
        log::warn!("managed worktree: dropping record with non-absolute path");
        return None;
    }
    let branch = branch_raw.trim();
    if branch.is_empty() || branch_slug(branch).is_empty() {
        log::warn!("managed worktree: dropping record with invalid branch");
        return None;
    }
    if !is_splitlane_worktree_dir(&repo_root, branch, &path) {
        log::warn!(
            "managed worktree: dropping record outside Splitlane worktree dir: {}",
            path.display()
        );
        return None;
    }
    if !has_owner_marker(&path) {
        log::warn!(
            "managed worktree: dropping record without owner marker: {}",
            path.display()
        );
        return None;
    }
    let teardown = match teardown_raw {
        "auto" => TeardownPolicy::Auto,
        "keep" => TeardownPolicy::Keep,
        other => {
            log::warn!("managed worktree: unknown teardown policy {other:?}; keeping");
            TeardownPolicy::Keep
        }
    };
    Some(ManagedWorktree {
        path,
        repo_root,
        branch: branch.to_string(),
        teardown,
    })
}

/// One entry of `git worktree list --porcelain`.
#[derive(Debug, Clone, PartialEq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    /// `None` for a detached-HEAD worktree.
    pub branch: Option<String>,
}

/// Filesystem-safe directory name for a branch (`feat/x` → `feat-x`).
/// Conservative whitelist: anything outside `[A-Za-z0-9._-]` becomes `-`.
/// Leading/trailing `-` AND `.` are trimmed: a dot-only branch (`.`/`..`)
/// would otherwise survive as a path-traversal component of the (destructive)
/// worktree path, and a leading dot would hide the directory. May return ""
/// for degenerate input - spec validation rejects that before any git call,
/// and [`worktree_dir`] falls back to a safe constant as defense in depth.
pub fn branch_slug(branch: &str) -> String {
    let slug: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches(|c: char| c == '-' || c == '.')
        .to_string()
}

fn branch_slug_or_default(branch: &str) -> String {
    let slug = branch_slug(branch);
    if slug.is_empty() {
        "branch".to_string()
    } else {
        slug
    }
}

fn branch_hash_suffix(branch: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in branch.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")[..8].to_string()
}

fn worktrees_parent(repo_root: &Path) -> PathBuf {
    let repo_name = repo_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let parent = repo_root.parent().unwrap_or(repo_root);
    parent.join(format!("{repo_name}.worktrees"))
}

/// Sibling worktree directory for a branch: `<repo>.worktrees/<slug>`, next to
/// the repo (NOT inside it - recursive watchers must not descend into it).
/// Total function: a branch whose slug is empty (dot-only - rejected upstream
/// by spec validation) maps to the constant `branch` so the result can never
/// resolve outside `<repo>.worktrees/`.
pub fn worktree_dir(repo_root: &Path, branch: &str) -> PathBuf {
    worktrees_parent(repo_root).join(branch_slug_or_default(branch))
}

/// Collision-resistant sibling directory for a branch. Kept separate from
/// [`worktree_dir`] so existing readable paths remain valid; planners switch
/// to this path only when the slug path is already claimed by another branch.
pub fn worktree_dir_hashed(repo_root: &Path, branch: &str) -> PathBuf {
    let slug = branch_slug_or_default(branch);
    worktrees_parent(repo_root).join(format!("{slug}-{}", branch_hash_suffix(branch)))
}

pub fn is_splitlane_worktree_dir(repo_root: &Path, branch: &str, path: &Path) -> bool {
    path == worktree_dir(repo_root, branch) || path == worktree_dir_hashed(repo_root, branch)
}

/// Run a git plumbing command and return trimmed stdout, mapping every
/// failure mode (spawn, timeout, non-zero exit) to a displayable message.
fn run_git(repo: &Path, args: &[&str], deadline: Duration) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(repo).args(args);
    let out = splitlane_process::run_with_timeout(cmd, deadline, STDOUT_CAP)
        .map_err(|e| format!("git {} failed: {e}", args.join(" ")))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            stderr.trim().lines().last().unwrap_or("non-zero exit")
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `git worktree list --porcelain`, parsed.
pub fn list_worktrees(repo_root: &Path) -> Result<Vec<WorktreeEntry>, String> {
    let stdout = run_git(
        repo_root,
        &["worktree", "list", "--porcelain"],
        GIT_DEADLINE,
    )?;
    Ok(parse_worktree_porcelain(&stdout))
}

/// Pure porcelain parser (unit-tested). Entries are blank-line separated;
/// `branch refs/heads/<name>` is absent for detached or bare entries.
pub fn parse_worktree_porcelain(stdout: &str) -> Vec<WorktreeEntry> {
    let mut entries = Vec::new();
    let mut path: Option<PathBuf> = None;
    let mut branch: Option<String> = None;
    for line in stdout.lines().chain(std::iter::once("")) {
        if line.is_empty() {
            if let Some(p) = path.take() {
                entries.push(WorktreeEntry {
                    path: p,
                    branch: branch.take(),
                });
            }
            branch = None;
            continue;
        }
        if let Some(p) = line.strip_prefix("worktree ") {
            path = Some(PathBuf::from(p));
        } else if let Some(b) = line.strip_prefix("branch ") {
            branch = Some(b.strip_prefix("refs/heads/").unwrap_or(b).to_string());
        }
    }
    entries
}

/// True when `branch` exists locally in the repo.
pub fn branch_exists(repo_root: &Path, branch: &str) -> bool {
    run_git(
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
        GIT_DEADLINE,
    )
    .is_ok()
}

/// `git worktree add <path> [-b] <branch>`. `create_branch` chooses between
/// branching off HEAD (`-b`) and checking out the existing branch.
pub fn add_worktree(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    create_branch: bool,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
    }
    let path_s = path.to_string_lossy();
    let mut args: Vec<&str> = vec!["worktree", "add", &path_s];
    if create_branch {
        args.push("-b");
    }
    args.push(branch);
    run_git(repo_root, &args, ADD_DEADLINE)?;
    if let Err(e) = write_owner_marker(path, repo_root, branch) {
        let _ = remove_worktree(repo_root, path);
        return Err(e);
    }
    Ok(())
}

/// True when the worktree has no uncommitted changes (`status --porcelain`
/// empty). An error (worktree gone, git missing) is NOT "clean" - the caller
/// must keep its hands off when it cannot prove cleanliness.
pub fn is_clean(worktree_path: &Path) -> Result<bool, String> {
    run_git(worktree_path, &["status", "--porcelain"], GIT_DEADLINE).map(|out| out.is_empty())
}

/// `git worktree remove <path>`. Refuses dirty worktrees by itself too (git
/// native), but callers must check [`is_clean`] first to control messaging.
/// The BRANCH IS NEVER DELETED - that is an invariant of this module, not a TODO.
pub fn remove_worktree(repo_root: &Path, path: &Path) -> Result<(), String> {
    let path_s = path.to_string_lossy();
    run_git(repo_root, &["worktree", "remove", &path_s], GIT_DEADLINE).map(|_| ())
}

/// `git worktree prune` - drops references whose directory no longer exists.
/// Git-native guarantee: a worktree whose directory still exists is untouched,
/// so this is safe to run blindly at startup.
pub fn prune(repo_root: &Path) -> Result<(), String> {
    run_git(repo_root, &["worktree", "prune"], GIT_DEADLINE).map(|_| ())
}

/// Read a remembered `setup` command: blank in any form means "run nothing".
///
/// One home for a rule two sides state - what the `+ worktree` dialog writes
/// onto the project, and what restore reads back off `session.json`. They
/// have to agree, because a hand-edited `"worktree_setup": "  "` that
/// restored as `Some("  ")` would put whitespace in the dialog's field and
/// then hand it to a shell.
pub fn remembered_setup(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

/// Default wall-clock bound for a worktree's `setup` command.
pub const DEFAULT_SETUP_TIMEOUT: Duration = Duration::from_secs(300);

/// Run a freshly created worktree's `setup` command inside it.
///
/// **The one place both doors bootstrap a tree.** `worktree::add_worktree` has
/// two callers - `splitlane up` (`cli/up_cmd.rs::execute_worktree_plan`) and
/// the rail's `+ worktree` dialog - and a setup runner screwed into one of
/// them would silently skip the other. That is why this lives beside
/// `add_worktree` and `copy_env_files` rather than in either caller.
///
/// **Creation only** is the caller's job to honour: a reused worktree already
/// had its bootstrap, and re-running an install behind somebody's back is a
/// surprise, not a service.
///
/// **A failure is returned, never fatal**. A broken install must
/// not block the agent launch, because the human can fix it in the pane it
/// landed in, so each caller reports the message in its own voice: the CLI on
/// stderr, the dialog in a toast. An empty or blank command runs nothing and
/// succeeds.
pub fn run_setup(worktree_path: &Path, setup: &str, timeout: Duration) -> Result<(), String> {
    let setup = setup.trim();
    if setup.is_empty() {
        return Ok(());
    }
    #[cfg(unix)]
    let mut cmd = {
        let mut c = Command::new("sh");
        c.arg("-c").arg(setup);
        c
    };
    #[cfg(windows)]
    let mut cmd = {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(setup);
        c
    };
    cmd.current_dir(worktree_path);
    match splitlane_process::run_with_timeout(cmd, timeout, STDOUT_CAP) {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let code = out.status.code().unwrap_or(-1);
            match last_meaningful_line(&out.stderr).or_else(|| last_meaningful_line(&out.stdout)) {
                Some(said) => Err(format!("exit {code}: {said}")),
                None => Err(format!("exit {code}")),
            }
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The last non-blank line of a command's output, capped for a one-line
/// report.
///
/// `run_with_timeout` captures both streams; dropping them left every failed
/// bootstrap reading `exit 1` in a toast that disappears, with an agent now
/// sitting in a tree that has no dependencies in it. The last line is what a
/// build tool puts its actual complaint on, and a cap keeps a single long
/// line out of a toast that has no room for it.
fn last_meaningful_line(raw: &[u8]) -> Option<String> {
    const MAX: usize = 160;
    let text = String::from_utf8_lossy(raw);
    let line = text.lines().rev().find(|l| !l.trim().is_empty())?.trim();
    let mut out: String = line.chars().take(MAX).collect();
    if out.chars().count() < line.chars().count() {
        out.push('\u{2026}');
    }
    Some(out)
}

/// Copy top-level `.env*` FILES from `src_root` into `dst_root`, skipping any
/// that already exist there (a tracked `.env.example` arrives via checkout -
/// don't clobber it). Best-effort by design: a missing source dir or
/// an unreadable entry yields an empty/partial copy, never an error. Returns
/// the file names copied.
pub fn copy_env_files(src_root: &Path, dst_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(src_root) else {
        return Vec::new();
    };
    let mut copied = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name_s = name.to_string_lossy();
        if !name_s.starts_with(".env") {
            continue;
        }
        if !entry.path().is_file() {
            continue;
        }
        let dst = dst_root.join(&name);
        if dst.exists() {
            continue;
        }
        if std::fs::copy(entry.path(), &dst).is_ok() {
            copied.push(name_s.into_owned());
        }
    }
    copied.sort();
    copied
}

/// True when `open` is `worktree` itself or lives underneath it - i.e. when
/// removing `worktree` would take `open` with it.
///
/// Both sides are canonicalized before comparing, because the two paths reach
/// here by different routes: a worktree path is composed by
/// [`worktree_dir`] from the repo root, while a container's directory is
/// whatever the user opened (a symlink, `/tmp` on macOS, a differently-spelled
/// case). Canonicalization can fail - a path that vanished, a permissions
/// hole - and then the raw comparison is the honest fallback: it can only
/// answer "not the same", which errs toward removing, so the caller checks
/// existence first.
fn contains_or_equals(worktree: &Path, open: &Path) -> bool {
    let real_worktree = worktree.canonicalize();
    let real_open = open.canonicalize();
    match (&real_worktree, &real_open) {
        (Ok(w), Ok(o)) => o.starts_with(w),
        _ => open.starts_with(worktree),
    }
}

/// Tear down a batch of managed worktrees (blocking - run via `smol::unblock`
/// on the app side). Per entry: `Keep` policy → skip; **open as a container →
/// skip**; dirty or unverifiable → keep + warn (NEVER remove what might hold
/// work); clean → remove. The branch is never touched.
///
/// `open_containers` is a **snapshot** taken on the main thread, because this
/// function runs on a background one and cannot read app state. It is a
/// snapshot rather than a live answer, and the gap is real: `is_clean` shells
/// out to `git status` under a ten-second deadline, so a container opened
/// inside a doomed worktree *after* the close and *before* the removal is not
/// in the list and is not protected. Closing that window would mean hopping
/// back to the main thread before each removal; the narrower race is the
/// better trade while the wide one - no guard at all - is what this replaced.
/// It exists
/// because ownership and visibility are different questions: Splitlane may own
/// a worktree it created through `splitlane up` **and** have it open as an
/// ordinary container, opened later by the user from the rail. Without the
/// guard, closing the *creating* container deletes a directory somebody is
/// working in - the marker says it is ours, `is_clean` says nothing is at
/// risk, and both are true right up to the moment the panes go dark.
pub fn teardown_all(worktrees: Vec<ManagedWorktree>, open_containers: &[PathBuf]) {
    for wt in worktrees {
        if wt.teardown == TeardownPolicy::Keep {
            continue;
        }
        if let Some(open) = open_containers
            .iter()
            .find(|open| contains_or_equals(&wt.path, open))
        {
            log::info!("worktree kept: open as a container ({})", open.display());
            continue;
        }
        if !wt.path.exists() {
            // Directory already gone (user rm -rf'd it): just prune the ref.
            let _ = prune(&wt.repo_root);
            continue;
        }
        if !has_owner_marker(&wt.path) {
            log::warn!(
                "worktree kept: missing Splitlane owner marker in {}",
                wt.path.display()
            );
            continue;
        }
        match is_clean(&wt.path) {
            Ok(true) => match remove_worktree(&wt.repo_root, &wt.path) {
                Ok(()) => log::info!("worktree removed: {}", wt.path.display()),
                Err(e) => log::warn!("worktree kept ({}): {e}", wt.path.display()),
            },
            Ok(false) => log::warn!(
                "worktree kept: uncommitted changes in {}",
                wt.path.display()
            ),
            Err(e) => log::warn!(
                "worktree kept (cannot verify cleanliness): {} - {e}",
                wt.path.display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The teardown guard: a worktree that is open as a container survives the
    /// close of the container that created it.
    ///
    /// Uses real directories, because the guard canonicalizes and the whole
    /// point is that two spellings of one directory must compare equal.
    #[test]
    fn an_open_container_is_not_torn_down() {
        let tmp =
            std::env::temp_dir().join(format!("splitlane-teardown-guard-{}", std::process::id()));
        let worktree = tmp.join("repo.worktrees").join("feat-x");
        let nested = worktree.join("crates").join("thing");
        let elsewhere = tmp.join("repo.worktrees").join("feat-y");
        std::fs::create_dir_all(&nested).expect("temp tree");
        std::fs::create_dir_all(&elsewhere).expect("temp tree");

        assert!(
            contains_or_equals(&worktree, &worktree),
            "the worktree opened as a container is the case that loses work"
        );
        assert!(
            contains_or_equals(&worktree, &nested),
            "removing the worktree takes everything under it, so a container \
             opened deeper inside must protect it too"
        );
        assert!(
            !contains_or_equals(&worktree, &elsewhere),
            "a sibling worktree is not this one and must not block its removal"
        );

        // A path that does not exist cannot be canonicalized; the fallback
        // still has to answer, and answer the same way for the equal case.
        let gone = tmp.join("never-created");
        assert!(contains_or_equals(&gone, &gone));
        assert!(!contains_or_equals(&gone, &worktree));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn branch_slug_is_filesystem_safe() {
        assert_eq!(
            branch_slug("feat/cli-orchestration"),
            "feat-cli-orchestration"
        );
        assert_eq!(branch_slug("fix/ABC-006_teardown"), "fix-ABC-006_teardown");
        assert_eq!(branch_slug("a b\\c:d"), "a-b-c-d");
        // Leading/trailing separators are trimmed so the dir never hides.
        assert_eq!(branch_slug("/weird/"), "weird");
        assert_eq!(branch_slug(".hidden"), "hidden");
        // Inner dots survive (version-style branches stay readable).
        assert_eq!(branch_slug("release/v1.2.3"), "release-v1.2.3");
    }

    #[test]
    fn branch_slug_neutralizes_dot_only_traversal() {
        // NFR (orchestration-v2): the slug is the one untrusted component of
        // a destructive path - `.`/`..` must never survive as a path segment.
        assert_eq!(branch_slug(".."), "");
        assert_eq!(branch_slug("."), "");
        assert_eq!(branch_slug("..."), "");
        assert_eq!(branch_slug("-..-"), "");
    }

    #[test]
    fn worktree_dir_never_escapes_the_worktrees_dir() {
        // Defense in depth below spec validation: even a dot-only branch maps
        // INSIDE `<repo>.worktrees/` (fallback slug), never to its parent.
        let dir = worktree_dir(Path::new("/home/a/dev/splitlane"), "..");
        assert_eq!(dir, PathBuf::from("/home/a/dev/splitlane.worktrees/branch"));
    }

    #[test]
    fn worktree_dir_is_a_sibling_of_the_repo() {
        let dir = worktree_dir(Path::new("/home/a/dev/splitlane"), "feat/x");
        assert_eq!(dir, PathBuf::from("/home/a/dev/splitlane.worktrees/feat-x"));
        // NOT inside the repo: recursive watchers must not see it.
        assert!(!dir.starts_with("/home/a/dev/splitlane/"));
    }

    #[test]
    fn hashed_worktree_dir_disambiguates_slug_collisions() {
        let repo = Path::new("/home/a/dev/splitlane");
        let a = "feat/a b";
        let b = "feat/a-b";
        assert_eq!(branch_slug(a), branch_slug(b));
        assert_eq!(worktree_dir(repo, a), worktree_dir(repo, b));

        let hashed_a = worktree_dir_hashed(repo, a);
        let hashed_b = worktree_dir_hashed(repo, b);
        assert_ne!(hashed_a, hashed_b);
        assert!(is_splitlane_worktree_dir(repo, a, &hashed_a));
        assert!(is_splitlane_worktree_dir(repo, b, &hashed_b));
        assert!(!hashed_a.starts_with("/home/a/dev/splitlane/"));
    }

    #[test]
    fn parses_worktree_porcelain_with_detached_and_branches() {
        let out = "worktree /home/a/dev/repo\nHEAD 1111111111111111111111111111111111111111\nbranch refs/heads/main\n\nworktree /home/a/dev/repo.worktrees/feat-x\nHEAD 2222222222222222222222222222222222222222\nbranch refs/heads/feat/x\n\nworktree /tmp/detached\nHEAD 3333333333333333333333333333333333333333\ndetached\n";
        let entries = parse_worktree_porcelain(out);
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
        assert_eq!(
            entries[1].path,
            PathBuf::from("/home/a/dev/repo.worktrees/feat-x")
        );
        assert_eq!(entries[1].branch.as_deref(), Some("feat/x"));
        assert_eq!(entries[2].branch, None, "detached HEAD has no branch");
    }

    #[test]
    fn parse_worktree_porcelain_handles_missing_trailing_blank() {
        let out = "worktree /r\nbranch refs/heads/main";
        let entries = parse_worktree_porcelain(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].branch.as_deref(), Some("main"));
    }

    #[test]
    fn managed_worktree_record_requires_marker_and_generated_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        let branch = "feat/hardening";
        let path = worktree_dir(&repo_root, branch);
        std::fs::create_dir_all(&path).expect("worktree dir");

        assert!(
            managed_worktree_from_record(
                &path.to_string_lossy(),
                &repo_root.to_string_lossy(),
                branch,
                "auto",
            )
            .is_none(),
            "a matching path without owner marker is not enough"
        );

        std::fs::write(owner_marker_path(&path), "owner=splitlane\n").expect("marker");
        let restored = managed_worktree_from_record(
            &path.to_string_lossy(),
            &repo_root.to_string_lossy(),
            branch,
            "delete",
        )
        .expect("marker-backed record restores");
        assert_eq!(restored.path, path);
        assert_eq!(restored.teardown, TeardownPolicy::Keep);

        let outside = tmp.path().join("external");
        std::fs::create_dir_all(&outside).expect("outside dir");
        std::fs::write(owner_marker_path(&outside), "owner=splitlane\n").expect("outside marker");
        assert!(
            managed_worktree_from_record(
                &outside.to_string_lossy(),
                &repo_root.to_string_lossy(),
                branch,
                "auto",
            )
            .is_none(),
            "marker cannot bless a path outside the deterministic Splitlane dir"
        );
    }

    #[test]
    fn managed_worktree_record_accepts_hashed_path() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let repo_root = tmp.path().join("repo");
        let branch = "feat/a-b";
        let path = worktree_dir_hashed(&repo_root, branch);
        std::fs::create_dir_all(&path).expect("worktree dir");
        std::fs::write(owner_marker_path(&path), "owner=splitlane\n").expect("marker");

        let restored = managed_worktree_from_record(
            &path.to_string_lossy(),
            &repo_root.to_string_lossy(),
            branch,
            "auto",
        )
        .expect("hashed path restores");

        assert_eq!(restored.path, path);
        assert_eq!(restored.branch, branch);
    }

    #[test]
    fn copy_env_files_copies_top_level_env_only_and_never_clobbers() {
        let src = tempfile::tempdir().expect("src");
        let dst = tempfile::tempdir().expect("dst");
        std::fs::write(src.path().join(".env"), "A=1").unwrap();
        std::fs::write(src.path().join(".env.local"), "B=2").unwrap();
        std::fs::write(src.path().join("notenv"), "x").unwrap();
        // Nested .env must NOT be picked up (top-level only).
        std::fs::create_dir(src.path().join("sub")).unwrap();
        std::fs::write(src.path().join("sub/.env"), "C=3").unwrap();
        // Pre-existing destination file must survive (checkout owns it).
        std::fs::write(dst.path().join(".env"), "KEEP").unwrap();

        let copied = copy_env_files(src.path(), dst.path());
        assert_eq!(copied, vec![".env.local".to_string()]);
        assert_eq!(
            std::fs::read_to_string(dst.path().join(".env")).unwrap(),
            "KEEP",
            "existing destination file is never clobbered"
        );
        assert!(dst.path().join(".env.local").exists());
        assert!(!dst.path().join("notenv").exists());
    }

    #[test]
    fn copy_env_files_missing_source_is_silent_empty() {
        let dst = tempfile::tempdir().expect("dst");
        let copied = copy_env_files(Path::new("/nonexistent-splitlane-test"), dst.path());
        assert!(copied.is_empty());
    }

    /// A command that writes a file, so the test proves *where* setup ran
    /// rather than only that it exited zero. The whole point of the function
    /// is the working directory: a setup that installs into the main checkout
    /// instead of the new worktree would pass every exit-code assertion and
    /// still be the bug.
    #[cfg(unix)]
    const MARKER_CMD: &str = "printf ran > marker.txt";
    #[cfg(windows)]
    const MARKER_CMD: &str = "echo ran> marker.txt";
    #[cfg(unix)]
    const FAILING_CMD: &str = "exit 3";
    #[cfg(windows)]
    const FAILING_CMD: &str = "exit /b 3";

    #[test]
    fn run_setup_runs_inside_the_worktree_and_reports_a_failure() {
        let tree = tempfile::tempdir().expect("tempdir");

        run_setup(tree.path(), MARKER_CMD, Duration::from_secs(30)).expect("setup succeeds");
        assert!(
            tree.path().join("marker.txt").is_file(),
            "setup must run with the worktree as its working directory"
        );

        let failed = run_setup(tree.path(), FAILING_CMD, Duration::from_secs(30))
            .expect_err("a non-zero exit is a failure");
        assert!(
            failed.contains('3'),
            "the failure must name the exit code, not just that there was one: {failed}"
        );
    }

    #[test]
    fn a_blank_remembered_setup_is_no_setup() {
        assert_eq!(remembered_setup("npm ci"), Some("npm ci".to_string()));
        assert_eq!(
            remembered_setup("  make setup  "),
            Some("make setup".to_string()),
            "the stored command is what runs, so it is stored trimmed"
        );
        for blank in ["", "   ", "\n\t "] {
            assert_eq!(
                remembered_setup(blank),
                None,
                "whitespace must not reach a shell, or the field, as a command"
            );
        }
    }

    #[test]
    fn a_failed_setup_says_what_the_command_said() {
        let tree = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        let noisy = "echo 'npm ERR! code ENOENT' >&2; exit 1";
        #[cfg(windows)]
        let noisy = "echo npm ERR! code ENOENT 1>&2& exit /b 1";
        let err = run_setup(tree.path(), noisy, Duration::from_secs(30))
            .expect_err("a non-zero exit is a failure");
        assert!(
            err.contains("ENOENT"),
            "a toast that says only the exit code is not a report: {err}"
        );
    }

    #[test]
    fn run_setup_with_nothing_to_run_spawns_nothing() {
        // A blank field is the ordinary state of the dialog's Setup row, and
        // it must not cost a shell per worktree - nor report a failure when
        // no shell exists to fail.
        let missing = Path::new("/nonexistent-splitlane-setup-cwd");
        for blank in ["", "   ", "\n\t "] {
            assert!(
                run_setup(missing, blank, Duration::from_secs(1)).is_ok(),
                "a blank setup runs nothing, even where the cwd does not exist"
            );
        }
    }

    #[test]
    fn run_setup_is_bounded_by_its_timeout() {
        // The bound is what makes a `setup` that waits for input - a prompt,
        // a hung install - survivable: `splitlane up` hands this the pane's
        // own `setup_timeout_secs`, and the dialog hands it the same default.
        let tree = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        let hang = "sleep 30";
        #[cfg(windows)]
        let hang = "ping -n 30 127.0.0.1 > nul";
        let started = std::time::Instant::now();
        let err = run_setup(tree.path(), hang, Duration::from_millis(300))
            .expect_err("a command past its deadline is a failure");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the timeout must cut the command, not wait it out: {err}"
        );
    }
}

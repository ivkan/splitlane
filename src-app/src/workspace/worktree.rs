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
//! - a worktree with uncommitted changes is NEVER removed, and neither is one
//!   holding a file git would not report - ignored, or hidden from status;
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

use crate::git_command::{self, GitProfile};

/// Wall-clock bound for plumbing git calls (list/status/remove/prune).
const GIT_DEADLINE: Duration = Duration::from_secs(10);
/// `worktree add` checks out a full tree - give it more room on big repos.
const ADD_DEADLINE: Duration = Duration::from_secs(120);
const STDOUT_CAP: u64 = 256 * 1024;
/// `ls-files -v` names every tracked file, so its listing is sized for a
/// large repository rather than for a plumbing answer.
const INDEX_LISTING_CAP: u64 = 64 * 1024 * 1024;
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
    /// Branch checked out in the worktree.
    pub branch: String,
    pub teardown: TeardownPolicy,
    /// The commit the branch was made at, when Splitlane made the branch for
    /// this worktree. Teardown deletes such a branch only while it still
    /// points there: nobody committed on it, so deleting it loses nothing.
    /// `None` for a branch somebody named - that one is never deleted.
    pub base: Option<String>,
    /// The first of the ten ports this worktree's sessions are given, as
    /// `SPLITLANE_PORT`, so two worktrees of one project can each run a dev
    /// server. Kept for the worktree's life, because a setup command may
    /// have written it into a file.
    pub port: Option<u16>,
    /// The project's branch the worktree's branch was made from, for saying
    /// so: `splitlane/ab12cd · from main`.
    pub from: Option<String>,
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
        base: None,
        port: None,
        from: None,
    })
}

/// Whether `raw` reads as a full commit id (SHA-1 or SHA-256), the only thing
/// a stored [`ManagedWorktree::base`] may be.
pub fn is_commit_id(raw: &str) -> bool {
    matches!(raw.len(), 40 | 64) && raw.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The variable a worktree's sessions and its setup command find its first
/// port in. Not `PORT`: that one changes what many tools do without being
/// asked, and a person opts into it by writing `PORT=$SPLITLANE_PORT`.
pub const PORT_VAR: &str = "SPLITLANE_PORT";

/// Where worktree ports start. Below the range the operating systems hand
/// out to outgoing connections (49152 and up), and above the 3000s a
/// `splitlane up` preset counts from by default.
const PORT_BASE: u16 = 4000;
/// The highest first port handed out, so a block of ten stays below 49152.
const PORT_LAST: u16 = 49140;
/// Ports per worktree: a dev server and the few services beside it.
pub const PORT_BLOCK: u16 = 10;

/// The first port of a free block of [`PORT_BLOCK`] for a new worktree. A
/// block another worktree holds is never given out again while it exists,
/// whether or not its server is running: a stopped server leaves its ports
/// looking free, and the next worktree would take them. A block something
/// else is listening in is skipped too. Blocking: it binds to probe.
pub fn allocate_port(held: &[u16]) -> Option<u16> {
    let mut first = PORT_BASE;
    while first <= PORT_LAST {
        if !port_block_overlaps(first, held) && (first..first + PORT_BLOCK).all(port_is_free) {
            return Some(first);
        }
        first += PORT_BLOCK;
    }
    None
}

/// Whether nothing listens on `port`, on IPv4 or IPv6 - a dev server may
/// take either. A machine without IPv6 answers for IPv4 alone.
fn port_is_free(port: u16) -> bool {
    use std::io::ErrorKind;
    if std::net::TcpListener::bind((std::net::Ipv4Addr::UNSPECIFIED, port)).is_err() {
        return false;
    }
    match std::net::TcpListener::bind((std::net::Ipv6Addr::UNSPECIFIED, port)) {
        Ok(_) => true,
        Err(e) => matches!(
            e.kind(),
            ErrorKind::AddrNotAvailable | ErrorKind::Unsupported
        ),
    }
}

/// Whether the block starting at `first` overlaps one in `held`.
pub fn port_block_overlaps(first: u16, held: &[u16]) -> bool {
    held.iter()
        .any(|other| first < other.saturating_add(PORT_BLOCK) && *other < first + PORT_BLOCK)
}

/// The environment a session running in `cwd` gets from the worktree it is
/// in: [`PORT_VAR`] when that worktree holds a port.
pub fn port_env<'a>(
    cwd: &Path,
    worktrees: impl IntoIterator<Item = (&'a Path, Option<u16>)>,
) -> Option<std::collections::HashMap<String, String>> {
    worktrees
        .into_iter()
        .find(|(path, _)| cwd.starts_with(path))
        .and_then(|(_, port)| port)
        .map(|port| std::collections::HashMap::from([(PORT_VAR.to_string(), port.to_string())]))
}

/// The git config key that marks a branch as one this app made for a session.
pub fn session_branch_mark_key(branch: &str) -> String {
    format!("branch.{branch}.splitlane-session")
}

/// The prefix of the branches new sessions are given.
pub const SESSION_BRANCH_PREFIX: &str = "splitlane/";

/// Make a new session its own worktree, on a new branch off the checkout's
/// `HEAD`, and say so in the record: [`ManagedWorktree::base`] is set, so the
/// branch goes with the worktree if nothing was committed on it. Blocking.
pub fn add_session_worktree(repo_root: &Path) -> Result<ManagedWorktree, String> {
    add_session_worktree_at(repo_root, "HEAD")
}

/// [`add_session_worktree`] with the new branch made at `start` rather than
/// at `HEAD`.
pub fn add_session_worktree_at(repo_root: &Path, start: &str) -> Result<ManagedWorktree, String> {
    for _ in 0..8 {
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let branch = format!("{SESSION_BRANCH_PREFIX}{}", &suffix[..6]);
        let path = worktree_dir(repo_root, &branch);
        if path.exists() || branch_exists(repo_root, &branch) {
            continue;
        }
        add_worktree_from(repo_root, &path, &branch, Some(start))?;
        // Marked as this app's, so nothing later mistakes a branch somebody
        // named `splitlane/...` for one it may delete.
        let _ = run_git(
            GitProfile::UserAction,
            repo_root,
            &["config", &session_branch_mark_key(&branch), "true"],
            GIT_DEADLINE,
        );
        let base = run_git(
            GitProfile::Probe,
            &path,
            &["rev-parse", "HEAD"],
            GIT_DEADLINE,
        )
        .ok()
        .filter(|id| is_commit_id(id));
        if base.is_none() {
            log::warn!(
                "session worktree: no base commit read for {branch}; the branch will be kept"
            );
        }
        return Ok(ManagedWorktree {
            path,
            repo_root: repo_root.to_path_buf(),
            branch,
            teardown: TeardownPolicy::Auto,
            base,
            port: None,
            from: None,
        });
    }
    Err("no free branch name was found".to_string())
}

/// Delete a branch Splitlane made, if it still points at the commit it was
/// made at. `update-ref` compares and deletes in one step, so a commit made
/// between a check and the deletion cannot be lost. Returns whether it went.
fn delete_branch_if_untouched(wt: &ManagedWorktree) -> bool {
    let Some(base) = wt.base.as_deref().filter(|id| is_commit_id(id)) else {
        return false;
    };
    // Only a branch of the shape this app makes, and only one no worktree has
    // checked out: `update-ref` would delete a checked-out branch from under
    // its checkout, and a record read from disk can name any branch.
    if !wt.branch.starts_with(SESSION_BRANCH_PREFIX) {
        return false;
    }
    let Ok(entries) = list_worktrees(&wt.repo_root) else {
        return false;
    };
    if entries
        .iter()
        .any(|entry| entry.branch.as_deref() == Some(wt.branch.as_str()))
    {
        return false;
    }
    run_git(
        GitProfile::UserAction,
        &wt.repo_root,
        &[
            "update-ref",
            "-d",
            &format!("refs/heads/{}", wt.branch),
            base,
        ],
        GIT_DEADLINE,
    )
    .is_ok()
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

/// `<repo>.worktrees`, the directory every branch's worktree is made in.
pub fn worktrees_parent(repo_root: &Path) -> PathBuf {
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
pub(crate) fn run_git(
    profile: GitProfile,
    repo: &Path,
    args: &[&str],
    deadline: Duration,
) -> Result<String, String> {
    let out = git_command::run(profile, repo, args, deadline, STDOUT_CAP)
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
        GitProfile::Probe,
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
        GitProfile::Probe,
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
    if create_branch {
        add_worktree_from(repo_root, path, branch, None)
    } else {
        add_worktree_inner(repo_root, path, branch, false, None)
    }
}

/// A worktree on a new `branch` made at `start` (`HEAD` when `None`).
fn add_worktree_from(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    start: Option<&str>,
) -> Result<(), String> {
    add_worktree_inner(repo_root, path, branch, true, start)
}

fn add_worktree_inner(
    repo_root: &Path,
    path: &Path,
    branch: &str,
    create_branch: bool,
    start: Option<&str>,
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
    if let Some(start) = start {
        args.push(start);
    }
    // Creating a worktree is the person's request, so the repository's
    // checkout hooks and filters run as they would from their own shell.
    run_git(GitProfile::UserAction, repo_root, &args, ADD_DEADLINE)?;
    if let Err(e) = write_owner_marker(path, repo_root, branch) {
        let _ = remove_worktree(repo_root, path);
        return Err(e);
    }
    Ok(())
}

/// True when removing the worktree would lose nothing: every file in it is
/// either committed or something Splitlane put there itself.
///
/// A plain `git status --porcelain` is not that question. It leaves out
/// ignored files, it leaves out untracked files when the repository sets
/// `status.showUntrackedFiles=no`, and it leaves out edits to a file marked
/// assume-unchanged or skip-worktree - and `git worktree remove` asks git the
/// same plain question, so it deletes all three without a word. An ignored
/// `.env` an agent filled in is exactly the file nobody committed.
///
/// So this asks for everything, and then excuses two things: Splitlane's
/// marker file, and an ignored file or link that is still what the checkout
/// holds at the same path - what [`copy_local_files`] made, unchanged.
/// Anything else keeps the worktree, an installed
/// dependency directory included: it cannot be told from a directory of
/// notes, and a kept directory can be deleted by hand while a deleted one
/// cannot be brought back.
///
/// A submodule's own ignored files are not listed either, and need not be:
/// git refuses to remove a worktree that has a submodule checked out.
///
/// An error (worktree gone, git missing, a listing too long to read in full,
/// files hidden from status) is NOT "clean" - the caller must keep its hands
/// off when it cannot prove cleanliness.
pub fn is_clean(worktree_path: &Path, repo_root: &Path) -> Result<bool, String> {
    let status = git_command::run(
        GitProfile::Probe,
        worktree_path,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=all",
            "--ignored=matching",
            "--ignore-submodules=none",
        ],
        GIT_DEADLINE,
        STDOUT_CAP,
    )
    .map_err(|e| format!("git status failed: {e}"))?;
    if !status.status.success() {
        let stderr = String::from_utf8_lossy(&status.stderr);
        return Err(format!(
            "git status failed: {}",
            stderr.trim().lines().last().unwrap_or("non-zero exit")
        ));
    }
    // A cut listing ends mid-record; the records before the cut are whole,
    // and one that holds work settles the answer without the rest.
    let mut records: Vec<&[u8]> = status.stdout.split(|byte| *byte == 0).collect();
    records.pop();
    if records
        .iter()
        .any(|record| !is_splitlanes_own(record, worktree_path, repo_root))
    {
        return Ok(false);
    }
    if status.stdout_truncated {
        return Err("git status listed more than could be read".to_string());
    }

    let index = git_command::run(
        GitProfile::Probe,
        worktree_path,
        &["ls-files", "-v", "-z"],
        GIT_DEADLINE,
        INDEX_LISTING_CAP,
    )
    .map_err(|e| format!("git ls-files failed: {e}"))?;
    if !index.status.success() || index.stdout_truncated {
        return Err("the index could not be listed in full".to_string());
    }
    // `-v` tags each entry: lowercase for assume-unchanged, `S` for
    // skip-worktree. Either means status did not look at the file.
    let hidden = index
        .stdout
        .split(|byte| *byte == 0)
        .filter(|entry| {
            entry
                .first()
                .is_some_and(|tag| tag.is_ascii_lowercase() || *tag == b'S')
        })
        .count();
    if hidden > 0 {
        return Err(format!(
            "{hidden} tracked file(s) are marked assume-unchanged or skip-worktree, \
             so their edits cannot be seen"
        ));
    }
    Ok(true)
}

/// Whether one `status --porcelain -z` record names a file Splitlane itself
/// put in the worktree. A record is `XY <path>`.
fn is_splitlanes_own(record: &[u8], worktree_path: &Path, repo_root: &Path) -> bool {
    let Some((state, path)) = record.split_at_checked(3) else {
        return false;
    };
    let Ok(path) = std::str::from_utf8(path) else {
        return false;
    };
    match state {
        b"?? " | b"!! " if path == OWNER_MARKER_FILE => true,
        // Only an ignored copy: git refuses to remove a worktree that holds
        // an untracked file, so excusing one here would promise a removal
        // that then does not happen. Any ignored file or link that is still
        // what the checkout holds at the same path loses nothing with the
        // worktree - which covers everything `copy_local_files` put there
        // and nobody changed. A directory is reported whole and is not
        // compared, so one holds the worktree back.
        b"!! " if !path.ends_with('/') => {
            is_unchanged_copy(&repo_root.join(path), &worktree_path.join(path))
        }
        _ => false,
    }
}

pub(crate) fn is_unchanged_copy(source: &Path, copy: &Path) -> bool {
    let (Ok(source_meta), Ok(copy_meta)) = (
        std::fs::symlink_metadata(source),
        std::fs::symlink_metadata(copy),
    ) else {
        return false;
    };
    if source_meta.file_type().is_symlink() && copy_meta.file_type().is_symlink() {
        return matches!(
            (link_destination(source), link_destination(copy)),
            (Some(a), Some(b)) if a == b
        );
    }
    // A changed mode is a change too: `chmod +x` on a copy is somebody's work.
    if !source_meta.is_file()
        || !copy_meta.is_file()
        || source_meta.len() != copy_meta.len()
        || source_meta.permissions() != copy_meta.permissions()
    {
        return false;
    }
    match (std::fs::read(source), std::fs::read(copy)) {
        (Ok(source), Ok(copy)) => source == copy,
        _ => false,
    }
}

/// `git worktree remove <path>`. Refuses dirty worktrees by itself too (git
/// native), but callers must check [`is_clean`] first to control messaging.
/// The branch stays: only [`teardown_one`] deletes one, and only a branch
/// Splitlane made that nobody committed on.
pub fn remove_worktree(repo_root: &Path, path: &Path) -> Result<(), String> {
    let path_s = path.to_string_lossy();
    run_git(
        GitProfile::UserAction,
        repo_root,
        &["worktree", "remove", &path_s],
        GIT_DEADLINE,
    )
    .map(|_| ())
}

/// Remove a worktree [`is_clean`] has just vouched for.
///
/// Splitlane's marker is an untracked file, and git refuses to remove a
/// worktree that holds one - so with the marker in place no managed worktree
/// was ever removed. It goes first, and comes back if git still says no: a
/// worktree that stays must stay Splitlane's, or the next close would not
/// try again.
fn remove_clean_worktree(repo_root: &Path, path: &Path) -> Result<(), String> {
    let marker = owner_marker_path(path);
    let contents = std::fs::read(&marker)
        .map_err(|e| format!("cannot read owner marker {}: {e}", marker.display()))?;
    std::fs::remove_file(&marker)
        .map_err(|e| format!("cannot remove owner marker {}: {e}", marker.display()))?;
    // Asked again, as late as it can be asked: git's own check before the
    // removal does not look at ignored files, so a file an agent wrote while
    // its pane was closing is only ever seen here. This narrows that gap; it
    // does not close it.
    let removed = match is_clean(path, repo_root) {
        Ok(true) => remove_worktree(repo_root, path),
        Ok(false) => Err("files appeared while the worktree was being removed".to_string()),
        Err(e) => Err(e),
    };
    if removed.is_err()
        && path.is_dir()
        && let Err(e) = std::fs::write(&marker, &contents)
    {
        log::warn!(
            "worktree kept without its marker, so it will not be removed later ({}): {e}",
            path.display()
        );
    }
    removed
}

/// `git worktree prune` - drops references whose directory no longer exists.
/// Git-native guarantee: a worktree whose directory still exists is untouched,
/// so this is safe to run blindly at startup.
pub fn prune(repo_root: &Path) -> Result<(), String> {
    run_git(
        GitProfile::UserAction,
        repo_root,
        &["worktree", "prune"],
        GIT_DEADLINE,
    )
    .map(|_| ())
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
/// `add_worktree` and `copy_local_files` rather than in either caller.
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
    run_setup_with_port(worktree_path, None, setup, timeout)
}

/// [`run_setup`] with the worktree's [`PORT_VAR`] set, so a setup command can
/// write the port into the file the project's dev server reads.
pub fn run_setup_with_port(
    worktree_path: &Path,
    port: Option<u16>,
    setup: &str,
    timeout: Duration,
) -> Result<(), String> {
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
    if let Some(port) = port {
        cmd.env(PORT_VAR, port.to_string());
    }
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

/// The file in a checkout's root that lists what else a new worktree of it
/// needs from the checkout: one gitignore-syntax entry per line, relative to
/// the root, `#` for a comment. The name and the syntax are Claude Code's, so
/// one file serves both.
pub const WORKTREE_INCLUDE_FILE: &str = ".worktreeinclude";

/// Files copied into every new worktree when the checkout has them, with or
/// without a [`WORKTREE_INCLUDE_FILE`]: the agents' own local instructions.
/// They are kept out of git by design, so a worktree would otherwise start
/// without the notes its agent is told to read.
const LOCAL_INSTRUCTION_FILES: &[&str] = &["CLAUDE.local.md", "AGENTS.override.md"];

/// How deep a listed directory is copied. A list entry names a few things a
/// person keeps beside the code, not a tree to mirror.
const MAX_COPY_DEPTH: u32 = 16;

/// Copy what a new worktree needs from the checkout but git does not bring:
/// the top-level `.env*` files, [`LOCAL_INSTRUCTION_FILES`], and every path
/// the checkout's [`WORKTREE_INCLUDE_FILE`] lists. A symbolic link is copied as
/// a link. Nothing that already exists in the worktree is replaced - a tracked
/// file arrived with the checkout and is the branch's own. Best-effort: what
/// cannot be read or written is skipped. Returns the relative paths copied,
/// sorted.
///
/// A plain path in the list is looked up directly. A pattern is matched
/// against what git reports as ignored and untracked in the checkout, with
/// ignored directories reported whole, so a build directory is not walked.
pub fn copy_local_files(src_root: &Path, dst_root: &Path) -> Vec<String> {
    let mut copied = Vec::new();
    let ignored = ignored_untracked(src_root);
    let mut wanted: Vec<String> = LOCAL_INSTRUCTION_FILES
        .iter()
        .map(|name| name.to_string())
        .chain(top_level_env_files(src_root))
        .collect();
    if let Ok(list) = std::fs::read_to_string(src_root.join(WORKTREE_INCLUDE_FILE)) {
        wanted.extend(included_paths(src_root, &list, &ignored));
    }
    wanted.sort();
    wanted.dedup();
    for relative in wanted {
        // Only what git ignores. A tracked path arrived with the checkout,
        // and an untracked copy would make git refuse to remove the worktree
        // for good, so the project's close could never clean it up.
        if !is_ignored(&relative, &ignored) {
            continue;
        }
        let source = src_root.join(&relative);
        if copy_entry(dst_root, &source, &dst_root.join(&relative), MAX_COPY_DEPTH) {
            copied.push(relative);
        }
    }
    copied.sort();
    copied.dedup();
    copied
}

/// The checkout-relative paths a [`WORKTREE_INCLUDE_FILE`] names. A line that
/// names an existing path is that path, even if it holds a character a
/// pattern would read; any other line with `*`, `?`, `[` or a leading `!` is
/// a pattern, matched against `ignored`.
fn included_paths(src_root: &Path, list: &str, ignored: &[(String, bool)]) -> Vec<String> {
    let mut literal = Vec::new();
    let mut patterns = ignore::gitignore::GitignoreBuilder::new(src_root);
    let mut has_patterns = false;
    for line in list.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let relative = line.trim_start_matches('/').trim_end_matches('/');
        let names_a_path = is_safe_relative(relative)
            && std::fs::symlink_metadata(src_root.join(relative)).is_ok();
        if !names_a_path && (line.contains(['*', '?', '[']) || line.starts_with('!')) {
            has_patterns |= patterns.add_line(None, line).is_ok();
        } else if is_safe_relative(relative) {
            literal.push(relative.to_string());
        }
    }
    if has_patterns && let Ok(matcher) = patterns.build() {
        literal.extend(
            ignored
                .iter()
                .filter(|(path, is_dir)| matcher.matched(path, *is_dir).is_ignore())
                .map(|(path, _)| path.clone()),
        );
    }
    literal
}

/// Whether git ignores `relative`, itself or as part of an ignored directory.
fn is_ignored(relative: &str, ignored: &[(String, bool)]) -> bool {
    ignored.iter().any(|(path, is_dir)| {
        relative == path || (*is_dir && Path::new(relative).starts_with(path))
    })
}

/// A relative path that stays inside the root it is joined to.
fn is_safe_relative(path: &str) -> bool {
    !path.is_empty()
        && Path::new(path)
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// What git reports as ignored and untracked in `root`, a directory reported
/// once rather than walked, with whether each is a directory.
fn ignored_untracked(root: &Path) -> Vec<(String, bool)> {
    let Ok(out) = git_command::run(
        GitProfile::Probe,
        root,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
            "-z",
        ],
        GIT_DEADLINE,
        STDOUT_CAP,
    ) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    out.stdout
        .split(|byte| *byte == 0)
        .filter_map(|entry| std::str::from_utf8(entry).ok())
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            let is_dir = entry.ends_with('/');
            (entry.trim_end_matches('/').to_string(), is_dir)
        })
        .filter(|(path, _)| is_safe_relative(path))
        .collect()
}

/// Whether writing `dst` stays inside `root`: no directory on the way down
/// from `root` is a link. The branch's own tree can hold a tracked link where
/// the checkout has an ignored directory, and writing through it would put
/// the copy wherever the link points.
fn lands_inside(root: &Path, dst: &Path) -> bool {
    let Ok(rest) = dst.strip_prefix(root) else {
        return false;
    };
    let parts: Vec<_> = rest.components().collect();
    let mut at = root.to_path_buf();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => return false,
            Ok(_) => {}
            // The rest is made by this copy, as directories.
            Err(_) => break,
        }
    }
    true
}

/// Copy one file, link or directory to `dst` inside `root`. Nothing that
/// already exists is replaced. Returns whether anything was copied.
fn copy_entry(root: &Path, src: &Path, dst: &Path, depth_left: u32) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(src) else {
        return false;
    };
    if std::fs::symlink_metadata(dst).is_ok() || !lands_inside(root, dst) {
        return false;
    }
    if let Some(parent) = dst.parent()
        && std::fs::create_dir_all(parent).is_err()
    {
        return false;
    }
    if meta.file_type().is_symlink() {
        return copy_symlink(src, dst);
    }
    if meta.is_file() {
        return copy_new_file(src, dst, meta.permissions());
    }
    if !meta.is_dir() || depth_left == 0 || std::fs::create_dir(dst).is_err() {
        return false;
    }
    if let Ok(entries) = std::fs::read_dir(src) {
        for entry in entries.flatten() {
            copy_entry(
                root,
                &entry.path(),
                &dst.join(entry.file_name()),
                depth_left - 1,
            );
        }
    }
    true
}

/// Copy a file to a path that must not exist, with the source's permissions.
fn copy_new_file(src: &Path, dst: &Path, permissions: std::fs::Permissions) -> bool {
    let (Ok(mut from), Ok(mut to)) = (
        std::fs::File::open(src),
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dst),
    ) else {
        return false;
    };
    std::io::copy(&mut from, &mut to).is_ok() && std::fs::set_permissions(dst, permissions).is_ok()
}

/// Where a link points, as a path that means the same from anywhere: a
/// relative target is read from the link's own directory.
fn link_destination(link: &Path) -> Option<PathBuf> {
    let target = std::fs::read_link(link).ok()?;
    Some(if target.is_relative() {
        link.parent()?.join(target)
    } else {
        target
    })
}

/// A link is recreated pointing at the same place, so a link to notes kept
/// elsewhere reaches the same notes - a relative one too, though the worktree
/// sits at another depth. Windows only lets a privileged or developer-mode
/// process create a link; there it is skipped.
fn copy_symlink(src: &Path, dst: &Path) -> bool {
    let Some(target) = link_destination(src) else {
        return false;
    };
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, dst).is_ok()
    }
    #[cfg(windows)]
    {
        if target.is_dir() {
            std::os::windows::fs::symlink_dir(&target, dst).is_ok()
        } else {
            std::os::windows::fs::symlink_file(&target, dst).is_ok()
        }
    }
}

/// The names of the top-level `.env*` files in `src_root`.
fn top_level_env_files(src_root: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(src_root) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".env"))
        .collect()
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
/// work); clean → remove. A branch somebody named is never touched; a branch
/// Splitlane made for a session goes too when nothing was committed on it
/// ([`ManagedWorktree::base`]), and stays with its commits otherwise.
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
        teardown_one(&wt, open_containers);
    }
}

/// What tearing one worktree down came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Teardown {
    /// The worktree is gone, and with it its branch when that was Splitlane's
    /// and nothing was committed on it.
    Removed { branch_deleted: bool },
    /// The worktree stays, for the reason given.
    Kept(String),
}

/// Tear one managed worktree down, under the rules [`teardown_all`] states.
/// Blocking.
pub fn teardown_one(wt: &ManagedWorktree, open_containers: &[PathBuf]) -> Teardown {
    if wt.teardown == TeardownPolicy::Keep {
        return Teardown::Kept("it is set to be kept".to_string());
    }
    if let Some(open) = open_containers
        .iter()
        .find(|open| contains_or_equals(&wt.path, open))
    {
        log::info!("worktree kept: open as a container ({})", open.display());
        return Teardown::Kept("it is open as a project".to_string());
    }
    if !wt.path.exists() {
        // Directory already gone (user rm -rf'd it): just prune the ref.
        let _ = prune(&wt.repo_root);
        let branch_deleted = delete_branch_if_untouched(wt);
        return Teardown::Removed { branch_deleted };
    }
    if !has_owner_marker(&wt.path) {
        log::warn!(
            "worktree kept: missing Splitlane owner marker in {}",
            wt.path.display()
        );
        return Teardown::Kept("Splitlane's marker is missing from it".to_string());
    }
    match is_clean(&wt.path, &wt.repo_root) {
        Ok(true) => match remove_clean_worktree(&wt.repo_root, &wt.path) {
            Ok(()) => {
                log::info!("worktree removed: {}", wt.path.display());
                let branch_deleted = delete_branch_if_untouched(wt);
                Teardown::Removed { branch_deleted }
            }
            Err(e) => {
                log::warn!("worktree kept ({}): {e}", wt.path.display());
                Teardown::Kept(e)
            }
        },
        Ok(false) => {
            log::warn!(
                "worktree kept: uncommitted or ignored files in {}",
                wt.path.display()
            );
            Teardown::Kept("it holds changes nobody committed".to_string())
        }
        Err(e) => {
            log::warn!(
                "worktree kept (cannot verify cleanliness): {} - {e}",
                wt.path.display()
            );
            Teardown::Kept(format!("its state could not be read: {e}"))
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

    fn test_git(cwd: &Path, args: &[&str]) {
        let out = git_command::run(GitProfile::UserAction, cwd, args, ADD_DEADLINE, STDOUT_CAP)
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository that ignores `.env` and `build/`, holding an ignored
    /// `.env`, with one managed worktree made the way both doors make it.
    fn repo_with_managed_worktree(name: &str) -> (PathBuf, ManagedWorktree) {
        let tmp =
            std::env::temp_dir().join(format!("splitlane-teardown-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        test_git(&repo, &["init", "-q"]);
        test_git(&repo, &["config", "user.email", "test@example.com"]);
        test_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join(".gitignore"), ".env\nbuild/\n").expect("gitignore");
        std::fs::write(repo.join("a.txt"), "one\n").expect("file");
        test_git(&repo, &["add", "."]);
        test_git(&repo, &["commit", "-q", "-m", "init"]);
        std::fs::write(repo.join(".env"), "KEY=source\n").expect("env");

        let path = worktree_dir(&repo, "feat");
        add_worktree(&repo, &path, "feat", true).expect("worktree");
        assert_eq!(copy_local_files(&repo, &path), vec![".env".to_string()]);
        let managed = ManagedWorktree {
            path,
            repo_root: repo,
            branch: "feat".to_string(),
            teardown: TeardownPolicy::Auto,
            base: None,
            port: None,
            from: None,
        };
        (tmp, managed)
    }

    /// A repository with local files beside the code, a `.worktreeinclude`
<<<<<<< HEAD
    /// naming some of them, and a fresh worktree of it.
=======
    /// naming some of them, and a fresh worktree of it. Only the Unix tests
    /// use it: they make symlinks, which Windows hands out by privilege.
    #[cfg(unix)]
>>>>>>> origin/main
    fn repo_with_local_files(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let tmp = std::env::temp_dir().join(format!(
            "splitlane-local-files-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        let elsewhere = tmp.join("notes-kept-elsewhere");
        std::fs::create_dir_all(&repo).expect("repo dir");
        std::fs::create_dir_all(&elsewhere).expect("notes dir");
        test_git(&repo, &["init", "-q"]);
        test_git(&repo, &["config", "user.email", "test@example.com"]);
        test_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(
            repo.join(".gitignore"),
            ".env\nCLAUDE.local.md\nnotes\nsecrets/\n*.pem\nbuild/\n.worktreeinclude\n",
        )
        .expect("gitignore");
        std::fs::write(repo.join("README.md"), "tracked\n").expect("file");
        test_git(&repo, &["add", "."]);
        test_git(&repo, &["commit", "-q", "-m", "init"]);
        std::fs::write(repo.join(".env"), "KEY=source\n").expect("env");
        std::fs::write(repo.join("CLAUDE.local.md"), "read the notes\n").expect("local");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&elsewhere, repo.join("notes")).expect("link");
        std::fs::create_dir_all(repo.join("secrets")).expect("secrets");
        std::fs::write(repo.join("secrets").join("api.json"), "{}").expect("secret");
        std::fs::write(repo.join("key.pem"), "pem").expect("pem");
        // Untracked and not ignored: copying it would keep git from ever
        // removing the worktree.
        std::fs::write(repo.join("AGENTS.override.md"), "local\n").expect("agents");
        std::fs::create_dir_all(repo.join("build")).expect("build");
        std::fs::write(repo.join("build").join("out.bin"), "big").expect("build");
        std::fs::write(
            repo.join(WORKTREE_INCLUDE_FILE),
            "# what a worktree needs\nnotes\n/secrets/\n*.pem\n../outside\n",
        )
        .expect("include");
        let path = worktree_dir(&repo, "feat");
        add_worktree(&repo, &path, "feat", true).expect("worktree");
        (tmp, repo, path)
    }

    #[cfg(unix)]
    #[test]
    fn local_files_are_copied_and_nothing_else() {
        let (tmp, repo, path) = repo_with_local_files("copy");
        // Tracked, so already there: it must not be replaced.
        std::fs::write(path.join("README.md"), "the branch's own\n").expect("edit");

        let copied = copy_local_files(&repo, &path);

        assert_eq!(
            copied,
            vec![".env", "CLAUDE.local.md", "key.pem", "notes", "secrets"]
        );
        assert_eq!(
            std::fs::read_link(path.join("notes")).expect("a link"),
            repo.parent().expect("tmp").join("notes-kept-elsewhere")
        );
        assert!(path.join("secrets").join("api.json").is_file());
        assert!(
            !path.join("build").exists(),
            "an unlisted ignored directory stays"
        );
        assert_eq!(
            std::fs::read_to_string(path.join("README.md")).expect("read"),
            "the branch's own\n"
        );
        assert!(!tmp.join("outside").exists());
        assert!(
            !path.join("AGENTS.override.md").exists(),
            "not ignored, not copied"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_never_writes_through_a_link_in_the_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (src, dst, outside) = (
            tmp.path().join("src"),
            tmp.path().join("dst"),
            tmp.path().join("outside"),
        );
        for dir in [&src.join("vault"), &dst, &outside] {
            std::fs::create_dir_all(dir).expect("mkdir");
        }
        std::fs::write(src.join("vault").join("key.pem"), "secret").expect("write");
        // The branch tracks `vault` as a link where the checkout has a folder.
        std::os::unix::fs::symlink(&outside, dst.join("vault")).expect("link");

        assert!(!copy_entry(
            &dst,
            &src.join("vault").join("key.pem"),
            &dst.join("vault").join("key.pem"),
            MAX_COPY_DEPTH
        ));
        assert!(!outside.join("key.pem").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_relative_link_still_reaches_its_target_from_the_worktree() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let src = tmp.path().join("repo");
        let dst = tmp.path().join("repo.worktrees").join("feat");
        std::fs::create_dir_all(tmp.path().join("shared")).expect("shared");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");
        std::os::unix::fs::symlink("../shared", src.join("notes")).expect("link");

        assert!(copy_entry(
            &dst,
            &src.join("notes"),
            &dst.join("notes"),
            MAX_COPY_DEPTH
        ));

        assert!(
            dst.join("notes").join(".").is_dir(),
            "the copy reaches the folder"
        );
        assert!(is_unchanged_copy(&src.join("notes"), &dst.join("notes")));
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_whose_mode_changed_is_not_unchanged() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tempdir");
        let (src, dst) = (tmp.path().join("run.sh"), tmp.path().join("copy.sh"));
        std::fs::write(&src, "echo hi\n").expect("write");
        std::fs::write(&dst, "echo hi\n").expect("write");
        std::fs::set_permissions(&src, std::fs::Permissions::from_mode(0o644)).expect("mode");
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o644)).expect("mode");
        assert!(is_unchanged_copy(&src, &dst));
        std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(0o755)).expect("mode");
        assert!(!is_unchanged_copy(&src, &dst));
    }

    /// A committed repository, for the session-worktree tests.
    fn committed_repo(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "splitlane-session-wt-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).expect("repo dir");
        test_git(&repo, &["init", "-q"]);
        test_git(&repo, &["config", "user.email", "test@example.com"]);
        test_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "one\n").expect("file");
        test_git(&repo, &["add", "."]);
        test_git(&repo, &["commit", "-q", "-m", "init"]);
        repo
    }

    #[test]
    fn an_untouched_session_worktree_goes_with_its_branch() {
        let repo = committed_repo("untouched");
        let made = add_session_worktree(&repo).expect("made");
        assert!(made.branch.starts_with(SESSION_BRANCH_PREFIX));
        assert!(made.base.as_deref().is_some_and(is_commit_id));
        assert!(made.path.is_dir());

        assert_eq!(
            teardown_one(&made, &[]),
            Teardown::Removed {
                branch_deleted: true
            }
        );
        assert!(!made.path.exists());
        assert!(!branch_exists(&repo, &made.branch));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn a_session_branch_with_a_commit_stays() {
        let repo = committed_repo("committed");
        let made = add_session_worktree(&repo).expect("made");
        std::fs::write(made.path.join("b.txt"), "two\n").expect("file");
        test_git(&made.path, &["add", "b.txt"]);
        test_git(&made.path, &["commit", "-q", "-m", "work"]);

        assert_eq!(
            teardown_one(&made, &[]),
            Teardown::Removed {
                branch_deleted: false
            }
        );
        assert!(
            branch_exists(&repo, &made.branch),
            "a commit keeps its branch"
        );
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn a_session_worktree_with_an_edit_stays_with_its_branch() {
        let repo = committed_repo("edited");
        let made = add_session_worktree(&repo).expect("made");
        std::fs::write(made.path.join("a.txt"), "changed\n").expect("edit");

        assert!(matches!(teardown_one(&made, &[]), Teardown::Kept(_)));
        assert!(made.path.join("a.txt").is_file());
        assert!(branch_exists(&repo, &made.branch));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn a_named_branch_is_never_deleted() {
        let repo = committed_repo("named");
        let mut made = add_session_worktree(&repo).expect("made");
        made.base = None;
        assert_eq!(
            teardown_one(&made, &[]),
            Teardown::Removed {
                branch_deleted: false
            }
        );
        assert!(branch_exists(&repo, &made.branch));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    /// A record read from disk can name any branch with its current commit
    /// as the base. Only a branch of this app's shape that nothing has
    /// checked out may go.
    #[test]
    fn a_record_naming_main_never_deletes_it() {
        let repo = committed_repo("record-main");
        let tip = run_git(
            GitProfile::Probe,
            &repo,
            &["rev-parse", "HEAD"],
            GIT_DEADLINE,
        )
        .expect("tip");
        let current = run_git(
            GitProfile::Probe,
            &repo,
            &["rev-parse", "--abbrev-ref", "HEAD"],
            GIT_DEADLINE,
        )
        .expect("branch");
        let forged = ManagedWorktree {
            path: worktree_dir(&repo, "gone"),
            repo_root: repo.clone(),
            branch: current.clone(),
            teardown: TeardownPolicy::Auto,
            base: Some(tip.clone()),
            port: None,
            from: None,
        };
        assert_eq!(
            teardown_one(&forged, &[]),
            Teardown::Removed {
                branch_deleted: false
            }
        );
        assert!(branch_exists(&repo, &current));
        // Nor a session-shaped branch somebody has checked out.
        test_git(&repo, &["branch", "splitlane/zz99"]);
        test_git(&repo, &["checkout", "-q", "splitlane/zz99"]);
        let checked_out = ManagedWorktree {
            branch: "splitlane/zz99".to_string(),
            ..forged
        };
        teardown_one(&checked_out, &[]);
        assert!(branch_exists(&repo, "splitlane/zz99"));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn ports_are_handed_out_in_blocks_no_worktree_shares() {
        let first = allocate_port(&[]).expect("a block");
        assert!((PORT_BASE..=PORT_LAST).contains(&first));
        assert_eq!(first % PORT_BLOCK, 0);
        let second = allocate_port(&[first]).expect("another block");
        assert!(second >= first + PORT_BLOCK || second + PORT_BLOCK <= first);
        // A block held but not listening on is still not handed out.
        let held: Vec<u16> = (PORT_BASE..PORT_BASE + 50).step_by(10).collect();
        assert!(allocate_port(&held).expect("block") >= PORT_BASE + 50);
    }

    #[test]
    fn a_session_in_a_worktree_with_a_port_gets_it() {
        let worktree = Path::new("/work/app.worktrees/splitlane-ab12");
        let env = port_env(
            &worktree.join("src"),
            [
                (worktree, Some(4010)),
                (Path::new("/work/other"), Some(4020)),
            ],
        )
        .expect("env");
        assert_eq!(env.get(PORT_VAR).map(String::as_str), Some("4010"));
        assert!(port_env(Path::new("/work/app"), [(worktree, Some(4010))]).is_none());
    }

    /// Copies nobody changed are not work: a worktree holding only those is
    /// removed when the project closes, the link to the notes included.
    #[cfg(unix)]
    #[test]
    fn a_worktree_holding_unchanged_copies_is_removed() {
        let (tmp, repo, path) = repo_with_local_files("teardown");
        std::fs::remove_dir_all(repo.join("secrets")).expect("no directory copy");
        copy_local_files(&repo, &path);
        let managed = ManagedWorktree {
            path: path.clone(),
            repo_root: repo.clone(),
            branch: "feat".to_string(),
            teardown: TeardownPolicy::Auto,
            base: None,
            port: None,
            from: None,
        };

        teardown_all(vec![managed], &[]);

        assert!(!path.exists(), "unchanged copies do not hold the worktree");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The marker and the copied `.env` are both in every managed worktree,
    /// and neither is the person's work. With either counted as work the
    /// promise on the dialog's checkbox was never kept.
    #[test]
    fn a_worktree_holding_only_what_splitlane_put_there_is_removed() {
        let (tmp, managed) = repo_with_managed_worktree("clean");
        let path = managed.path.clone();
        let repo = managed.repo_root.clone();

        teardown_all(vec![managed], &[]);

        assert!(!path.exists(), "a clean managed worktree is removed");
        assert!(branch_exists(&repo, "feat"), "the branch is never deleted");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_holding_an_ignored_file_is_kept() {
        let (tmp, managed) = repo_with_managed_worktree("ignored");
        let path = managed.path.clone();
        std::fs::create_dir_all(path.join("build")).expect("build dir");
        std::fs::write(path.join("build").join("notes.txt"), "work\n").expect("ignored file");

        teardown_all(vec![managed], &[]);

        assert!(path.join("build").join("notes.txt").exists());
        assert!(has_owner_marker(&path), "a kept worktree stays managed");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_whose_env_copy_was_edited_is_kept() {
        let (tmp, managed) = repo_with_managed_worktree("env");
        let path = managed.path.clone();
        std::fs::write(path.join(".env"), "KEY=filled-in-by-hand\n").expect("edit");

        teardown_all(vec![managed], &[]);

        assert!(path.join(".env").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_holding_an_untracked_file_the_repository_hides_is_kept() {
        let (tmp, managed) = repo_with_managed_worktree("hidden-untracked");
        let path = managed.path.clone();
        test_git(
            &managed.repo_root,
            &["config", "status.showUntrackedFiles", "no"],
        );
        std::fs::write(path.join("draft.txt"), "work\n").expect("untracked file");

        teardown_all(vec![managed], &[]);

        assert!(path.join("draft.txt").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_with_an_edit_git_was_told_not_to_look_at_is_kept() {
        let (tmp, managed) = repo_with_managed_worktree("assume-unchanged");
        let path = managed.path.clone();
        test_git(&path, &["update-index", "--assume-unchanged", "a.txt"]);
        std::fs::write(path.join("a.txt"), "one\nedited\n").expect("edit");

        assert!(is_clean(&path, &managed.repo_root).is_err());
        teardown_all(vec![managed], &[]);

        assert!(path.join("a.txt").exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_worktree_with_a_tracked_edit_is_kept() {
        let (tmp, managed) = repo_with_managed_worktree("tracked");
        let path = managed.path.clone();
        std::fs::write(path.join("a.txt"), "one\nedited\n").expect("edit");

        assert_eq!(is_clean(&path, &managed.repo_root), Ok(false));
        teardown_all(vec![managed], &[]);

        assert!(path.exists());
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

    /// A `.env` git does not ignore is not copied: in the worktree it would be
    /// an untracked file, and git refuses to remove a worktree that holds
    /// one, so the session's close could never clean it up.
    #[test]
    fn an_env_file_git_does_not_ignore_is_not_copied() {
        let repo = committed_repo("env-not-ignored");
        std::fs::write(repo.join(".env"), "A=1\n").expect("env");
        let worktree = worktree_dir(&repo, "feat");
        add_worktree(&repo, &worktree, "feat", true).expect("worktree");

        assert!(copy_local_files(&repo, &worktree).is_empty());
        assert!(!worktree.join(".env").exists());
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn copying_from_a_missing_checkout_is_silent_and_empty() {
        let dst = tempfile::tempdir().expect("dst");
        let copied = copy_local_files(Path::new("/nonexistent-splitlane-test"), dst.path());
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

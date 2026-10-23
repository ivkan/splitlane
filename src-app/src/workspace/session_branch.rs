//! What a session's branch holds, and what is left of it after the session.
//!
//! A session's worktree and branch go quietly when nothing in them is new.
//! When something is, the person chooses at the session's close: keep the
//! branch, or delete everything. Deleting everything first saves a
//! **snapshot**, so the choice can be taken back: a commit holding the
//! worktree as it was, uncommitted changes and new files included, made on
//! top of the branch's last commit so the branch's own commits are in it
//! too. It lives under [`SNAPSHOT_REFS`], which no branch listing shows and
//! no push sends, for [`SNAPSHOT_DAYS`] days. An ignored file that differs
//! from the checkout's - an `.env` an agent filled in - is in it; whole
//! ignored directories are not.
//!
//! Everything here is blocking git; the app calls it off the render thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::git_command::{self, GitProfile};
use crate::workspace::worktree::{
    self, ManagedWorktree, SESSION_BRANCH_PREFIX, TeardownPolicy, run_git,
};

/// Where snapshots are kept, one ref each.
pub const SNAPSHOT_REFS: &str = "refs/splitlane/snapshots/";
/// How long a snapshot is kept before it is pruned.
pub const SNAPSHOT_DAYS: u64 = 30;

const DEADLINE: Duration = Duration::from_secs(20);
const LISTING_CAP: u64 = 1024 * 1024;

/// What a session's branch holds that is not anywhere else.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BranchWork {
    /// Files changed or added in the worktree and not committed.
    pub changed_files: usize,
    /// Lines added and removed in tracked files, against the last commit.
    pub insertions: usize,
    pub deletions: usize,
    /// Commits on the branch that no other branch or remote branch has.
    pub commits: usize,
}

impl BranchWork {
    pub fn is_empty(&self) -> bool {
        self.changed_files == 0 && self.commits == 0
    }
}

/// Count what `wt` holds that would be lost with it.
pub fn branch_work(wt: &ManagedWorktree) -> Result<BranchWork, String> {
    let mut work = BranchWork::default();
    if wt.path.is_dir() {
        let status = run_git(
            GitProfile::Probe,
            &wt.path,
            &["status", "--porcelain", "-z", "--untracked-files=all"],
            DEADLINE,
        )?;
        work.changed_files = status
            .split('\0')
            .filter(|record| record.len() > 3)
            .filter(|record| !record[3..].starts_with(".splitlane-worktree"))
            .count()
            + changed_ignored_files(wt).len();
        let stat = run_git(
            GitProfile::Probe,
            &wt.path,
            &["diff", "HEAD", "--shortstat"],
            DEADLINE,
        )?;
        (work.insertions, work.deletions) = parse_shortstat(&stat);
    }
    let branch = format!("refs/heads/{}", wt.branch);
    // `--exclude` before `--branches` takes the name without `refs/heads/`.
    let exclude = format!("--exclude={}", wt.branch);
    let count = run_git(
        GitProfile::Probe,
        &wt.repo_root,
        &[
            "rev-list",
            "--count",
            &branch,
            "--not",
            &exclude,
            "--branches",
            "--remotes",
        ],
        DEADLINE,
    )?;
    work.commits = count.trim().parse().unwrap_or(0);
    Ok(work)
}

/// Ignored files in `wt` that are not what the checkout holds at the same
/// path: an `.env` an agent filled in. Kept in a snapshot though git ignores
/// them, since nothing else holds them. Whole ignored directories (installed
/// dependencies, build output) are not.
fn changed_ignored_files(wt: &ManagedWorktree) -> Vec<String> {
    let Ok(status) = run_git(
        GitProfile::Probe,
        &wt.path,
        &[
            "status",
            "--porcelain",
            "-z",
            "--ignored=matching",
            "--untracked-files=all",
        ],
        DEADLINE,
    ) else {
        return Vec::new();
    };
    status
        .split('\0')
        .filter_map(|record| record.strip_prefix("!! "))
        .filter(|path| !path.ends_with('/') && *path != ".splitlane-worktree")
        .filter(|path| !worktree::is_unchanged_copy(&wt.repo_root.join(path), &wt.path.join(path)))
        .map(str::to_string)
        .collect()
}

/// Whether `branch` is one this app made for a session: marked when it was
/// made, or known by the record's base commit.
fn is_session_branch(wt: &ManagedWorktree) -> bool {
    wt.branch.starts_with(SESSION_BRANCH_PREFIX)
        && (wt.base.is_some()
            || run_git(
                GitProfile::Probe,
                &wt.repo_root,
                &[
                    "config",
                    "--get",
                    &worktree::session_branch_mark_key(&wt.branch),
                ],
                DEADLINE,
            )
            .is_ok_and(|value| value.trim() == "true"))
}

/// `N files changed, A insertions(+), D deletions(-)` → `(A, D)`.
fn parse_shortstat(line: &str) -> (usize, usize) {
    let mut insertions = 0;
    let mut deletions = 0;
    for part in line.split(',') {
        let part = part.trim();
        let number = part
            .split_whitespace()
            .next()
            .and_then(|n| n.parse().ok())
            .unwrap_or(0);
        if part.contains("insertion") {
            insertions = number;
        } else if part.contains("deletion") {
            deletions = number;
        }
    }
    (insertions, deletions)
}

/// Save the worktree as it is under a new snapshot ref, and return the ref.
/// The snapshot is a commit on top of the branch's last commit, so the
/// branch's commits are in it; its message names the session, so the person
/// recognises it later.
pub fn snapshot(wt: &ManagedWorktree, session: &str) -> Result<String, String> {
    let index = std::env::temp_dir().join(format!(
        "splitlane-snapshot-{}.index",
        uuid::Uuid::new_v4().simple()
    ));
    let result = snapshot_with_index(wt, session, &index);
    let _ = std::fs::remove_file(&index);
    result
}

fn snapshot_with_index(
    wt: &ManagedWorktree,
    session: &str,
    index: &Path,
) -> Result<String, String> {
    let env: [(&str, &std::ffi::OsStr); 5] = [
        ("GIT_INDEX_FILE", index.as_os_str()),
        ("GIT_AUTHOR_NAME", "Splitlane".as_ref()),
        ("GIT_AUTHOR_EMAIL", "snapshot@splitlane.invalid".as_ref()),
        ("GIT_COMMITTER_NAME", "Splitlane".as_ref()),
        ("GIT_COMMITTER_EMAIL", "snapshot@splitlane.invalid".as_ref()),
    ];
    let git = |args: &[&str]| -> Result<String, String> {
        let out = git_command::run_with_env(
            GitProfile::UserAction,
            &wt.path,
            args,
            &env,
            DEADLINE,
            LISTING_CAP,
        )
        .map_err(|e| format!("git {} failed: {e}", args[0]))?;
        if !out.status.success() {
            return Err(format!(
                "git {} failed: {}",
                args[0],
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    git(&["read-tree", "HEAD"])?;
    git(&["add", "-A", "--", "."])?;
    let ignored = changed_ignored_files(wt);
    if !ignored.is_empty() {
        let mut args = vec!["add", "-f", "--"];
        args.extend(ignored.iter().map(String::as_str));
        git(&args)?;
    }
    // The marker is this app's, not the person's work.
    let _ = git(&[
        "rm",
        "--cached",
        "-q",
        "--ignore-unmatch",
        ".splitlane-worktree",
    ]);
    let tree = git(&["write-tree"])?;
    let message = format!(
        "{}\n\nbranch: {}\nsession: {}",
        snapshot_title(session),
        wt.branch,
        session
    );
    let commit = git(&["commit-tree", &tree, "-p", "HEAD", "-m", &message])?;
    if !worktree::is_commit_id(&commit) {
        return Err("git commit-tree gave no commit".to_string());
    }
    let name = format!(
        "{SNAPSHOT_REFS}{}-{}",
        worktree::branch_slug(&wt.branch),
        now_secs()
    );
    run_git(
        GitProfile::UserAction,
        &wt.repo_root,
        &["update-ref", &name, &commit],
        DEADLINE,
    )?;
    Ok(name)
}

fn snapshot_title(session: &str) -> String {
    let session = session.trim();
    if session.is_empty() {
        "Snapshot".to_string()
    } else {
        session.lines().next().unwrap_or(session).to_string()
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Delete everything: snapshot what is there, remove the worktree even with
/// changes in it, and delete the branch. Returns the snapshot made, if there
/// was anything to keep. Only for a branch this app made for a session.
pub fn discard(wt: &ManagedWorktree, session: &str) -> Result<Option<String>, String> {
    if !is_session_branch(wt) {
        return Err(format!("{} is not a branch Splitlane made", wt.branch));
    }
    let work = branch_work(wt)?;
    let saved = if work.is_empty() {
        None
    } else {
        Some(snapshot(wt, session)?)
    };
    if wt.path.exists() {
        let path = wt.path.to_string_lossy();
        run_git(
            GitProfile::UserAction,
            &wt.repo_root,
            &["worktree", "remove", "--force", &path],
            DEADLINE,
        )?;
    } else {
        let _ = worktree::prune(&wt.repo_root);
    }
    run_git(
        GitProfile::UserAction,
        &wt.repo_root,
        &["branch", "-D", &wt.branch],
        DEADLINE,
    )?;
    Ok(saved)
}

/// Remember which session a kept branch was, so a list of them can be read
/// by name rather than by `ab12cd`. Git's own branch description.
pub fn describe_branch(wt: &ManagedWorktree, session: &str) {
    let key = format!("branch.{}.description", wt.branch);
    let _ = run_git(
        GitProfile::UserAction,
        &wt.repo_root,
        &["config", &key, &snapshot_title(session)],
        DEADLINE,
    );
}

/// A branch or snapshot the session left behind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftBehind {
    pub repo_root: PathBuf,
    pub kind: LeftKind,
    /// The session's name, or the branch when nothing better is known.
    pub title: String,
    /// When it was last written, as seconds since the epoch.
    pub when_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LeftKind {
    /// A `splitlane/*` branch no live session works on.
    Branch { branch: String },
    /// A snapshot, by its ref.
    Snapshot { reference: String, branch: String },
}

/// Every `splitlane/*` branch of `repo_root` that none of `live` (the
/// branches open sessions work on) is, and every snapshot. Snapshots past
/// [`SNAPSHOT_DAYS`] are pruned on the way.
pub fn left_behind(repo_root: &Path, live: &[String]) -> Vec<LeftBehind> {
    let mut out = Vec::new();
    let descriptions = run_git(
        GitProfile::Probe,
        repo_root,
        &[
            "config",
            "--get-regexp",
            r"^branch\..*\.(description|splitlane-session)$",
        ],
        DEADLINE,
    )
    .unwrap_or_default();
    let description_of = |branch: &str| -> Option<String> {
        let key = format!("branch.{branch}.description ");
        descriptions
            .lines()
            .find_map(|line| line.strip_prefix(&key))
            .map(str::to_string)
    };
    // A branch checked out in a worktree that survives its session (kept
    // with changes) is still something left behind; only a live session's
    // branch is not.
    if let Ok(listing) = run_git(
        GitProfile::Probe,
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%00%(committerdate:unix)",
            &format!("refs/heads/{SESSION_BRANCH_PREFIX}"),
        ],
        DEADLINE,
    ) {
        for line in listing.lines() {
            let mut parts = line.split('\0');
            let (Some(branch), Some(when)) = (parts.next(), parts.next()) else {
                continue;
            };
            if live.iter().any(|open| open == branch) {
                continue;
            }
            let mark = worktree::session_branch_mark_key(branch);
            if !descriptions
                .lines()
                .any(|line| line.strip_prefix(&mark).is_some_and(|v| v.trim() == "true"))
            {
                continue;
            }
            out.push(LeftBehind {
                repo_root: repo_root.to_path_buf(),
                title: description_of(branch).unwrap_or_else(|| branch.to_string()),
                when_secs: when.parse().unwrap_or(0),
                kind: LeftKind::Branch {
                    branch: branch.to_string(),
                },
            });
        }
    }
    let cutoff = now_secs().saturating_sub(SNAPSHOT_DAYS * 24 * 60 * 60);
    if let Ok(listing) = run_git(
        GitProfile::Probe,
        repo_root,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(committerdate:unix)%00%(contents)%01",
            SNAPSHOT_REFS,
        ],
        DEADLINE,
    ) {
        for record in listing.split('\u{1}') {
            let record = record.trim_start_matches('\n');
            let mut parts = record.splitn(3, '\0');
            let (Some(reference), Some(when), Some(message)) =
                (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let when_secs: u64 = when.trim().parse().unwrap_or(0);
            if when_secs < cutoff {
                let _ = run_git(
                    GitProfile::UserAction,
                    repo_root,
                    &["update-ref", "-d", reference],
                    DEADLINE,
                );
                continue;
            }
            let field = |name: &str| {
                message
                    .lines()
                    .find_map(|line| line.strip_prefix(name))
                    .map(|v| v.trim().to_string())
            };
            out.push(LeftBehind {
                repo_root: repo_root.to_path_buf(),
                title: field("session:")
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| message.lines().next().unwrap_or("Snapshot").to_string()),
                when_secs,
                kind: LeftKind::Snapshot {
                    reference: reference.to_string(),
                    branch: field("branch:").unwrap_or_default(),
                },
            });
        }
    }
    out.sort_by_key(|item| std::cmp::Reverse(item.when_secs));
    out
}

/// The worktree record for a branch a session left behind: its worktree when
/// one has it checked out, otherwise the path one would have.
pub fn left_branch_worktree(repo_root: &Path, branch: &str) -> Result<ManagedWorktree, String> {
    let path = worktree::list_worktrees(repo_root)?
        .into_iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
        .map(|entry| entry.path)
        .unwrap_or_else(|| worktree::worktree_dir(repo_root, branch));
    Ok(ManagedWorktree {
        path,
        repo_root: repo_root.to_path_buf(),
        branch: branch.to_string(),
        teardown: TeardownPolicy::Auto,
        base: None,
        port: None,
        from: None,
    })
}

/// What a left-behind branch or snapshot holds, for its row:
/// `2 commits · 4 changed files`.
pub fn what_is_left(item: &LeftBehind) -> String {
    let work = match &item.kind {
        LeftKind::Branch { branch } => left_branch_worktree(&item.repo_root, branch)
            .and_then(|wt| branch_work(&wt))
            .unwrap_or_default(),
        LeftKind::Snapshot { reference, .. } => {
            let parent = format!("{reference}^");
            let files = run_git(
                GitProfile::Probe,
                &item.repo_root,
                &["diff", "--name-only", &parent, reference],
                DEADLINE,
            )
            .map(|names| names.lines().filter(|l| !l.is_empty()).count())
            .unwrap_or(0);
            let commits = run_git(
                GitProfile::Probe,
                &item.repo_root,
                &[
                    "rev-list",
                    "--count",
                    &parent,
                    "--not",
                    "--branches",
                    "--remotes",
                ],
                DEADLINE,
            )
            .ok()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0);
            BranchWork {
                changed_files: files,
                commits,
                ..BranchWork::default()
            }
        }
    };
    work_summary(&work).unwrap_or_else(|| "nothing new".to_string())
}

/// What a branch holds, in a few words - `3 changed files · 2 commits` -
/// or `None` when it holds nothing.
pub fn work_summary(work: &BranchWork) -> Option<String> {
    let mut parts = Vec::new();
    match work.changed_files {
        0 => {}
        1 => parts.push("1 changed file".to_string()),
        n => parts.push(format!("{n} changed files")),
    }
    match work.commits {
        0 => {}
        1 => parts.push("1 commit".to_string()),
        n => parts.push(format!("{n} commits")),
    }
    (!parts.is_empty()).then(|| parts.join(" \u{b7} "))
}

/// A worktree for a kept branch, so a new session can work on it again. The
/// record carries no base: a branch with history is never deleted on close.
pub fn reopen_branch(repo_root: &Path, branch: &str) -> Result<ManagedWorktree, String> {
    if !branch.starts_with(SESSION_BRANCH_PREFIX) {
        return Err(format!("{branch} is not a session branch"));
    }
    if let Some(open) = worktree::list_worktrees(repo_root)?
        .into_iter()
        .find(|entry| entry.branch.as_deref() == Some(branch))
    {
        return Ok(ManagedWorktree {
            path: open.path,
            repo_root: repo_root.to_path_buf(),
            branch: branch.to_string(),
            teardown: TeardownPolicy::Auto,
            base: None,
            port: None,
            from: None,
        });
    }
    let path = worktree::worktree_dir(repo_root, branch);
    worktree::add_worktree(repo_root, &path, branch, false)?;
    worktree::copy_local_files(repo_root, &path);
    Ok(ManagedWorktree {
        path,
        repo_root: repo_root.to_path_buf(),
        branch: branch.to_string(),
        teardown: TeardownPolicy::Auto,
        base: None,
        port: None,
        from: None,
    })
}

/// Bring a snapshot back: a new session branch at the commit the snapshot was
/// made on (the old branch's commits come with it), and the snapshot's files
/// as uncommitted changes on top. The snapshot ref goes once it is back.
pub fn restore_snapshot(repo_root: &Path, reference: &str) -> Result<ManagedWorktree, String> {
    if !reference.starts_with(SNAPSHOT_REFS) {
        return Err(format!("{reference} is not a snapshot"));
    }
    let commit = run_git(
        GitProfile::Probe,
        repo_root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{reference}^{{commit}}"),
        ],
        DEADLINE,
    )?;
    if !worktree::is_commit_id(&commit) {
        return Err(format!("{reference} does not name a commit"));
    }
    let made = worktree::add_session_worktree_at(repo_root, &format!("{commit}^"))?;
    run_git(
        GitProfile::UserAction,
        &made.path,
        &["restore", "--source", &commit, "--worktree", "--", "."],
        DEADLINE,
    )?;
    // Brought back with work on it: not a branch to drop quietly on close.
    // The snapshot stays until the caller has recorded the new worktree.
    Ok(ManagedWorktree { base: None, ..made })
}

/// Drop a snapshot that has been brought back.
pub fn drop_snapshot(repo_root: &Path, reference: &str) {
    if reference.starts_with(SNAPSHOT_REFS) {
        let _ = run_git(
            GitProfile::UserAction,
            repo_root,
            &["update-ref", "-d", reference],
            DEADLINE,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .expect("git");
        assert!(status.success(), "git {args:?}");
    }

    fn repo(name: &str) -> PathBuf {
        let tmp = std::env::temp_dir().join(format!(
            "splitlane-session-branch-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&tmp);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).expect("dir");
        git(&repo, &["init", "-q"]);
        git(&repo, &["config", "user.email", "test@example.com"]);
        git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("file");
        std::fs::write(repo.join("gone.txt"), "tracked\n").expect("file");
        std::fs::write(repo.join(".gitignore"), ".env\n").expect("ignore");
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "init"]);
        repo
    }

    #[test]
    fn work_counts_changes_lines_and_commits_only_this_branch_has() {
        let repo = repo("work");
        let made = worktree::add_session_worktree(&repo).expect("made");
        assert!(branch_work(&made).expect("work").is_empty());

        std::fs::write(made.path.join("b.txt"), "new\n").expect("file");
        git(&made.path, &["add", "b.txt"]);
        git(&made.path, &["commit", "-q", "-m", "one"]);
        std::fs::write(made.path.join("a.txt"), "one\nchanged\nthree\n").expect("edit");
        std::fs::write(made.path.join("c.txt"), "untracked\n").expect("file");

        let work = branch_work(&made).expect("work");
        assert_eq!(work.commits, 1);
        assert_eq!(work.changed_files, 2);
        assert_eq!((work.insertions, work.deletions), (2, 1));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    /// Delete everything is not red because it can be taken back - which
    /// holds only if the snapshot keeps the branch's commits as well as its
    /// uncommitted files. Restoring it brings both.
    #[test]
    fn a_discarded_branch_comes_back_from_its_snapshot_with_its_commits() {
        let repo = repo("discard");
        let made = worktree::add_session_worktree(&repo).expect("made");
        std::fs::write(made.path.join("b.txt"), "committed\n").expect("file");
        git(&made.path, &["add", "b.txt"]);
        git(&made.path, &["commit", "-q", "-m", "work on the branch"]);
        std::fs::write(made.path.join("a.txt"), "edited\n").expect("edit");
        std::fs::write(made.path.join("c.txt"), "new file\n").expect("file");
        // A tracked file the session deleted stays deleted when it comes back.
        std::fs::remove_file(made.path.join("gone.txt")).expect("delete");
        // An ignored file an agent filled in has nothing else holding it.
        std::fs::write(made.path.join(".env"), "KEY=filled\n").expect("env");
        std::fs::remove_file(made.path.join(".splitlane-worktree")).ok();

        let saved = discard(&made, "Fix login")
            .expect("discarded")
            .expect("a snapshot");
        assert!(!made.path.exists());
        assert!(!worktree::branch_exists(&repo, &made.branch));

        let listed = left_behind(&repo, &[]);
        assert!(listed.iter().any(|item| item.title == "Fix login"
            && matches!(&item.kind, LeftKind::Snapshot { reference, .. } if *reference == saved)));

        let back = restore_snapshot(&repo, &saved).expect("restored");
        assert_eq!(
            std::fs::read_to_string(back.path.join("b.txt")).expect("commit"),
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(back.path.join("a.txt")).expect("edit"),
            "edited\n"
        );
        assert!(back.path.join("c.txt").is_file());
        assert!(!back.path.join("gone.txt").exists());
        assert_eq!(
            std::fs::read_to_string(back.path.join(".env")).expect("env"),
            "KEY=filled\n"
        );
        assert_eq!(back.base, None, "restored work is never dropped quietly");
        drop_snapshot(&repo, &saved);
        assert!(
            left_behind(&repo, std::slice::from_ref(&back.branch)).is_empty(),
            "the snapshot goes once it is back"
        );
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn a_kept_branch_is_listed_by_its_session_and_a_live_one_is_not() {
        let repo = repo("kept");
        let kept = worktree::add_session_worktree(&repo).expect("made");
        let live = worktree::add_session_worktree(&repo).expect("made");
        describe_branch(&kept, "Search index");

        let listed = left_behind(&repo, std::slice::from_ref(&live.branch));
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "Search index");
        assert_eq!(
            listed[0].kind,
            LeftKind::Branch {
                branch: kept.branch.clone()
            }
        );
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    /// A branch a person named `splitlane/...` is not this app's: not
    /// listed, and not deleted.
    #[test]
    fn a_person_named_splitlane_branch_is_left_alone() {
        let repo = repo("named");
        git(&repo, &["branch", "splitlane/mine"]);
        assert!(left_behind(&repo, &[]).is_empty());
        let wt = left_branch_worktree(&repo, "splitlane/mine").expect("record");
        assert!(discard(&wt, "x").is_err());
        assert!(worktree::branch_exists(&repo, "splitlane/mine"));
        let _ = std::fs::remove_dir_all(repo.parent().expect("tmp"));
    }

    #[test]
    fn shortstat_reads_either_half() {
        assert_eq!(
            parse_shortstat(" 2 files changed, 42 insertions(+), 7 deletions(-)"),
            (42, 7)
        );
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (0, 1));
        assert_eq!(parse_shortstat(""), (0, 0));
    }

    #[test]
    fn what_a_branch_holds_is_said_in_a_few_words() {
        let work = |changed_files, commits| BranchWork {
            changed_files,
            commits,
            ..BranchWork::default()
        };
        assert_eq!(work_summary(&work(0, 0)), None);
        assert_eq!(work_summary(&work(1, 0)).as_deref(), Some("1 changed file"));
        assert_eq!(
            work_summary(&work(3, 2)).as_deref(),
            Some("3 changed files \u{b7} 2 commits")
        );
    }
}

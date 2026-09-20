#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! The product's former name does not come back.
//!
//! The product was renamed to `splitlane` on 6 September 2026, in one pass
//! across five layers: identifiers, paths, commands, environment variables and
//! prose. A rename that big does not fail all at once; it fails a word at a
//! time, months later, when somebody copies an old comment, restores a
//! workflow from history, or writes a path from memory. This repository has
//! watched exactly that happen twice already - `workspace` and `Conductor`
//! both came back in pieces after being renamed - and each time the only thing
//! that would have caught it was a check that reads every file.
//!
//! So this is a lint rather than a convention, for the same reason
//! `design_token_policy` and `svg_icon_color_policy` are: nothing else in
//! the build can observe the defect. A stray former name compiles, tests
//! green, and the only symptom is a user reading two names for one product.
//!
//! It scans every file git tracks, so a new file is covered the moment it is
//! added and nothing has to be kept in sync by hand.
//!
//! # What is allowed, and why each one is
//!
//! Whole paths, because they are records rather than statements:
//!
//! - `debian/changelog` - the history of releases that were actually made
//!   under that name. Rewriting a changelog entry makes it false.
//! - four files under `native/libghostty/` - see ALLOWED_PATHS. They are
//!   checksum-pinned records of a build that happened under the former name,
//!   and editing one both falsifies it and breaks the pin.
//! - the directories the working repository keeps out of publication, read
//!   from `scripts/publish-public.sh` when that script is present (see
//!   [`unpublished_directories`]). A published tree has neither the script nor
//!   the directories, so there the list is empty.
//!
//! Two literals, anywhere they appear:
//!
//! - the upstream repository's path, which the README's Acknowledgements must
//!   name - GPL-3.0 requires the attribution to be true.
//! - the upstream project's domain, which is not ours to rename.
//!
//! And a few more, each in one named file, because that is the whole of their
//! legitimate use - see ALLOWED_LITERALS_BY_PATH, where each states its reason.
//!
//! If a further genuinely-needed exception turns up, add it here with its
//! reason. An exception with no reason beside it is how the first rename
//! leaked.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The name that must not come back, lower-cased. Every capitalisation of it
/// is one comparison, because the whole point of the rename was that several
/// spellings of one word were never distinct.
const OLD_NAME: &str = "paneflow";

/// Paths whose whole contents are records of what happened, not claims about
/// what is.
const ALLOWED_PATHS: &[&str] = &[
    "debian/changelog",
    "src-app/tests/product_name_policy.rs",
    // The libghostty vendoring records. Four files here are CHECKSUM-PINNED in
    // `native/libghostty/manifest.toml` and verified by
    // `crates/splitlane-libghostty-sys/build.rs` before it will link anything:
    // the third-party notices, the SBOM, the Windows `build-info.txt`, and the
    // manifest itself, which records the canonical source path that build
    // used. They describe a build that actually happened, on a machine, under
    // the former name - so renaming a byte in them both falsifies the record
    // and breaks the pin, which is what it did: the rename pass edited all four
    // and the Windows leg refused to build with `checksum mismatch`. The prose
    // files beside them (`README.md`, `alacritty-baseline.md`,
    // `windows-smoke.c`) are not pinned and were renamed normally.
    "native/libghostty/THIRD_PARTY_NOTICES.md",
    "native/libghostty/sbom.cdx.json",
    "native/libghostty/manifest.toml",
    "native/libghostty/prebuilt/",
];

/// Literals that legitimately contain the old name. A line is cleared of
/// these before it is judged, so an allowed literal passes and the same line
/// with a second, bare occurrence does not.
const ALLOWED_LITERALS: &[&str] = &["arthjean/paneflow", "paneflow.dev"];

/// Literals allowed in ONE file each. Narrower than `ALLOWED_LITERALS` on
/// purpose: each of these is legitimate exactly where it is and nowhere else,
/// and a per-path entry says so, where a global one would quietly permit the
/// next copy of it somewhere it does not belong.
const ALLOWED_LITERALS_BY_PATH: &[(&str, &str)] = &[
    // The header of a maintainer script says which apt/dnf source file it
    // used to write and no longer does.
    ("packaging/debian/postinst", "paneflow.list"),
    ("packaging/rpm/postinst.sh", "paneflow.repo"),
    // The one sentence in the working repository's CLAUDE.md that states this
    // rule has to name the name it forbids. Pinned to that phrase so the rest
    // of the file is still checked.
    ("CLAUDE.md", "was `paneflow` until"),
    // The release-smoke assertions that keep the removed package channel
    // removed. They have to name the file the OLD postinst wrote, because a
    // postinst restored from history would bring that file back - an
    // assertion that only knew the new name would pass while the old channel
    // was back on every installing machine.
    (".github/workflows/release.yml", "paneflow.list"),
    (".github/workflows/release.yml", "paneflow.repo"),
    (".github/workflows/release.yml", "paneflow|splitlane"),
    // The publication boundary script names the repositories it must never
    // publish to, which is its entire job; every occurrence there is
    // load-bearing. A per-literal entry does not work for it: this file would
    // then have to spell a repository name the script exists to keep out of
    // the published tree.
    ("scripts/publish-public.sh", "paneflow"),
    // The libghostty Windows reproducibility triangle. The committed prebuilt
    // was built under the former name, and its `build-info.txt` records the
    // directories it used. The build script reconstructs those paths to check
    // the record field by field, and the producer script writes them - so
    // producer, record and consumer must name the same directory or the
    // Windows leg refuses to link. They move together the day the prebuilt is
    // rebuilt, and not before.
    ("crates/splitlane-libghostty-sys/build.rs", "paneflow-zig"),
    ("scripts/build-libghostty-windows.ps1", "paneflow"),
];

/// The script that draws the publication boundary. Present in the working
/// repository, excluded from the published tree.
const PUBLISH_SCRIPT: &str = "scripts/publish-public.sh";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("src-app has a parent")
        .to_path_buf()
}

/// Ask git for the tracked files. A directory walk would have to re-implement
/// `.gitignore` and would wander into `target/`, which holds a copy of every
/// source file this test is checking.
fn tracked_files(root: &Path) -> Vec<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .unwrap_or_else(|e| panic!("failed to run `git ls-files`: {e}"));
    assert!(out.status.success(), "`git ls-files` failed in {root:?}");
    String::from_utf8_lossy(&out.stdout)
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// The directories the publication script keeps back, read from its `EXCLUDE`
/// array. They are working notes that never reach a reader of the published
/// tree, and they discuss the former name by necessity.
///
/// Only **directory** entries (a trailing `/`) are taken. A single file the
/// script withholds - the working repository's `CLAUDE.md`, say - is still a
/// document somebody reads and is still checked; it gets a per-path literal if
/// it needs one.
///
/// Reading the list rather than repeating it keeps the two from drifting. A
/// script that is present but yields no directory is a parse failure, not an
/// empty boundary, and fails loudly.
fn unpublished_directories(root: &Path) -> Vec<String> {
    let Ok(script) = std::fs::read_to_string(root.join(PUBLISH_SCRIPT)) else {
        return Vec::new();
    };
    let dirs: Vec<String> = script
        .lines()
        .skip_while(|line| line.trim() != "EXCLUDE=(")
        .skip(1)
        .take_while(|line| line.trim() != ")")
        .map(str::trim)
        .filter(|entry| !entry.is_empty() && !entry.starts_with('#'))
        .filter(|entry| entry.ends_with('/'))
        .map(str::to_string)
        .collect();
    assert!(
        !dirs.is_empty(),
        "{PUBLISH_SCRIPT} exists but no directory entry was read from its EXCLUDE array"
    );
    dirs
}

fn is_allowed_path(rel: &str, unpublished: &[String]) -> bool {
    ALLOWED_PATHS
        .iter()
        .copied()
        .chain(unpublished.iter().map(String::as_str))
        .any(|allowed| rel == allowed || rel.starts_with(allowed))
}

#[test]
fn the_old_product_name_does_not_come_back() {
    let root = repo_root();
    let unpublished = unpublished_directories(&root);
    let mut hits: Vec<String> = Vec::new();

    for rel in tracked_files(&root) {
        if is_allowed_path(&rel, &unpublished) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            // Binary, or a file this checkout does not have (a submodule
            // path). Neither can carry a name a reader would see as prose.
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            let mut cleared = line.to_ascii_lowercase();
            for literal in ALLOWED_LITERALS {
                cleared = cleared.replace(literal, "");
            }
            for (path, literal) in ALLOWED_LITERALS_BY_PATH {
                if rel == *path {
                    cleared = cleared.replace(literal, "");
                }
            }
            if cleared.contains(OLD_NAME) {
                hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "the old product name is back in {} place(s). The product is \
         `splitlane` (identifiers, paths, commands) and `Splitlane` (anything \
         a person reads); there is no third spelling. If one of these is a \
         genuine exception, add it to ALLOWED_PATHS or ALLOWED_LITERALS in \
         this file WITH its reason.\n{}",
        hits.len(),
        hits.join("\n")
    );
}

/// The same name, spelled one letter per string literal, which the grep above
/// cannot see.
///
/// Not hypothetical: the startup splash shimmers each letter on its own clock,
/// so the wordmark was written as an array of one-letter strings. There was no
/// whole word in that line for the test above to find, so the rename passed it
/// five times and the app went on announcing itself under the former name at
/// every launch - on the one screen whose whole job is to say what this
/// program is. It was found by watching the app start.
///
/// Only **runs of adjacent** single-character literals are joined, so this
/// cannot fire on unrelated one-character strings scattered through a file.
#[test]
fn the_old_name_is_not_spelled_out_letter_by_letter() {
    let root = repo_root();
    let unpublished = unpublished_directories(&root);
    let mut hits: Vec<String> = Vec::new();

    for rel in tracked_files(&root) {
        if is_allowed_path(&rel, &unpublished) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(root.join(&rel)) else {
            continue;
        };
        for (i, line) in text.lines().enumerate() {
            for run in adjacent_single_char_literals(line) {
                if run.to_ascii_lowercase().contains(OLD_NAME) {
                    hits.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                    break;
                }
            }
        }
    }

    assert!(
        hits.is_empty(),
        "the old product name is spelled out letter by letter in {} place(s). \
         Splitting a word across literals does not make it a different word - \
         see the startup splash, which is what this test was written for.\n{}",
        hits.len(),
        hits.join("\n")
    );
}

/// Every run of one-character string literals on a line, joined back into the
/// word they spell.
///
/// A run continues while the only things between two literals are the
/// separators an array or a call puts there. Anything else - another token, a
/// longer literal - ends it, which is what keeps this from stitching together
/// characters that were never a word.
fn adjacent_single_char_literals(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut runs: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut i = 0usize;
    let mut gap_start: Option<usize> = None;

    while i < bytes.len() {
        // A one-character literal is exactly `"x"`, and the character may not
        // be a quote or a backslash - an escape is not one character here.
        let is_literal = bytes[i] == b'"'
            && i + 2 < bytes.len()
            && bytes[i + 2] == b'"'
            && bytes[i + 1] != b'"'
            && bytes[i + 1] != b'\\';
        if is_literal {
            let separated_only = gap_start.is_none_or(|start| {
                line[start..i]
                    .chars()
                    .all(|c| c.is_whitespace() || matches!(c, ',' | '[' | ']' | '(' | ')'))
            });
            if !separated_only && !current.is_empty() {
                runs.push(std::mem::take(&mut current));
            }
            current.push(bytes[i + 1] as char);
            i += 3;
            gap_start = Some(i);
            continue;
        }
        i += 1;
    }
    if !current.is_empty() {
        runs.push(current);
    }
    runs
}

/// The other half of the same invariant: the exceptions have to still be
/// real. An allowlist nobody checks is how a temporary exemption becomes
/// permanent - and this one guards an attribution the licence requires.
#[test]
fn the_upstream_attribution_is_still_there() {
    let readme = std::fs::read_to_string(repo_root().join("README.md"))
        .expect("README.md must exist at the repository root");
    assert!(
        readme.contains("arthjean/paneflow"),
        "README.md must keep its Acknowledgements link to the upstream \
         repository - GPL-3.0-or-later requires the attribution"
    );
}

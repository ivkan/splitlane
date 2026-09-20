#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! Two questions look like one: *which pane has the keyboard* and *which pane
//! is the user in*.
//!
//! `LayoutTree::focused_pane` answers the first. It asks the window, and the
//! window says `None` whenever focus rests anywhere but a pane - the rail, the
//! palette, a dialog, an inline rename, the Files tree. That is exact and it is
//! right for the jobs that are genuinely about the keyboard.
//!
//! `SplitlaneApp::focused_pane_as_shown` answers the second, in three steps, and
//! it is the answer the app **draws**: the pane border, the slot header's fill
//! and the rail row's accent bar are all painted from it. So at any moment a
//! person can point at the pane it returns, and a control that acts on "the
//! pane you are in" has to mean that one.
//!
//! # Why a test and not a convention
//!
//! Because the failure is a control that does nothing, in a state no test in
//! this suite sets up. `split` asked the window, so `Add pane` did nothing at
//! all after a click on the rail - while the border, the slot header and the
//! rail's own accent bar went on naming a pane. Reported from live use, with
//! the workaround that gave the cause away: click another project and come back
//! (which calls `focus_first`) and the button works again. Zoom and swap mode
//! had the same silence. Nothing failed, nothing logged, and every runtime test
//! passed, because a test that opens a window focuses a pane and leaves it
//! focused.
//!
//! Three separate places had also written the fallback out by hand - the status
//! bar, the broadcast helper and the diff host - which is the other half of the
//! same problem: one answer, four spellings, free to drift.
//!
//! # The exemption
//!
//! A site that must ask the window carries `// focus-exact: <reason>` in the
//! four lines above it, the way an off-scale size carries `// ui-token-exempt:`
//! and a control that must not take the pointer carries `// cursor-exempt:`.
//! They fall into three kinds, and each says which it is at the site:
//!
//! - the machinery that **maintains** the fallback (`track_pane_focus`) and the
//!   helper's own first step - asking themselves would be circular;
//! - a job that is about the keystroke that just happened, where the live
//!   answer *is* the point (the swap target, after focus has been moved);
//! - a **destructive** action, where "nothing happened" is recoverable and
//!   "closed the surface you were not looking at" is not.
//!
//! # What it can and cannot see
//!
//! It reads `src-app/src/app/`, which is where the app's controls live; the
//! `layout/` implementation of `focused_pane` is out of scope, and so is any
//! caller that reaches the tree through a helper rather than naming the method.
//! That is a real limit rather than a hidden one - it catches the shape this
//! defect actually took, which is a handler asking the tree directly.

use std::path::{Path, PathBuf};

/// Walk `dir` and return every `.rs` file under it.
fn rust_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        panic!("failed to read source dir {}", dir.display());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(rust_sources(&path));
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
    out
}

/// How far above a call site the marker may sit.
///
/// Four lines, because a reason worth writing rarely fits on one and rustfmt
/// puts the call on a line of its own.
const MARKER_LOOKBEHIND: usize = 4;

const MARKER: &str = "// focus-exact:";

#[test]
fn a_control_acts_on_the_pane_the_screen_shows_as_focused() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/app");
    let mut offenders: Vec<String> = Vec::new();

    for path in rust_sources(&root) {
        let Ok(src) = std::fs::read_to_string(&path) else {
            panic!("failed to read {}", path.display());
        };
        let lines: Vec<&str> = src.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if !line.contains(".focused_pane(") {
                continue;
            }
            // The helper this rule points at, and the field behind it.
            if line.contains("focused_pane_as_shown") || line.contains("focused_pane_now") {
                continue;
            }
            let start = idx.saturating_sub(MARKER_LOOKBEHIND);
            let exempt = lines[start..=idx].iter().any(|l| l.contains(MARKER));
            if !exempt {
                offenders.push(format!(
                    "{}:{}: {}",
                    path.strip_prefix(env!("CARGO_MANIFEST_DIR"))
                        .unwrap_or(&path)
                        .display(),
                    idx + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these ask the window which pane has the keyboard, which is `None` \
         whenever focus rests on the rail, the palette or a dialog - so the \
         control does nothing while the screen still names a focused pane.\n\
         Use `SplitlaneApp::focused_pane_as_shown`, or state why the exact \
         answer is the right one with `{MARKER} <reason>` above the call.\n\n{}",
        offenders.join("\n")
    );
}

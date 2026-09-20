#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! Type sizes and corner radii come from `ui_tokens`, never from a literal.
//!
//! The point of a scale is that it is closed. A call site that writes
//! `.text_size(px(12.))` instead of picking a token has not chosen a role, and
//! nothing downstream can tell whether that 12 was meant as a row, a control or
//! a caption - so the next change to the scale silently skips it. The same goes
//! for a radius: eight named radii mean eight kinds of surface, and a literal
//! belongs to none of them.
//!
//! This is a lint rather than a convention because the failure is invisible:
//! the app builds, renders and looks almost right, and the drift only shows up
//! as a hierarchy that has quietly flattened by a pixel at a time - which is
//! what the whole S8 inventory found the first time.
//!
//! Two exemptions, both deliberate:
//!
//! - `terminal/element/` renders terminal cells, whose size is the user's
//!   setting (`splitlane.json#font_size`), not a UI token.
//! - `ui_tokens.rs` itself, which is where the numbers live.

use std::path::{Path, PathBuf};

/// A call site may state its own number by carrying this marker on the line
/// above, followed by the reason. Making the exemption say why - in the source,
/// next to the number - is the difference between an exception and a leak.
const EXEMPT: &str = "ui-token-exempt:";

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

fn ui_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    rust_sources(&root)
        .into_iter()
        .filter(|path| {
            let text = path.to_string_lossy().replace('\\', "/");
            !text.contains("/terminal/element/") && !text.ends_with("/ui_tokens.rs")
        })
        .collect()
}

/// Collect `file:line` for every line matching `needle`, skipping comments
/// (where the literal is being described, not drawn) and any line covered by an
/// [`EXEMPT`] marker in the comment block directly above it.
fn offenders(needle: &str) -> Vec<String> {
    let mut found = Vec::new();
    for path in ui_sources() {
        let Ok(source) = std::fs::read_to_string(&path) else {
            panic!("failed to read {}", path.display());
        };
        let lines: Vec<&str> = source.lines().collect();
        for (idx, line) in lines.iter().enumerate() {
            if line.trim_start().starts_with("//") || !line.contains(needle) {
                continue;
            }
            let exempt = lines[..idx]
                .iter()
                .rev()
                .take_while(|above| above.trim_start().starts_with("//"))
                .any(|above| above.contains(EXEMPT));
            if !exempt {
                found.push(format!("{}:{}", path.display(), idx + 1));
            }
        }
    }
    found
}

#[test]
fn no_ui_call_site_states_its_own_type_size() {
    let found = offenders(".text_size(px(");
    assert!(
        found.is_empty(),
        "these call sites state a type size instead of picking a role from \
         `ui_tokens::text` / `ui_tokens::mono`:\n{}",
        found.join("\n")
    );
}

#[test]
fn no_ui_call_site_states_its_own_radius() {
    let found = offenders(".rounded(px(");
    assert!(
        found.is_empty(),
        "these call sites state a corner radius instead of picking one of the \
         eight in `ui_tokens::radius`:\n{}",
        found.join("\n")
    );
}

/// GPUI's own `rounded_sm` / `rounded_md` / `rounded_lg` are a second radius
/// scale with different values (2 / 6 / 8 px), and mixing the two is how a
/// menu and a dialog end up one pixel apart for no stated reason.
#[test]
fn no_ui_call_site_uses_the_gpui_radius_shorthands() {
    for shorthand in [".rounded_sm()", ".rounded_md()", ".rounded_lg()"] {
        let found = offenders(shorthand);
        assert!(
            found.is_empty(),
            "`{shorthand}` is GPUI's radius scale, not the design's; pick one \
             of `ui_tokens::radius`:\n{}",
            found.join("\n")
        );
    }
}

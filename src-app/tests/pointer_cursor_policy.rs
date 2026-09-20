#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! Anything that answers a click says so under the pointer.
//!
//! The complaint that produced this rule was about the rail, and the measurement
//! was worse than the complaint: 96 of the 133 elements in this app carrying an
//! `on_click` set no cursor at all, and the two places that had made a decision
//! had made opposite ones - Settings rows took `CursorStyle::PointingHand`,
//! menu rows explicitly took `CursorStyle::Arrow` back. So it was never one
//! answer applied unevenly; it was no answer, plus two.
//!
//! The answer now is one: an element with an `on_click` takes the pointer.
//! Every rung of every list, every icon button, every menu row.
//!
//! # Why a test and not a convention
//!
//! Because the failure is invisible to everything else in the suite and nearly
//! invisible in review. The control lays out, hit-tests, hovers, shows its
//! tooltip and runs its handler; the only thing missing is the one signal that
//! told the user it was worth pressing. That is the same shape as
//! `svg_icon_color_policy`, and it came back the same way: one control at a
//! time, over months, each one reasonable on its own.
//!
//! # What it can and cannot see
//!
//! It checks chains whose head is a literal `div()` or `svg()` - the ones a
//! reader can settle by looking at the chain. String literals and comments are
//! blanked first, so a `;` inside a label cannot cut a chain's head off and an
//! unbalanced bracket inside a prose comment cannot truncate one. A chain that starts at a helper
//! (`icon_button_sm`, `toolbar_pill`, `surface_row_frame`, `select_item`,
//! `button_frame`, `header_icon_button`) is that helper's business, and each of
//! those sets the cursor once for all its callers. That is a real limit rather
//! than a hidden one: a new helper can still ship without a cursor, and only
//! its own literal `div()` - which carries no `on_click` - would be there to
//! notice. Naming it here is cheaper than a whole-program analysis to close it.
//!
//! # The exemption
//!
//! A site that must not take the pointer carries `// cursor-exempt: <reason>`
//! in its chain, the way an off-scale size carries `// ui-token-exempt:`. There
//! are two, and both are the same kind of thing: a surface rather than a
//! control (the diff body, whose click picks the hunk under the pointer) and
//! the platform's own frame (the window buttons under client-side decorations).

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

const OPENERS: [char; 3] = ['(', '[', '{'];
const CLOSERS: [char; 3] = [')', ']', '}'];

/// The source with every string literal, char literal and comment blanked to
/// spaces, byte for byte.
///
/// The delimiter walk below cannot tell a `;` in a label from a `;` that ends a
/// statement, and a cross-vendor pass named what that costs both ways: a chain
/// carrying a label with a `;` in it has its head cut off and is skipped
/// silently - the exact drift this test exists to stop - while an unbalanced
/// bracket inside a prose comment can truncate a compliant chain and fail the
/// build on it. Blanking keeps every byte offset, so bounds computed here index
/// the real source, and the exemption marker is still read off that.
fn mask_literals_and_comments(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\n' => {
                out[i] = b'\n';
                i += 1;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < bytes.len() && !(bytes[i] == b'*' && bytes.get(i + 1) == Some(&b'/')) {
                    if bytes[i] == b'\n' {
                        out[i] = b'\n';
                    }
                    i += 1;
                }
                i = (i + 2).min(bytes.len());
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    if bytes.get(i) == Some(&b'\n') {
                        out[i] = b'\n';
                    }
                    i += 1;
                }
                i = (i + 1).min(bytes.len());
            }
            // A char literal, and only that. A lifetime is an apostrophe with
            // no closing quote and must not swallow the rest of the file, so
            // this only fires on a shape that closes where a char literal does.
            b'\'' => {
                let close = if bytes.get(i + 1) == Some(&b'\\') {
                    (bytes.get(i + 3) == Some(&b'\'')).then_some(i + 3)
                } else {
                    (bytes.get(i + 2) == Some(&b'\'')).then_some(i + 2)
                };
                match close {
                    Some(end) => i = end + 1,
                    None => {
                        out[i] = bytes[i];
                        i += 1;
                    }
                }
            }
            c => {
                out[i] = c;
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("masking keeps the source ASCII-safe")
}

/// The whole builder chain around the `.on_click(` at `at`.
///
/// Both ends are found by balancing delimiters rather than by looking for a
/// `;`, because a click handler is a closure and its body is full of them. The
/// chain ends where its enclosing scope does: the first delimiter that closes
/// something this chain never opened, or a `;` at depth zero.
fn chain_around<'a>(src: &'a str, mask: &str, at: usize) -> &'a str {
    let bytes: Vec<char> = mask.chars().collect();
    // `at` is a byte offset into ASCII-safe territory (`.on_click(`), but the
    // file may hold multi-byte text elsewhere, so walk by char index.
    let at_char = mask[..at].chars().count();

    let mut depth = 0usize;
    let mut start = 0usize;
    for i in (0..at_char).rev() {
        let c = bytes[i];
        if CLOSERS.contains(&c) {
            depth += 1;
        } else if OPENERS.contains(&c) {
            if depth == 0 {
                start = i + 1;
                break;
            }
            depth -= 1;
        } else if depth == 0 && c == ';' {
            start = i + 1;
            break;
        }
    }

    let mut depth = 0usize;
    let mut end = bytes.len();
    for (i, &c) in bytes.iter().enumerate().skip(at_char) {
        if OPENERS.contains(&c) {
            depth += 1;
        } else if CLOSERS.contains(&c) {
            if depth == 0 {
                end = i;
                break;
            }
            depth -= 1;
        } else if depth == 0 && c == ';' {
            end = i;
            break;
        }
    }

    let byte_start = mask
        .char_indices()
        .nth(start)
        .map_or(src.len(), |(idx, _)| idx);
    let byte_end = mask
        .char_indices()
        .nth(end)
        .map_or(src.len(), |(idx, _)| idx);
    &src[byte_start..byte_end]
}

/// Whether this chain is one the test can settle by reading it.
fn head_is_a_literal_element(chain: &str) -> bool {
    let head = chain.trim_start();
    head.starts_with("div()")
        || head.starts_with("svg()")
        || (head.starts_with("let ") && {
            let first_line = head.lines().take(3).collect::<String>();
            first_line.contains("div()") || first_line.contains("svg()")
        })
}

#[test]
fn every_clickable_element_says_so_under_the_pointer() {
    let src_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut offenders = Vec::new();
    let mut checked = 0usize;

    for file in rust_sources(&src_dir) {
        let Ok(source) = std::fs::read_to_string(&file) else {
            panic!("failed to read {}", file.display());
        };
        let mask = mask_literals_and_comments(&source);
        for (offset, _) in source.match_indices(".on_click(") {
            let chain = chain_around(&source, &mask, offset);
            if !head_is_a_literal_element(chain) {
                continue;
            }
            if chain.contains("// cursor-exempt:") {
                continue;
            }
            checked += 1;
            if chain.contains(".cursor(") || chain.contains(".cursor_") {
                continue;
            }
            let line = source[..offset].matches('\n').count() + 1;
            offenders.push(format!("{}:{line}", file.display()));
        }
    }

    assert!(
        checked > 40,
        "the scan found only {checked} clickable elements, which means it stopped \
         seeing them rather than that they went away - check `chain_around`"
    );
    assert!(
        offenders.is_empty(),
        "these elements answer a click and draw no cursor for it. Add \
         `.cursor_pointer()`, or `// cursor-exempt: <why>` in the chain if it \
         is a surface rather than a control:\n  {}",
        offenders.join("\n  ")
    );
}

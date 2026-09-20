#![allow(
    clippy::panic,
    reason = "integration test setup failures need contextual diagnostics"
)]

//! Every surface that takes the keyboard is named in the render pass's
//! focus-holder list.
//!
//! GPUI dispatches an action along the **focus chain**. A `FocusHandle` that is
//! not in the rendered frame's dispatch tree falls back to the window root -
//! which sits *above* this app's own root `div`, and every `on_action` lives on
//! that `div`. So a panel that took focus and then closed without handing it
//! back does not misroute one keystroke: it kills every action in the app at
//! once, chord and button alike.
//!
//! `SplitlaneApp::render` closes that by asking, once a frame, whether the
//! window is pointing at a handle whose surface is off screen, and handing the
//! keyboard back to the pane the screen already names
//! (`return_focus_to_panes`). The question is answered from **one list**, which
//! also says whether anything is legitimately holding the keyboard away from
//! the panes.
//!
//! # Why a test and not a convention
//!
//! Because the list is only right while it is complete, and a handle that
//! forgets to join it fails in the two ways this repository keeps closing:
//! either its own keystrokes are pulled away a frame after it opens, or - the
//! shape that was reported from live use - closing it silently disables the
//! whole app. Nothing errors and nothing logs. Measured twice: `⌘B` worked
//! exactly twice, to open the Files panel and to close it; and then, months
//! later, "open Files, close it, and Add pane stops adding until I click a
//! pane".
//!
//! # What it can and cannot see
//!
//! It reads `src-app/src/main.rs`: the struct fields that own a `FocusHandle`,
//! and the holder list in `render`. A handle owned by a sub-entity (a
//! `TextInput`'s own, a rename field's) is out of scope by construction - it
//! leaves GPUI's focus map when the entity drops, which is the *other* case the
//! render pass answers, by asking whether anything live holds focus at all.

use std::path::PathBuf;

/// Handles that are deliberately not holders.
///
/// `empty_panes_focus` is where the keyboard is handed *to* when a container
/// has no panes, and `pending_focus` is a request to focus something on the
/// next frame rather than a surface holding it now.
const NOT_A_HOLDER: &[&str] = &["empty_panes_focus", "pending_focus"];

fn main_rs() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/main.rs")
}

/// The fields of every struct in `main.rs` whose type is a `FocusHandle`.
fn focus_handle_fields(source: &str) -> Vec<String> {
    source
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (name, ty) = line.strip_suffix(',')?.split_once(": ")?;
            if !ty.contains("FocusHandle") {
                return None;
            }
            if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return None;
            }
            Some(name.to_string())
        })
        .collect()
}

/// The body of the `let (holder_on_screen, stale_holder_focus) = { … };` block.
fn holder_list(source: &str) -> String {
    let anchor = "let (holder_on_screen, stale_holder_focus) = {";
    let start = source.find(anchor).unwrap_or_else(|| {
        panic!(
            "src-app/src/main.rs no longer builds its focus-holder list at `{anchor}`. \
             The list is what keeps a closed panel from taking every action in the app \
             down with it - if it moved, point this test at where it lives now."
        )
    });
    let rest = &source[start..];
    let end = rest.find("\n        };").unwrap_or_else(|| {
        panic!("the focus-holder list in src-app/src/main.rs has no closing brace")
    });
    rest[..end].to_string()
}

#[test]
fn every_focus_handle_is_named_in_the_holder_list() {
    let source = std::fs::read_to_string(main_rs()).expect("read src-app/src/main.rs");
    let holders = holder_list(&source);
    let missing: Vec<String> = focus_handle_fields(&source)
        .into_iter()
        .filter(|name| !NOT_A_HOLDER.contains(&name.as_str()))
        .filter(|name| !holders.contains(name.as_str()))
        .collect();
    assert!(
        missing.is_empty(),
        "these `FocusHandle`s take the keyboard but are not in the focus-holder list \
         in `SplitlaneApp::render`: {missing:?}.\n\
         A handle missing from that list fails twice over: while its surface is open the \
         render pass hands the keyboard back to a pane a frame later, and when the surface \
         closes the window is left pointing at a handle whose element is gone - which \
         dispatches every action to the window root, above this app's own `on_action`s, so \
         nothing in the app responds at all.\n\
         Add `(<is it on screen>, Some(&self.{}))` to the list, or name it in NOT_A_HOLDER \
         with the reason it is not one.",
        missing.first().map(String::as_str).unwrap_or("<field>")
    );
}

#[test]
fn the_holder_list_hands_the_keyboard_back() {
    let source = std::fs::read_to_string(main_rs()).expect("read src-app/src/main.rs");
    assert!(
        source.contains("stale_holder_focus || window.focused(cx).is_none()"),
        "the render pass no longer asks both halves of the stale-focus question. \
         A handle owned by the app outlives its element (`window.focused()` answers Some); \
         one owned by an input entity leaves the focus map with it (`window.focused()` \
         answers None). Both leave `window.focus` pointing outside the dispatch tree, and \
         both have to hand the keyboard back."
    );
    assert!(
        source.contains("self.return_focus_to_panes(window, cx)"),
        "the render pass no longer hands the keyboard back to the pane the screen names"
    );
}

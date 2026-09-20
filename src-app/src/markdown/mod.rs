//! Markdown viewer pane.
//!
//! Three-layer split:
//! - `parser` - pulldown-cmark event walker → owned `MdNode` AST. Pure Rust,
//!   no GPUI deps; unit-tested in isolation.
//! - `theme`  - semantic palette derived from the active `TerminalTheme`. The
//!   markdown viewer never owns colors; it borrows the terminal palette so a
//!   user theme switch repaints everything consistently.
//!
//! The surface that shows a file is **not** here: it is `crate::file_view`,
//! which renders markdown through this module and plain text and unopenable
//! files without it. This module is the markdown renderer; the pane's file
//! surface is a different thing and no longer shares a name with it.
//!
//! Out of scope here: live reload, scroll-state persistence,
//! syntax highlighting (P2 follow-up via `syntect`).

pub(crate) mod parser;
// `security` is currently unreferenced because the StyledText-based render
// path doesn't yet support per-run click hit-testing - link spans are styled
// but not clickable. The URL validator stays in-tree on purpose so the
// follow-up that restores click handling has a reviewed safeguard ready.
#[allow(dead_code)]
pub(crate) mod security;
pub(crate) mod state;
pub(crate) mod theme;

// Public surface: the parser primitives, consumed by `crate::file_view` and
// re-exported for the stories that walk the AST (live reload re-runs
// `parse_with_limit`; find-in-pane walks `MdNode` for its corpus).
#[allow(unused_imports)]
pub use parser::{MAX_INPUT_BYTES, MdNode, ParseError, Span, SpanStyle, parse_with_limit};
// The agent-question display path reuses
// the same bidi/zero-width strip as rendered markdown (one sanitizer, every
// untrusted display surface).
pub(crate) use parser::strip_bidi_zero_width;

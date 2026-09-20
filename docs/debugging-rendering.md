# Debugging terminal rendering

This page is for contributors. The terminal renderer has two debug probes:
one measures input latency, the other records where things are drawn. Both
exist only in debug builds (`#[cfg(debug_assertions)]`). A release build
doesn't contain either one, so you run them with `cargo run`, not
`cargo run --release`.

Each probe reads its environment variable once, the first time it's needed,
and caches the value for the rest of the process. Changing the variable while
Splitlane is running has no effect, so restart.

## `SPLITLANE_LATENCY_PROBE=1`: keystroke-to-pixel timing

This probe times the path from a key press to the frame that shows it, and
logs only slow cases:

| Log line | Logged when | Measured in |
| --- | --- | --- |
| `[latency] keystroke→PTY: <ms>` | 2 ms or more | [`terminal/input.rs`](../src-app/src/terminal/input.rs) |
| `[latency] paint: <ms>` | above 1 ms | [`terminal/element/mod.rs`](../src-app/src/terminal/element/mod.rs) |
| `[latency] keystroke→pixel: <ms> (pty_write→paint_start: <ms>, paint: <ms>)` | above 8 ms | [`terminal/element/mod.rs`](../src-app/src/terminal/element/mod.rs) |

The lines are logged at `warn` level, and debug builds show `warn` by default,
so you don't need `RUST_LOG`:

```sh
SPLITLANE_LATENCY_PROBE=1 cargo run
```

The switch itself is `probe_enabled()` in
[`terminal/view.rs`](../src-app/src/terminal/view.rs).

## `SPLITLANE_PIXEL_PROBE=1`: cell and glyph coordinates

This probe logs the exact coordinates the renderer hands to GPUI. Use it for
visual artifacts such as gaps between cells, misaligned glyphs, or seams in
block characters. It lives in
[`terminal/element/pixel_probe.rs`](../src-app/src/terminal/element/pixel_probe.rs).

The records are `debug` lines on the `splitlane::pixel_probe` target, so turn
that target on:

```sh
SPLITLANE_PIXEL_PROBE=1 RUST_LOG=splitlane::pixel_probe=debug cargo run 2> probe.log
```

### Output format

Numeric values are written as `<value>|frac=<+0.000000>`, so a fractional
remainder stands out immediately.

| Record | Fields | How often |
| --- | --- | --- |
| `cell_dims` | `cell_width_raw`, `cell_width_snapped`, `line_height_raw`, `line_height_snapped`, `scale_factor` | once per frame metrics resolution |
| `origin` | `x`, `y` | once per paint |
| `glyph` | `line`, `col`, `x`, `y` | per text run starting in columns 0-15 |
| `bg` | `col`, `line`, `x`, `y`, `w`, `h` | per cell background starting in columns 0-15 |
| `block_quad` | `col`, `line`, `x`, `y`, `w`, `h` | per block-element quad (`▀ ▄ █` and similar), every column |

The `glyph` and `bg` records cover only the first 16 columns of each row. That
keeps the log manageable on wide terminals, and alignment problems almost
always show up at the left edge first.

### Example

```sh
SPLITLANE_PIXEL_PROBE=1 RUST_LOG=splitlane::pixel_probe=debug cargo run 2> probe.log
# In a pane, run a TUI that draws block characters, then quit Splitlane.
grep cell_dims probe.log | head -3
grep block_quad probe.log | head -10
```

A snapped value should have `frac=+0.000000`. A result like `frac=+0.400000`
means a non-integer position reached the paint call, and that is a likely
cause of a one-pixel seam.

## `SPLITLANE_PIXEL_PROBE_OVERLAY=1`: cell outlines on screen

This works independently of `SPLITLANE_PIXEL_PROBE`. It draws a translucent
red outline, one physical pixel wide, around every cell of every visible
terminal. The outlines are painted after the text, so they sit on top of the
glyphs.

```sh
SPLITLANE_PIXEL_PROBE_OVERLAY=1 cargo run
```

Use it alone for a quick visual check, or together with
`SPLITLANE_PIXEL_PROBE=1` to match what's on screen against the logged
coordinates. It draws one quad per cell per frame, so expect it to be slow on
large terminals.

## Asserting alignment in tests

`pixel_probe::assert_pixel_aligned(value, label)` panics when `value` is not
within `1e-6` of a whole pixel. It is compiled only for tests in debug builds,
so `cargo test --release` doesn't include those tests. The text paint tests use
it to catch unsnapped coordinates:

```rust
#[cfg(test)]
use crate::terminal::element::pixel_probe::assert_pixel_aligned;

assert_pixel_aligned(glyph_x.as_f32(), "glyph_x after snapping");
```

## What a release build contains

The `pixel_probe` module is declared under `#[cfg(debug_assertions)]` in
[`terminal/element/mod.rs`](../src-app/src/terminal/element/mod.rs). Each call
into it is gated the same way: in the font metrics code, `paint`, the text run
and cell background passes, and the block quad pass. The overlay pass is also
behind the runtime `overlay_enabled()` check. The latency probe's timing code
is gated the same way. None of it ends up in a release build.

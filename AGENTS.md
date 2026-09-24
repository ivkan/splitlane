# AGENTS.md

Instructions for coding agents (and people) working in this repository.
For what the app is, read [README.md](README.md); for how it is put together,
[ARCHITECTURE.md](ARCHITECTURE.md).

## What this is

Splitlane is a native desktop app for running several CLI coding agents side
by side and seeing which one needs attention. It is a Rust workspace built on
[GPUI](https://github.com/zed-industries/zed/tree/main/crates/gpui), Zed's UI
framework, with two terminal backends (`alacritty_terminal` and libghostty).
It targets Linux (Wayland and X11), macOS and Windows.

## Layout

| Path | What lives there |
|---|---|
| `src-app/` | The `splitlane` binary: all UI, panes, PTY sessions, IPC server, CLI subcommands, self-update |
| `src-app/tests/` | Integration tests, including the policy tests listed below |
| `crates/splitlane-config/` | Config and session schema, JSON loader, file watcher, path resolution |
| `crates/splitlane-terminal-ghostty/`, `crates/splitlane-libghostty-sys/` | The libghostty backend and its FFI |
| `crates/splitlane-shim/`, `crates/splitlane-ai-hook/` | The `PATH` shim around agent CLIs and the hook payloads it reports |
| `crates/splitlane-mcp/`, `crates/splitlane-mcp-install/` | The read-only MCP bridge and its installer |
| `crates/splitlane-ipc-client/`, `crates/splitlane-process/`, `crates/splitlane-telemetry/`, `crates/splitlane-acp/` | Small shared libraries |
| `native/libghostty/` | Vendored libghostty sources and checksum-pinned prebuilts - do not edit by hand |
| `docs/` | User documentation; `docs/internals/` holds the reasoning behind UI and agent-state behaviour |
| `packaging/`, `assets/`, `debian/` | Installers, icons, desktop metadata |

Only `splitlane-app` links GPUI. Keep it that way: the shim, hook, MCP bridge
and installer run outside the GUI process and must stay GPU-free.

## Build and check

Run everything from the repository root. The toolchain is pinned in
`rust-toolchain.toml` and installed automatically by rustup.

```bash
cargo build                    # debug build of the app
cargo run                      # launch it (needs a GPU: Metal, Vulkan or DirectX)
RUST_LOG=info cargo run        # with logging
cargo test -p splitlane-config # one crate
cargo test <name> -- --nocapture
```

A change is ready when all three pass:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-public-hygiene.sh
```

The last one fails on text a reader outside the project cannot follow: a path
into private notes, a tracker id, a round number, a person named rather than
described. It covers commit messages too (`--message`), and
`git config core.hooksPath scripts/hooks` makes git run both for you. Write
the reason in place; do not point at where it is written down.

Use `--all-targets` for Clippy: without it the tests are not linted, and a test
function that lost its `#[test]` attribute stops running without failing.

Debug builds keep their config, session and IPC socket under `splitlane-dev`
instead of `splitlane`, so a `cargo run` build never shares state with an
installed release. A from-source build starting with no projects is expected.

That separation does not hold inside a Splitlane pane. Every pane exports
`SPLITLANE_SOCKET_PATH`, pointing at the instance that owns it, and both the
app and the CLI honour it. So `cargo run` from a pane refuses to start
("another Splitlane instance is already running"), and `splitlane ...` from
a debug build talks to the running release instead of your build. Clear the
variable for both: `env -u SPLITLANE_SOCKET_PATH cargo run`, and the same
for the CLI. Do not reach for `SPLITLANE_ALLOW_MULTIPLE=1` here: with the
variable still set, the second instance binds the first one's socket.

## Cross-platform rules

Every change must build and behave on Linux, macOS and Windows.

- No hardcoded POSIX paths, separators or shell commands. Use `PathBuf`,
  `std::env` and the `dirs` crate.
- Guard platform code with `#[cfg(target_os = "...")]` and give the other two
  platforms a working path or a documented fallback.
- Prefer cross-platform crates (`portable-pty`, `notify`, `dirs`, `which`).
- Keybindings use GPUI's `secondary-` modifier (Cmd on macOS, Ctrl elsewhere),
  not a literal `ctrl-`.
- If you could not verify a platform, say so in the pull request.

## Code style

- `cargo fmt` formatting, Rust naming defaults, small focused modules.
- Clippy denies `panic!`, `unimplemented!` and `dbg!`; `unwrap`/`expect` warn
  outside tests. Prefer `?` and explicit handling.
- GPUI styling is inline builder chains. Mutable UI state lives in `Entity<T>`;
  use `Rc<Cell<_>>` rather than `Arc<Mutex<_>>` for single-threaded shared state.
- Never do blocking I/O on the GPUI main thread. Filesystem walks, git calls
  and session-store reads go through a background executor.
- Sizes, radii, spacing and row heights come from `src-app/src/ui_tokens.rs`,
  chosen by role (`text::ROW`, `radius::MENU`), never by numeric value.
  Colours come from the theme's `UiColors`, never from a hex literal.
- Write comments for why, not what. Keep them in English.

## Policy tests

Several rules are enforced by tests in `src-app/tests/`, because breaking them
compiles fine and looks almost right:

| Test | Fails when |
|---|---|
| `design_token_policy` | a literal `.text_size(px(..))`, `.rounded(px(..))` or `rounded_sm/md/lg` appears outside `ui_tokens` |
| `svg_icon_color_policy` | an `svg()` has no `text_color` of its own (it would paint nothing) |
| `pointer_cursor_policy` | an element with `on_click` does not take the pointer cursor |
| `focused_pane_policy` | code asks the window for the focused pane without saying why |
| `focus_holder_policy` | a surface that takes keyboard focus is missing from the render pass's focus-holder list |
| `product_name_policy` | the product's former name reappears |
| `external_contract_names` | a published name (env var, IPC method, config key) is renamed |
| `dependency_source_policy` | a git dependency is not pinned to a full revision |

A genuine exception carries the exemption comment the test documents
(for example `// ui-token-exempt: <reason>`). Do not weaken the test.

Also enforced: every action in `src-app/src/app/actions.rs` is registered in
`keybindings/registry.rs`, and every default keybinding parses.

## Dependencies

GPUI comes from the upstream Zed git repository, pinned to one revision for
`gpui`, `gpui_platform` and `collections`. Bump all three together. Do not
replace them with crates.io versions, and do not add Zed's `markdown`, `ui` or
`language` crates - Markdown rendering is in-tree.

## Where the reasoning is

Before changing how panes, the rail, focus, notifications or agent status
behave, read the matching file in `docs/internals/`. Each rule there states the
case that forced it; add to those files rather than restating rules in code
comments.

## Commits and pull requests

Commit messages follow `type(scope): description` (`feat`, `fix`, `refactor`,
`docs`, `chore`, `test`). One logical change per commit. See
[CONTRIBUTING.md](CONTRIBUTING.md) for the pull request process.

# Contributing to Splitlane

Thanks for taking the time to contribute. Bug reports, fixes and well-scoped
improvements are all welcome. Please follow the
[Code of Conduct](.github/CODE_OF_CONDUCT.md) in issues and pull requests.

## Before you start

- For a bug, open an [issue](https://github.com/ivkan/splitlane/issues) with
  steps to reproduce, your OS and `splitlane --version` (or the commit you
  built).
- For anything larger than a small fix, open an issue first and describe the
  change you have in mind, so the approach can be agreed before you invest
  time in it.
- Security problems go through the process in [SECURITY.md](SECURITY.md), not
  a public issue.

## Building

Install Rust with [rustup](https://rustup.rs) - the toolchain is pinned in
`rust-toolchain.toml` - and the platform build tools listed under
[Build from source](README.md#build-from-source). Then:

```bash
cargo build                  # debug build
cargo run                    # launch the app
RUST_LOG=info cargo run      # with logging
```

A debug build keeps its settings and session under `splitlane-dev`, separate
from an installed release, so it starts with no projects.

[AGENTS.md](AGENTS.md) describes the repository layout, the code style and the
policy tests. It is written for coding agents and is just as useful for people.

## Before you open a pull request

Run these checks; CI runs them on every pull request:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
scripts/check-public-hygiene.sh
```

The last one keeps comments, docs and commit messages readable by people
outside the project: it fails on a reference only the author can follow - a
path into private notes, a tracker id, a round number from a design
conversation, someone named rather than described. State the reason instead of
pointing at it. Let git run it for you:

```bash
git config core.hooksPath scripts/hooks
```

- **Cross-platform by default.** A change must build and behave on Linux
  (Wayland and X11), macOS and Windows. Guard OS-specific code with
  `#[cfg(target_os = "...")]` and give the other platforms a working path. If
  you could not test a platform, say which in the pull request.
- **UI changes need a manual check.** Tests cannot see a painted pixel;
  include a screenshot or a short recording.
- **Keep it focused.** One logical change per pull request is much easier to
  review than several.

## Pull requests

- Branch from `main` (`feat/<description>`, `fix/<description>`).
- Describe what changed, what a user will notice, and how you verified it.
  The pull request template lists the checks.
- Commit messages follow `type(scope): description`:

  ```
  feat(rail): show the branch beside each project
  fix(terminal): keep the selection when the pane resizes
  docs: explain the MCP bridge install on Windows
  ```

  Types in use: `feat`, `fix`, `refactor`, `perf`, `test`, `docs`, `chore`.

- Using an AI assistant is fine. You are responsible for the change: you
  should understand it, have run it, and be able to answer review questions
  about it yourself.

## License

Splitlane is licensed under [GPL-3.0-or-later](LICENSE). By submitting a
contribution you agree that it is licensed under the same terms.

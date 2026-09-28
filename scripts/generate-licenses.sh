#!/usr/bin/env bash
# Generate THIRD_PARTY_LICENSES.md, the license notices every Splitlane
# package ships beside LICENSE.
#
# The file is THIRD_PARTY_NOTICES.md (bundled assets and adapted code)
# followed by the license text of every Rust crate compiled into the shipped
# binaries, which cargo-about collects from the dependency graph using
# about.toml and packaging/licenses/about.hbs.
#
# A crate whose license cargo-about cannot identify, or whose license is not
# accepted in about.toml, fails the run. CI runs it on every dependency change
# so that happens before a release, not during one.
#
# Usage:
#   scripts/generate-licenses.sh                 # target/licenses/THIRD_PARTY_LICENSES.md
#   scripts/generate-licenses.sh path/to/out.md
#
# Requires cargo-about at the pinned version:
#   cargo install cargo-about --version 0.9.2 --locked --features cli
set -euo pipefail

CARGO_ABOUT_VERSION="0.9.2"

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"
OUT="${1:-$REPO_ROOT/target/licenses/THIRD_PARTY_LICENSES.md}"

installed="$(cargo about --version 2>/dev/null || true)"
if [ "$installed" != "cargo-about $CARGO_ABOUT_VERSION" ]; then
    echo "error: cargo-about $CARGO_ABOUT_VERSION is required (found: ${installed:-none})" >&2
    echo "hint:  cargo install cargo-about --version $CARGO_ABOUT_VERSION --locked --features cli" >&2
    exit 1
fi

mkdir -p "$(dirname "$OUT")"
crates="$(mktemp)"
trap 'rm -f "$crates"' EXIT

# Every feature and every packaged target: one file covers all packages, and
# a dependency only one platform links is still listed.
cargo about generate \
    --manifest-path "$REPO_ROOT/Cargo.toml" \
    --config "$REPO_ROOT/about.toml" \
    --workspace \
    --all-features \
    --locked \
    --fail \
    --output-file "$crates" \
    "$REPO_ROOT/packaging/licenses/about.hbs"

{
    cat "$REPO_ROOT/THIRD_PARTY_NOTICES.md"
    printf '\n'
    cat "$crates"
} > "$OUT"

echo "$OUT"

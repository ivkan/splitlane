#!/usr/bin/env bash
# Assemble a macOS .app bundle from a cargo-built release binary.
#
# Output layout:
#   dist/Splitlane.app/Contents/
#     MacOS/splitlane                  (executable, chmod 755)
#     Info.plist                      (from assets/Info.plist, @VERSION@ substituted)
#     Resources/Splitlane.icns         (from assets/Splitlane.icns, produced by generate-icns.sh)
#
# Usage:
#   scripts/bundle-macos.sh --version 0.2.0 --arch aarch64
#   scripts/bundle-macos.sh --version 0.2.0 --arch x86_64 \
#       --target-dir target/x86_64-apple-darwin/release
#
# Arguments:
#   --version <string>       Version to stamp into Info.plist (required).
#   --arch <aarch64|x86_64>  Target architecture (required).
#   --target-dir <path>      Directory containing the built `splitlane` binary.
#                            Defaults to target/<triple>/release where
#                            <triple> is aarch64-apple-darwin or
#                            x86_64-apple-darwin depending on --arch.
#
# Signing, notarization, and .dmg creation are intentionally out of scope -
# see sign-macos.sh + notarize-macos.sh (codesign + notarytool) and
# create-dmg.sh (hdiutil .dmg).
#
# Portable enough to run on Linux for structural verification (the shell
# logic doesn't depend on Darwin-only tools); the resulting bundle is only
# *useful* on macOS.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"

VERSION=""
ARCH=""
TARGET_DIR=""

usage() {
    cat >&2 <<EOF
Usage: $0 --version <ver> --arch {aarch64|x86_64} [--target-dir <path>]
EOF
}

die() {
    echo "error: $*" >&2
    exit 1
}

while [ "$#" -gt 0 ]; do
    case "$1" in
        --version)
            [ "$#" -ge 2 ] || die "--version requires an argument"
            VERSION="$2"
            shift 2
            ;;
        --arch)
            [ "$#" -ge 2 ] || die "--arch requires an argument"
            ARCH="$2"
            shift 2
            ;;
        --target-dir)
            [ "$#" -ge 2 ] || die "--target-dir requires an argument"
            TARGET_DIR="$2"
            shift 2
            ;;
        -h|--help)
            usage
            exit 0
            ;;
        *)
            usage
            die "unknown argument: $1"
            ;;
    esac
done

# --- Validate required inputs ---------------------------------------------
[ -n "$VERSION" ] || { usage; die "--version is required"; }
[ -n "$ARCH" ]    || { usage; die "--arch is required"; }

case "$ARCH" in
    aarch64) TRIPLE="aarch64-apple-darwin" ;;
    x86_64)  TRIPLE="x86_64-apple-darwin"  ;;
    *)       die "--arch must be 'aarch64' or 'x86_64' (got '$ARCH')" ;;
esac

if [ -z "$TARGET_DIR" ]; then
    TARGET_DIR="$REPO_ROOT/target/$TRIPLE/release"
fi

BIN="$TARGET_DIR/splitlane"
INFO_PLIST_SRC="$REPO_ROOT/assets/Info.plist"
ICNS_SRC="$REPO_ROOT/assets/Splitlane.icns"

# Fail fast and loud - every missing input names the path that wasn't
# found, so a failing CI log tells you exactly what to check.
[ -f "$BIN" ]              || die "release binary not found at $BIN (did you run 'cargo build --release --target $TRIPLE -p splitlane-app'?)"
[ -f "$INFO_PLIST_SRC" ]   || die "Info.plist template not found at $INFO_PLIST_SRC"
[ -f "$ICNS_SRC" ]         || die "Splitlane.icns not found at $ICNS_SRC (scripts/build-icons.sh generates it from the macOS icon master)"

# ── Which binary this bundle is actually made of ─────────────────────────
#
# `--arch` names two different things and they only agree when you meant them
# to: the architecture of the output bundle, and which `target/` directory the
# binary comes from. CI builds with an explicit `cargo build --target $TRIPLE`,
# so its binary lands in `target/<triple>/release` and the default above is
# right. A plain local `cargo build --release` lands in `target/release`, and
# the default then quietly picks up whatever `target/<triple>/release` still
# holds from the last cross-compile - which on 2 September 2026 was a binary
# two days stale. The script found a file, said nothing, and shipped it.
#
# The resolution is deliberately NOT changed: CI depends on it, and a rule that
# is right on a laptop and wrong in the release pipeline is worse than the
# ambiguity. What changes is that the choice is now impossible to miss - the
# path and its date are printed, and a fresher sibling is named out loud.
bin_mtime() { stat -f '%Sm' -t '%Y-%m-%d %H:%M:%S' "$1" 2>/dev/null || echo "unknown"; }

warn_if_a_fresher_binary_exists() {
    local chosen="$1" candidate newest="" newest_path=""
    for candidate in "$REPO_ROOT"/target/release/splitlane \
                     "$REPO_ROOT"/target/*/release/splitlane; do
        [ -f "$candidate" ] || continue
        [ "$candidate" -ef "$chosen" ] && continue
        if [ "$candidate" -nt "$chosen" ] && { [ -z "$newest" ] || [ "$candidate" -nt "$newest" ]; }; then
            newest="$candidate"
            newest_path="$candidate"
        fi
    done
    [ -n "$newest_path" ] || return 0
    cat >&2 <<WARN

  ⚠  A NEWER splitlane binary exists than the one being bundled.

       bundling: $chosen
                 $(bin_mtime "$chosen")
        but see: $newest_path
                 $(bin_mtime "$newest_path")

     If you just ran a plain 'cargo build --release', that is the one you
     meant. Re-run with:

       $0 --version $VERSION --arch $ARCH --target-dir "$REPO_ROOT/target/release"

     Bundling anyway. Verify the result by a string only the new code has -
     the version number is identical in both and proves nothing.

WARN
}

warn_if_a_fresher_binary_exists "$BIN"

# --- Assemble bundle ------------------------------------------------------
APP="$REPO_ROOT/dist/Splitlane.app"
CONTENTS="$APP/Contents"
MACOS_DIR="$CONTENTS/MacOS"
RESOURCES_DIR="$CONTENTS/Resources"

rm -rf "$APP"
mkdir -p "$MACOS_DIR" "$RESOURCES_DIR"

install -m 0755 "$BIN" "$MACOS_DIR/splitlane"
install -m 0644 "$ICNS_SRC" "$RESOURCES_DIR/Splitlane.icns"

# Substitute @VERSION@ in the Info.plist template. `sed -e` keeps the
# command portable between BSD sed (macOS) and GNU sed (Linux CI).
sed -e "s/@VERSION@/$VERSION/g" "$INFO_PLIST_SRC" > "$CONTENTS/Info.plist"
chmod 0644 "$CONTENTS/Info.plist"

# Ad-hoc sign the bundle under its OWN identifier, and do it here rather than
# leaving whatever the linker put on the binary.
#
# On Apple Silicon every binary is ad-hoc signed at link time, under an
# identifier the linker derives from the file - `splitlane-03fb9d385aae0c6a` -
# which therefore changes with every build. macOS keys TCC (the Desktop /
# Documents / Downloads grants) on the signing identity, so a bundle whose
# identifier changed is a different application to the OS: every folder
# permission the person granted is gone and the prompts come back. That is the
# prompt people see, arriving for a reason that has nothing to do
# with what the app reads.
#
# `-i` pins it to the bundle identifier the Info.plist already states, so the
# two cannot drift. This is only for a locally installed build: the release
# path re-signs with the Developer ID (`scripts/sign-macos.sh` uses
# `codesign --force`), which replaces this outright.
if command -v codesign >/dev/null 2>&1; then
    BUNDLE_ID="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' \
        "$CONTENTS/Info.plist" 2>/dev/null || echo "")"
    if [ -n "$BUNDLE_ID" ]; then
        codesign --force --sign - -i "$BUNDLE_ID" "$APP" >/dev/null 2>&1 \
            || echo "warning: could not ad-hoc sign the bundle" >&2
    fi
fi

echo "Built bundle: $APP ($ARCH, v$VERSION)"
# The binary and its date, because "Built bundle" plus a version number is
# exactly what a stale build also prints - the version lives in Cargo.toml and
# Info.plist and is identical across builds of the same release.
echo "         from: $BIN"
echo "         built: $(bin_mtime "$BIN")"

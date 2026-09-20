#!/usr/bin/env bash
# Bundle Splitlane into a Zed-style user-local tar.gz.
#
# Output layout:
#   splitlane.app/
#     bin/splitlane
#     share/applications/splitlane.desktop
#     share/icons/hicolor/{16,32,48,128,256,512}x{..}/apps/splitlane.png
#     share/metainfo/io.github.ivkan.splitlane.metainfo.xml
#     LICENSE
#     THIRD_PARTY_NOTICES.md
#     README.md
#     install.sh
#
# Usage:
#   scripts/bundle-tarball.sh                  # reads version from Cargo.toml
#   scripts/bundle-tarball.sh 0.1.7            # explicit version
#   TARGET=x86_64-unknown-linux-gnu scripts/bundle-tarball.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd -P)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd -P)"

ARCH="${ARCH:-x86_64}"
TARGET_TRIPLE="${TARGET:-}"

if [ "$#" -ge 1 ]; then
    VERSION="$1"
else
    VERSION="$(awk -F'"' '/^version = / { print $2; exit }' "$REPO_ROOT/Cargo.toml")"
fi
if [ -z "${VERSION:-}" ]; then
    echo "error: could not determine version (pass as arg or set in Cargo.toml)" >&2
    exit 1
fi

if [ -n "$TARGET_TRIPLE" ]; then
    BIN="$REPO_ROOT/target/$TARGET_TRIPLE/release/splitlane"
else
    BIN="$REPO_ROOT/target/release/splitlane"
fi

if [ ! -x "$BIN" ]; then
    echo "error: release binary not found at $BIN" >&2
    echo "hint:  run 'cargo build --release${TARGET_TRIPLE:+ --target $TARGET_TRIPLE} -p splitlane-app' first" >&2
    exit 1
fi

BUNDLE_DIR="$REPO_ROOT/target/bundle"
APP="$BUNDLE_DIR/splitlane.app"
TARBALL="$BUNDLE_DIR/splitlane-${VERSION}-${ARCH}.tar.gz"

rm -rf "$APP"
mkdir -p "$APP/bin" \
         "$APP/share/applications" \
         "$APP/share/metainfo"

install -m 755 "$BIN" "$APP/bin/splitlane"
install -m 644 "$REPO_ROOT/assets/splitlane.desktop" "$APP/share/applications/splitlane.desktop"
install -m 644 "$REPO_ROOT/assets/io.github.ivkan.splitlane.metainfo.xml" \
               "$APP/share/metainfo/io.github.ivkan.splitlane.metainfo.xml"

for size in 16 32 48 128 256 512; do
    dest="$APP/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$dest"
    install -m 644 "$REPO_ROOT/assets/icons/splitlane-${size}.png" "$dest/splitlane.png"
done

install -m 644 "$REPO_ROOT/LICENSE"   "$APP/LICENSE"
install -m 644 "$REPO_ROOT/README.md" "$APP/README.md"
install -m 644 "$REPO_ROOT/native/libghostty/THIRD_PARTY_NOTICES.md" \
               "$APP/THIRD_PARTY_NOTICES.md"
install -m 755 "$SCRIPT_DIR/tarball-install.sh" "$APP/install.sh"

# Reproducible tar: sorted entries, fixed ownership, fixed mtime.
MTIME="${SOURCE_DATE_EPOCH:-$(date +%s)}"
( cd "$BUNDLE_DIR" \
  && tar \
       --sort=name \
       --owner=0 --group=0 --numeric-owner \
       --mtime="@$MTIME" \
       -cf - splitlane.app \
     | gzip -n -9 > "$TARBALL" )

echo "$TARBALL"

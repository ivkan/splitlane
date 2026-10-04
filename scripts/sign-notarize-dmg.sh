#!/usr/bin/env bash
# Sign, notarize and staple the Splitlane .dmg itself.
#
# The app inside the image is already signed, notarized and stapled by
# `scripts/sign-macos.sh` and `scripts/notarize-macos.sh`. That covers the
# app once it is in /Applications. It does not cover the image: an unsigned
# .dmg has no ticket of its own, so opening it on a machine that cannot reach
# Apple gets no Gatekeeper answer for the download, and "signed and
# notarized" said about the release asset is only true of what is inside it.
#
# Run this on a macOS runner AFTER `scripts/create-dmg.sh`, and BEFORE the
# image is checksummed: stapling rewrites the file.
#
# This repeats the keychain setup and the notary polling from the two app
# scripts rather than sharing them. Those two gate every release and are
# proven; this step is new, and it should be able to fail without anything
# in them having changed.
#
# Required env vars (sourced from GitHub Secrets in release.yml):
#   APPLE_DEVELOPER_CERT_P12        base64-encoded .p12 file
#   APPLE_DEVELOPER_CERT_PASSWORD   password to decrypt the .p12
#   APPLE_ID                        developer account email
#   APPLE_APP_SPECIFIC_PASSWORD     app-specific password
#   APPLE_TEAM_ID                   10-char team ID
#
# Usage:
#   scripts/sign-notarize-dmg.sh dist/splitlane-<version>-<arch>-apple-darwin.dmg
set -euo pipefail

DMG="${1:-}"

[ -n "$DMG" ] || { echo "usage: $0 <path-to.dmg>" >&2; exit 1; }
[ -f "$DMG" ] || { echo "error: image not found: $DMG" >&2; exit 1; }

: "${APPLE_DEVELOPER_CERT_P12:?APPLE_DEVELOPER_CERT_P12 env var is required}"
: "${APPLE_DEVELOPER_CERT_PASSWORD:?APPLE_DEVELOPER_CERT_PASSWORD env var is required}"
: "${APPLE_ID:?APPLE_ID env var is required}"
: "${APPLE_APP_SPECIFIC_PASSWORD:?APPLE_APP_SPECIFIC_PASSWORD env var is required}"
: "${APPLE_TEAM_ID:?APPLE_TEAM_ID env var is required}"

# --- Ephemeral keychain ---------------------------------------------------
# A different name from the app-signing keychain: that one is deleted when
# its script exits, and a leftover from a killed run must not be mistaken
# for this one.
KEYCHAIN_PASSWORD="$(openssl rand -hex 32)"
KEYCHAIN="build-dmg.keychain"
CERT_P12="$(mktemp -t splitlane-cert.XXXXXX).p12"

cleanup() {
    security delete-keychain "$KEYCHAIN" 2>/dev/null || true
    rm -f "$CERT_P12"
}
trap cleanup EXIT

# BSD base64 (`-D`); the here-string keeps the secret out of argv.
base64 -D > "$CERT_P12" <<< "$APPLE_DEVELOPER_CERT_P12"

security delete-keychain "$KEYCHAIN" 2>/dev/null || true
security create-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"
security set-keychain-settings -lut 3600 "$KEYCHAIN"
security unlock-keychain -p "$KEYCHAIN_PASSWORD" "$KEYCHAIN"

# shellcheck disable=SC2046
security list-keychains -d user -s "$KEYCHAIN" $(security list-keychains -d user | tr -d '"')

security import "$CERT_P12" \
    -k "$KEYCHAIN" \
    -P "$APPLE_DEVELOPER_CERT_PASSWORD" \
    -T /usr/bin/codesign

# Without this, codesign asks for the key through a GUI prompt and the job
# hangs.
security set-key-partition-list \
    -S apple-tool:,apple:,codesign: \
    -s -k "$KEYCHAIN_PASSWORD" \
    "$KEYCHAIN" > /dev/null

IDENTITY="$(security find-identity -v -p codesigning "$KEYCHAIN" \
    | awk -F'"' '/Developer ID Application/ { print $2; exit }')"

if [ -z "$IDENTITY" ]; then
    echo "error: no 'Developer ID Application' identity in $KEYCHAIN" >&2
    security find-identity -v "$KEYCHAIN" >&2 || true
    exit 1
fi

if [[ "$IDENTITY" != *"($APPLE_TEAM_ID)"* ]]; then
    echo "error: signing identity team ID does not match APPLE_TEAM_ID" >&2
    exit 1
fi

# --- Sign -----------------------------------------------------------------
# No `--options runtime` and no entitlements: those describe an executable,
# and a disk image is not one. `--timestamp` is what the notary requires.
codesign --force --sign "$IDENTITY" --timestamp --keychain "$KEYCHAIN" "$DMG"
codesign --verify --verbose=2 "$DMG"

# --- Notarize -------------------------------------------------------------
# A .dmg is submitted as it is; only an .app needs wrapping in an archive.
# Submit without `--wait` and poll, for the same reasons as the app script:
# a heartbeat in the log, a submission id that survives a killed job, and a
# bounded wait.
echo "Submitting $DMG to notarytool..."
SUBMIT_JSON="$(xcrun notarytool submit "$DMG" \
    --apple-id "$APPLE_ID" \
    --password "$APPLE_APP_SPECIFIC_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" \
    --output-format json)"

SUBMISSION_ID="$(python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])' <<< "$SUBMIT_JSON")"

echo "Submission ID: $SUBMISSION_ID"
echo "(If this job is killed mid-poll, recover status from any Mac with:"
echo "    xcrun notarytool info $SUBMISSION_ID --apple-id <APPLE_ID> --team-id $APPLE_TEAM_ID --password <APP_SPECIFIC_PASSWORD>)"

POLL_INTERVAL=30
MAX_WAIT_SECONDS=$((90 * 60))
START_TIME=$(date +%s)

while true; do
    INFO_JSON="$(xcrun notarytool info "$SUBMISSION_ID" \
        --apple-id "$APPLE_ID" \
        --password "$APPLE_APP_SPECIFIC_PASSWORD" \
        --team-id "$APPLE_TEAM_ID" \
        --output-format json)"
    STATUS="$(python3 -c 'import json,sys; print(json.load(sys.stdin).get("status", "Unknown"))' <<< "$INFO_JSON")"

    NOW=$(date +%s)
    ELAPSED=$((NOW - START_TIME))
    ELAPSED_FMT="$(printf '%02d:%02d' $((ELAPSED / 60)) $((ELAPSED % 60)))"

    case "$STATUS" in
        Accepted)
            echo "[+${ELAPSED_FMT}] Accepted by Apple"
            break
            ;;
        Invalid|Rejected)
            echo "::error title=Notarization::Apple rejected the image (status=$STATUS, id=$SUBMISSION_ID)"
            echo "--- notarytool log $SUBMISSION_ID ---" >&2
            xcrun notarytool log "$SUBMISSION_ID" \
                --apple-id "$APPLE_ID" \
                --password "$APPLE_APP_SPECIFIC_PASSWORD" \
                --team-id "$APPLE_TEAM_ID" \
                >&2 || echo "(failed to retrieve log - Apple may still be processing)" >&2
            exit 1
            ;;
        "In Progress")
            echo "[+${ELAPSED_FMT}] In Progress... (next poll in ${POLL_INTERVAL}s)"
            ;;
        *)
            echo "[+${ELAPSED_FMT}] Unexpected status: $STATUS - continuing to poll"
            ;;
    esac

    if [ "$ELAPSED" -ge "$MAX_WAIT_SECONDS" ]; then
        echo "::error title=Notarization timeout::Submission $SUBMISSION_ID still pending after $((MAX_WAIT_SECONDS / 60)) minutes."
        echo "::error::If it later reaches Accepted, staple manually with: xcrun stapler staple $DMG"
        exit 1
    fi

    sleep "$POLL_INTERVAL"
done

# --- Staple ---------------------------------------------------------------
xcrun stapler staple "$DMG"
xcrun stapler validate "$DMG"

# --- Gatekeeper -----------------------------------------------------------
# The check Finder makes when a downloaded image is opened. `-t open` with
# the primary-signature context is the form for a disk image; `-t exec`, used
# for the app, rejects an image whatever its signature.
spctl --assess --type open --context context:primary-signature --verbose "$DMG"

echo "Signed, notarized and stapled: $DMG (submission_id=$SUBMISSION_ID)"

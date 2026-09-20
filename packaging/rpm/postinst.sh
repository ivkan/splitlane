#!/bin/sh
# RPM `%post` scriptlet - runs after the package files are written.
#
# It does not add a dnf/zypper repository. Splitlane has no package
# repository yet, so `dnf upgrade` does not pull new releases; a new version
# is installed from the GitHub release. When a repository exists, this is
# where /etc/yum.repos.d/splitlane.repo gets written, together with
# release-workflow smoke tests that check what was written.
#
# Scriptlet argument conventions:
#   $1 = 1 on a fresh install
#   $1 = 2 on an upgrade (from any previous version)

set -e

# --- Icon + desktop cache refresh ------------------------------------
# Without this, the freedesktop hicolor icon cache under
# /usr/share/icons/hicolor/icon-theme.cache and the application DB
# under /usr/share/applications/mimeinfo.cache keep pointing at the
# previous version's artwork, so GNOME Shell / KDE Plasma / docks /
# launchers keep showing stale icons after `dnf upgrade splitlane`
# even though the new PNGs are already on disk.
#
# Both commands are safe to re-run on every install and every upgrade:
# they rebuild deterministically from the current filesystem state.
# The `|| true` guard keeps the transaction green on minimal distros
# that don't ship these tools (server installs, some containers) -
# the icons work everywhere else unaffected.
if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -f /usr/share/icons/hicolor >/dev/null 2>&1 || true
fi
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q /usr/share/applications >/dev/null 2>&1 || true
fi

exit 0

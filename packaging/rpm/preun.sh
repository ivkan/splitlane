#!/bin/sh
# RPM `%preun` scriptlet - runs before a package is removed.
# Cleans up package-manager repo files on FULL uninstall only
# (a counterpart to what the `%post` scriptlet would write).
#
# Scriptlet argument conventions:
#   $1 = 0  → final uninstall (the package is going away for good)
#   $1 = 1  → upgrade in progress (an older version is being swept aside
#             so a newer one can take its place; KEEP the repo file)
#   $1 >= 2 → extremely rare parallel-install scenarios; treat like upgrade
#
# Running `rm -f` on upgrade would leave the user without a repo source
# mid-transaction - `dnf upgrade splitlane` would complete, then the very
# next `dnf check-update` would silently drop our source. So the $1 = 0
# guard is load-bearing, not ceremonial.

set -e

# NOTE: these paths are dead until the package writes them again.
# The matching postinst writes no repo file yet, so there is nothing
# here to clean up on a machine this build installed. The lines are
# kept so the cleanup half is already correct on the day it does.
if [ "$1" = "0" ]; then
    rm -f /etc/yum.repos.d/splitlane.repo
    rm -f /etc/zypp/repos.d/splitlane.repo
fi

exit 0

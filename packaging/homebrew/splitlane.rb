# Splitlane Homebrew cask - canonical template.
#
# This file is the SOURCE OF TRUTH that
# `.github/workflows/update_cask.yml` copies (and version-stamps) into
# the external tap repo `ivkan/homebrew-splitlane` on every
# release. The tap repo's `Casks/splitlane.rb` is a derived artifact; do
# not hand-edit it - commit changes here instead and let the workflow
# propagate on the next release.
#
# Operator setup (one-time):
#   1. Create the public repo https://github.com/ivkan/homebrew-splitlane
#   2. Initialise it with an empty `Casks/` directory (`git init && mkdir
#      Casks && git commit -m "bootstrap"`).
#   3. Create an ed25519 SSH deploy key on the tap repo with write access,
#      add the private half to this repo's Actions secrets as
#      `HOMEBREW_TAP_DEPLOY_KEY`.
#   4. The next release (update_cask.yml chains off the `release`
#      workflow's completion via workflow_run) will render + push the
#      tap's `Casks/splitlane.rb`; the workflow keeps it current
#      thereafter.

cask "splitlane" do
  # These two lines are rewritten on every release by the CI workflow.
  # The placeholders keep the file syntactically valid (so `brew style`
  # passes in CI) and flag that a human-edited version is stale.
  version "0.0.0"
  sha256 "0000000000000000000000000000000000000000000000000000000000000000"

  url "https://github.com/ivkan/splitlane/releases/download/v#{version}/splitlane-#{version}-aarch64-apple-darwin.dmg",
      verified: "github.com/ivkan/splitlane/"

  name "Splitlane"
  desc "Native terminal workspace for parallel coding agents"
  homepage "https://github.com/ivkan/splitlane"

  # `ventura` (macOS 13) is the floor because GPUI's macOS backend targets
  # that era. Same value as `LSMinimumSystemVersion` in assets/Info.plist.
  # Bumping this here without bumping the plist (or vice versa)
  # causes install-time confusion - keep them synchronised.
  depends_on macos: ">= :ventura"
  depends_on arch: :arm64

  # Gatekeeper on macOS extracts the bundle from the DMG and installs it;
  # Homebrew handles mount/copy/unmount around this `app` stanza.
  app "Splitlane.app"

  # `zap trash:` is Homebrew's opt-in deep-clean; `brew uninstall --zap`
  # moves these directories to the user's Trash. The paths match what
  # Splitlane writes at runtime:
  #   ~/Library/Application Support/splitlane   → session.json, config.json
  #   ~/Library/Caches/splitlane                → scrollback, update-check cache
  # We intentionally do NOT zap ~/Library/Preferences/* - those may hold
  # Apple-system-managed state (window sizes, traffic-light geometry)
  # that shouldn't be nuked on an uninstall.
  zap trash: [
    "~/Library/Application Support/splitlane",
    "~/Library/Caches/splitlane",
  ]
end

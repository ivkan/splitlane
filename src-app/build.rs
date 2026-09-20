// Build scripts idiomatically `panic!` on fatal errors - that is how
// Cargo surfaces build-time failures to the user. The workspace-wide
// `clippy::panic = "deny"` policy targets production runtime code, not
// build tooling; a `?`-returning `main() -> Result<…>` here would only
// produce worse error messages via `Termination`. Allow-listed at file
// level with this justification.
#![allow(clippy::panic)]

//! Build script for `splitlane-app`.
//!
//! Responsibilities:
//! 1. Invalidate the build when telemetry-related compile-time env vars
//!    change. `option_env!("POSTHOG_API_KEY")` and
//!    `option_env!("POSTHOG_HOST")` are resolved at compile time (see
//!    `src-app/src/app/bootstrap.rs`); without these `rerun-if-env-changed`
//!    directives Cargo has no way to know the macro output depends on those
//!    vars, so rotating the key or host in CI would produce a binary that
//!    still embeds the previous value until an unrelated source change
//!    forces a rebuild.
//!
//! 2. **Embedded binary staging.** Build the
//!    `splitlane-shim`, `splitlane-ai-hook` and `splitlane-mcp` workspace
//!    binaries for the current target triple and stage them into
//!    `src-app/target/embed/bin/<target>/` so the `Bins` `RustEmbed` struct
//!    in `src-app/src/assets.rs` picks them up at compile time. A nested
//!    `cargo build` is used rather than relying on workspace build ordering
//!    because `splitlane-app` does not directly depend on any of those
//!    crates - without this step they would not be guaranteed to exist when
//!    `rust-embed` expands. `splitlane-mcp` (the MCP pane-context bridge) is
//!    embedded here so every package ships it with zero new CI step; it is
//!    extracted at launch to a stable path by
//!    `ai_hooks::extract::ensure_bridge_extracted`.
//!
//!    The nested build uses a **separate `--target-dir`**
//!    (`<workspace>/target/embed-build`) so it does not fight the outer
//!    cargo for the same target-dir lock. The cost is duplicated
//!    compilation of the shim + hook + bridge dependency closure; all three
//!    closures are tiny (serde_json, tempfile, interprocess) so the overhead
//!    is acceptable and far cheaper than designing a shared build graph.
//!
//!    Size budget: total embedded bytes per target triple must stay
//!    ≤ the documented cap on `EMBED_SIZE_LIMIT_BYTES`. The check fails the
//!    outer build when exceeded rather than silently shipping a bloated
//!    `splitlane` binary.
//!
//!    Escape hatch: setting `SPLITLANE_SKIP_EMBED_BUILD=1` skips the nested
//!    build - useful in CI pre-stages that build the nested crates
//!    separately and pre-populate `target/embed/bin/<target>/`, and for
//!    fast iteration on the main crate when the nested binaries have not
//!    changed. The staging dir must still be populated when the `Bins`
//!    `RustEmbed` macro expands - rust-embed 8.x panics on missing folders.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Hard cap on the total bytes staged under `target/embed/bin/<target>/`.
/// Enforced to keep the main Splitlane binary slim.
///
/// Measured release-min sizes (Linux x86_64, 2026-05-29):
///
/// ```text
///   splitlane-shim      455_320 B
///   splitlane-ai-hook   360_448 B
///   splitlane-mcp       426_216 B   (added with the MCP bridge)
///   ----------------------------
///   total            1_241_984 B  (~1.18 MB)
/// ```
///
/// The previous 1 MB cap (shim + ai-hook only ≈ 815 KB) no longer fits once
/// the MCP bridge is embedded. Raised to 1.75 MiB (1_835_008 B), leaving
/// ~593 KB / 48% headroom over the Linux total to absorb per-triple variance
/// (Windows `.exe` and macOS Mach-O binaries run larger than ELF). The guard
/// stays active: the outer build still fails if the staged total exceeds
/// this cap, so an unexpectedly bloated dependency cannot ship silently.
const EMBED_SIZE_LIMIT_BYTES: u64 = 1_835_008;
fn main() {
    // 1. Telemetry env vars (unchanged behavior - preserved so a key
    //    rotation forces the downstream `option_env!` to be re-resolved).
    println!("cargo:rerun-if-env-changed=POSTHOG_API_KEY");
    println!("cargo:rerun-if-env-changed=POSTHOG_HOST");
    println!("cargo:rerun-if-env-changed=SPLITLANE_SKIP_EMBED_BUILD");
    // Baked into the Windows self-updater by `option_env!`
    // (`update/windows/msi.rs`): the publisher an update MSI must be
    // signed by. Without this line a rebuild after changing it would
    // silently keep the old value.
    println!("cargo:rerun-if-env-changed=SPLITLANE_EXPECTED_SIGNER_ORG");

    // 2. Stage the AI-hook binaries into a dir that
    //    `assets::Bins` (rust-embed) will ingest.
    let target = std::env::var("TARGET").expect("cargo always sets TARGET for build scripts");
    // Expose the triple to source code via `env!("SPLITLANE_TARGET_TRIPLE")`
    // so `ai_hooks::extract` can locate the correct sub-folder under
    // `bin/<triple>/` at runtime without re-deriving it from `std::env::consts`.
    println!("cargo:rustc-env=SPLITLANE_TARGET_TRIPLE={target}");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("cargo always sets CARGO_MANIFEST_DIR for build scripts"),
    );
    let workspace_root = manifest_dir
        .parent()
        .expect("src-app manifest dir has a parent (the workspace root)")
        .to_path_buf();

    // Windows: embed the multi-resolution app icon into splitlane.exe as a
    // resource. The bare .exe otherwise shows the generic Windows icon in
    // Explorer; the GPUI runtime window icon (set from the embedded PNG) is
    // unaffected. Uses the same assets/Splitlane.ico that cargo-wix ships in
    // the MSI, regenerated by scripts/build-icons.sh.
    #[cfg(windows)]
    embed_windows_app_icon(&workspace_root);

    // 3. Build identity for the About dialog.
    emit_build_revision(&workspace_root);

    // The folder `RustEmbed` points at, relative to CARGO_MANIFEST_DIR.
    // Keep the in-memory/on-disk folder layout aligned with the macro.
    let embed_root = manifest_dir.join("target").join("embed").join("bin");
    let embed_dir = embed_root.join(&target);
    fs::create_dir_all(&embed_dir).unwrap_or_else(|e| {
        panic!(
            "cannot create embed staging dir {}: {e}",
            embed_dir.display()
        )
    });

    // Rerun when the shim / hook crate sources change. Cargo watches
    // directories recursively when a directory path is emitted.
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("crates/splitlane-shim").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("crates/splitlane-ai-hook").display()
    );
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("crates/splitlane-mcp").display()
    );
    // Also rerun if the root manifest changes (workspace-wide lint policy,
    // dep version bumps, etc., affect the staged binaries).
    println!(
        "cargo:rerun-if-changed={}",
        workspace_root.join("Cargo.toml").display()
    );
    // Explicit per-FILE watches for the shim's `include_str!`'d plugin assets.
    // A directory `rerun-if-changed` only catches add/remove/rename (the dir
    // mtime), NOT a content edit of a nested file on Windows - so without these
    // an edited `*-splitlane-status.ts` would silently not be re-embedded.
    for asset in [
        "crates/splitlane-shim/assets/opencode-splitlane-status.ts",
        "crates/splitlane-shim/assets/pi-splitlane-status.ts",
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            workspace_root.join(asset).display()
        );
    }

    let skip_nested_build = std::env::var_os("SPLITLANE_SKIP_EMBED_BUILD").is_some();
    if !skip_nested_build {
        stage_ai_hook_binaries(&workspace_root, &target, &embed_dir);
    } else {
        println!(
            "cargo:warning=SPLITLANE_SKIP_EMBED_BUILD is set - assuming {} is already populated",
            embed_dir.display()
        );
    }

    // Whether the nested build ran or not, enforce the size budget so a
    // pre-populated staging dir also honors the cap.
    enforce_embed_size_budget(&embed_dir);
}

/// Emit `SPLITLANE_GIT_REV` - the short commit this binary was compiled from,
/// read back by the About dialog via `env!`.
///
/// Why it exists: `CARGO_PKG_VERSION` is identical between two builds of the
/// same version, so nothing on screen distinguishes a running instance from
/// the source tree in front of you. During the August 2026 interface work a
/// change was declared broken three separate times because an older binary
/// was still on screen.
///
/// Cargo only re-runs a build script when a watched input changes, so the
/// revision is kept fresh by watching `.git/HEAD` and whatever ref it points
/// at: a commit or a checkout invalidates it. The working tree is
/// deliberately **not** watched - a `-dirty` marker would need every source
/// file watched (relinking the whole crate on each edit), and the build
/// timestamp shown beside the revision answers the "is this binary older than
/// my edit?" question exactly. That timestamp is read at runtime from the
/// executable's own mtime rather than baked in here, precisely because a
/// baked-in constant goes stale whenever this script does not re-run.
///
/// Falls back to `unknown` when git is absent or the source is a tarball
/// without a repository - a release built that way still reports its version.
fn emit_build_revision(workspace_root: &Path) {
    // A worktree or submodule checkout has `.git` as a *file* pointing
    // elsewhere. The revision below is still read (git resolves it), only
    // the invalidation watches are skipped. Missing paths are never emitted
    // as watches: Cargo re-runs the script unconditionally for those, which
    // would relink the crate on every build.
    let git_dir = workspace_root.join(".git");
    if git_dir.is_dir() {
        let head = git_dir.join("HEAD");
        if let Ok(contents) = fs::read_to_string(&head) {
            println!("cargo:rerun-if-changed={}", head.display());
            if let Some(reference) = contents.strip_prefix("ref: ") {
                let loose = git_dir.join(reference.trim());
                if loose.is_file() {
                    println!("cargo:rerun-if-changed={}", loose.display());
                }
                // A freshly cloned or gc'd repository keeps the branch tip in
                // `packed-refs` instead of a loose file.
                let packed = git_dir.join("packed-refs");
                if packed.is_file() {
                    println!("cargo:rerun-if-changed={}", packed.display());
                }
            }
        }
    }

    let rev = Command::new("git")
        .current_dir(workspace_root)
        .args(["rev-parse", "--short=9", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|rev| !rev.is_empty())
        .unwrap_or_else(|| "unknown".to_owned());
    println!("cargo:rustc-env=SPLITLANE_GIT_REV={rev}");
}

/// Invoke a child `cargo build` against the workspace to produce the
/// `splitlane-shim`, `splitlane-ai-hook` and `splitlane-mcp` binaries for
/// `target`, then copy them into `embed_dir`. Panics (fails the outer
/// build) on any non-success exit, non-existent artifact, or IO error.
fn stage_ai_hook_binaries(workspace_root: &Path, target: &str, embed_dir: &Path) {
    // Use a dedicated `--target-dir` so we do not fight the outer cargo
    // for `target/debug/.cargo-lock` or `target/release/.cargo-lock`.
    // `embed-build` is a sibling of the outer `target/<profile>/` tree.
    let nested_target_dir = workspace_root.join("target").join("embed-build");

    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let profile = "release-min";

    // Run the nested cargo from the workspace root so `-p <crate>` is
    // resolved unambiguously and the workspace's `[patch.crates-io]`
    // block is honored.
    let mut cmd = Command::new(&cargo);
    cmd.current_dir(workspace_root)
        .arg("build")
        .arg("--profile")
        .arg(profile)
        .arg("--target")
        .arg(target)
        .arg("--target-dir")
        .arg(&nested_target_dir)
        .arg("-p")
        .arg("splitlane-shim")
        .arg("-p")
        .arg("splitlane-ai-hook")
        .arg("-p")
        .arg("splitlane-mcp")
        // Prevent the nested cargo from inheriting the outer cargo's
        // target-dir via `CARGO_TARGET_DIR` - the explicit `--target-dir`
        // above already pins it, but removing the env avoids confusion if
        // the parent environment sets it.
        .env_remove("CARGO_TARGET_DIR")
        // `RUSTFLAGS` changes (e.g. `-C link-arg=...` from sccache setups)
        // would invalidate the nested cache on every outer build. Leave
        // them alone; Cargo deals with that via its own fingerprinting.
        ;

    let status = cmd
        .status()
        .unwrap_or_else(|e| panic!("failed to spawn nested cargo build: {e}"));
    if !status.success() {
        panic!(
            "nested `cargo build --profile {profile} -p splitlane-shim -p splitlane-ai-hook -p splitlane-mcp --target {target}` \
             failed with {status}. Re-run the outer build with verbose logging to see the child cargo output."
        );
    }

    // Cargo lays artifacts out at
    // `<target-dir>/<triple>/<profile-dir>/<binary>[.exe]`.
    // For custom profiles the `<profile-dir>` equals the profile name
    // (release-min → release-min).
    let artifact_dir = nested_target_dir.join(target).join(profile);

    let bin_exe = if target.contains("windows") {
        ".exe"
    } else {
        ""
    };

    // Copy only the three binaries we need; anything else in
    // `artifact_dir` is a transitive build product we don't want to embed.
    for bin in ["splitlane-shim", "splitlane-ai-hook", "splitlane-mcp"] {
        let src = artifact_dir.join(format!("{bin}{bin_exe}"));
        let dst = embed_dir.join(format!("{bin}{bin_exe}"));

        if !src.exists() {
            panic!(
                "expected nested build artifact {} is missing - \
                 did the child cargo build silently skip this binary?",
                src.display()
            );
        }
        // `fs::copy` preserves mode on Unix; embedded bytes don't need
        // the executable bit (the extractor sets it), but a 0o755 here
        // keeps `ls -l target/embed/bin/<triple>/` self-documenting.
        fs::copy(&src, &dst)
            .unwrap_or_else(|e| panic!("copy {} → {} failed: {e}", src.display(), dst.display()));
    }
}

/// Enforce the `EMBED_SIZE_LIMIT_BYTES` total embedded-bytes cap.
/// Inspects only top-level files in `embed_dir` - there are no subdirs
/// in the per-target staging layout so a recursive walk is not warranted.
fn enforce_embed_size_budget(embed_dir: &Path) {
    let mut total: u64 = 0;
    let mut per_file: BTreeMap<String, u64> = BTreeMap::new();
    let iter = match fs::read_dir(embed_dir) {
        Ok(iter) => iter,
        Err(e) => panic!("cannot read embed staging dir {}: {e}", embed_dir.display()),
    };
    for entry in iter {
        let entry =
            entry.unwrap_or_else(|e| panic!("broken embed dir entry in {embed_dir:?}: {e}"));
        let metadata = entry
            .metadata()
            .unwrap_or_else(|e| panic!("cannot stat {}: {e}", entry.path().display()));
        if metadata.is_file() {
            let size = metadata.len();
            total = total.saturating_add(size);
            per_file.insert(entry.file_name().to_string_lossy().into_owned(), size);
        }
    }

    if total > EMBED_SIZE_LIMIT_BYTES {
        let mut details = String::new();
        for (name, size) in &per_file {
            details.push_str(&format!("  {name}: {size} bytes\n"));
        }
        panic!(
            "embedded binaries exceed the {EMBED_SIZE_LIMIT_BYTES}-byte cap ({total} bytes).\n\
             Staging dir: {}\n\
             Per-file:\n{details}\
             Shrink shim/ai-hook/splitlane-mcp via smaller deps or a tighter release-min profile, \
             or raise EMBED_SIZE_LIMIT_BYTES with a fresh measurement note.",
            embed_dir.display()
        );
    }
}

/// Embed the multi-resolution application icon into `splitlane.exe` via the
/// Windows resource compiler. Best-effort: a missing icon or resource
/// compiler (`rc.exe` from the Windows SDK) downgrades to a `cargo:warning`
/// and the build proceeds with the default Windows icon, so dev machines
/// without the SDK still build. winresource is a `cfg(windows)`
/// build-dependency, so this never compiles on Linux/macOS.
#[cfg(windows)]
fn embed_windows_app_icon(workspace_root: &Path) {
    let icon = workspace_root.join("assets").join("Splitlane.ico");
    let Some(icon_str) = icon.to_str() else {
        println!(
            "cargo:warning=winresource: icon path {} is not valid UTF-8; skipping exe icon embed",
            icon.display()
        );
        return;
    };
    if !icon.exists() {
        println!("cargo:warning=winresource: {icon_str} not found; skipping exe icon embed");
        return;
    }
    // Re-run the build script when the icon artwork changes.
    println!("cargo:rerun-if-changed={icon_str}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon_str);
    if let Err(e) = res.compile() {
        println!(
            "cargo:warning=winresource: failed to embed {icon_str} into splitlane.exe ({e}); \
             the exe will use the default Windows icon"
        );
    }
}

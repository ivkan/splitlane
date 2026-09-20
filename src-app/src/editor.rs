//! Cross-platform "open this source file at line:col" - invoked when the
//! user Cmd/Ctrl-clicks a `path:42:7` style reference in a terminal pane.
//!
//! Strategy (in order):
//! 1. `$VISUAL` then `$EDITOR` env. The string is parsed as a shell command
//!    (binary + flags) so users running `EDITOR="code --wait"` get their
//!    pre-set flags carried over. If the binary is one of the well-known
//!    editors with a documented line:col syntax (code/zed/subl/cursor/
//!    nvim/vim/helix/emacs), the right argv is appended.
//! 2. Probed fallback chain - `code`, `cursor`, `zed`, `subl`, `nvim`,
//!    `vim`, `hx`, `emacs` (in that order). First binary found on `PATH`
//!    wins.
//! 3. Last-resort: `open::that(path)` so the OS launcher (`xdg-open` /
//!    `open` / `start`) hands the file to its registered handler. Loses
//!    the line/col target but always does something useful.
//!
//! Platform notes:
//! - Linux/macOS: editor names are looked up via `which` on `$PATH`.
//! - Windows: same. `code.cmd` is the common shim under `%LocalAppData%
//!   \Programs\Microsoft VS Code\bin`, which `which` resolves correctly
//!   when that dir is on `Path`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Family of recognised editor binaries, each with a distinct argv shape
/// for "open at line and column".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorKind {
    /// VS Code / Cursor / Codium clones - `code -g path:line:col`
    VsCodeLike,
    /// Zed - `zed path:line:col` (no flag needed; the colon syntax is
    /// recognised since 0.130).
    Zed,
    /// Sublime Text - `subl path:line:col`
    Sublime,
    /// Neovim / Vim - `nvim +line path` (col not natively supported as
    /// argv; we drop it). Could be extended with `+call cursor(L, C)`
    /// but that gets messy across remote/server modes.
    VimFamily,
    /// Helix - `hx path:line:col`
    Helix,
    /// Emacs - `emacs +line:col path` (line and optional col separated
    /// by `:`)
    Emacs,
    /// Unknown binary - invoke with bare `path` only (no location).
    Unknown,
}

impl EditorKind {
    fn from_binary_name(name: &str) -> Self {
        // Strip the directory portion and any `.exe` / `.cmd` suffix so the
        // matcher is OS-agnostic - `which code.cmd` on Windows still maps
        // to `VsCodeLike`.
        let base = Path::new(name)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or(name)
            .to_ascii_lowercase();
        match base.as_str() {
            "code" | "code-insiders" | "codium" | "cursor" | "windsurf" => Self::VsCodeLike,
            "zed" | "zed-preview" | "zed-nightly" => Self::Zed,
            "subl" | "sublime_text" => Self::Sublime,
            "nvim" | "vim" | "vi" | "nvim-qt" | "gvim" | "mvim" => Self::VimFamily,
            "hx" | "helix" => Self::Helix,
            "emacs" | "emacsclient" => Self::Emacs,
            _ => Self::Unknown,
        }
    }

    /// Build the argv tail that opens `path` at `line` / `col` for this
    /// editor family. Caller prepends the editor binary itself.
    fn argv_for(self, path: &Path, line: Option<u32>, col: Option<u32>) -> Vec<String> {
        let path_str = path.to_string_lossy().into_owned();
        match self {
            Self::VsCodeLike => {
                let mut args = vec!["-g".to_string()];
                args.push(format_path_line_col(&path_str, line, col));
                args
            }
            Self::Zed | Self::Sublime | Self::Helix => {
                // Bare positional, colon syntax recognised by the editor.
                vec![format_path_line_col(&path_str, line, col)]
            }
            Self::VimFamily => {
                let mut args = Vec::new();
                if let Some(l) = line {
                    args.push(format!("+{l}"));
                }
                args.push(path_str);
                args
            }
            Self::Emacs => {
                let mut args = Vec::new();
                if let Some(l) = line {
                    let token = match col {
                        Some(c) => format!("+{l}:{c}"),
                        None => format!("+{l}"),
                    };
                    args.push(token);
                }
                args.push(path_str);
                args
            }
            Self::Unknown => vec![path_str],
        }
    }
}

fn format_path_line_col(path: &str, line: Option<u32>, col: Option<u32>) -> String {
    match (line, col) {
        (Some(l), Some(c)) => format!("{path}:{l}:{c}"),
        (Some(l), None) => format!("{path}:{l}"),
        (None, _) => path.to_string(),
    }
}

/// Parse a shell-style env value into (binary, leading-flags). Splits on
/// whitespace outside quotes; the first token is the binary, the rest are
/// extra flags the user pre-configured (e.g. `EDITOR="code --wait"`).
/// Returns `None` when the value is empty after trim.
fn parse_env_editor(value: &str) -> Option<(String, Vec<String>)> {
    let mut parts = split_editor_command_line(value).into_iter();
    let bin = parts.next()?;
    if bin.is_empty() {
        return None;
    }
    Some((bin, parts.collect()))
}

fn split_editor_command_line(value: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = value.chars().peekable();
    let mut quote: Option<char> = None;
    let mut token_started = false;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    current.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' if matches!(chars.peek(), Some('"') | Some('\\')) => {
                    current.push(chars.next().expect("peeked char exists"));
                }
                _ => current.push(ch),
            },
            Some(_) => unreachable!("only single and double quotes are set"),
            None if ch.is_whitespace() => {
                if token_started {
                    args.push(std::mem::take(&mut current));
                    token_started = false;
                }
            }
            None if matches!(ch, '\'' | '"') => {
                quote = Some(ch);
                token_started = true;
            }
            None => {
                current.push(ch);
                token_started = true;
            }
        }
    }

    if token_started {
        args.push(current);
    }
    args
}

fn resolve_editor_command(command: &str) -> PathBuf {
    let path = Path::new(command);
    if path.is_absolute() || path.components().count() > 1 {
        PathBuf::from(command)
    } else {
        crate::app::workspace_ops::resolve_editor_binary(command)
    }
}

/// Ordered probe list for the fallback chain when no `$VISUAL`/`$EDITOR`
/// is set. Order matters: GUI editors first (more likely the user's
/// daily driver), then terminal editors.
const FALLBACK_PROBES: &[&str] = &[
    "code",
    "cursor",
    "zed",
    "subl",
    "code-insiders",
    "windsurf",
    "hx",
    "nvim",
    "vim",
    "emacs",
];

/// What the `external_editor` setting says, resolved to what this module does
/// about it.
///
/// Named as an enum rather than passed as a string so the three cases are
/// exhaustive here rather than in every caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EditorPreference {
    /// `auto`, or nothing set: `$VISUAL` / `$EDITOR`, then the probe list,
    /// then the OS handler.
    Auto,
    /// `system`: hand the file straight to the OS handler. Loses line and
    /// column, which is what "let the system decide" costs.
    System,
    /// A named binary. Forced ahead of everything, and **falls through to
    /// `Auto` when it is not installed** rather than failing: a machine where
    /// the chosen editor is missing should still open the file.
    Named(String),
}

impl EditorPreference {
    /// Read the config's `external_editor`. Anything this build does not
    /// recognise is [`EditorPreference::Auto`] - the same tolerant reading the
    /// rest of the config gets, and the setting's own default.
    pub fn from_config(config: &splitlane_config::schema::SplitlaneConfig) -> Self {
        match config.external_editor.as_deref() {
            None | Some("auto") | Some("") => Self::Auto,
            Some("system") => Self::System,
            Some(name) => Self::Named(name.to_string()),
        }
    }
}

/// Open `path` in the user's preferred editor at the given location.
/// Spawns the editor process detached - does not wait for it to exit.
///
/// Errors are logged at `warn` level and swallowed so a misconfigured
/// editor never panics the renderer. The boolean return signals only
/// whether something was actually spawned (useful for tests).
///
/// **The preference is honoured here and nowhere else.** `external_editor` had
/// been in the schema, in the loader and on a Settings row since before this
/// module existed, and had no consumer at all: the one real "open in an editor"
/// path went straight to `$VISUAL` and the probe list, so Settings -> General
/// stated a choice that changed nothing. A setting that states a choice it does
/// not make is the same defect class as a label that names the wrong thing.
pub fn open_at_location_with(
    path: &Path,
    line: Option<u32>,
    col: Option<u32>,
    preference: &EditorPreference,
) -> bool {
    match preference {
        // The OS handler is the whole answer here, not a last resort, so the
        // chain below is not consulted at all.
        EditorPreference::System => {
            return match open::that_detached(path) {
                Ok(()) => true,
                Err(err) => {
                    // The doc above promises warn-level logging, and this arm
                    // was the one place that swallowed the error whole: with
                    // no OS handler registered, `Enter` did nothing and left
                    // nothing behind to explain it.
                    log::warn!(
                        "editor: system handler could not open {}: {err}",
                        path.display()
                    );
                    false
                }
            };
        }
        EditorPreference::Named(name) => {
            // **Try the spawn; do not try to predict it.** The first version
            // of this asked `resolved != Path::new(name)` as a stand-in for
            // "was it found", which conflates two different answers:
            // `resolve_editor_command` returns an *absolute* path unchanged,
            // so `external_editor = "/usr/local/bin/myed"` - the most literal
            // way to name an editor - looked exactly like "not found" and was
            // silently ignored. `try_spawn` already logs and reports, so the
            // spawn itself is the only honest test of whether the binary is
            // there, and the fall-through below is the same either way.
            let resolved = resolve_editor_command(name);
            let kind = EditorKind::from_binary_name(name);
            let args = kind.argv_for(path, line, col);
            if try_spawn(&resolved.to_string_lossy(), &args) {
                return true;
            }
            log::warn!(
                "editor: configured external_editor {name:?} did not spawn \
                 - falling through to the usual chain"
            );
        }
        EditorPreference::Auto => {}
    }
    open_at_location(path, line, col)
}

/// The chain, with no preference consulted. Kept public because it is what
/// [`EditorPreference::Auto`] means and what every other arm falls back to.
pub fn open_at_location(path: &Path, line: Option<u32>, col: Option<u32>) -> bool {
    // 1. $VISUAL → $EDITOR
    for var in &["VISUAL", "EDITOR"] {
        if let Ok(value) = std::env::var(var)
            && let Some((bin, extra_args)) = parse_env_editor(&value)
        {
            let kind = EditorKind::from_binary_name(&bin);
            let mut args = extra_args;
            args.extend(kind.argv_for(path, line, col));
            let resolved = resolve_editor_command(&bin);
            if try_spawn(&resolved.to_string_lossy(), &args) {
                return true;
            }
            log::warn!("editor: ${var}={value:?} failed to spawn - falling through");
        }
    }

    // 2. Fallback probe
    for probe in FALLBACK_PROBES {
        let found = resolve_editor_command(probe);
        if found == PathBuf::from(probe) {
            continue;
        }
        let kind = EditorKind::from_binary_name(probe);
        let args = kind.argv_for(path, line, col);
        if try_spawn(&found.to_string_lossy(), &args) {
            return true;
        }
    }

    // 3. Last-resort: OS handler (loses line/col).
    log::warn!(
        "editor: no $VISUAL/$EDITOR and none of {:?} on PATH - falling back to OS handler",
        FALLBACK_PROBES
    );
    open::that(path).is_ok()
}

fn try_spawn(bin: &str, args: &[String]) -> bool {
    match Command::new(bin).args(args).spawn() {
        Ok(_) => {
            log::info!("editor: spawned {bin} {args:?}");
            true
        }
        Err(e) => {
            log::warn!("editor: spawn {bin} {args:?} failed: {e}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> &Path {
        Path::new(s)
    }

    #[test]
    fn editor_kind_recognises_vscode_family() {
        assert_eq!(EditorKind::from_binary_name("code"), EditorKind::VsCodeLike);
        assert_eq!(
            EditorKind::from_binary_name("cursor"),
            EditorKind::VsCodeLike
        );
        assert_eq!(
            EditorKind::from_binary_name("/usr/bin/code"),
            EditorKind::VsCodeLike
        );
        assert_eq!(
            EditorKind::from_binary_name("code.cmd"),
            EditorKind::VsCodeLike
        );
    }

    #[test]
    fn editor_kind_recognises_zed_vim_helix_emacs() {
        assert_eq!(EditorKind::from_binary_name("zed"), EditorKind::Zed);
        assert_eq!(EditorKind::from_binary_name("nvim"), EditorKind::VimFamily);
        assert_eq!(EditorKind::from_binary_name("vim"), EditorKind::VimFamily);
        assert_eq!(EditorKind::from_binary_name("hx"), EditorKind::Helix);
        assert_eq!(EditorKind::from_binary_name("emacs"), EditorKind::Emacs);
        assert_eq!(
            EditorKind::from_binary_name("emacsclient"),
            EditorKind::Emacs
        );
    }

    #[test]
    fn editor_kind_unknown_falls_back() {
        assert_eq!(
            EditorKind::from_binary_name("my-weird-editor"),
            EditorKind::Unknown
        );
        assert_eq!(EditorKind::from_binary_name(""), EditorKind::Unknown);
    }

    #[test]
    fn argv_vscode_uses_g_flag() {
        let args = EditorKind::VsCodeLike.argv_for(p("/tmp/x.rs"), Some(42), Some(7));
        assert_eq!(args, vec!["-g".to_string(), "/tmp/x.rs:42:7".to_string()]);
    }

    #[test]
    fn argv_vim_uses_plus_line_no_col() {
        let args = EditorKind::VimFamily.argv_for(p("/tmp/x.rs"), Some(42), Some(7));
        assert_eq!(args, vec!["+42".to_string(), "/tmp/x.rs".to_string()]);
    }

    #[test]
    fn argv_emacs_uses_plus_line_col() {
        let args = EditorKind::Emacs.argv_for(p("/tmp/x.rs"), Some(42), Some(7));
        assert_eq!(args, vec!["+42:7".to_string(), "/tmp/x.rs".to_string()]);
    }

    #[test]
    fn argv_zed_bare_path_colon_line() {
        let args = EditorKind::Zed.argv_for(p("/tmp/x.rs"), Some(42), None);
        assert_eq!(args, vec!["/tmp/x.rs:42".to_string()]);
    }

    #[test]
    fn argv_unknown_drops_location() {
        let args = EditorKind::Unknown.argv_for(p("/tmp/x.rs"), Some(42), Some(7));
        assert_eq!(args, vec!["/tmp/x.rs".to_string()]);
    }

    #[test]
    fn argv_no_line_no_col_just_path() {
        let args = EditorKind::VsCodeLike.argv_for(p("/tmp/x.rs"), None, None);
        assert_eq!(args, vec!["-g".to_string(), "/tmp/x.rs".to_string()]);
    }

    #[test]
    fn parse_env_editor_splits_binary_and_flags() {
        let (bin, args) = parse_env_editor("code --wait").unwrap();
        assert_eq!(bin, "code");
        assert_eq!(args, vec!["--wait".to_string()]);
    }

    #[test]
    fn parse_env_editor_preserves_quoted_windows_binary() {
        let (bin, args) =
            parse_env_editor(r#""C:\Program Files\Microsoft VS Code\bin\code.cmd" --wait"#)
                .unwrap();
        assert_eq!(bin, r"C:\Program Files\Microsoft VS Code\bin\code.cmd");
        assert_eq!(args, vec!["--wait".to_string()]);
    }

    #[test]
    fn parse_env_editor_preserves_quoted_flag_value() {
        let (bin, args) = parse_env_editor(r#"code --profile "Arthur Dev""#).unwrap();
        assert_eq!(bin, "code");
        assert_eq!(
            args,
            vec!["--profile".to_string(), "Arthur Dev".to_string()]
        );
    }

    #[test]
    fn parse_env_editor_empty_is_none() {
        assert!(parse_env_editor("").is_none());
        assert!(parse_env_editor("   ").is_none());
    }

    #[test]
    fn parse_env_editor_only_binary() {
        let (bin, args) = parse_env_editor("nvim").unwrap();
        assert_eq!(bin, "nvim");
        assert!(args.is_empty());
    }

    #[test]
    fn format_path_line_col_combinations() {
        assert_eq!(format_path_line_col("x.rs", None, None), "x.rs");
        assert_eq!(format_path_line_col("x.rs", Some(1), None), "x.rs:1");
        assert_eq!(format_path_line_col("x.rs", Some(1), Some(2)), "x.rs:1:2");
        // No line + col is invalid: col is dropped silently.
        assert_eq!(format_path_line_col("x.rs", None, Some(7)), "x.rs");
    }

    /// The setting once had no consumer at all - it was in the
    /// schema, in the loader and on a Settings row, and this module went
    /// straight to `$VISUAL`. These are the three readings it can have.
    #[test]
    fn the_configured_preference_is_read_as_three_cases() {
        let with = |value: Option<&str>| {
            let config = splitlane_config::schema::SplitlaneConfig {
                external_editor: value.map(str::to_string),
                ..Default::default()
            };
            EditorPreference::from_config(&config)
        };
        assert_eq!(with(None), EditorPreference::Auto);
        assert_eq!(with(Some("auto")), EditorPreference::Auto);
        // An empty string is what a settings row that has been cleared writes,
        // and "no opinion" is the honest reading of it.
        assert_eq!(with(Some("")), EditorPreference::Auto);
        assert_eq!(with(Some("system")), EditorPreference::System);
        assert_eq!(
            with(Some("zed")),
            EditorPreference::Named("zed".to_string())
        );
        // Not an allowlist: any binary on PATH is a legitimate answer, and a
        // name that turns out not to be installed falls back to the chain
        // rather than failing.
        assert_eq!(with(Some("hx")), EditorPreference::Named("hx".to_string()));
    }
}

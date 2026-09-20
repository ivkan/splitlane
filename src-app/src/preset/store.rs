//! Where presets live: one TOML file per preset, in a folder.
//!
//! # Why files, and why TOML
//!
//! `splitlane up` already reads this format, so "one file for the GUI and the
//! CLI" is reached by *keeping* the CLI's format rather than inventing a
//! second one. Files also give away for free what a registry of named presets
//! would have to build: a stable identity (the filename), a way to hand one to
//! a colleague, a way to open one in `$EDITOR`, and a folder the Launch pad
//! can reveal.
//!
//! The library is global rather than per project, which follows from the
//! preset itself: it carries a directory, and each of its panes carries its
//! own, so one preset can span two repositories. A per-project library could
//! not hold that preset without picking one of the two projects to own it.
//!
//! # The three rules that make GUI saving safe
//!
//! A preset file is hand-editable, and the whole point of the format is that
//! people will hand-edit it. Saving one from the interface must therefore not
//! quietly drop what the interface does not draw. Three things together give
//! that guarantee, and only together:
//!
//! 1. **A tolerant reader.** Unknown keys are reported and set aside, never
//!    refused. A file written by a later release still runs; a typo is still
//!    visible, as `pane 2: ignoring unknown key 'agnt'`.
//! 2. **A DOM writer.** [`save`] mutates the parsed document in place -
//!    only the keys the interface draws - and serializes the document. It
//!    never rebuilds the file from a struct. Reconstruction from a typed
//!    struct is exactly the mechanism by which a GUI destroys `env`,
//!    `port_base` and the worktree fields; comments and key order go the same
//!    way.
//! 3. **Additive schema evolution.** A new field is added, never repurposed,
//!    so an older release reading a newer file sets it aside instead of
//!    misreading it.
//!
//! Writes are atomic (temp + rename) because these are files a user may have
//! open in an editor.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::{PanePreset, Preset};

/// Keys a preset table may carry. Anything else is set aside with a warning.
const PRESET_KEYS: &[&str] = &["name", "layout", "cwd", "color", "port_base", "panes"];

/// Keys a `[[panes]]` table may carry.
const PANE_KEYS: &[&str] = &[
    "cwd",
    "agent",
    "command",
    "prompt",
    "focus",
    "env",
    "name",
    "worktree",
    "copy_env",
    "setup",
    "setup_timeout_secs",
    "worktree_teardown",
];

/// A preset as it exists on disk.
#[derive(Debug, Clone)]
pub struct StoredPreset {
    /// The file's stem - the preset's stable identity, and what the Launch
    /// pad's chord indexes.
    pub id: String,
    pub path: PathBuf,
    pub preset: Preset,
    /// What the reader set aside, in the reader's words. Shown once, where
    /// the preset is listed, rather than swallowed.
    pub warnings: Vec<String>,
}

/// The presets folder. `None` only when the platform has no config dir.
pub fn presets_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| {
        dir.join(splitlane_config::loader::APP_SUBDIR)
            .join("presets")
    })
}

/// Every preset in the folder, ordered by file name.
///
/// A file that cannot be read or does not parse is skipped with a log line
/// rather than failing the whole list: one broken preset must not take the
/// other four off the screen. Blocking I/O - call it off the render thread.
pub fn load_all() -> Vec<StoredPreset> {
    let Some(dir) = presets_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "toml"))
        .collect();
    paths.sort();

    let mut out = Vec::with_capacity(paths.len());
    for path in paths {
        match std::fs::read_to_string(&path) {
            Ok(src) => match parse(&src) {
                Ok((mut preset, warnings)) => {
                    let id = file_stem(&path);
                    // A file with no `name` is named by its file, which is
                    // why the stem is the identity rather than the name.
                    if preset.name.is_none() {
                        preset.name = Some(id.clone());
                    }
                    out.push(StoredPreset {
                        id,
                        path,
                        preset,
                        warnings,
                    });
                }
                Err(err) => log::warn!("preset {}: {err}", path.display()),
            },
            Err(err) => log::warn!("preset {}: {err}", path.display()),
        }
    }
    out
}

/// Parse one preset file: set aside what is not known, then type it.
///
/// The two halves are in this order on purpose. Checking keys first means a
/// typo is reported in this module's words and once, instead of surfacing as
/// a serde error whose text names a Rust type; and it lets the typed struct
/// keep `deny_unknown_fields`, which the flow spec - which nothing rewrites -
/// still wants.
pub fn parse(src: &str) -> Result<(Preset, Vec<String>), String> {
    let mut doc: DocumentMut = src.parse().map_err(|e| format!("{e}"))?;
    let warnings = strip_unknown(&mut doc);
    let preset: Preset = toml::from_str(&doc.to_string()).map_err(|e| e.to_string())?;
    preset.validate()?;
    Ok((preset, warnings))
}

/// Remove keys neither the reader nor the writer knows, reporting each.
fn strip_unknown(doc: &mut DocumentMut) -> Vec<String> {
    let mut warnings = Vec::new();
    let unknown: Vec<String> = doc
        .as_table()
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !PRESET_KEYS.contains(&key.as_str()))
        .collect();
    for key in unknown {
        warnings.push(format!("ignoring unknown key '{key}'"));
        doc.as_table_mut().remove(&key);
    }
    // `[[panes]]` and `panes = [{ … }]` are the same list to TOML and two
    // different types to `toml_edit`, and a reader that only knows the first
    // drops an inline file whole on a typo - the opposite of tolerance.
    match doc.get_mut("panes") {
        Some(Item::ArrayOfTables(panes)) => {
            for (idx, pane) in panes.iter_mut().enumerate() {
                strip_unknown_pane_keys(idx, pane, &mut warnings);
            }
        }
        Some(Item::Value(toml_edit::Value::Array(panes))) => {
            for (idx, pane) in panes.iter_mut().enumerate() {
                if let Some(pane) = pane.as_inline_table_mut() {
                    let unknown: Vec<String> = pane
                        .iter()
                        .map(|(key, _)| key.to_string())
                        .filter(|key| !PANE_KEYS.contains(&key.as_str()))
                        .collect();
                    for key in unknown {
                        warnings.push(format!("pane {idx}: ignoring unknown key '{key}'"));
                        pane.remove(&key);
                    }
                }
            }
        }
        _ => {}
    }
    warnings
}

fn strip_unknown_pane_keys(idx: usize, pane: &mut Table, warnings: &mut Vec<String>) {
    let unknown: Vec<String> = pane
        .iter()
        .map(|(key, _)| key.to_string())
        .filter(|key| !PANE_KEYS.contains(&key.as_str()))
        .collect();
    for key in unknown {
        warnings.push(format!("pane {idx}: ignoring unknown key '{key}'"));
        pane.remove(&key);
    }
}

/// Where each of `preset`'s panes came from in the document being written.
///
/// `origins[i]` is the index of the `[[panes]]` table that pane `i` was read
/// from, or `None` for a pane the editor added. This is **not** optional
/// bookkeeping: without it the writer has only position to go on, and
/// position is wrong the moment a pane is removed from anywhere but the end.
/// Removing pane 0 of three would leave pane 1's text in slot 0 - so the
/// plain shell that moved up would inherit the deleted pane's `worktree` and
/// `setup`, and the last pane's would be dropped. It parses, it validates,
/// and it silently spawns a shell inside somebody else's git worktree.
///
/// A caller writing a file that does not exist yet passes `&[]`.
pub type PaneOrigins<'a> = &'a [Option<usize>];

/// Write `preset` to `path`, preserving everything the interface does not
/// draw.
///
/// The document is the one already on disk when there is one: the drawn keys
/// are set, everything else in a pane's table travels **with that pane**
/// (see [`PaneOrigins`]) rather than with its position, and comments and key
/// order survive. A pane removed in the editor takes its whole table -
/// including its `env` and its worktree fields - with it, which is what
/// removing a pane means.
///
/// A field the document does not already carry is written out, so writing a
/// `Preset` into a *fresh* document is lossless too. That is the case the
/// migration out of `splitlane.json` is in, and without it every migrated
/// pane's `env` would go missing under a toast saying the presets had moved.
pub fn save(path: &Path, preset: &Preset, origins: PaneOrigins) -> Result<(), String> {
    let mut doc: DocumentMut = match std::fs::read_to_string(path) {
        Ok(src) => src.parse().unwrap_or_default(),
        Err(_) => DocumentMut::new(),
    };

    set_or_clear(doc.as_table_mut(), "name", preset.name.as_deref());
    doc["layout"] = value(preset.layout.as_ipc());
    set_or_clear(doc.as_table_mut(), "cwd", preset.cwd.as_deref());
    set_or_clear(doc.as_table_mut(), "color", preset.color.as_deref());
    if !doc.contains_key("port_base")
        && let Some(port_base) = preset.port_base
    {
        doc["port_base"] = value(i64::from(port_base));
    }

    let existing = doc
        .remove("panes")
        .and_then(|item| item.into_array_of_tables().ok())
        .unwrap_or_default();
    let mut panes = ArrayOfTables::new();
    for (idx, pane) in preset.panes.iter().enumerate() {
        // The table this pane came from, not the one that happens to sit at
        // this position now.
        let mut table = origins
            .get(idx)
            .copied()
            .flatten()
            .and_then(|origin| existing.get(origin).cloned())
            .unwrap_or_default();
        write_pane(&mut table, pane);
        panes.push(table);
    }
    doc["panes"] = Item::ArrayOfTables(panes);

    write_atomically(path, &doc.to_string())
}

/// The keys the interface draws are set; the ones it does not are written
/// only when the table has none, so a hand-written value is never rewritten
/// by a face that cannot show it.
fn write_pane(table: &mut Table, pane: &PanePreset) {
    set_or_clear(table, "cwd", pane.cwd.as_deref());
    set_or_clear(table, "agent", pane.agent.as_deref());
    set_or_clear(table, "command", pane.command.as_deref());
    set_or_clear(table, "prompt", pane.prompt.as_deref());
    set_or_clear(table, "name", pane.name.as_deref());
    match pane.focus {
        Some(true) => table["focus"] = value(true),
        // `focus = false` is the default; writing it would add a key to every
        // pane of every file the GUI touches.
        _ => {
            table.remove("focus");
        }
    }

    fill_absent(table, "worktree", pane.worktree.as_deref().map(value));
    fill_absent(table, "copy_env", pane.copy_env.map(value));
    fill_absent(table, "setup", pane.setup.as_deref().map(value));
    fill_absent(
        table,
        "setup_timeout_secs",
        pane.setup_timeout_secs
            .and_then(|secs| i64::try_from(secs).ok())
            .map(value),
    );
    fill_absent(
        table,
        "worktree_teardown",
        pane.worktree_teardown.as_deref().map(value),
    );
    if !table.contains_key("env")
        && let Some(env) = pane.env.as_ref().filter(|env| !env.is_empty())
    {
        let mut env_table = Table::new();
        // Sorted, because a `HashMap`'s order is not one - two saves of the
        // same preset must not produce two different files.
        let mut pairs: Vec<(&String, &String)> = env.iter().collect();
        pairs.sort();
        for (key, val) in pairs {
            env_table[key.as_str()] = value(val.as_str());
        }
        table["env"] = Item::Table(env_table);
    }
}

/// Write a key the interface does not draw, but only into a table that has
/// none - the fresh-document case.
fn fill_absent(table: &mut Table, key: &str, item: Option<Item>) {
    if table.contains_key(key) {
        return;
    }
    if let Some(item) = item {
        table[key] = item;
    }
}

/// Set a string key, or remove it when the value is blank. A blank field in
/// the editor means "no opinion", which is an absent key rather than `""`.
fn set_or_clear(table: &mut Table, key: &str, val: Option<&str>) {
    match val.map(str::trim).filter(|v| !v.is_empty()) {
        Some(v) => table[key] = value(v),
        None => {
            table.remove(key);
        }
    }
}

/// Temp + rename, because a user may have this file open.
fn write_atomically(path: &Path, contents: &str) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, contents).map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// A filename for `name` that no existing preset already uses.
///
/// The stem is the identity, so it has to be filesystem-safe and unique; the
/// preset's own `name` key carries whatever the user actually typed.
pub fn unused_path(dir: &Path, name: &str) -> PathBuf {
    let taken: BTreeSet<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| file_stem(&entry.path()))
                .collect()
        })
        .unwrap_or_default();
    let base = slug(name);
    if !taken.contains(&base) {
        return dir.join(format!("{base}.toml"));
    }
    for n in 2u32.. {
        let candidate = format!("{base}-{n}");
        if !taken.contains(&candidate) {
            return dir.join(format!("{candidate}.toml"));
        }
    }
    unreachable!("u32 exhausted")
}

/// Lowercase, ASCII-safe, hyphen-separated. Non-ASCII names collapse to
/// `preset`, which is a name, not a failure.
fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = true;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "preset".to_string()
    } else {
        trimmed
    }
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Move the presets that used to live in `splitlane.json` into the folder.
///
/// Runs once, at launch, and only when the old key is non-empty and the
/// folder is empty. Nothing after this release reads `commands[].workspace`,
/// so a user who has presets would otherwise find them gone with no way to
/// know they ever existed - "recreate it in a minute" is only true if you can
/// see what to recreate. The old key is left in place and readable; there is
/// no converter back.
///
/// Returns the names written, for the toast that says so. Blocking I/O.
pub fn migrate_from_commands(
    commands: &[splitlane_config::schema::CommandDefinition],
) -> Vec<String> {
    let Some(dir) = presets_dir() else {
        return Vec::new();
    };
    // "The folder is empty" asks the reader, not the directory listing: a
    // folder holding one unreadable file has still been used, and copying the
    // old key over that is how a user loses the edit they were in the middle
    // of.
    if !load_all().is_empty() {
        return Vec::new();
    }
    let legacy: Vec<(&str, &splitlane_config::schema::WorkspaceDefinition)> = commands
        .iter()
        .filter_map(|command| Some((command.name.as_str(), command.workspace.as_ref()?)))
        .collect();
    if legacy.is_empty() {
        return Vec::new();
    }
    if let Err(err) = std::fs::create_dir_all(&dir) {
        log::warn!("presets: cannot create {}: {err}", dir.display());
        return Vec::new();
    }

    let mut written = Vec::new();
    for (name, workspace) in legacy {
        let preset = super::legacy::preset_from_workspace_definition(workspace, name);
        if preset.panes.is_empty() {
            continue;
        }
        let path = unused_path(&dir, preset.name.as_deref().unwrap_or(name));
        match save(&path, &preset, &[]) {
            Ok(()) => written.push(preset.name.clone().unwrap_or_else(|| name.to_string())),
            Err(err) => log::warn!("presets: cannot write {}: {err}", path.display()),
        }
    }
    written
}

/// Every key this module can emit, for the golden test.
#[cfg(test)]
fn emitted_keys(src: &str) -> BTreeSet<String> {
    let doc: DocumentMut = src.parse().expect("written TOML parses");
    let mut keys: BTreeSet<String> = doc
        .as_table()
        .iter()
        .map(|(key, _)| key.to_string())
        .collect();
    if let Some(panes) = doc.get("panes").and_then(Item::as_array_of_tables) {
        for pane in panes.iter() {
            keys.extend(pane.iter().map(|(key, _)| key.to_string()));
        }
    }
    keys
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset::PresetLayout;
    use std::collections::HashMap;

    /// Keys the writer must never emit.
    ///
    /// Live state, not a recipe. `scrollback` is the one that actually
    /// matters - up to 400 KB of terminal output in a file someone will open
    /// in `$EDITOR` - but all four describe a *running* surface and none can
    /// mean anything in a saved set of panes. `SurfaceDefinition` mixes the
    /// two and is not being split; this test is the seam instead.
    const NEVER_WRITTEN: &[&str] = &["scrollback", "surface_id", "font_size", "custom_name"];

    #[test]
    fn the_writer_never_emits_live_state() {
        // The one real risk this whole design has: 400 KB of scrollback
        // landing in a file someone then opens in `$EDITOR`. A preset is a
        // recipe; none of these four can mean anything in one.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("full-stack.toml");
        let preset = Preset {
            name: Some("Full stack".to_string()),
            layout: PresetLayout::EvenV,
            cwd: Some("/work/atlas".to_string()),
            color: Some("ff6600".to_string()),
            port_base: Some(4000),
            panes: vec![
                PanePreset {
                    cwd: Some("/dev/backend".to_string()),
                    agent: Some("claude_code".to_string()),
                    prompt: Some("review the diff".to_string()),
                    focus: Some(true),
                    name: Some("api".to_string()),
                    ..Default::default()
                },
                PanePreset {
                    command: Some("pnpm dev".to_string()),
                    ..Default::default()
                },
            ],
        };
        save(&path, &preset, &[]).expect("save");
        let src = std::fs::read_to_string(&path).expect("read back");
        for forbidden in NEVER_WRITTEN {
            assert!(
                !emitted_keys(&src).contains(*forbidden),
                "writer emitted '{forbidden}':\n{src}"
            );
        }
    }

    #[test]
    fn saving_preserves_what_the_interface_does_not_draw() {
        // The guarantee that makes GUI saving safe on a hand-written file.
        // A struct round-trip would lose every line below the first pane.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("hand-written.toml");
        std::fs::write(
            &path,
            r#"# my morning setup
name = "Morning"
layout = "even_h"
port_base = 4500

[[panes]]
cwd = "/dev/backend"
agent = "claude"
worktree = "feat/x"
setup = "bun install"
worktree_teardown = "keep"

[panes.env]
API_PORT = "${port_offset}"
"#,
        )
        .expect("seed");

        let (mut preset, warnings) =
            parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        assert!(warnings.is_empty(), "{warnings:?}");
        // What the editor can change.
        preset.name = Some("Morning routine".to_string());
        preset.panes[0].prompt = Some("read the plan".to_string());
        save(&path, &preset, &[Some(0)]).expect("save");

        let src = std::fs::read_to_string(&path).expect("read back");
        assert!(src.contains("# my morning setup"), "{src}");
        assert!(src.contains("port_base = 4500"), "{src}");
        assert!(src.contains(r#"worktree = "feat/x""#), "{src}");
        assert!(src.contains(r#"setup = "bun install""#), "{src}");
        assert!(src.contains(r#"worktree_teardown = "keep""#), "{src}");
        assert!(src.contains(r#"API_PORT = "${port_offset}""#), "{src}");
        assert!(src.contains(r#"name = "Morning routine""#), "{src}");
        assert!(src.contains(r#"prompt = "read the plan""#), "{src}");
    }

    #[test]
    fn an_unknown_key_is_reported_and_the_file_still_runs() {
        // Strictness and round-trip contradict each other: a strict reader
        // refuses what it does not know, and round-trip requires keeping it.
        // The warning is what replaces the refusal - the typo is still
        // visible, and a file written by a later release still launches.
        let (preset, warnings) = parse(
            r#"
name = "typo"
future_key = 1

[[panes]]
agnt = "claude"
command = "ls"
"#,
        )
        .expect("tolerated");
        assert_eq!(preset.panes.len(), 1);
        assert_eq!(preset.panes[0].command.as_deref(), Some("ls"));
        assert!(
            warnings.iter().any(|w| w.contains("future_key")),
            "{warnings:?}"
        );
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("pane 0") && w.contains("agnt")),
            "{warnings:?}"
        );
    }

    #[test]
    fn semantic_invariants_are_still_hard_errors() {
        // Tolerance is about keys, not about contradictions. `agent` and
        // `command` on one pane is not something a future release resolves.
        let err = parse("[[panes]]\nagent = \"claude\"\ncommand = \"vim\"\n").unwrap_err();
        assert!(err.contains("either"), "got: {err}");
    }

    #[test]
    fn a_removed_pane_takes_its_whole_table() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("two.toml");
        std::fs::write(
            &path,
            "[[panes]]\ncommand = \"a\"\n\n[[panes]]\ncommand = \"b\"\nsetup = \"x\"\nworktree = \"w\"\ncwd = \"/tmp\"\n",
        )
        .expect("seed");
        let (mut preset, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        preset.panes.remove(1);
        save(&path, &preset, &[Some(0)]).expect("save");
        let src = std::fs::read_to_string(&path).expect("read back");
        assert!(!src.contains("setup"), "{src}");
        assert!(src.contains(r#"command = "a""#), "{src}");
    }

    #[test]
    fn removing_the_first_pane_does_not_hand_its_worktree_to_the_second() {
        // The defect the old test could not see, because removing the LAST
        // pane is the one index where position happens to be provenance.
        // Removing pane 0 of three used to leave pane 1's text in slot 0: a
        // plain shell inherited the deleted pane's `worktree` and `setup` and
        // silently spawned inside somebody else's checkout running
        // `bun install`, while the last pane quietly stopped being isolated.
        // It parsed and it validated, so nothing warned.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("three.toml");
        std::fs::write(
            &path,
            "[[panes]]\ncwd = \"/repo\"\nagent = \"claude\"\nworktree = \"feat/a\"\nsetup = \"bun install\"\n\n             [panes.env]\nAPI_PORT = \"${port_offset}\"\n\n             [[panes]]\ncwd = \"/repo/b\"\ncommand = \"pnpm dev\"\n\n             [[panes]]\ncwd = \"/repo\"\nagent = \"codex\"\nworktree = \"feat/c\"\n",
        )
        .expect("seed");
        let (mut preset, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        preset.panes.remove(0);
        // What the editor's draft knows: the two survivors came from tables 1
        // and 2 of the file.
        save(&path, &preset, &[Some(1), Some(2)]).expect("save");

        let (reread, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("re-parse");
        assert_eq!(reread.panes.len(), 2);
        assert_eq!(reread.panes[0].command.as_deref(), Some("pnpm dev"));
        assert_eq!(reread.panes[0].worktree, None, "shell inherited a worktree");
        assert_eq!(reread.panes[0].setup, None, "shell inherited a setup");
        assert_eq!(reread.panes[0].env, None, "shell inherited an env");
        assert_eq!(reread.panes[1].agent.as_deref(), Some("codex"));
        assert_eq!(
            reread.panes[1].worktree.as_deref(),
            Some("feat/c"),
            "the last pane lost its own worktree"
        );
    }

    #[test]
    fn writing_a_fresh_document_loses_nothing() {
        // The migration's case: no file on disk, so there is no DOM to
        // preserve anything. Every field the type can hold has to be written
        // out, or a migrated preset quietly stops setting its `env` under a
        // toast saying the presets moved.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("fresh.toml");
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "${port_offset}".to_string());
        env.insert("API".to_string(), "1".to_string());
        let preset = Preset {
            name: Some("Migrated".to_string()),
            layout: PresetLayout::EvenV,
            cwd: Some("/work/atlas".to_string()),
            color: Some("ff6600".to_string()),
            port_base: Some(4500),
            panes: vec![PanePreset {
                cwd: Some("/work/atlas".to_string()),
                agent: Some("claude".to_string()),
                env: Some(env.clone()),
                worktree: Some("feat/x".to_string()),
                copy_env: Some(false),
                setup: Some("bun install".to_string()),
                setup_timeout_secs: Some(90),
                worktree_teardown: Some("keep".to_string()),
                ..Default::default()
            }],
        };
        save(&path, &preset, &[]).expect("save");
        let (reread, warnings) = parse(&std::fs::read_to_string(&path).unwrap()).expect("re-parse");
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(reread, preset);
    }

    #[test]
    fn an_env_the_file_already_has_is_not_rewritten() {
        // The other half of the same rule: the interface cannot show `env`,
        // so it must not restate it either - the file's own text, formatting
        // and key order win.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("kept.toml");
        std::fs::write(
            &path,
            "[[panes]]\ncommand = \"ls\"\n\n[panes.env]\n# why this port\nPORT = \"8080\"\n",
        )
        .expect("seed");
        let (preset, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        save(&path, &preset, &[Some(0)]).expect("save");
        let src = std::fs::read_to_string(&path).expect("read back");
        assert!(src.contains("# why this port"), "{src}");
    }

    #[test]
    fn an_unknown_key_in_an_inline_pane_table_is_reported_too() {
        // `[[panes]]` and `panes = [{ … }]` are one list to TOML and two
        // types to `toml_edit`. A reader that knows only the first drops an
        // inline file whole on a typo - the opposite of tolerance.
        let (preset, warnings) =
            parse("panes = [{ command = \"ls\", agnt = \"claude\" }]\n").expect("tolerated");
        assert_eq!(preset.panes.len(), 1);
        assert_eq!(preset.panes[0].command.as_deref(), Some("ls"));
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("pane 0") && w.contains("agnt")),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_blank_field_removes_its_key_rather_than_writing_an_empty_string() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("blank.toml");
        std::fs::write(&path, "[[panes]]\ncwd = \"/tmp\"\ncommand = \"ls\"\n").expect("seed");
        let (mut preset, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        preset.panes[0].cwd = Some("   ".to_string());
        save(&path, &preset, &[Some(0)]).expect("save");
        let src = std::fs::read_to_string(&path).expect("read back");
        assert!(!src.contains("cwd"), "{src}");
    }

    #[test]
    fn env_survives_a_save_that_never_read_it() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("env.toml");
        let mut env = HashMap::new();
        env.insert("PORT".to_string(), "${port_offset}".to_string());
        std::fs::write(
            &path,
            "[[panes]]\ncommand = \"ls\"\n\n[panes.env]\nPORT = \"${port_offset}\"\n",
        )
        .expect("seed");
        let (preset, _) = parse(&std::fs::read_to_string(&path).unwrap()).expect("parse");
        assert_eq!(preset.panes[0].env, Some(env));
        save(&path, &preset, &[Some(0)]).expect("save");
        let src = std::fs::read_to_string(&path).expect("read back");
        assert!(src.contains("PORT"), "{src}");
    }

    #[test]
    fn unused_path_never_collides() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let first = unused_path(tmp.path(), "Backend + frontend");
        assert_eq!(file_stem(&first), "backend-frontend");
        std::fs::write(&first, "").expect("touch");
        let second = unused_path(tmp.path(), "Backend + frontend");
        assert_eq!(file_stem(&second), "backend-frontend-2");
    }

    #[test]
    fn a_name_with_no_ascii_still_gets_a_filename() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(file_stem(&unused_path(tmp.path(), "утро")), "preset");
    }
}

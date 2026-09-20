//! Render shortcut entries for the settings UI + menu bar.

use std::collections::{HashMap, HashSet};

use gpui::Keystroke;

use super::apply::canonical_keystroke;
use super::defaults::{DEFAULTS, MACOS_ONLY_DEFAULTS};
use super::registry::{ACTIONS, action_description, canonical_action_name};

/// A resolved shortcut entry for display in the settings page.
pub struct ShortcutEntry {
    pub key: String,
    pub description: String,
    /// The registry's `KeyBindingContextPredicate` for this action, verbatim
    /// (`""` = global). The shortcuts screen groups by it and the command
    /// palette shows it, so it has to survive the trip out of the registry -
    /// it used to stop here, known to `ACTIONS` and invisible to every reader.
    pub context: &'static str,
    /// The action this row rebinds, as the canonical `&'static str`
    /// from the registry. The settings editor MUST key its rebind off this,
    /// not off the row's positional index: the displayed list chains
    /// `MACOS_ONLY_DEFAULTS`, skips unbound rows, and appends user-only
    /// actions, so `index → DEFAULTS[index]` is only correct in the trivial
    /// (zero-override) case. Indexing `DEFAULTS` by row would silently rebind
    /// the *wrong* action and corrupt `splitlane.json`.
    pub action_name: &'static str,
}

/// Format a GPUI keystroke string for display.
///
/// On Linux: `"secondary-shift-d"` → `"Ctrl+Shift+D"` (readable, plus-separated).
/// On macOS: `"secondary-shift-d"` → `"⌘⇧D"` (Apple HIG glyphs, no separator -
/// matches the native macOS menu bar convention used by the app menu).
///
/// `secondary` is GPUI's cross-platform shorthand that resolves to `cmd` on
/// macOS and `ctrl` elsewhere (see `Keystroke::parse`). Rendering it here
/// mirrors that resolution so the menu bar always shows the actual key the
/// user will press.
pub fn format_keystroke(key: &str) -> String {
    let is_macos = cfg!(target_os = "macos");
    let parts = key.split('-').map(|part| match part {
        // Modifiers - platform-dependent rendering.
        "secondary" => {
            if is_macos {
                "\u{2318}".to_string() // ⌘
            } else {
                "Ctrl".to_string()
            }
        }
        "cmd" | "super" | "win" => {
            if is_macos {
                "\u{2318}".to_string() // ⌘
            } else {
                "Super".to_string()
            }
        }
        "ctrl" => {
            if is_macos {
                "\u{2303}".to_string() // ⌃
            } else {
                "Ctrl".to_string()
            }
        }
        "shift" => {
            if is_macos {
                "\u{21E7}".to_string() // ⇧
            } else {
                "Shift".to_string()
            }
        }
        "alt" => {
            if is_macos {
                "\u{2325}".to_string() // ⌥
            } else {
                "Alt".to_string()
            }
        }
        // Non-modifier tokens - same on both platforms, just capitalized.
        "tab" => "Tab".to_string(),
        "pageup" => "PageUp".to_string(),
        "pagedown" => "PageDown".to_string(),
        "left" => "Left".to_string(),
        "right" => "Right".to_string(),
        "up" => "Up".to_string(),
        "down" => "Down".to_string(),
        other => other.to_uppercase(),
    });
    if is_macos {
        // Apple HIG: modifier glyphs flow directly into the key label, no `+`.
        parts.collect::<String>()
    } else {
        parts.collect::<Vec<_>>().join("+")
    }
}

/// Compute the effective shortcut list by merging defaults with user overrides.
///
/// User overrides replace default bindings for the same action. Additional user
/// bindings are appended. Registry actions with no binding are still listed as
/// `Unassigned` so every action exposed by the keybinding registry is rebindable
/// from Settings.
pub fn effective_shortcuts(user_shortcuts: &HashMap<String, String>) -> Vec<ShortcutEntry> {
    // Build reverse map: action_name -> user key (last one wins if duplicates).
    let mut user_by_action: HashMap<&str, &str> = HashMap::new();
    for (key, action_name) in user_shortcuts {
        // Through the canonicaliser, so an override written against a renamed
        // action shows the key it is actually bound to rather than the default
        // this screen would otherwise draw.
        let action_name = canonical_action_name(action_name);
        if action_name != "none" && ACTIONS.iter().any(|a| a.name == action_name) {
            user_by_action.insert(action_name, key.as_str());
        }
    }

    let unbound_canonical: HashSet<Keystroke> = user_shortcuts
        .iter()
        .filter(|(_, v)| v.as_str() == "none")
        .filter_map(|(k, _)| canonical_keystroke(k))
        .collect();
    let user_bound_canonical: HashSet<Keystroke> = user_shortcuts
        .iter()
        .filter(|(_, v)| v.as_str() != "none")
        .filter(|(_, action_name)| {
            let name = canonical_action_name(action_name);
            ACTIONS.iter().any(|a| a.name == name)
        })
        .filter_map(|(k, _)| canonical_keystroke(k))
        .collect();
    let is_unbound =
        |key: &str| canonical_keystroke(key).is_some_and(|k| unbound_canonical.contains(&k));
    let is_user_claimed =
        |key: &str| canonical_keystroke(key).is_some_and(|k| user_bound_canonical.contains(&k));

    let mut entries = Vec::new();
    let mut seen_actions: HashSet<&'static str> = HashSet::new();

    // Defaults first, with user overrides applied. Include the
    // macOS-only defaults so the settings page reflects cmd-c/cmd-v on
    // macOS (and stays unchanged on Linux where MACOS_ONLY_DEFAULTS is empty).
    for d in DEFAULTS.iter().chain(MACOS_ONLY_DEFAULTS.iter()) {
        let Some(meta) = ACTIONS.iter().find(|a| a.name == d.action_name) else {
            continue;
        };

        // If user overrode this action to a different key, show that key. If a
        // different action claimed this default chord, mirror apply_keybindings
        // and hide the displaced default row until it is explicitly rebound.
        let key = if let Some(user_key) = user_by_action.get(d.action_name) {
            format_keystroke(user_key)
        } else {
            if is_unbound(d.key) || is_user_claimed(d.key) {
                continue;
            }
            format_keystroke(d.key)
        };

        seen_actions.insert(meta.name);
        entries.push(ShortcutEntry {
            key,
            description: meta.description.to_string(),
            context: meta.context,
            action_name: meta.name,
        });
    }

    // Add user bindings for actions that are not in the default tables.
    for (key, action_name) in user_shortcuts {
        if action_name == "none" {
            continue;
        }
        if let Some(meta) = ACTIONS.iter().find(|a| a.name == action_name)
            && seen_actions.insert(meta.name)
        {
            entries.push(ShortcutEntry {
                key: format_keystroke(key),
                description: meta.description.to_string(),
                context: meta.context,
                action_name: meta.name,
            });
        }
    }

    for meta in ACTIONS {
        if seen_actions.insert(meta.name) {
            entries.push(ShortcutEntry {
                key: "Unassigned".to_string(),
                description: action_description(meta.name).to_string(),
                context: meta.context,
                action_name: meta.name,
            });
        }
    }

    entries
}

/// The group a shortcut row belongs to, derived from its context predicate.
///
/// A context is a `KeyBindingContextPredicate` source string, which can be a
/// bare identifier (`"Terminal"`) or an expression
/// (`"DiffView && !Terminal && !TextInput"`). Only the leading identifier names
/// the surface the action belongs to; the negations are exclusions, not
/// membership. Taking the first token therefore folds all five `DiffView &&
/// …` rows into one group. `""` (global) maps to `"Global"`.
///
/// The returned slice borrows the `&'static str` from the registry, so groups
/// can be compared by pointer-free string equality and stored without cloning.
pub fn context_group(context: &'static str) -> &'static str {
    let token = context
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .find(|t| !t.is_empty())
        .unwrap_or("");
    if token.is_empty() { "Global" } else { token }
}

/// Human-readable label for a [`context_group`] key. Unknown groups fall
/// through verbatim - a new context shows up under its own identifier rather
/// than silently joining "Global".
pub fn context_group_label(group: &str) -> &str {
    match group {
        "Global" => "Global",
        "Terminal" => "Terminal",
        "Search" => "Find in terminal",
        "Markdown" => "Markdown",
        "MarkdownSearch" => "Markdown find",
        "DiffView" => "Review (diff)",
        other => other,
    }
}

/// Case-insensitive substring match used by BOTH the shortcuts screen filter
/// and the command palette. `needle` must already be lowercased by the caller
/// (both read it from a `TextInput` once per render, not once per row).
///
/// Matches on everything a user might type: the description, the rendered
/// keystroke, the group label, and the raw action name - the last so
/// `splitlane.json` authors can search by the identifier they have to write.
pub fn shortcut_matches(entry: &ShortcutEntry, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let group = context_group(entry.context);
    entry.description.to_lowercase().contains(needle)
        || entry.key.to_lowercase().contains(needle)
        || entry.action_name.to_lowercase().contains(needle)
        || context_group_label(group).to_lowercase().contains(needle)
}

/// Group the entries by context for display, preserving the registry's order
/// both between groups (first appearance wins) and within them.
///
/// Yields `(group label, rows)` where each row carries its index into the
/// ORIGINAL `entries` slice. That index is the shortcuts editor's rebind key
/// (`recording_shortcut_idx`), so grouping and filtering must never renumber
/// it - see the `ShortcutEntry::action_name` note for what indexing the
/// displayed order instead would cost.
pub fn group_shortcuts<'a>(
    entries: &'a [ShortcutEntry],
    needle: &str,
) -> Vec<(&'a str, Vec<(usize, &'a ShortcutEntry)>)> {
    let mut groups: Vec<(&'a str, Vec<(usize, &'a ShortcutEntry)>)> = Vec::new();
    for (idx, entry) in entries.iter().enumerate() {
        if !shortcut_matches(entry, needle) {
            continue;
        }
        let label = context_group_label(context_group(entry.context));
        match groups.iter_mut().find(|(existing, _)| *existing == label) {
            Some((_, rows)) => rows.push((idx, entry)),
            None => groups.push((label, vec![(idx, entry)])),
        }
    }
    groups
}

/// Returns `true` if the keystroke is a bare modifier press (no actual key).
pub fn is_bare_modifier(keystroke: &Keystroke) -> bool {
    matches!(
        keystroke.key.as_str(),
        "shift" | "control" | "alt" | "platform" | "function"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_group_takes_the_leading_identifier() {
        assert_eq!(context_group(""), "Global");
        assert_eq!(context_group("Terminal"), "Terminal");
        // All five review actions share one group: the negations exclude
        // surfaces, they do not name a second one.
        assert_eq!(
            context_group("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
            "DiffView"
        );
        assert_eq!(context_group_label("DiffView"), "Review (diff)");
        // An unknown context is shown, not swallowed into Global.
        assert_eq!(context_group_label("BrandNewSurface"), "BrandNewSurface");
    }

    #[test]
    fn group_shortcuts_preserves_registry_order_and_original_indices() {
        let entries = effective_shortcuts(&HashMap::new());
        let groups = group_shortcuts(&entries, "");

        // Global comes first because the registry starts with the splits.
        assert_eq!(groups[0].0, "Global");
        // Every row is a real entry at the index it reports - the shortcuts
        // editor rebinds by that index, so a renumbering here would rebind
        // the wrong action.
        let total: usize = groups.iter().map(|(_, rows)| rows.len()).sum();
        assert_eq!(total, entries.len(), "grouping must not drop rows");
        for (_, rows) in &groups {
            for (idx, entry) in rows {
                assert_eq!(entry.action_name, entries[*idx].action_name);
            }
        }
        // No group appears twice.
        let mut seen = HashSet::new();
        for (label, _) in &groups {
            assert!(seen.insert(*label), "duplicate group `{label}`");
        }
    }

    #[test]
    fn shortcut_filter_matches_description_action_name_and_group() {
        let entries = effective_shortcuts(&HashMap::new());
        let split = entries
            .iter()
            .find(|e| e.action_name == "split_horizontally")
            .expect("split_horizontally is registered");
        assert!(shortcut_matches(split, ""), "empty query matches all");
        assert!(shortcut_matches(split, "split hor"));
        assert!(shortcut_matches(split, "split_horizontally"));
        assert!(shortcut_matches(split, "global"));
        assert!(!shortcut_matches(split, "markdown"));

        // Filtering narrows the groups, and the surviving rows keep their
        // original indices.
        let groups = group_shortcuts(&entries, "markdown");
        assert!(!groups.is_empty());
        for (_, rows) in &groups {
            for (idx, entry) in rows {
                assert!(shortcut_matches(entry, "markdown"));
                assert_eq!(entry.action_name, entries[*idx].action_name);
            }
        }
    }

    #[test]
    fn every_row_carries_its_registry_context() {
        // Regression: `context` used to stop at the registry. Terminal
        // actions must arrive in the UI still knowing they are terminal ones.
        let entries = effective_shortcuts(&HashMap::new());
        let copy = entries
            .iter()
            .find(|e| e.action_name == "terminal_copy")
            .expect("terminal_copy is registered");
        assert_eq!(copy.context, "Terminal");
        let split = entries
            .iter()
            .find(|e| e.action_name == "split_horizontally")
            .expect("split_horizontally is registered");
        assert_eq!(split.context, "");
    }

    #[test]
    fn effective_shortcuts_defaults_include_core_actions() {
        let entries = effective_shortcuts(&HashMap::new());
        let descriptions: Vec<&str> = entries.iter().map(|e| e.description.as_str()).collect();
        assert!(
            descriptions.contains(&"Split horizontal"),
            "Missing split horizontal"
        );
        assert!(
            descriptions.contains(&"Split vertical"),
            "Missing split vertical"
        );
        assert!(descriptions.contains(&"Close pane"), "Missing close pane");
        assert!(
            descriptions.contains(&"Next project"),
            "Missing next project"
        );
        assert!(descriptions.contains(&"Focus left"), "Missing focus left");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn effective_shortcuts_user_override_replaces_key() {
        let mut overrides = HashMap::new();
        overrides.insert("ctrl-alt-h".to_string(), "split_horizontally".to_string());
        let entries = effective_shortcuts(&overrides);
        let split_h = entries
            .iter()
            .find(|e| e.description == "Split horizontal")
            .expect("Split horizontal should be in effective list");
        assert_eq!(
            split_h.key, "Ctrl+Alt+H",
            "User override should replace the default key"
        );
    }

    #[test]
    fn effective_shortcuts_carry_matching_action_name() {
        // Every row knows the action it rebinds. The editor keys off
        // this, so it must line up with the row's description.
        let entries = effective_shortcuts(&HashMap::new());
        for e in &entries {
            assert_eq!(
                e.description,
                action_description(e.action_name),
                "row description must match its action_name"
            );
        }
    }

    #[test]
    fn effective_shortcuts_action_name_survives_unbind_shift() {
        // Regression for the `action_name_at(idx) → DEFAULTS[idx]` bug: once a
        // default is unbound the displayed list shifts, so the row at index 0
        // is the SECOND default - not `DEFAULTS[0]`. Reading the carried
        // `action_name` must reflect the shifted row, otherwise the editor
        // rebinds the wrong action.
        let mut overrides = HashMap::new();
        overrides.insert("secondary-shift-d".to_string(), "none".to_string());
        let entries = effective_shortcuts(&overrides);
        assert_eq!(
            entries[0].action_name, "split_vertically",
            "first row should be the second default after the first is unbound"
        );
        assert_ne!(
            entries[0].action_name, "split_horizontally",
            "indexing DEFAULTS[0] here would rebind the wrong (unbound) action"
        );
    }

    #[test]
    fn effective_shortcuts_none_unbinds_key() {
        let mut overrides = HashMap::new();
        // The default is `secondary-shift-d`; unbinding requires the
        // canonical default key string.
        overrides.insert("secondary-shift-d".to_string(), "none".to_string());
        let entries = effective_shortcuts(&overrides);
        let split_h = entries
            .iter()
            .find(|e| e.action_name == "split_horizontally")
            .expect("unbound actions remain visible for rebinding");
        assert_eq!(split_h.key, "Unassigned");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn effective_shortcuts_none_unbinds_canonical_equivalent_key() {
        let mut overrides = HashMap::new();
        overrides.insert("ctrl+shift+d".to_string(), "none".to_string());
        let entries = effective_shortcuts(&overrides);
        let split_h = entries
            .iter()
            .find(|e| e.action_name == "split_horizontally")
            .expect("unbound actions remain visible for rebinding");
        assert_eq!(split_h.key, "Unassigned");
    }

    #[test]
    fn effective_shortcuts_lists_registry_actions_without_defaults() {
        let entries = effective_shortcuts(&HashMap::new());
        let close_window = entries
            .iter()
            .find(|e| e.action_name == "close_window")
            .expect("registry action should be visible in shortcuts settings");
        assert_eq!(close_window.key, "Unassigned");
    }

    #[test]
    fn effective_shortcuts_invalid_action_ignored() {
        let mut overrides = HashMap::new();
        overrides.insert("ctrl+x".to_string(), "bogus_action".to_string());
        let entries = effective_shortcuts(&overrides);
        // Invalid action should not appear
        let has_bogus = entries
            .iter()
            .any(|e| e.description == "Unknown" && e.key == "Ctrl+X");
        assert!(!has_bogus, "Invalid action should not be in effective list");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn effective_shortcuts_preserves_unoverridden_defaults() {
        let mut overrides = HashMap::new();
        overrides.insert("ctrl+alt+h".to_string(), "split_horizontally".to_string());
        let entries = effective_shortcuts(&overrides);
        // close_pane should still be at its default key. The default is
        // `secondary-shift-w`, which renders as "Ctrl+Shift+W" on Linux.
        let close = entries
            .iter()
            .find(|e| e.description == "Close pane")
            .expect("Close pane should be in effective list");
        assert_eq!(
            close.key, "Ctrl+Shift+W",
            "Unoverridden action should keep default key"
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn format_keystroke_produces_readable_output() {
        assert_eq!(format_keystroke("ctrl-shift-d"), "Ctrl+Shift+D");
        assert_eq!(format_keystroke("alt-left"), "Alt+Left");
        assert_eq!(format_keystroke("ctrl-1"), "Ctrl+1");
        assert_eq!(format_keystroke("shift-pageup"), "Shift+PageUp");
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn secondary_renders_as_ctrl_on_linux() {
        // Secondary resolves to Ctrl on Linux; format_keystroke mirrors
        // that so the menu bar / shortcut list shows the key the user will
        // actually press.
        assert_eq!(format_keystroke("secondary-shift-d"), "Ctrl+Shift+D");
        assert_eq!(format_keystroke("secondary-tab"), "Ctrl+Tab");
        assert_eq!(format_keystroke("secondary-1"), "Ctrl+1");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn secondary_renders_as_cmd_glyph_on_macos() {
        // The macOS menu bar expects Apple HIG glyphs, no plus separator.
        assert_eq!(format_keystroke("secondary-shift-d"), "\u{2318}\u{21E7}D");
        assert_eq!(format_keystroke("secondary-tab"), "\u{2318}Tab");
        assert_eq!(format_keystroke("secondary-1"), "\u{2318}1");
        // Explicit `cmd` token also renders as ⌘ (the user override form).
        assert_eq!(format_keystroke("cmd-shift-d"), "\u{2318}\u{21E7}D");
    }
}

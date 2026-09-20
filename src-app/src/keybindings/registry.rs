//! Unified action registry.
//!
//! A single [`ActionMeta`] table (`ACTIONS`) replaces three parallel match
//! statements - `action_from_name`, `context_for_action`, `action_description`
//! so adding an action requires exactly one edit.

use gpui::Action;

use crate::{
    ClearScrollHistory, ClosePane, CloseTab, CloseWindow, CloseWorkspace, CopyWorkspacePath,
    DiffFullWindow, DismissSearch, FocusDown, FocusLeft, FocusRight, FocusUp, JumpNextPrompt,
    JumpNextWaiting, JumpPrevPrompt, LayoutEvenHorizontal, LayoutEvenVertical, LayoutGrid,
    MarkdownCopy, MarkdownFindDismiss, MarkdownFindNext, MarkdownFindOpen, MarkdownFindPrev,
    MarkdownScrollPageDown, MarkdownScrollPageUp, NewTab, NewWorkspace, NextWorkspace,
    OpenFileInEditor, OpenWorkspaceInCursor, OpenWorkspaceInVsCode, OpenWorkspaceInWindsurf,
    OpenWorkspaceInZed, Quit, ResetTerminal, RevealWorkspaceInFileManager, ScrollPageDown,
    ScrollPageUp, SearchNext, SearchPrev, SelectWorkspace1, SelectWorkspace2, SelectWorkspace3,
    SelectWorkspace4, SelectWorkspace5, SelectWorkspace6, SelectWorkspace7, SelectWorkspace8,
    SelectWorkspace9, SplitEqualize, SplitHorizontally, SplitVertically, SwapPane, TerminalCopy,
    TerminalPaste, ToggleCopyMode, ToggleSearch, ToggleSearchCase, ToggleSearchRegex, ToggleZoom,
    UndoClosePane,
};
use crate::{FontSizeDecrease, FontSizeIncrease, FontSizeReset, ToggleFleetSearch};

/// Metadata for a single dispatchable action.
///
/// Empty `context` means the action is global (no `KeyBindingContextPredicate`).
/// `factory` boxes a fresh action instance on each call so GPUI's
/// `KeyBinding::load` can own it.
pub(super) struct ActionMeta {
    pub(super) name: &'static str,
    pub(super) factory: fn() -> Box<dyn Action>,
    pub(super) context: &'static str,
    pub(super) description: &'static str,
}

/// The one source of truth for every action dispatched by `keybindings/`.
///
/// Order matches the historical groupings (splits, workspaces, focus, tabs,
/// terminal, layouts, search, scroll-backward) so the settings page preserves
/// its visual grouping when iterating.
pub(super) const ACTIONS: &[ActionMeta] = &[
    ActionMeta {
        name: "split_horizontally",
        factory: || Box::new(SplitHorizontally),
        context: "",
        description: "Split horizontal",
    },
    ActionMeta {
        name: "split_vertically",
        factory: || Box::new(SplitVertically),
        context: "",
        description: "Split vertical",
    },
    ActionMeta {
        name: "close_pane",
        factory: || Box::new(ClosePane),
        context: "",
        description: "Close pane",
    },
    ActionMeta {
        name: "new_workspace",
        factory: || Box::new(NewWorkspace),
        context: "",
        description: "New project",
    },
    ActionMeta {
        name: "close_workspace",
        factory: || Box::new(CloseWorkspace),
        context: "",
        description: "Close project",
    },
    ActionMeta {
        name: "copy_workspace_path",
        factory: || Box::new(CopyWorkspacePath),
        context: "",
        description: "Copy path",
    },
    ActionMeta {
        name: "reveal_workspace_in_file_manager",
        factory: || Box::new(RevealWorkspaceInFileManager),
        context: "",
        description: "Reveal in file manager",
    },
    ActionMeta {
        name: "open_workspace_in_zed",
        factory: || Box::new(OpenWorkspaceInZed),
        context: "",
        description: "Open in Zed",
    },
    ActionMeta {
        name: "open_workspace_in_cursor",
        factory: || Box::new(OpenWorkspaceInCursor),
        context: "",
        description: "Open in Cursor",
    },
    ActionMeta {
        name: "open_workspace_in_vscode",
        factory: || Box::new(OpenWorkspaceInVsCode),
        context: "",
        description: "Open in VS Code",
    },
    ActionMeta {
        name: "open_workspace_in_windsurf",
        factory: || Box::new(OpenWorkspaceInWindsurf),
        context: "",
        description: "Open in Windsurf",
    },
    ActionMeta {
        name: "next_workspace",
        factory: || Box::new(NextWorkspace),
        context: "",
        description: "Next project",
    },
    ActionMeta {
        name: "focus_left",
        factory: || Box::new(FocusLeft),
        context: "",
        description: "Focus left",
    },
    ActionMeta {
        name: "focus_right",
        factory: || Box::new(FocusRight),
        context: "",
        description: "Focus right",
    },
    ActionMeta {
        name: "focus_up",
        factory: || Box::new(FocusUp),
        context: "",
        description: "Focus up",
    },
    ActionMeta {
        name: "focus_down",
        factory: || Box::new(FocusDown),
        context: "",
        description: "Focus down",
    },
    ActionMeta {
        name: "jump_next_waiting",
        factory: || Box::new(JumpNextWaiting),
        context: "",
        description: "Jump to next waiting agent",
    },
    ActionMeta {
        name: "select_workspace_1",
        factory: || Box::new(SelectWorkspace1),
        context: "",
        description: "Select workspace 1",
    },
    ActionMeta {
        name: "select_workspace_2",
        factory: || Box::new(SelectWorkspace2),
        context: "",
        description: "Select workspace 2",
    },
    ActionMeta {
        name: "select_workspace_3",
        factory: || Box::new(SelectWorkspace3),
        context: "",
        description: "Select workspace 3",
    },
    ActionMeta {
        name: "select_workspace_4",
        factory: || Box::new(SelectWorkspace4),
        context: "",
        description: "Select workspace 4",
    },
    ActionMeta {
        name: "select_workspace_5",
        factory: || Box::new(SelectWorkspace5),
        context: "",
        description: "Select workspace 5",
    },
    ActionMeta {
        name: "select_workspace_6",
        factory: || Box::new(SelectWorkspace6),
        context: "",
        description: "Select workspace 6",
    },
    ActionMeta {
        name: "select_workspace_7",
        factory: || Box::new(SelectWorkspace7),
        context: "",
        description: "Select workspace 7",
    },
    ActionMeta {
        name: "select_workspace_8",
        factory: || Box::new(SelectWorkspace8),
        context: "",
        description: "Select workspace 8",
    },
    ActionMeta {
        name: "select_workspace_9",
        factory: || Box::new(SelectWorkspace9),
        context: "",
        description: "Select workspace 9",
    },
    ActionMeta {
        name: "new_tab",
        factory: || Box::new(NewTab),
        context: "",
        description: "New tab",
    },
    ActionMeta {
        name: "close_tab",
        factory: || Box::new(CloseTab),
        context: "",
        description: "Close tab",
    },
    ActionMeta {
        name: "terminal_copy",
        factory: || Box::new(TerminalCopy),
        context: "Terminal",
        description: "Copy",
    },
    ActionMeta {
        name: "terminal_paste",
        factory: || Box::new(TerminalPaste),
        context: "Terminal",
        description: "Paste",
    },
    ActionMeta {
        name: "scroll_page_up",
        factory: || Box::new(ScrollPageUp),
        context: "Terminal",
        description: "Scroll up",
    },
    ActionMeta {
        name: "scroll_page_down",
        factory: || Box::new(ScrollPageDown),
        context: "Terminal",
        description: "Scroll down",
    },
    ActionMeta {
        name: "jump_prev_prompt",
        factory: || Box::new(JumpPrevPrompt),
        context: "Terminal",
        description: "Jump to previous prompt",
    },
    ActionMeta {
        name: "jump_next_prompt",
        factory: || Box::new(JumpNextPrompt),
        context: "Terminal",
        description: "Jump to next prompt",
    },
    ActionMeta {
        name: "close_window",
        factory: || Box::new(CloseWindow),
        context: "",
        description: "Close window",
    },
    ActionMeta {
        name: "toggle_zoom",
        factory: || Box::new(ToggleZoom),
        context: "",
        description: "Toggle zoom",
    },
    ActionMeta {
        name: "layout_even_horizontal",
        factory: || Box::new(LayoutEvenHorizontal),
        context: "",
        description: "Layout even horizontal",
    },
    ActionMeta {
        name: "layout_even_vertical",
        factory: || Box::new(LayoutEvenVertical),
        context: "",
        description: "Layout even vertical",
    },
    // The grid, the third layout form. No default chord: the design gives the
    // segment and no keystroke, and every unclaimed letter left in a
    // terminal-facing context is one a shell would rather have. Reachable from Settings ->
    // Shortcuts like the other fifteen unassigned actions.
    ActionMeta {
        name: "layout_grid",
        factory: || Box::new(LayoutGrid),
        context: "",
        description: "Layout grid",
    },
    // The hand-off from the file surface to the editor. The context excludes the find
    // bar explicitly: both layer onto the same node, and `enter` there belongs
    // to `markdown_find_next`. A predicate rather than a third context word,
    // because "the document, unless it is being searched" is a fact about the
    // two contexts that already exist.
    ActionMeta {
        name: "open_file_in_editor",
        factory: || Box::new(OpenFileInEditor),
        context: "Markdown && !MarkdownSearch",
        description: "Open in editor",
    },
    ActionMeta {
        name: "split_equalize",
        factory: || Box::new(SplitEqualize),
        context: "",
        description: "Equalize panes",
    },
    ActionMeta {
        name: "swap_pane",
        factory: || Box::new(SwapPane),
        context: "",
        description: "Swap pane",
    },
    ActionMeta {
        name: "undo_close_pane",
        factory: || Box::new(UndoClosePane),
        context: "",
        description: "Undo close pane",
    },
    ActionMeta {
        name: "toggle_copy_mode",
        factory: || Box::new(ToggleCopyMode),
        context: "Terminal",
        description: "Toggle copy mode",
    },
    ActionMeta {
        name: "toggle_search",
        factory: || Box::new(ToggleSearch),
        context: "Terminal",
        description: "Toggle search",
    },
    ActionMeta {
        name: "font_size_increase",
        factory: || Box::new(FontSizeIncrease),
        context: "Terminal",
        description: "Increase pane font size",
    },
    ActionMeta {
        name: "font_size_decrease",
        factory: || Box::new(FontSizeDecrease),
        context: "Terminal",
        description: "Decrease pane font size",
    },
    ActionMeta {
        name: "font_size_reset",
        factory: || Box::new(FontSizeReset),
        context: "Terminal",
        description: "Reset pane font size",
    },
    ActionMeta {
        name: "toggle_fleet_search",
        factory: || Box::new(ToggleFleetSearch),
        context: "Search",
        description: "Search every session's output",
    },
    ActionMeta {
        name: "toggle_search_regex",
        factory: || Box::new(ToggleSearchRegex),
        context: "Search",
        description: "Toggle search regex",
    },
    // The find bar's `Aa`. Unassigned by default: the design gives it a
    // button and no chord, and every unclaimed letter left in the Search
    // context is one a shell would rather have. Reachable from the palette,
    // which is what keeps "Unassigned" from meaning "unavailable".
    // The design gives `⤢` a button on the Review header and no chord.
    // Unassigned, and reachable from the palette.
    ActionMeta {
        name: "diff_full_window",
        factory: || Box::new(DiffFullWindow),
        context: "Global",
        description: "Give the review the whole window",
    },
    ActionMeta {
        name: "toggle_search_case",
        factory: || Box::new(ToggleSearchCase),
        context: "Search",
        description: "Match case in the find bar",
    },
    ActionMeta {
        name: "search_next",
        factory: || Box::new(SearchNext),
        context: "Search",
        description: "Search next",
    },
    ActionMeta {
        name: "search_prev",
        factory: || Box::new(SearchPrev),
        context: "Search",
        description: "Search previous",
    },
    ActionMeta {
        name: "dismiss_search",
        factory: || Box::new(DismissSearch),
        context: "Search",
        description: "Dismiss search",
    },
    ActionMeta {
        name: "clear_scroll_history",
        factory: || Box::new(ClearScrollHistory),
        context: "Terminal",
        description: "Clear scroll history",
    },
    ActionMeta {
        name: "reset_terminal",
        factory: || Box::new(ResetTerminal),
        context: "Terminal",
        description: "Reset terminal",
    },
    // Quit menu action (bound to cmd-q on macOS via
    // MACOS_ONLY_DEFAULTS; also reachable from Splitlane > Quit Splitlane).
    ActionMeta {
        name: "quit",
        factory: || Box::new(Quit),
        context: "",
        description: "Quit",
    },
    // Markdown pane navigation. Scroll + copy bind on the root
    // `Markdown` context; find-overlay actions bind on `MarkdownSearch`
    // (active only while the search bar is open).
    ActionMeta {
        name: "markdown_scroll_page_up",
        factory: || Box::new(MarkdownScrollPageUp),
        context: "Markdown",
        description: "Markdown: scroll up one page",
    },
    ActionMeta {
        name: "markdown_scroll_page_down",
        factory: || Box::new(MarkdownScrollPageDown),
        context: "Markdown",
        description: "Markdown: scroll down one page",
    },
    ActionMeta {
        name: "markdown_find_open",
        factory: || Box::new(MarkdownFindOpen),
        context: "Markdown",
        description: "Markdown: open find bar",
    },
    ActionMeta {
        name: "markdown_copy",
        factory: || Box::new(MarkdownCopy),
        context: "Markdown",
        description: "Markdown: copy selection / current match",
    },
    ActionMeta {
        name: "markdown_find_next",
        factory: || Box::new(MarkdownFindNext),
        context: "MarkdownSearch",
        description: "Markdown: jump to next match",
    },
    ActionMeta {
        name: "markdown_find_prev",
        factory: || Box::new(MarkdownFindPrev),
        context: "MarkdownSearch",
        description: "Markdown: jump to previous match",
    },
    ActionMeta {
        name: "markdown_find_dismiss",
        factory: || Box::new(MarkdownFindDismiss),
        context: "MarkdownSearch",
        description: "Markdown: close find bar",
    },
    // Toggle the dedicated Git
    // Diff mode (AppMode::Diff).
    ActionMeta {
        name: "open_diff_view",
        factory: || Box::new(crate::OpenDiffView),
        context: "",
        description: "Open the project's changes",
    },
    ActionMeta {
        name: "toggle_files_sidebar",
        factory: || Box::new(crate::ToggleFilesSidebar),
        context: "",
        description: "Toggle Files sidebar",
    },
    // Copy the hunk under the cursor as a
    // unified diff. Scoped to the DiffView context so Ctrl+Shift+C there never
    // collides with the global markdown / terminal copy bindings.
    ActionMeta {
        name: "copy_diff_hunk",
        factory: || Box::new(crate::CopyDiffHunk),
        context: "DiffView",
        description: "Copy hunk as diff",
    },
    // Keyboard-first review loop.
    // Keep these off embedded terminals and text widgets inside DiffView.
    ActionMeta {
        name: "diff_next_hunk",
        factory: || Box::new(crate::DiffNextHunk),
        context: "DiffView && !Terminal && !TextInput && !SplitlaneTextArea",
        description: "Diff: next hunk",
    },
    ActionMeta {
        name: "diff_prev_hunk",
        factory: || Box::new(crate::DiffPrevHunk),
        context: "DiffView && !Terminal && !TextInput && !SplitlaneTextArea",
        description: "Diff: previous hunk",
    },
    ActionMeta {
        name: "diff_toggle_view",
        factory: || Box::new(crate::DiffToggleView),
        context: "DiffView && !Terminal && !TextInput && !SplitlaneTextArea",
        description: "Diff: toggle unified / split",
    },
    ActionMeta {
        name: "diff_toggle_sync",
        factory: || Box::new(crate::DiffToggleSync),
        context: "DiffView && !Terminal && !TextInput && !SplitlaneTextArea",
        description: "Diff: toggle scroll sync",
    },
    ActionMeta {
        name: "diff_dismiss",
        factory: || Box::new(crate::DiffDismiss),
        context: "DiffView && !Terminal && !TextInput && !SplitlaneTextArea",
        description: "Diff: close popover / refocus body",
    },
    // Pane steering. Scoped to `Terminal`: each of
    // these acts on a pane, so the context is the gate.
    ActionMeta {
        name: "open_composer",
        factory: || Box::new(crate::OpenComposer),
        context: "Terminal",
        description: "Send to agent",
    },
    ActionMeta {
        name: "toggle_broadcast_member",
        factory: || Box::new(crate::ToggleBroadcastMember),
        context: "Terminal",
        description: "Toggle pane in broadcast group",
    },
    ActionMeta {
        name: "open_broadcast_groups",
        factory: || Box::new(crate::OpenBroadcastGroups),
        context: "Terminal",
        description: "Broadcast groups",
    },
    // Triage & launch.
    ActionMeta {
        name: "open_attention_queue",
        factory: || Box::new(crate::OpenAttentionQueue),
        context: "Terminal",
        description: "Attention queue",
    },
    ActionMeta {
        name: "open_launch_pad",
        factory: || Box::new(crate::OpenLaunchPad),
        context: "",
        description: "Launch pad",
    },
    ActionMeta {
        name: "start_preset_1",
        factory: || Box::new(crate::StartPreset1),
        context: "",
        description: "Start preset 1",
    },
    ActionMeta {
        name: "start_preset_2",
        factory: || Box::new(crate::StartPreset2),
        context: "",
        description: "Start preset 2",
    },
    ActionMeta {
        name: "start_preset_3",
        factory: || Box::new(crate::StartPreset3),
        context: "",
        description: "Start preset 3",
    },
    ActionMeta {
        name: "new_worktree",
        factory: || Box::new(crate::NewWorktree),
        context: "",
        description: "New worktree in the active project",
    },
    // The actions that were dispatched but never registered, so they appeared
    // neither in the shortcuts screen nor - once it existed - in the command
    // palette. Every action must be reachable - registered, and so listed in
    // Settings -> Shortcuts - which makes registration an invariant, enforced
    // below by `every_splitlane_action_is_registered`.
    //
    // `Copy` / `Paste` are the app-level menu actions that delegate to
    // `TerminalCopy` / `TerminalPaste`; they keep distinct descriptions so
    // the palette never shows two rows both reading "Copy".
    ActionMeta {
        name: "about",
        factory: || Box::new(crate::About),
        context: "",
        description: "About Splitlane",
    },
    ActionMeta {
        name: "copy",
        factory: || Box::new(crate::Copy),
        context: "",
        description: "Copy selection",
    },
    ActionMeta {
        name: "paste",
        factory: || Box::new(crate::Paste),
        context: "",
        description: "Paste from clipboard",
    },
    ActionMeta {
        name: "open_help",
        factory: || Box::new(crate::OpenHelp),
        context: "",
        description: "Open help",
    },
    ActionMeta {
        name: "open_multi_diff",
        factory: || Box::new(crate::OpenMultiDiff),
        context: "",
        description: "Open multi-worktree diff",
    },
    ActionMeta {
        name: "new_home_agent",
        factory: || Box::new(crate::NewHomeAgent),
        context: "",
        description: "New agent in the home directory",
    },
    ActionMeta {
        name: "new_agent",
        factory: || Box::new(crate::NewAgent),
        context: "",
        description: "New agent in the active container",
    },
    ActionMeta {
        name: "open_agents_thread_menu",
        factory: || Box::new(crate::OpenAgentsThreadMenu),
        context: "",
        description: "Session overflow menu",
    },
    ActionMeta {
        name: "start_self_update",
        factory: || Box::new(crate::StartSelfUpdate),
        context: "",
        description: "Install the available update",
    },
    ActionMeta {
        name: "dismiss_update",
        factory: || Box::new(crate::DismissUpdate),
        context: "",
        description: "Dismiss the update notice",
    },
    ActionMeta {
        name: "copy_last_answer",
        factory: || Box::new(crate::CopyLastAnswer),
        context: "",
        description: "Copy the agent's last answer as markdown",
    },
    // The palette lists itself, and by the reachability rule it has to - the
    // rule admits no exception for the surface that enforces it.
    ActionMeta {
        name: "open_command_palette",
        factory: || Box::new(crate::OpenCommandPalette),
        context: "",
        description: "Command palette",
    },
    // The three entries the design's keyboard table named and
    // this build had no action for.
    ActionMeta {
        name: "focus_previous_pane",
        factory: || Box::new(crate::FocusPreviousPane),
        context: "",
        description: "Previous pane",
    },
    ActionMeta {
        name: "add_pane",
        factory: || Box::new(crate::AddPane),
        context: "",
        description: "Add pane",
    },
    ActionMeta {
        name: "new_shell",
        factory: || Box::new(crate::NewShell),
        context: "",
        description: "New shell in the active container",
    },
];

/// Keys a user's `shortcuts` map may still carry from before an action was
/// renamed.
///
/// Renaming an action's key silently unbinds every override written against the
/// old one, which is why `CLAUDE.md` says not to rename them - a rule that then
/// forces a stale word to live forever in a file people read. This is the way
/// out of that trade: the new name is the only one written down, listed and
/// shown, and the old one still resolves.
///
/// Old name first, current name second. Nothing here is documented anywhere
/// else; a key in this table is dead weight the moment nobody's config has it,
/// and removing one is a decision about how old a config we still open.
const RENAMED: &[(&str, &str)] = &[
    // `Split` became `Add pane` and stopped toggling.
    ("toggle_split", "add_pane"),
];

/// The name `name` means today.
///
/// **Every place that compares a name out of the user's `shortcuts` map against
/// `ACTIONS` has to go through this**, not just the lookup that builds the
/// action. A cross-vendor pass found the half that did not: `action_from_name`
/// aliased, so the user's chord bound and worked - while `effective_shortcuts`
/// tested the raw string against `ACTIONS`, found nothing, and drew the default
/// chord on the shortcuts screen instead of the one the user had set. The
/// binding and the screen that documents it disagreed, which is the worst of
/// the three possible outcomes.
pub(super) fn canonical_action_name(name: &str) -> &str {
    RENAMED
        .iter()
        .find_map(|(was, now)| (*was == name).then_some(*now))
        .unwrap_or(name)
}

fn find(name: &str) -> Option<&'static ActionMeta> {
    let name = canonical_action_name(name);
    ACTIONS.iter().find(|a| a.name == name)
}

/// Resolve an action name string to a boxed GPUI action.
///
/// Public beyond `keybindings/` for the command palette, which activates a
/// row by dispatching the action its registry entry names.
pub fn action_from_name(name: &str) -> Option<Box<dyn Action>> {
    find(name).map(|meta| (meta.factory)())
}

/// Context predicate for a given action name. `None` is global.
pub(super) fn context_for_action(name: &str) -> Option<&'static str> {
    find(name)
        .map(|meta| meta.context)
        .filter(|ctx| !ctx.is_empty())
}

/// Human-readable description for an action name, or `"Unknown"`.
pub(super) fn action_description(name: &str) -> &'static str {
    find(name).map(|meta| meta.description).unwrap_or("Unknown")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_from_name_known_actions() {
        assert!(action_from_name("split_horizontally").is_some());
        assert!(action_from_name("close_pane").is_some());
        assert!(action_from_name("toggle_zoom").is_some());
        assert!(action_from_name("undo_close_pane").is_some());
        assert!(action_from_name("swap_pane").is_some());
        assert!(action_from_name("split_equalize").is_some());
        assert!(action_from_name("toggle_copy_mode").is_some());
        assert!(action_from_name("toggle_files_sidebar").is_some());
    }

    /// A rename must not unbind an override written against the old key.
    #[test]
    fn a_renamed_action_still_resolves_under_its_old_key() {
        for (was, now) in RENAMED {
            let old = action_from_name(was).expect("the old key still resolves");
            let new = action_from_name(now).expect("the current key resolves");
            assert_eq!(old.name(), new.name(), "{was} no longer reaches {now}");
        }
        // The shortcuts screen and the palette show the current name only: an
        // alias is a door, not a second entry.
        assert!(!ACTIONS.iter().any(|a| a.name == "toggle_split"));
        assert!(ACTIONS.iter().any(|a| a.name == "add_pane"));
    }

    #[test]
    fn action_from_name_unknown_returns_none() {
        assert!(action_from_name("nonexistent_action").is_none());
        assert!(action_from_name("").is_none());
    }

    #[test]
    fn context_for_terminal_actions() {
        assert_eq!(context_for_action("terminal_copy"), Some("Terminal"));
        assert_eq!(context_for_action("toggle_copy_mode"), Some("Terminal"));
        assert_eq!(context_for_action("toggle_search"), Some("Terminal"));
        assert_eq!(context_for_action("split_horizontally"), None);
        assert_eq!(context_for_action("toggle_files_sidebar"), None);
    }

    #[test]
    fn registry_has_unique_action_names() {
        // A duplicate name would silently shadow another entry's context or
        // description. Catch it early.
        let mut seen = std::collections::HashSet::new();
        for meta in ACTIONS {
            assert!(
                seen.insert(meta.name),
                "duplicate action name `{}` in ACTIONS",
                meta.name
            );
        }
    }

    #[test]
    fn every_splitlane_action_is_registered() {
        // The machine form of the reachability rule (every action is
        // registered and listed in Settings -> Shortcuts; formerly "the
        // palette is the floor of reachability"): an action that is dispatchable but absent from
        // `ACTIONS` shows up in neither the shortcuts screen nor the command
        // palette, and nothing else in the build notices.
        //
        // `generate_list_of_all_registered_actions` walks GPUI's inventory of
        // every `actions!`-generated type linked into the binary, so the check
        // needs no manual list to drift out of date. Filtered to the
        // `splitlane` namespace: `text_input` and `splitlane_text_area` are
        // widget-internal and bind their own keys directly.
        let registered: std::collections::HashSet<&'static str> =
            ACTIONS.iter().map(|meta| (meta.factory)().name()).collect();
        let mut missing: Vec<&'static str> = gpui::generate_list_of_all_registered_actions()
            .map(|action| action.name)
            .filter(|name| name.starts_with("splitlane::"))
            .filter(|name| !registered.contains(name))
            .collect();
        missing.sort_unstable();
        assert!(
            missing.is_empty(),
            "these actions are dispatchable but missing from ACTIONS \
             (add an ActionMeta with a context and a description): {missing:?}"
        );
    }

    #[test]
    fn quit_action_name_resolves() {
        // Cross-platform: `action_from_name` must resolve "quit" to a real
        // Action instance so MACOS_ONLY_DEFAULTS registration succeeds on
        // macOS and user config overrides like `"quit": "secondary-alt-q"`
        // work on any platform.
        assert!(action_from_name("quit").is_some());
    }
}

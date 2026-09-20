//! Default keybinding tables (cross-platform + macOS-only layer).

/// A default keybinding entry: keystroke string, action name, GPUI context filter.
pub(super) struct DefaultBinding {
    pub(super) key: &'static str,
    pub(super) action_name: &'static str,
    pub(super) context: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// The chords the design's keyboard table states, and the ones it
// cannot have on every platform.
//
// The table is written for macOS, and five of its entries are single-modifier
// `\u{2318}`+letter chords. `secondary-` would spell those on macOS and spell
// `ctrl`+letter on Linux and Windows, where every one of the five is a readline
// binding a terminal workspace must not swallow: `ctrl-k` kill-line, `ctrl-b`
// backward-char (and tmux's prefix), `ctrl-f` forward-char, `ctrl-n`
// next-history, `ctrl-d` EOF. The project's own no-shadow rule (default chords
// never shadow a common shell, readline or TUI chord; see the
// note beside `secondary-shift-k` below) already refused exactly this trade.
//
// So each such action carries ONE row whose key is chosen per platform: the
// design's chord on macOS, the chord this build already taught on Linux and
// Windows. One row and not two, because `effective_shortcuts` prints every row
// it finds - two rows would give two answers to "what opens the palette?".

/// Command palette. `\u{2318}K` per the design; `ctrl-k` is kill-line.
#[cfg(target_os = "macos")]
const KEY_COMMAND_PALETTE: &str = "cmd-k";
#[cfg(not(target_os = "macos"))]
const KEY_COMMAND_PALETTE: &str = "ctrl-shift-p";

/// Cycle sessions waiting for you. `\u{2325}\u{21e5}` per the design; on Linux
/// and Windows `alt-tab` belongs to the window manager, which is the same trap
/// `cmd-tab` was on macOS.
#[cfg(target_os = "macos")]
const KEY_JUMP_NEXT_WAITING: &str = "alt-tab";
#[cfg(not(target_os = "macos"))]
const KEY_JUMP_NEXT_WAITING: &str = "ctrl-shift-j";

/// File tree. `\u{2318}B` per the design; `ctrl-b` is backward-char and tmux's
/// prefix.
#[cfg(target_os = "macos")]
const KEY_TOGGLE_FILES: &str = "cmd-b";
#[cfg(not(target_os = "macos"))]
const KEY_TOGGLE_FILES: &str = "ctrl-alt-f";

/// Find in pane. `\u{2318}F` per the design; `ctrl-f` is forward-char.
#[cfg(target_os = "macos")]
const KEY_FIND_IN_PANE: &str = "cmd-f";
#[cfg(not(target_os = "macos"))]
const KEY_FIND_IN_PANE: &str = "ctrl-shift-f";

/// New agent session. `\u{2318}N` per the design; `ctrl-n` is next-history.
#[cfg(target_os = "macos")]
const KEY_NEW_AGENT: &str = "cmd-n";
#[cfg(not(target_os = "macos"))]
const KEY_NEW_AGENT: &str = "ctrl-shift-n";

/// All default keybindings. Order matches the original registration order.
pub(super) const DEFAULTS: &[DefaultBinding] = &[
    // App-global split/workspace bindings use the `secondary`
    // modifier so GPUI resolves to `cmd` on macOS and `ctrl` on Linux/Windows.
    // `secondary` keeps Linux on Ctrl+Shift+… (no user-visible regression)
    // while giving macOS users the expected Cmd+Shift+… shortcuts.
    DefaultBinding {
        key: "secondary-shift-d",
        action_name: "split_horizontally",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-e",
        action_name: "split_vertically",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-w",
        action_name: "close_pane",
        context: None,
    },
    // The design's "Add project from folder" is `\u{21e7}\u{2318}O`.
    // Shift-bearing, so `secondary-shift-o` spells it on all three platforms
    // and shadows nothing. It used to sit on `secondary-shift-n`, which the
    // same table gives to "New agent session".
    DefaultBinding {
        key: "secondary-shift-o",
        action_name: "new_workspace",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-q",
        action_name: "close_workspace",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-shift-alt-c",
        action_name: "copy_workspace_path",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-alt-r",
        action_name: "reveal_workspace_in_file_manager",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-alt-z",
        action_name: "open_workspace_in_zed",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-alt-c",
        action_name: "open_workspace_in_cursor",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-alt-v",
        action_name: "open_workspace_in_vscode",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-alt-w",
        action_name: "open_workspace_in_windsurf",
        context: None,
    },
    // `next_workspace` used to sit on `secondary-tab`, which GPUI resolves to
    // `cmd-tab` on macOS - the system application switcher. The binding was
    // therefore dead on one of the three platforms and alive on the other two,
    // and the design's keyboard table has no chord for "next project" at all:
    // moving between projects is the palette's job. The chord is FREED rather
    // than re-pointed, the same call the two mode switchers got.
    // `next_workspace` stays in the registry and stays reachable from the
    // palette (every action stays reachable).
    DefaultBinding {
        key: "alt-left",
        action_name: "focus_left",
        context: None,
    },
    DefaultBinding {
        key: "alt-right",
        action_name: "focus_right",
        context: None,
    },
    DefaultBinding {
        key: "alt-up",
        action_name: "focus_up",
        context: None,
    },
    DefaultBinding {
        key: "alt-down",
        action_name: "focus_down",
        context: None,
    },
    // Cycle to the next pane whose agent waits
    // for input, cross-workspace. The design's chord is `\u{2325}\u{21e5}`;
    // see `KEY_JUMP_NEXT_WAITING` for why it is macOS-only.
    DefaultBinding {
        key: KEY_JUMP_NEXT_WAITING,
        action_name: "jump_next_waiting",
        context: None,
    },
    // The design's "Previous pane" (`\u{2303}\u{21e5}`): a return to the pane
    // focus came from, not a directional step. `ctrl-tab` is the same chord on
    // all three platforms and no terminal claims it - it is also the chord
    // `next_workspace` used to take on Linux and Windows, freed above.
    DefaultBinding {
        key: "ctrl-tab",
        action_name: "focus_previous_pane",
        context: None,
    },
    // The design's `\u{2325}\`. Alt-bearing, so it is the same chord
    // everywhere and shadows no readline binding.
    DefaultBinding {
        key: "alt-\\",
        action_name: "add_pane",
        context: None,
    },
    // The design's `\u{2318}N`, the keyboard half of the rail's `+ agent`.
    DefaultBinding {
        key: KEY_NEW_AGENT,
        action_name: "new_agent",
        context: None,
    },
    // The presets, in the Launch pad's list order (the design: "shortcuts
    // follow the position in the list"). `secondary-shift-` rather than a bare
    // `secondary-` because `⌘1-9` already selects a project.
    DefaultBinding {
        key: "secondary-shift-1",
        action_name: "start_preset_1",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-2",
        action_name: "start_preset_2",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-3",
        action_name: "start_preset_3",
        context: None,
    },
    DefaultBinding {
        key: "secondary-1",
        action_name: "select_workspace_1",
        context: None,
    },
    DefaultBinding {
        key: "secondary-2",
        action_name: "select_workspace_2",
        context: None,
    },
    DefaultBinding {
        key: "secondary-3",
        action_name: "select_workspace_3",
        context: None,
    },
    DefaultBinding {
        key: "secondary-4",
        action_name: "select_workspace_4",
        context: None,
    },
    DefaultBinding {
        key: "secondary-5",
        action_name: "select_workspace_5",
        context: None,
    },
    DefaultBinding {
        key: "secondary-6",
        action_name: "select_workspace_6",
        context: None,
    },
    DefaultBinding {
        key: "secondary-7",
        action_name: "select_workspace_7",
        context: None,
    },
    DefaultBinding {
        key: "secondary-8",
        action_name: "select_workspace_8",
        context: None,
    },
    DefaultBinding {
        key: "secondary-9",
        action_name: "select_workspace_9",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-t",
        action_name: "undo_close_pane",
        context: None,
    },
    DefaultBinding {
        key: "secondary-alt-t",
        action_name: "new_tab",
        context: None,
    },
    DefaultBinding {
        key: "secondary-w",
        action_name: "close_tab",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-shift-c",
        action_name: "terminal_copy",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "ctrl-shift-v",
        action_name: "terminal_paste",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "shift-pageup",
        action_name: "scroll_page_up",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "shift-pagedown",
        action_name: "scroll_page_down",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-shift-up",
        action_name: "jump_prev_prompt",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-shift-down",
        action_name: "jump_next_prompt",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-shift-z",
        action_name: "toggle_zoom",
        context: None,
    },
    DefaultBinding {
        key: "secondary-alt-1",
        action_name: "layout_even_horizontal",
        context: None,
    },
    DefaultBinding {
        key: "secondary-alt-2",
        action_name: "layout_even_vertical",
        context: None,
    },
    // `secondary-alt-3` (main vertical) and `secondary-alt-4` (tiled) used to
    // follow. Both actions are gone with the nested trees they drew; the two
    // chords above are the Orientation control and are all a row-or-column
    // container has to say.
    DefaultBinding {
        key: "secondary-shift-=",
        action_name: "split_equalize",
        context: None,
    },
    DefaultBinding {
        key: "secondary-shift-s",
        action_name: "swap_pane",
        context: None,
    },
    DefaultBinding {
        key: "ctrl-shift-x",
        action_name: "toggle_copy_mode",
        context: Some("Terminal"),
    },
    // The design's "Find in pane" is `\u{2318}F`; see `KEY_FIND_IN_PANE`.
    DefaultBinding {
        key: KEY_FIND_IN_PANE,
        action_name: "toggle_search",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "enter",
        action_name: "search_next",
        context: Some("Search"),
    },
    DefaultBinding {
        key: "shift-enter",
        action_name: "search_prev",
        context: Some("Search"),
    },
    DefaultBinding {
        key: "escape",
        action_name: "dismiss_search",
        context: Some("Search"),
    },
    DefaultBinding {
        key: "alt-r",
        action_name: "toggle_search_regex",
        context: Some("Search"),
    },
    // Fan the open search out to every pane (fleet grep).
    // Search context only, so no terminal chord is shadowed.
    DefaultBinding {
        key: "alt-f",
        action_name: "toggle_fleet_search",
        context: Some("Search"),
    },
    // Per-pane font zoom. These DO shadow readline's
    // C-- (undo) / C-0 (digit-argument) in the focused terminal: a
    // deliberate, remappable exception to not shadowing shell chords,
    // matching the zoom convention of gnome-terminal/Ghostty on Linux.
    DefaultBinding {
        key: "secondary-=",
        action_name: "font_size_increase",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary--",
        action_name: "font_size_decrease",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-0",
        action_name: "font_size_reset",
        context: Some("Terminal"),
    },
    // Markdown pane navigation. Same chord vocabulary as the
    // terminal pane so muscle memory transfers cleanly between pane types.
    DefaultBinding {
        key: "shift-pageup",
        action_name: "markdown_scroll_page_up",
        context: Some("Markdown"),
    },
    DefaultBinding {
        key: "shift-pagedown",
        action_name: "markdown_scroll_page_down",
        context: Some("Markdown"),
    },
    // "⌘F works here" - the design gives one find chord for every pane kind,
    // so this is the same `KEY_FIND_IN_PANE` the terminal's find bar answers
    // to, not a second one. It used to be a bare `ctrl-f`, which on macOS was
    // not the chord the design names and on Linux was readline's
    // forward-char.
    DefaultBinding {
        key: KEY_FIND_IN_PANE,
        action_name: "markdown_find_open",
        context: Some("Markdown"),
    },
    DefaultBinding {
        key: "ctrl-shift-c",
        action_name: "markdown_copy",
        context: Some("Markdown"),
    },
    // "`\u{23ce}` to hand off" to the editor. Not bare `Markdown`: the find bar
    // layers `MarkdownSearch` onto the same node and `enter` there is
    // `markdown_find_next`, so the document's own Enter has to stand down while
    // a query is being typed.
    DefaultBinding {
        key: "enter",
        action_name: "open_file_in_editor",
        context: Some("Markdown && !MarkdownSearch"),
    },
    DefaultBinding {
        key: "enter",
        action_name: "markdown_find_next",
        context: Some("MarkdownSearch"),
    },
    DefaultBinding {
        key: "shift-enter",
        action_name: "markdown_find_prev",
        context: Some("MarkdownSearch"),
    },
    DefaultBinding {
        key: "escape",
        action_name: "markdown_find_dismiss",
        context: Some("MarkdownSearch"),
    },
    // `secondary-shift-a` (Agents mode) and `secondary-shift-g` (Diff mode)
    // used to live here. Both switched modes, and there are no modes; the
    // design's call was to FREE the chords rather than re-point them, because
    // silently redefining a learned chord is worse than losing it. Neither is
    // bound to anything now. `open_diff_view` survives as an action - it
    // selects the active container's `Changes` surface - and stays reachable
    // from the palette and from the rail row of the same name.
    // The file tree. The design's chord is `\u{2318}B`; see
    // `KEY_TOGGLE_FILES` for why Linux and Windows keep `ctrl-alt-f`, which is
    // what `secondary-alt-f` resolved to there anyway.
    DefaultBinding {
        key: KEY_TOGGLE_FILES,
        action_name: "toggle_files_sidebar",
        context: None,
    },
    // Copy the hunk under the cursor as a
    // unified diff, only while the Git Diff view holds focus. Same chord as the
    // terminal / markdown copies - disambiguated by the `DiffView` context.
    DefaultBinding {
        key: "ctrl-shift-c",
        action_name: "copy_diff_hunk",
        context: Some("DiffView"),
    },
    // Keyboard-first review loop.
    // Bare keys, scoped away from terminals and text widgets so focus children
    // of the DiffView do not lose a keystroke.
    DefaultBinding {
        key: "]",
        action_name: "diff_next_hunk",
        context: Some("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
    },
    DefaultBinding {
        key: "[",
        action_name: "diff_prev_hunk",
        context: Some("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
    },
    DefaultBinding {
        key: "u",
        action_name: "diff_toggle_view",
        context: Some("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
    },
    DefaultBinding {
        key: "s",
        action_name: "diff_toggle_sync",
        context: Some("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
    },
    DefaultBinding {
        key: "escape",
        action_name: "diff_dismiss",
        context: Some("DiffView && !Terminal && !TextInput && !SplitlaneTextArea"),
    },
    // Composer + broadcast
    // groups. All three are unclaimed `secondary-shift-…` slots (taken set
    // before this block: d/e/w/n/q/j/t/z/=/s/a/g) and none shadows a common
    // shell/readline/TUI chord - Ctrl+Shift+Space/B/M mean nothing to
    // readline, vim or nano. Remappable like every entry in this table.
    //
    // These five are scoped to `Terminal`: they act on a pane, and outside a
    // focused pane they used to fire into a mode check and return silently.
    // The context is the gate, and it is visible - the palette and the
    // shortcuts screen both group by it.
    DefaultBinding {
        key: "secondary-shift-space",
        action_name: "open_composer",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-shift-b",
        action_name: "toggle_broadcast_member",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "secondary-shift-m",
        action_name: "open_broadcast_groups",
        context: Some("Terminal"),
    },
    // Attention Queue + Launch Pad. `secondary-shift-k`
    // and `secondary-shift-l` are unclaimed (taken set before this block:
    // d/e/w/n/q/j/t/z/=/s/a/g/space/b/m) and shadow no shell/readline/TUI
    // chord (Ctrl+K kill-line is BARE ctrl, not ctrl+shift).
    DefaultBinding {
        key: "secondary-shift-k",
        action_name: "open_attention_queue",
        context: Some("Terminal"),
    },
    // Global, not `Terminal`, since the rewrite: the Launch pad lists the
    // preset library, which belongs to no pane, and its old pane-scoped guard
    // went with the worktree launcher that used to live behind this chord.
    DefaultBinding {
        key: "secondary-shift-l",
        action_name: "open_launch_pad",
        context: None,
    },
    // The command palette. The design
    // puts it on `\u{2318}K` and hangs the whole of "go to project or session"
    // off that one chord; see `KEY_COMMAND_PALETTE` for what Linux and Windows
    // keep instead. Global context - the palette has to open from a focused
    // terminal, from the diff and from an agent surface alike.
    DefaultBinding {
        key: KEY_COMMAND_PALETTE,
        action_name: "open_command_palette",
        context: None,
    },
    // The design's ⌥⌘C. `secondary-alt-c` resolves to ⌥⌘C on
    // macOS and ctrl-alt-c on Linux and Windows, which is why the table says
    // `secondary-` and not `cmd-`. Global, because the report being copied is
    // usually read with the pointer somewhere other than the composer.
    DefaultBinding {
        key: "secondary-alt-c",
        action_name: "copy_last_answer",
        context: None,
    },
];

/// Platform-specific default bindings layered on top of [`DEFAULTS`].
///
/// Binds `cmd-c` / `cmd-v` to terminal copy/paste on macOS so muscle
/// memory from iTerm2 / Terminal.app / WezTerm works. Kept empty on Linux
/// because Linux keyboards don't have a `cmd` key by default. The
/// existing `ctrl-shift-c/v` Terminal bindings stay intact on both platforms -
/// these are purely additive.
#[cfg(target_os = "macos")]
pub(super) const MACOS_ONLY_DEFAULTS: &[DefaultBinding] = &[
    DefaultBinding {
        key: "cmd-c",
        action_name: "terminal_copy",
        context: Some("Terminal"),
    },
    DefaultBinding {
        key: "cmd-v",
        action_name: "terminal_paste",
        context: Some("Terminal"),
    },
    // The markdown pane's copy needs the same treatment: `ctrl-shift-c` is a
    // terminal convention that means nothing on macOS, so Cmd+C never reached
    // `markdown_copy`. Additive, exactly like the terminal pair above - the
    // `Markdown` context is disjoint from `Terminal`, and the markdown viewer
    // has no text input of its own (the find bar rides on the pane's own
    // `on_key_down`), so nothing native competes for this chord.
    DefaultBinding {
        key: "cmd-c",
        action_name: "markdown_copy",
        context: Some("Markdown"),
    },
    // The design's `\u{2303}\u{2318}N` for "New shell". A chord mixing Control
    // with the platform modifier has no Linux/Windows spelling at all
    // (`secondary` IS Control there), so the action is macOS-only in this
    // table and stays reachable everywhere from the palette and from the
    // rail's `+ shell`.
    DefaultBinding {
        key: "ctrl-cmd-n",
        action_name: "new_shell",
        context: None,
    },
    // The design's `\u{2318}D`, "Open diff for the project". `open_diff_view`
    // has had no chord since the mode switchers were freed; `ctrl-d` is EOF on
    // Linux and Windows, so this one is macOS-only too.
    DefaultBinding {
        key: "cmd-d",
        action_name: "open_diff_view",
        context: None,
    },
    // Cmd+Q quits the app and populates the "⌘Q" shortcut next to
    // the Quit Splitlane menu item. Global context so the menu picks it up
    // whether or not a terminal pane holds focus.
    DefaultBinding {
        key: "cmd-q",
        action_name: "quit",
        context: None,
    },
];

#[cfg(not(target_os = "macos"))]
pub(super) const MACOS_ONLY_DEFAULTS: &[DefaultBinding] = &[];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_cover_all_core_actions() {
        let action_names: Vec<&str> = DEFAULTS.iter().map(|d| d.action_name).collect();
        for name in &[
            "split_horizontally",
            "split_vertically",
            "close_pane",
            "focus_left",
            "focus_right",
            "focus_up",
            "focus_down",
            "terminal_copy",
            "terminal_paste",
            "toggle_zoom",
            "toggle_copy_mode",
            "toggle_search",
            "split_equalize",
            "swap_pane",
            "undo_close_pane",
            "toggle_files_sidebar",
        ] {
            assert!(
                action_names.contains(name),
                "Action '{name}' missing from DEFAULTS"
            );
        }
    }

    // -- secondary- modifier migration -----------------------------------

    #[test]
    fn migrated_defaults_use_secondary() {
        // The seven migrated actions must carry a `secondary-` prefix.
        let migrated = [
            "split_horizontally",
            "split_vertically",
            "close_pane",
            "new_workspace",
            "close_workspace",
            "select_workspace_1",
            "select_workspace_2",
            "select_workspace_3",
            "select_workspace_4",
            "select_workspace_5",
            "select_workspace_6",
            "select_workspace_7",
            "select_workspace_8",
            "select_workspace_9",
        ];
        for action in migrated {
            let entry = DEFAULTS
                .iter()
                .find(|d| d.action_name == action)
                .unwrap_or_else(|| panic!("missing DEFAULTS entry for {action}"));
            assert!(
                entry.key.starts_with("secondary-"),
                "action `{action}` still uses `{}` - it requires the `secondary-` prefix",
                entry.key,
            );
        }
    }

    #[test]
    fn terminal_copy_paste_untouched() {
        // Terminal copy/paste must keep `ctrl-shift-c/v` so Linux users
        // retain the terminal-standard bindings and Ctrl+C stays SIGINT-safe.
        let copy = DEFAULTS
            .iter()
            .find(|d| d.action_name == "terminal_copy")
            .expect("terminal_copy must be a default");
        assert_eq!(copy.key, "ctrl-shift-c");
        assert_eq!(copy.context, Some("Terminal"));

        let paste = DEFAULTS
            .iter()
            .find(|d| d.action_name == "terminal_paste")
            .expect("terminal_paste must be a default");
        assert_eq!(paste.key, "ctrl-shift-v");
        assert_eq!(paste.context, Some("Terminal"));
    }

    // -- macOS copy/paste -----------------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_c_cmd_v_bound_on_macos() {
        let copy = MACOS_ONLY_DEFAULTS
            .iter()
            .find(|d| d.key == "cmd-c")
            .expect("cmd-c must be a macOS default");
        assert_eq!(copy.action_name, "terminal_copy");
        assert_eq!(copy.context, Some("Terminal"));

        let paste = MACOS_ONLY_DEFAULTS
            .iter()
            .find(|d| d.key == "cmd-v")
            .expect("cmd-v must be a macOS default");
        assert_eq!(paste.action_name, "terminal_paste");
        assert_eq!(paste.context, Some("Terminal"));

        // Base DEFAULTS still hold the ctrl-shift-c/v entries - the macOS
        // bindings are ADDITIVE, not replacements.
        assert!(
            DEFAULTS
                .iter()
                .any(|d| d.key == "ctrl-shift-c" && d.action_name == "terminal_copy")
        );
        assert!(
            DEFAULTS
                .iter()
                .any(|d| d.key == "ctrl-shift-v" && d.action_name == "terminal_paste")
        );
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn no_cmd_bindings_on_linux() {
        assert!(
            MACOS_ONLY_DEFAULTS.is_empty(),
            "Linux build should carry zero macOS-only defaults, got {} entries",
            MACOS_ONLY_DEFAULTS.len()
        );
    }

    #[test]
    fn ctrl_c_never_bound_to_terminal_copy() {
        // Plain `ctrl-c` (without shift) must never reach terminal_copy
        // on any platform - the PTY needs to receive it so running
        // processes still get SIGINT.
        let leaked_actions: Vec<&'static str> = DEFAULTS
            .iter()
            .chain(MACOS_ONLY_DEFAULTS.iter())
            .filter(|d| d.key == "ctrl-c")
            .map(|d| d.action_name)
            .collect();
        assert!(
            leaked_actions.is_empty(),
            "ctrl-c must not appear in defaults (SIGINT safety); bound to: {leaked_actions:?}"
        );
    }

    // -- markdown copy on macOS ------------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn markdown_copy_bound_to_cmd_c_on_macos() {
        let copy = MACOS_ONLY_DEFAULTS
            .iter()
            .find(|d| d.key == "cmd-c" && d.action_name == "markdown_copy")
            .expect("cmd-c must reach markdown_copy on macOS");
        assert_eq!(copy.context, Some("Markdown"));
    }

    #[test]
    fn markdown_copy_keeps_ctrl_shift_c_everywhere() {
        // The macOS binding is additive: Linux and Windows keep the chord they
        // already learned, and it stays disjoint from terminal copy by context.
        let copy = DEFAULTS
            .iter()
            .find(|d| d.action_name == "markdown_copy")
            .expect("markdown_copy must be a default");
        assert_eq!(copy.key, "ctrl-shift-c");
        assert_eq!(copy.context, Some("Markdown"));
    }

    // -- macOS quit ----------------------------------------------------

    #[cfg(target_os = "macos")]
    #[test]
    fn cmd_q_bound_to_quit() {
        let quit = MACOS_ONLY_DEFAULTS
            .iter()
            .find(|d| d.key == "cmd-q")
            .expect("cmd-q must be a macOS default");
        assert_eq!(quit.action_name, "quit");
        assert_eq!(quit.context, None);
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn no_cmd_q_on_linux() {
        assert!(MACOS_ONLY_DEFAULTS.iter().all(|d| d.key != "cmd-q"));
    }
}

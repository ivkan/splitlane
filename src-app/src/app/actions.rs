//! GPUI action types dispatched through the focus chain.
//!
//! The `actions!` macro generates zero-sized types in the enclosing module,
//! all publicly visible, and registers them under the `splitlane` namespace
//! for JSON dispatch via `cx.dispatch_action`.

use gpui::actions;

actions!(
    splitlane,
    [
        SplitHorizontally,
        SplitVertically,
        ClosePane,
        NewTab,
        CloseTab,
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        JumpNextWaiting,
        NewWorkspace,
        CloseWorkspace,
        CopyWorkspacePath,
        RevealWorkspaceInFileManager,
        OpenWorkspaceInZed,
        OpenWorkspaceInCursor,
        OpenWorkspaceInVsCode,
        OpenWorkspaceInWindsurf,
        NextWorkspace,
        SelectWorkspace1,
        SelectWorkspace2,
        SelectWorkspace3,
        SelectWorkspace4,
        SelectWorkspace5,
        SelectWorkspace6,
        SelectWorkspace7,
        SelectWorkspace8,
        SelectWorkspace9,
        TerminalCopy,
        TerminalPaste,
        ScrollPageUp,
        ScrollPageDown,
        CloseWindow,
        ToggleZoom,
        // `LayoutMainVertical` and `LayoutTiled` used to sit here. Both drew
        // a nested tree, and a container's panes are one row or one column of
        // at most three now; a preset that produced a grid would have made the
        // toolbar's Orientation control - which states one of two answers -
        // wrong. Removed rather than re-pointed, like the mode switchers.
        // These two remain: they are the Orientation control, not presets.
        LayoutEvenHorizontal,
        LayoutEvenVertical,
        LayoutGrid,
        OpenFileInEditor,
        SplitEqualize,
        SwapPane,
        ToggleSearch,
        ToggleSearchRegex,
        ToggleSearchCase,
        UndoClosePane,
        SearchNext,
        SearchPrev,
        DismissSearch,
        ToggleCopyMode,
        ClearScrollHistory,
        ResetTerminal,
        JumpPrevPrompt,
        JumpNextPrompt,
        // Per-pane font zoom. Terminal
        // context; the secondary-=/-/-0 default chords follow the Linux
        // terminal-emulator zoom convention (gnome-terminal, Ghostty) -
        // the readline shadow is a deliberate, documented and remappable
        // exception to the no-shadow rule.
        FontSizeIncrease,
        FontSizeDecrease,
        FontSizeReset,
        // Widen the open search to every pane of every
        // workspace (fleet grep). Search context (find bar open).
        ToggleFleetSearch,
        StartSelfUpdate,
        /// Dismiss the update pill for the current launch
        /// (no persistence - re-prompts on next start). Dispatched by
        /// the `×` button on the Idle / Errored pill states.
        DismissUpdate,
        // macOS native menu-bar actions. Dispatched by `cx.set_menus`
        // via GPUI's `on_app_menu_action` → `cx.dispatch_action`, then caught
        // by the `.on_action(...)` handlers on the SplitlaneApp render root.
        // `SelectAll` used to live here, wired to a stub that only logged.
        // The Edit menu entry looked functional and was not; nothing in the
        // terminal implements select-all, so the action and its menu item
        // were removed together rather than advertised in the registry.
        Quit,
        About,
        Copy,
        Paste,
        OpenHelp,
        // Markdown pane navigation. Scoped to
        // the `Markdown` key context (root) and `MarkdownSearch` (when the
        // find overlay is open). Defined as separate actions from terminal
        // scroll/copy so the keybinding registry can scope them cleanly.
        MarkdownScrollPageUp,
        MarkdownScrollPageDown,
        MarkdownFindOpen,
        MarkdownFindNext,
        MarkdownFindPrev,
        MarkdownFindDismiss,
        MarkdownCopy,
        // Open the multi-worktree diff view for the active workspace's repo. Resolves
        // the repo from `active_idx`'s `repo_root` and opens a `DiffView` tab
        // seeded with every sibling worktree. Also invoked directly by the
        // sidebar group header's "Diff all" button.
        OpenMultiDiff,
        // Toggle the dedicated Git Diff mode (AppMode::Diff): a full-screen diff
        // surface entered via the CLI / Diff / Agents sidebar toggle.
        // Distinct from `OpenMultiDiff` (the ephemeral tab path), which
        // stays alive as a secondary entry.
        OpenDiffView,
        // The design's `⤢` on the Review header: give the review the whole
        // window (one pane, no Files panel), and give it back on the second
        // press.
        DiffFullWindow,
        // Copy the hunk under the
        // cursor as a unified diff (Ctrl+Shift+C inside the DiffView context).
        CopyDiffHunk,
        // Keyboard-first review
        // loop. All scoped to `DiffView && !Terminal && !TextInput` so they drive
        // the diff body without stealing keystrokes from an embedded review/shell
        // terminal or the base-branch filter input.
        // `[`/`]` step hunks (wired to `goto_hunk`), `u` toggles unified/split,
        // `s` toggles cross-column scroll sync, `Esc` dismisses any open
        // popover/menu and refocuses the body.
        DiffNextHunk,
        DiffPrevHunk,
        DiffToggleView,
        DiffToggleSync,
        DiffDismiss,
        // Open the overflow (`⋯`) menu for the current agent surface. Dispatched
        // by the title-bar `⋯` button (a SEPARATE `TitleBar` entity with no
        // access to agents state), caught by `SplitlaneApp` which resolves the
        // selected target and opens the shared thread context menu. The
        // button never calls agents methods directly - it dispatches this
        // typed action, mirroring the update pill's `StartSelfUpdate`.
        OpenAgentsThreadMenu,
        // The old rail header's "New chat": an agent in the home directory.
        // Free chats are agent surfaces of the home directory's container
        // now, so this opens the agent picker on that container, creating it
        // if the user has none. An action lives where its object lives, so the
        // button form of this belongs to the container's own row; this keeps
        // the shortcut a name in the palette (every action stays reachable by
        // name), which is where a create-anywhere action belongs.
        NewHomeAgent,
        // Launch an agent in the active container. The keyboard half of the
        // container row's `+`; the design names creation, presets and the
        // palette as the ways in once the tab strip's row of brand logos is gone, and
        // this is the palette one. It opens the picker rather than launching a
        // remembered agent: choosing which agent is the whole content of the
        // decision, and a remembered default would hide it.
        NewAgent,
        // CLI cockpit steering. `OpenComposer` anchors the multi-line prompt
        // Composer to the focused pane; `ToggleBroadcastMember` and
        // `OpenBroadcastGroups` manage the named pane groups the
        // Composer's broadcast mode targets.
        OpenComposer,
        ToggleBroadcastMember,
        OpenBroadcastGroups,
        // Triage & launch.
        // `OpenAttentionQueue` lists every WaitingForInput session
        // cross-workspace with its question + wait time; `OpenLaunchPad`
        // opens the preset library and the editor for one. It used to be the
        // worktree + split + agent + prefill modal; both halves of that are
        // on the rail now (`+ agent`, `+ worktree`).
        ToggleFilesSidebar,
        OpenAttentionQueue,
        OpenLaunchPad,
        // The rail's `+ worktree`, beside `+ agent` and `+ shell`. It has no
        // default chord: the design gives worktree creation an affordance
        // where the project lives, not a key, and every unclaimed letter in a
        // terminal-facing context is one a shell would rather have.
        NewWorktree,
        // `⇧⌘1-3` in the Launch pad's list order. Three fixed actions
        // indexing a list, rather than a binding generated per preset: a
        // preset is a file, and a file must not be able to add a row to the
        // action registry. The registry stays whole, and with it the rule that
        // every action is registered and reachable.
        StartPreset1,
        StartPreset2,
        StartPreset3,
        // The command palette.
        // Toggling, so the same chord (and the same title-bar control)
        // closes it. Not mode-gated: the palette was then the floor of
        // reachability (every action reachable from it) in every mode.
        OpenCommandPalette,
        // Copying an answer out. The main scenario is moving a
        // final report into another session, which is why it has a chord at
        // all. An agent surface is the CLI's own terminal, so there is no
        // message to hang an affordance on and no block boundary to copy
        // between: the answer is read out of the session's own `.jsonl`, which
        // is there whether anything is drawing it or not.
        CopyLastAnswer,
        // From the design's keyboard table.
        //
        // `FocusPreviousPane` is the design's "Previous pane" (⌃⇥): back to
        // the pane that held focus before this one, which is a different
        // question from any of the four directional moves - a return, not a
        // step.
        FocusPreviousPane,
        // `AddPane` is the design's ⌥\: one more pane, up to the limit. The
        // `Add pane` button in the toolbar is the same gesture with a mouse.
        //
        // It was `ToggleSplit` and it toggled - adding from one pane and
        // collapsing to the focused one from several. That was taken
        // apart: one name is one action, and the collapse it hid had no name at all.
        // The old **config key** still resolves (`keybindings::registry`), so a
        // user's `shortcuts` override survives the rename.
        AddPane,
        // `NewShell` is the design's ⌃⌘N, and the keyboard half of the rail's
        // `+ shell`. `NewAgent` already covered ⌘N; a shell had only the
        // button, which left it out of the palette entirely (every action must
        // be reachable by name).
        NewShell
    ]
);

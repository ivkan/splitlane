//! Per-file right-click context menu for the Files sidebar.
//!
//! Mirrors `render_workspace_context_menu` (`deferred().priority(3)`,
//! `occlude()`, `on_mouse_down_out` dismiss).
//!
//! It says what can be done with the row's path, and the rows are the file
//! surface's own (`app/sidebar/context_menu.rs`) rather than a second answer
//! to the same question: "Copy Path" and "Copy Relative Path" for any row,
//! then "Reveal in file manager", then - for a file - one open row, which is
//! "Open in editor" or "Open with the default app" depending on what the file
//! is. It used to stop after the two copies, so the tree could name a file and
//! not open it anywhere but in a pane.
//!
//! A directory gets the reveal and no open row: opening a folder *is* revealing
//! it, and an editor row on a directory would promise something this app has no
//! way to mean.

use gpui::{
    AnyElement, ClickEvent, Context, IntoElement, MouseButton, ParentElement, Styled, deferred,
    div, prelude::*, px,
};

use crate::app::files_tree;
use crate::app::sidebar::context_menu::clamped_context_menu_position;
use crate::ui_tokens as tok;
use crate::{FilesContextMenu, SplitlaneApp};

/// One row's share of the menu's height, and the padding around the list.
///
/// The menu is positioned before it is laid out - `clamped_context_menu_position`
/// needs a height to decide whether it fits below the click - so the height is
/// arithmetic rather than measured, and it has to count every row that will be
/// drawn. Two rows at the old constant meant 66px; a menu that grew and kept
/// that number would hang its last row off the window edge.
const MENU_ROW_PX: f32 = 29.;
const MENU_PADDING_PX: f32 = 8.;

impl SplitlaneApp {
    pub(crate) fn render_files_context_menu(
        &self,
        menu: FilesContextMenu,
        ui: crate::theme::UiColors,
        window: &mut gpui::Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        // A directory has three rows, a file four.
        let is_dir = menu.is_dir;
        // Answered on the right-click and carried here: this function runs on
        // every frame the menu is up.
        let in_editor = menu.opens_in_editor;
        let rows = if is_dir { 3. } else { 4. };
        let menu_height = px(MENU_PADDING_PX + rows * MENU_ROW_PX);
        let menu_width = px(220.);
        let menu_pos =
            clamped_context_menu_position(menu.position, menu_width, menu_height, window);

        let abs_path = menu.path.clone();
        let rel_root = self.files_tree.root.clone();
        let rel_path = menu.path.clone();
        let reveal_path = menu.path.clone();
        let open_path = menu.path.clone();

        let mut context_menu = div()
            .id("files-context-menu")
            .occlude()
            .absolute()
            .left(menu_pos.x)
            .top(menu_pos.y)
            .w(menu_width)
            .bg(ui.overlay)
            .border_1()
            .border_color(ui.border)
            .rounded(tok::radius::PANEL)
            .flex()
            .flex_col()
            .p(tok::space::XS)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.files_menu_open = None;
                cx.notify();
            }))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
            .child(self.render_context_menu_item(
                "files-context-copy-path".into(),
                "Copy Path",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    let value = abs_path.to_string_lossy().into_owned();
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(value));
                    this.files_menu_open = None;
                    this.show_toast("Copied path", cx);
                    cx.stop_propagation();
                }),
            ))
            .child(self.render_context_menu_item(
                "files-context-copy-rel".into(),
                "Copy Relative Path",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    let value = files_tree::workspace_relative_path(&rel_root, &rel_path);
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(value));
                    this.files_menu_open = None;
                    this.show_toast("Copied relative path", cx);
                    cx.stop_propagation();
                }),
            ))
            .child(self.render_context_menu_item(
                "files-context-reveal".into(),
                "Reveal in file manager",
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.files_menu_open = None;
                    let outcome = if is_dir {
                        crate::app::workspace_ops::reveal_in_file_manager(&reveal_path)
                    } else {
                        crate::app::workspace_ops::reveal_file_in_file_manager(&reveal_path)
                    };
                    if let Err(err) = outcome {
                        this.show_toast(err, cx);
                    }
                    cx.stop_propagation();
                }),
            ));

        if !is_dir {
            context_menu = context_menu.child(self.render_context_menu_item(
                "files-context-open".into(),
                if in_editor {
                    "Open in editor"
                } else {
                    "Open with the default app"
                },
                None,
                ui,
                cx.listener(move |this, _: &ClickEvent, _window, cx| {
                    this.files_menu_open = None;
                    if in_editor {
                        this.open_path_in_editor(open_path.clone(), cx);
                    } else if let Err(err) =
                        crate::app::workspace_ops::open_in_default_app(&open_path)
                    {
                        this.show_toast(err, cx);
                    }
                    cx.stop_propagation();
                }),
            ));
        }

        deferred(context_menu).priority(3).into_any_element()
    }
}

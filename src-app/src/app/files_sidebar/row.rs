//! Single Files-tree row render: indent + chevron + icon + name, with the
//! markdown/greyed styling, click-to-open / expand, markdown drag-to-pane, and
//! the right-click copy-path menu trigger. Split out of `view.rs` to keep each
//! file under the 250-line budget.

use gpui::{
    AnyElement, ClickEvent, Context, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, div, prelude::*, px, svg,
};

use super::{DIMMED_OPACITY, INDENT_STEP, ROW_HEIGHT};
use crate::SplitlaneApp;
use crate::app::files_tree::{self, VisibleRowRef};
use crate::pane_drag::{MarkdownFileDrag, TabDragPreview};
use crate::ui_primitives::{AnimatedHoverExt, lerp_color};
use crate::ui_tokens as tok;

impl SplitlaneApp {
    pub(super) fn files_row(
        &self,
        row: VisibleRowRef<'_>,
        selected: bool,
        ui: crate::theme::UiColors,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let node = row.node;
        let name: SharedString = files_tree::node_name(node).into();
        // Every row is actionable now: a directory expands and a file opens.
        // The greying that used to mark "only markdown opens" said something
        // that is no longer true, and a row that reads as inert while it
        // answers a click is worse than no distinction at all.
        let dimmed = node.is_ignored || node.is_hidden;
        let text_color = ui.text;
        let indent = px(8. + row.depth as f32 * INDENT_STEP);
        let path = node.path.clone();
        let is_dir = node.is_dir;
        let git_mark = (!is_dir)
            .then(|| self.files_git_marks.get(&node.path).copied())
            .flatten();

        // Leading 14px slot: a chevron for directories (right = collapsed,
        // down = expanded - a static swap, legible under reduced motion), an
        // invisible spacer for files so names align.
        let chevron = if is_dir {
            svg()
                .size(px(12.))
                .flex_none()
                .path(if row.expanded {
                    "icons/chevron-down.svg"
                } else {
                    "icons/chevron-right.svg"
                })
                .text_color(ui.muted)
                .into_any_element()
        } else {
            div().size(px(14.)).flex_none().into_any_element()
        };

        let icon = if is_dir {
            if row.expanded {
                "icons/folder-open.svg"
            } else {
                "icons/folder.svg"
            }
        } else {
            "icons/file-text.svg"
        };

        let mut el = div()
            .id(SharedString::from(format!(
                "files-row-{}",
                node.path.display()
            )))
            .flex()
            .flex_row()
            .items_center()
            .gap(tok::space::XS)
            .h(ROW_HEIGHT)
            // Quiet-row geometry (matches the sessions sidebar): inset rounded
            // rows so the hover fill reads as a discrete pill, not a full-bleed
            // band.
            .mx(tok::space::XS)
            .rounded(tok::radius::SMALL)
            .pl(indent)
            .pr(tok::space::MD)
            .when(selected, |s| {
                s.bg(crate::app::constants::sidebar_tab_active_background())
            })
            .when(dimmed, |s| s.opacity(DIMMED_OPACITY));

        // Right-click any row (markdown, greyed file, or directory) to
        // open the copy-path menu. Sits on the base row so it works regardless
        // of actionability.
        let menu_path = path.clone();
        el = el.on_aux_click(cx.listener(move |this, e: &ClickEvent, window, cx| {
            if e.is_right_click()
                && let Some(position) = e.mouse_position()
            {
                this.dismiss_transient_surfaces();
                this.files_focus.focus(window, cx);
                this.select_files_row(&menu_path);
                this.files_menu_open = Some(crate::FilesContextMenu {
                    path: menu_path.clone(),
                    is_dir,
                    // Asked here, once, because this is the gesture. See the
                    // field's own note.
                    opens_in_editor: !is_dir
                        && crate::file_view::path_opens_in_an_editor(&menu_path),
                    position,
                });
                cx.stop_propagation();
                cx.notify();
            }
        }));

        // Whole row toggles a directory or opens a file.
        let click_path = path.clone();
        el = el.on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
            this.files_focus.focus(window, cx);
            this.select_files_row(&click_path);
            if is_dir {
                this.toggle_dir(&click_path, cx);
            } else {
                this.open_file_in_active_pane(click_path.clone(), window, cx);
            }
            cx.stop_propagation();
        }));

        // Every file row drags into a pane, the way every file row now opens
        // into one. The ghost reuses the shared tab-drag preview.
        if !is_dir {
            let drag = MarkdownFileDrag {
                path: path.clone(),
                title: name.clone(),
                icon: SharedString::from("icons/file-text.svg"),
            };
            el = el.on_drag(drag, |drag, _offset, _window, cx| {
                cx.new(|_| TabDragPreview {
                    title: drag.title.clone(),
                    icon: drag.icon.clone(),
                })
            });
        }

        let el = el
            .child(chevron)
            .child(
                svg()
                    .size(px(12.))
                    .flex_none()
                    .path(icon)
                    .text_color(if is_dir { ui.muted } else { ui.accent }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    // A file name is a path, and the design sets the tree in
                    // mono for the same reason the rail is in mono.
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::BODY)
                    .text_color(text_color)
                    .overflow_x_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .child(name),
            )
            // The design's git mark: `M` for a file that differs from HEAD, `A`
            // for one the repository does not have yet. Files only - a folder's
            // mark would have to mean "something under here changed", which is
            // a different claim and one the design does not make.
            .children(git_mark.map(|mark| {
                div()
                    .flex_none()
                    .font_family(tok::font::MONO)
                    .text_size(tok::mono::LABEL)
                    .text_color(match mark {
                        crate::workspace::GitMark::Added => ui.vc_added,
                        crate::workspace::GitMark::Modified => ui.vc_modified,
                    })
                    .child(mark.letter())
            }));

        {
            let hover_background = crate::app::constants::sidebar_tab_hover_background();
            let resting_background = if selected {
                crate::app::constants::sidebar_tab_active_background()
            } else {
                hover_background.opacity(0.0)
            };
            el.animated_hover(move |style, delta| {
                style.bg(lerp_color(resting_background, hover_background, delta));
            })
            .into_any_element()
        }
    }
}

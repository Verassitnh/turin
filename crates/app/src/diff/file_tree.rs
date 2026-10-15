//! Port of upstream/apps/web/src/components/diffs/DiffFileTree.tsx.
//!
//! Upstream renders `@pierre/trees` (compact density, everything expanded,
//! single-child directories flattened, git status per file). This draws the
//! same rows with GPUI: 24px rows, 12px sans text, chevron + icon + name, and
//! the status letter in the 12px git lane.

use std::collections::HashSet;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, Context, CursorStyle, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, StatefulInteractiveElement as _, Styled as _, Window, div, px, rems,
};

use super::diff_file_tree_logic::{
    DiffFileTreeEntry, GitStatus, TreeRow, collect_directory_paths, flatten_visible_rows,
};
use super::panel::DiffPanel;
use crate::theme::color::{hex, with_alpha};
use crate::theme::tokens::{RADIUS_MD, TEXT_XS};
use crate::theme::{ActiveAppTheme, AppTheme};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

/// Tree-local state: directories the reader closed (`initialExpansion: "open"`).
#[derive(Default)]
pub struct TreeState {
    pub collapsed_dirs: HashSet<String>,
    pub known_file_keys: HashSet<String>,
    pub selected: Option<String>,
    /// The keyboard cursor (any row, directories included).
    pub cursor: Option<String>,
}

/// `--trees-item-padding-x` at compact density (8px × 0.8).
const TREE_ROW_PADDING: f32 = 6.4;

/// Left edge of a row's chevron/icon box at `depth`. `@pierre/trees` adds one
/// spacing item per level (border 1px + `--trees-level-gap` 6.4px − 1px), and
/// every item after the first is led in by `--trees-item-row-gap` 4.8px +
/// `icon/2` − .5px. The spacing section's own 7.5px lead-in and its 4.8px gap
/// to the icon cancel out, so each level is 6.4 + 4.8 + 8 − .5 = 18.7px.
fn row_indent(depth: usize) -> f32 {
    const LEVEL_STEP: f32 = 6.4 + 4.8 + 8. - 0.5;
    TREE_ROW_PADDING + depth as f32 * LEVEL_STEP
}

/// `--trees-git-*-color` light/dark.
fn status_color(status: GitStatus, dark: bool) -> Hsla {
    match (status, dark) {
        (GitStatus::Added, false) => hex(0x16a994),
        (GitStatus::Added, true) => hex(0x00cab1),
        (GitStatus::Modified, false) => hex(0x1ca1c7),
        (GitStatus::Modified, true) => hex(0x08c0ef),
        (GitStatus::Renamed, false) => hex(0xd5a910),
        (GitStatus::Renamed, true) => hex(0xffd452),
        (GitStatus::Deleted, false) => hex(0xff2e3f),
        (GitStatus::Deleted, true) => hex(0xff6762),
    }
    .into()
}

fn status_letter(status: GitStatus) -> &'static str {
    match status {
        GitStatus::Added => "A",
        GitStatus::Modified => "M",
        GitStatus::Renamed => "R",
        GitStatus::Deleted => "D",
    }
}

/// `--trees-file-icon-color-*`: the icon tint by language.
fn file_icon_color(path: &str, dark: bool) -> Hsla {
    let pick = |light: u32, dark_value: u32| hex(if dark { dark_value } else { light });
    let name = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let extension = name.rsplit_once('.').map_or("", |(_, ext)| ext);
    match extension {
        "ts" | "tsx" | "mts" | "cts" => pick(0x1a85d4, 0x69b1ff),
        "js" | "jsx" | "mjs" | "cjs" => pick(0xd5a910, 0xffd452),
        "rs" | "swift" | "svg" => pick(0xd47628, 0xffa359),
        "py" | "c" | "cpp" | "h" => pick(0x1a85d4, 0x69b1ff),
        "md" | "mdx" | "sh" => pick(0x199f43, 0x5ecc71),
        "json" | "html" => pick(0xd47628, 0xffa359),
        "css" | "scss" => pick(0x693acf, 0x9d6afb),
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "ico" => pick(0xd32a61, 0xff678d),
        "yml" | "yaml" | "rb" => pick(0xd52c36, 0xff6762),
        "go" => pick(0x1ca1c7, 0x68cdf2),
        _ => pick(0x84848a, 0xadadb1),
    }
    .into()
}

impl DiffPanel {
    fn visible_tree_rows(&self) -> Vec<TreeRow> {
        let paths: Vec<String> = self
            .derived
            .tree_entries
            .iter()
            .map(|entry| entry.path.clone())
            .collect();
        flatten_visible_rows(&paths)
            .into_iter()
            .filter(|row| {
                // A row hides when an ancestor directory is closed. Flattened
                // directory ids carry every merged segment, so closing the
                // merged row hides what is below its id.
                !self
                    .tree
                    .collapsed_dirs
                    .iter()
                    .any(|closed| row.path != *closed && row.path.starts_with(closed.as_str()))
            })
            .collect()
    }

    fn all_directories_expanded(&self) -> bool {
        self.tree.collapsed_dirs.is_empty()
    }

    pub(super) fn toggle_all_directories(&mut self, cx: &mut Context<Self>) {
        if self.all_directories_expanded() {
            let paths: Vec<String> = self
                .derived
                .tree_entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect();
            // Only directories that appear as rows can close.
            self.tree.collapsed_dirs = flatten_visible_rows(&paths)
                .into_iter()
                .filter(|row| row.is_directory)
                .map(|row| row.path)
                .collect();
            let _ = collect_directory_paths;
        } else {
            self.tree.collapsed_dirs.clear();
        }
        cx.notify();
    }

    /// Arrow-key navigation of the tree: up/down move the cursor, left/right
    /// close/open a directory (or step to its parent/first child), enter and
    /// space activate the row.
    pub(super) fn tree_key_down(&mut self, key: &str, cx: &mut Context<Self>) {
        let rows = self.visible_tree_rows();
        if rows.is_empty() {
            return;
        }
        let current = self
            .tree
            .cursor
            .clone()
            .or_else(|| self.tree.selected.clone())
            .and_then(|path| rows.iter().position(|row| row.path == path));
        let move_to = |index: usize| rows[index.min(rows.len() - 1)].path.clone();
        let next = match key {
            "down" => Some(move_to(current.map_or(0, |i| i + 1))),
            "up" => Some(move_to(current.map_or(0, |i| i.saturating_sub(1)))),
            "home" => Some(move_to(0)),
            "end" => Some(move_to(rows.len() - 1)),
            "right" | "left" | "enter" | "space" => None,
            _ => return,
        };
        if let Some(path) = next {
            self.tree.cursor = Some(path);
            cx.notify();
            return;
        }
        let Some(index) = current else {
            self.tree.cursor = Some(move_to(0));
            cx.notify();
            return;
        };
        let row = rows[index].clone();
        match key {
            "left" if row.is_directory && !self.tree.collapsed_dirs.contains(&row.path) => {
                self.tree.collapsed_dirs.insert(row.path);
            }
            "left" => {
                // The parent directory: the nearest row above that is shallower.
                if let Some(parent) = rows[..index]
                    .iter()
                    .rev()
                    .find(|candidate| candidate.is_directory && candidate.depth < row.depth)
                {
                    self.tree.cursor = Some(parent.path.clone());
                }
            }
            "right" if row.is_directory && self.tree.collapsed_dirs.contains(&row.path) => {
                self.tree.collapsed_dirs.remove(&row.path);
            }
            "right" if row.is_directory => {
                if let Some(child) = rows.get(index + 1) {
                    self.tree.cursor = Some(child.path.clone());
                }
            }
            _ => {
                if row.is_directory {
                    if !self.tree.collapsed_dirs.remove(&row.path) {
                        self.tree.collapsed_dirs.insert(row.path);
                    }
                } else {
                    self.tree.selected = Some(row.path.clone());
                    self.reveal_file(&row.path, cx);
                }
            }
        }
        cx.notify();
    }

    /// `DiffFileTree`.
    pub(super) fn render_file_tree(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        let rows = self.visible_tree_rows();
        let has_directories = rows.iter().any(|row| row.is_directory);
        let expanded = self.all_directories_expanded();
        let selected = self
            .tree
            .cursor
            .clone()
            .or_else(|| self.tree.selected.clone())
            .or_else(|| self.derived.selected_file_path.clone());
        let entry_count = self.derived.tree_entries.len();
        let panel = cx.entity();
        let toggle_panel = panel.clone();

        let header = div()
            .flex()
            .h(rems(1.75))
            .min_h(rems(1.75))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .mb_3()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .px_1()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child("Files"),
            )
            .child(div().ml_auto().child(entry_count.to_string()))
            .when(has_directories, |this| {
                this.child(
                    Button::new("diff-tree-toggle-folders")
                        .variant(ButtonVariant::Ghost)
                        .size(ButtonSize::IconXs)
                        .tooltip(Tooltip::new(if expanded {
                            "Collapse all folders"
                        } else {
                            "Expand all folders"
                        }))
                        .child(
                            Icon::new(if expanded {
                                IconName::ChevronsDownUp
                            } else {
                                IconName::ChevronsUpDown
                            })
                            .size(rems(0.875)),
                        )
                        .on_click(move |_, _, cx| {
                            toggle_panel.update(cx, |panel, cx| panel.toggle_all_directories(cx));
                        }),
                )
            });

        let mut list = div()
            .id("diff-file-tree-rows")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(8.))
            .pb_2();
        for row in rows {
            list = list.child(self.tree_row(&row, selected.as_deref(), &theme, dark, &panel));
        }
        div()
            .id("diff-file-tree")
            .key_context("DiffFileTree")
            .track_focus(&self.tree_focus)
            .on_key_down(cx.listener(|this, event: &gpui_kit::KeyDownEvent, _, cx| {
                this.tree_key_down(&event.keystroke.key, cx);
                cx.stop_propagation();
            }))
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .bg(theme.background)
            .child(header)
            .child(list)
            .into_any_element()
    }

    fn tree_row(
        &self,
        row: &TreeRow,
        selected: Option<&str>,
        theme: &AppTheme,
        dark: bool,
        panel: &gpui_kit::Entity<DiffPanel>,
    ) -> AnyElement {
        let entry: Option<&DiffFileTreeEntry> = (!row.is_directory)
            .then(|| {
                self.derived
                    .tree_entries
                    .iter()
                    .find(|e| e.path == row.path)
            })
            .flatten();
        let status = entry.map(|e| e.status);
        let is_selected = selected == Some(row.path.as_str());
        let foreground = theme.foreground;
        let closed = row.is_directory && self.tree.collapsed_dirs.contains(&row.path);
        let name_color: Hsla = match status {
            Some(status) => status_color(status, dark),
            None => foreground,
        };
        let icon_color: Hsla = match (row.is_directory, status) {
            (true, _) => with_alpha(theme.muted_foreground, 0.9).into(),
            (false, Some(status)) => status_color(status, dark),
            (false, None) => file_icon_color(&row.path, dark),
        };
        let path = row.path.clone();
        let click_panel = panel.clone();
        let is_directory = row.is_directory;
        div()
            .id(gpui_kit::ElementId::Name(
                format!("diff-tree:{}", row.path).into(),
            ))
            .flex()
            .flex_shrink_0()
            .h(px(24.))
            .items_center()
            .gap(px(4.8))
            .pl(px(row_indent(row.depth)))
            .pr(px(6.4))
            .rounded(RADIUS_MD)
            .cursor(CursorStyle::PointingHand)
            .text_size(TEXT_XS.size)
            .when(is_selected, |this| this.bg(with_alpha(foreground, 0.12)))
            .when(!is_selected, |this| {
                this.hover(move |style| style.bg(with_alpha(foreground, 0.07)))
            })
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .w(px(16.))
                    .justify_center()
                    .when(is_directory, |this| {
                        this.child(
                            Icon::new(if closed {
                                IconName::ChevronRight
                            } else {
                                IconName::ChevronDown
                            })
                            .size(px_rems(12.))
                            .color(theme.muted_foreground),
                        )
                    }),
            )
            .child(
                Icon::new(if is_directory {
                    if closed {
                        IconName::Folder
                    } else {
                        IconName::FolderOpen
                    }
                } else {
                    IconName::FileText
                })
                .size(rems(0.875))
                .color(icon_color),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .text_color(if is_directory { foreground } else { name_color })
                    .child(row.label.clone()),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .w(px(12.))
                    .justify_center()
                    .font_weight(FontWeight::SEMIBOLD)
                    .when_some(status, |this, status| {
                        this.text_color(status_color(status, dark))
                            .child(status_letter(status))
                    }),
            )
            .on_click(move |_, window, cx| {
                let path = path.clone();
                click_panel.update(cx, |panel, cx| {
                    window.focus(&panel.tree_focus, cx);
                    panel.tree.cursor = Some(path.clone());
                    if is_directory {
                        if !panel.tree.collapsed_dirs.remove(&path) {
                            panel.tree.collapsed_dirs.insert(path);
                        }
                        cx.notify();
                    } else {
                        panel.tree.selected = Some(path.clone());
                        panel.reveal_file(&path, cx);
                    }
                });
            })
            .into_any_element()
    }
}

fn px_rems(pixels: f32) -> gpui_kit::Rems {
    rems(pixels / 16.)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_level_steps_18_7px_like_pierre_compact_density() {
        assert!((row_indent(0) - 6.4).abs() < 1e-4);
        assert!((row_indent(1) - 25.1).abs() < 1e-4);
        assert!((row_indent(3) - 62.5).abs() < 1e-4);
    }
}

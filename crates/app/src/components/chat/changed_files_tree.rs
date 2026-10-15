//! Port of upstream/apps/web/src/components/chat/ChangedFilesTree.tsx
//! (`ChangedFilesCard`, `ChangedFilesTree`), DiffStatLabel.tsx and
//! upstream/apps/web/src/lib/turnDiffTree.ts: the per-turn changed files
//! summary under an assistant reply.
//!
//! Pierre file icons are not ported yet; files show a neutral file glyph.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled as _,
    Window, div, px, rems,
};

use super::timeline::turn_item::CheckpointFileSummary;
use crate::theme::ActiveAppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::{RADIUS_LG, RADIUS_MD, TEXT_2XS, TEXT_3XS, TEXT_XS};
use crate::ui::{Button, ButtonSize, ButtonVariant, Icon, IconName, Tooltip};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TurnDiffStat {
    pub additions: u64,
    pub deletions: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TurnDiffTreeNode {
    Directory {
        name: String,
        path: String,
        stat: TurnDiffStat,
        children: Vec<TurnDiffTreeNode>,
    },
    File {
        name: String,
        path: String,
        stat: Option<TurnDiffStat>,
    },
}

impl TurnDiffTreeNode {
    fn name(&self) -> &str {
        match self {
            Self::Directory { name, .. } | Self::File { name, .. } => name,
        }
    }
}

#[derive(Default)]
struct MutableDirectory {
    name: String,
    path: String,
    stat: TurnDiffStat,
    directories: BTreeMap<String, MutableDirectory>,
    files: Vec<TurnDiffTreeNode>,
}

/// `localeCompare(…, { numeric: true, sensitivity: "base" })`, approximately.
fn compare_names(a: &str, b: &str) -> std::cmp::Ordering {
    let key = |s: &str| {
        let mut parts: Vec<(bool, String)> = Vec::new();
        for c in s.chars() {
            let digit = c.is_ascii_digit();
            match parts.last_mut() {
                Some((was_digit, part)) if *was_digit == digit => part.push(c),
                _ => parts.push((digit, c.to_string())),
            }
        }
        parts
    };
    let (ka, kb) = (key(a), key(b));
    for ((da, pa), (db, pb)) in ka.iter().zip(kb.iter()) {
        let ordering = if *da && *db {
            pa.parse::<u128>()
                .unwrap_or(0)
                .cmp(&pb.parse::<u128>().unwrap_or(0))
        } else {
            pa.to_lowercase().cmp(&pb.to_lowercase())
        };
        if ordering != std::cmp::Ordering::Equal {
            return ordering;
        }
    }
    ka.len().cmp(&kb.len())
}

fn compact_directory(node: TurnDiffTreeNode) -> TurnDiffTreeNode {
    let TurnDiffTreeNode::Directory {
        name,
        path,
        stat,
        children,
    } = node
    else {
        return node;
    };
    let children: Vec<_> = children.into_iter().map(compact_directory).collect();
    let mut compacted = (name, path, stat, children);
    while compacted.3.len() == 1 && matches!(compacted.3[0], TurnDiffTreeNode::Directory { .. }) {
        let Some(TurnDiffTreeNode::Directory {
            name: child_name,
            path: child_path,
            stat: child_stat,
            children: grandchildren,
        }) = compacted.3.pop()
        else {
            break;
        };
        compacted = (
            format!("{}/{}", compacted.0, child_name),
            child_path,
            child_stat,
            grandchildren,
        );
    }
    TurnDiffTreeNode::Directory {
        name: compacted.0,
        path: compacted.1,
        stat: compacted.2,
        children: compacted.3,
    }
}

fn to_tree_nodes(directory: MutableDirectory) -> Vec<TurnDiffTreeNode> {
    let mut subdirectories: Vec<MutableDirectory> = directory.directories.into_values().collect();
    subdirectories.sort_by(|a, b| compare_names(&a.name, &b.name));
    let mut nodes: Vec<TurnDiffTreeNode> = subdirectories
        .into_iter()
        .map(|sub| {
            let name = sub.name.clone();
            let path = sub.path.clone();
            let stat = sub.stat;
            compact_directory(TurnDiffTreeNode::Directory {
                name,
                path,
                stat,
                children: to_tree_nodes(sub),
            })
        })
        .collect();
    let mut files = directory.files;
    files.sort_by(|a, b| compare_names(a.name(), b.name()));
    nodes.extend(files);
    nodes
}

/// `summarizeTurnDiffStats`.
pub fn summarize_turn_diff_stats(files: &[CheckpointFileSummary]) -> TurnDiffStat {
    files
        .iter()
        .fold(TurnDiffStat::default(), |acc, file| TurnDiffStat {
            additions: acc.additions + file.additions,
            deletions: acc.deletions + file.deletions,
        })
}

/// `buildTurnDiffTree`.
pub fn build_turn_diff_tree(files: &[CheckpointFileSummary]) -> Vec<TurnDiffTreeNode> {
    let mut root = MutableDirectory::default();
    for file in files {
        let normalized = file.path.replace('\\', "/");
        let segments: Vec<&str> = normalized.split('/').filter(|s| !s.is_empty()).collect();
        let Some((file_name, dirs)) = segments.split_last() else {
            continue;
        };
        let stat = TurnDiffStat {
            additions: file.additions,
            deletions: file.deletions,
        };
        root.stat.additions += stat.additions;
        root.stat.deletions += stat.deletions;
        let mut current = &mut root;
        for segment in dirs {
            let next_path = if current.path.is_empty() {
                segment.to_string()
            } else {
                format!("{}/{}", current.path, segment)
            };
            current = current
                .directories
                .entry(segment.to_string())
                .or_insert_with(|| MutableDirectory {
                    name: segment.to_string(),
                    path: next_path,
                    ..Default::default()
                });
            current.stat.additions += stat.additions;
            current.stat.deletions += stat.deletions;
        }
        current.files.push(TurnDiffTreeNode::File {
            name: file_name.to_string(),
            path: segments.join("/"),
            stat: Some(stat),
        });
    }
    to_tree_nodes(root)
}

/// `formatCompactDiffCount`.
pub fn format_compact_diff_count(value: u64) -> String {
    let compact = |v: f64, suffix: &str| {
        if v < 10. {
            let s = format!("{v:.1}");
            format!("{}{suffix}", s.strip_suffix(".0").unwrap_or(&s))
        } else {
            format!("{}{suffix}", v.round() as u64)
        }
    };
    match value {
        0..=999 => value.to_string(),
        1_000..=999_999 => compact(value as f64 / 1_000., "k"),
        1_000_000..=999_999_999 => compact(value as f64 / 1_000_000., "m"),
        _ => compact(value as f64 / 1_000_000_000., "b"),
    }
}

/// `DiffStatLabel` (`inline` layout).
pub fn diff_stat_label(stat: TurnDiffStat, cx: &App) -> AnyElement {
    let theme = cx.app_theme();
    div()
        .flex()
        .items_center()
        .gap(rems(0.25))
        .font_family(theme.fonts.mono.clone())
        .child(
            div()
                .text_color(theme.diff_addition_foreground)
                .child(format!("+{}", format_compact_diff_count(stat.additions))),
        )
        .child(
            div()
                .text_color(theme.diff_deletion_foreground)
                .child(format!("-{}", format_compact_diff_count(stat.deletions))),
        )
        .into_any_element()
}

type OpenDiff = Rc<dyn Fn(&str, Option<&str>, &mut Window, &mut App)>;

/// `ChangedFilesCard`.
#[derive(IntoElement)]
pub struct ChangedFilesCard {
    pub id: ElementId,
    pub run_id: SharedString,
    pub files: Arc<Vec<CheckpointFileSummary>>,
    pub on_open_turn_diff: OpenDiff,
}

struct ChangedFilesState {
    all_expanded: bool,
    overrides: HashMap<String, bool>,
}

impl RenderOnce for ChangedFilesCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme().clone();
        let dark = theme.is_dark();
        let state = window.use_keyed_state(
            ElementId::NamedChild(Arc::new(self.id.clone()), "state".into()),
            cx,
            |_, _| ChangedFilesState {
                all_expanded: false,
                overrides: HashMap::new(),
            },
        );
        let summary = summarize_turn_diff_stats(&self.files);
        let has_directories = self.files.iter().any(|f| f.path.contains(['/', '\\']));
        let all_expanded = state.read(cx).all_expanded;
        let bg = if dark {
            with_alpha(theme.input, 0.2).into()
        } else {
            theme.secondary
        };
        let count = self.files.len();
        let toggle_state = state.clone();
        let open = self.on_open_turn_diff.clone();
        let run_id = self.run_id.clone();
        let first_path = self.files.first().map(|f| f.path.clone());
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(rems(0.5))
            .rounded_t(RADIUS_LG)
            .px(rems(0.75))
            .py(rems(0.5))
            .child(
                div()
                    .flex()
                    .min_w_0()
                    .flex_wrap()
                    .items_center()
                    .gap_x(rems(0.75))
                    .text_size(TEXT_XS.size)
                    .line_height(rems(1.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme.foreground)
                    .child(format!(
                        "{count} changed file{}",
                        if count == 1 { "" } else { "s" }
                    ))
                    .when(summary.additions > 0 || summary.deletions > 0, |this| {
                        this.child(diff_stat_label(summary, cx))
                    }),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap(rems(0.25))
                    .when(has_directories, |this| {
                        let label = if all_expanded {
                            "Collapse all folders"
                        } else {
                            "Expand all folders"
                        };
                        this.child(
                            Button::new(ElementId::NamedChild(
                                Arc::new(self.id.clone()),
                                "toggle".into(),
                            ))
                            .variant(ButtonVariant::GhostMuted)
                            .size(ButtonSize::IconXs)
                            .tooltip(Tooltip::new(label))
                            .child(
                                Icon::new(if all_expanded {
                                    IconName::ChevronsDownUp
                                } else {
                                    IconName::ChevronsUpDown
                                })
                                .size(rems(0.75)),
                            )
                            .on_click(move |_, _, cx| {
                                toggle_state.update(cx, |state, cx| {
                                    state.all_expanded = !state.all_expanded;
                                    state.overrides.clear();
                                    cx.notify();
                                });
                            }),
                        )
                    })
                    .child(
                        Button::new(ElementId::NamedChild(
                            Arc::new(self.id.clone()),
                            "open".into(),
                        ))
                        .variant(ButtonVariant::GhostMuted)
                        .size(ButtonSize::Xs)
                        .tooltip(Tooltip::new("Open the full diff"))
                        .child(Icon::new(IconName::FileDiff).size(rems(0.75)))
                        .child("Open diff")
                        .on_click(move |_, window, cx| {
                            open(&run_id, first_path.as_deref(), window, cx)
                        }),
                    ),
            );
        let nodes = build_turn_diff_tree(&self.files);
        let has_directory_nodes = nodes
            .iter()
            .any(|n| matches!(n, TurnDiffTreeNode::Directory { .. }));
        let overrides = state.read(cx).overrides.clone();
        let mut tree = div().p(rems(0.5));
        for node in &nodes {
            tree = tree.child(render_node(
                &self,
                node,
                0,
                all_expanded,
                &overrides,
                has_directory_nodes,
                &state,
                cx,
            ));
        }
        div()
            .mt(rems(1.))
            .rounded(RADIUS_LG)
            .bg(bg)
            .child(header)
            .child(tree)
    }
}

#[allow(clippy::too_many_arguments)]
fn render_node(
    card: &ChangedFilesCard,
    node: &TurnDiffTreeNode,
    depth: usize,
    all_expanded: bool,
    overrides: &HashMap<String, bool>,
    has_directory_nodes: bool,
    state: &gpui_kit::Entity<ChangedFilesState>,
    cx: &App,
) -> AnyElement {
    let theme = cx.app_theme();
    let left = px(8. + depth as f32 * 14.);
    let row = |id: String| {
        div()
            .id(ElementId::NamedChild(Arc::new(card.id.clone()), id.into()))
            .group("changed-file")
            .flex()
            .w_full()
            .items_center()
            .gap(rems(0.5))
            .rounded(RADIUS_MD)
            .py(rems(0.375))
            .pr(rems(0.5))
            .pl(left)
            .cursor_pointer()
            .hover(|s| s.bg(with_alpha(theme.accent, 0.6)))
    };
    match node {
        TurnDiffTreeNode::Directory {
            name,
            path,
            stat,
            children,
        } => {
            let expanded = overrides.get(path).copied().unwrap_or(all_expanded);
            let toggle = state.clone();
            let toggle_path = path.clone();
            let header = row(format!("dir:{path}"))
                .child(
                    Icon::new(if expanded {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronRight
                    })
                    .size(rems(0.875))
                    .color(with_alpha(theme.muted_foreground, 0.7).into()),
                )
                .child(
                    Icon::new(if expanded {
                        IconName::FolderOpen
                    } else {
                        IconName::Folder
                    })
                    .size(rems(0.875))
                    .color(with_alpha(theme.muted_foreground, 0.75).into()),
                )
                .child(
                    div()
                        .truncate()
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_2XS.size)
                        .line_height(TEXT_2XS.line_height)
                        .text_color(with_alpha(theme.muted_foreground, 0.9))
                        .child(name.clone()),
                )
                .when(stat.additions > 0 || stat.deletions > 0, |this| {
                    this.child(
                        div()
                            .ml_auto()
                            .flex_shrink_0()
                            .text_size(TEXT_3XS.size)
                            .child(diff_stat_label(*stat, cx)),
                    )
                })
                .on_click(move |_, _, cx| {
                    let path = toggle_path.clone();
                    toggle.update(cx, |state, cx| {
                        let current = state
                            .overrides
                            .get(&path)
                            .copied()
                            .unwrap_or(state.all_expanded);
                        state.overrides.insert(path, !current);
                        cx.notify();
                    });
                });
            let mut column = div().child(header);
            if expanded {
                for child in children {
                    column = column.child(render_node(
                        card,
                        child,
                        depth + 1,
                        all_expanded,
                        overrides,
                        has_directory_nodes,
                        state,
                        cx,
                    ));
                }
            }
            column.into_any_element()
        }
        TurnDiffTreeNode::File { name, path, stat } => {
            let open = card.on_open_turn_diff.clone();
            let run_id = card.run_id.clone();
            let file_path = path.clone();
            row(format!("file:{path}"))
                .when(has_directory_nodes || depth > 0, |this| {
                    this.child(div().size(rems(0.875)).flex_shrink_0())
                })
                .child(
                    Icon::new(IconName::FileText)
                        .size(rems(0.875))
                        .color(with_alpha(theme.muted_foreground, 0.7).into()),
                )
                .child(
                    div()
                        .min_w_0()
                        .truncate()
                        .font_family(theme.fonts.mono.clone())
                        .text_size(TEXT_XS.size)
                        .line_height(TEXT_XS.line_height)
                        .text_color(with_alpha(theme.foreground, 0.85))
                        .child(name.clone()),
                )
                .when_some(*stat, |this, stat| {
                    this.child(
                        div()
                            .ml_auto()
                            .flex_shrink_0()
                            .text_size(TEXT_3XS.size)
                            .child(diff_stat_label(stat, cx)),
                    )
                })
                .on_click(move |_, window, cx| open(&run_id, Some(&file_path), window, cx))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, additions: u64, deletions: u64) -> CheckpointFileSummary {
        CheckpointFileSummary {
            path: path.into(),
            kind: "modified".into(),
            additions,
            deletions,
        }
    }

    #[test]
    fn builds_compacted_sorted_tree() {
        let tree = build_turn_diff_tree(&[
            file("src/a/b/c.ts", 1, 2),
            file("src/a/b/d.ts", 3, 0),
            file("README.md", 1, 1),
        ]);
        assert_eq!(tree.len(), 2);
        match &tree[0] {
            TurnDiffTreeNode::Directory {
                name,
                stat,
                children,
                ..
            } => {
                assert_eq!(name, "src/a/b");
                assert_eq!(
                    *stat,
                    TurnDiffStat {
                        additions: 4,
                        deletions: 2
                    }
                );
                assert_eq!(children.len(), 2);
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(&tree[1], TurnDiffTreeNode::File { name, .. } if name == "README.md"));
    }

    #[test]
    fn numeric_name_order() {
        assert_eq!(compare_names("file2", "file10"), std::cmp::Ordering::Less);
    }

    #[test]
    fn compact_counts() {
        assert_eq!(format_compact_diff_count(999), "999");
        assert_eq!(format_compact_diff_count(1200), "1.2k");
        assert_eq!(format_compact_diff_count(1000), "1k");
        assert_eq!(format_compact_diff_count(25_000), "25k");
        assert_eq!(format_compact_diff_count(3_400_000), "3.4m");
    }
}

//! Rendering of the Code tab: the toolbar, the list rows and the states around them.

use super::*;

/// The code face and the line metrics the rows share.
#[derive(Clone)]
struct Metrics {
    font: SharedString,
    size: Pixels,
    line: Pixels,
}

use gpui_kit::Pixels;

fn digits(value: i64) -> usize {
    value.max(1).to_string().len()
}

/// The gutter holds the widest line number of the file, plus pierre's padding.
fn gutter_width(view: &FileView, metrics: &Metrics) -> Pixels {
    let widest = view
        .rows
        .iter()
        .flat_map(|row| [row.old_line, row.new_line])
        .flatten()
        .max()
        .unwrap_or(1);
    let columns = digits(widest).max(3);
    px(columns as f32 * f32::from(metrics.size) * 0.62 + 26.)
}

impl CodeTab {
    fn metrics(&self, theme: &AppTheme) -> Metrics {
        let size = theme.fonts.size_code;
        Metrics {
            font: SharedString::from(theme.fonts.mono.clone()),
            size: px(size),
            line: px((size * 1.5).round().max(20.)),
        }
    }

    fn tokens_for(&self, file: usize, view: &FileView, dark: bool) -> Rc<Vec<LineTokens>> {
        let key = format!("{file}:{}", view.key);
        if let Some(tokens) = self.tokens.borrow().get(&key) {
            return tokens.clone();
        }
        let tokens = Rc::new(row_tokens(view, dark));
        self.tokens.borrow_mut().insert(key, tokens.clone());
        tokens
    }

    /// Whether the viewer may start a line comment on this diff.
    fn can_comment_on_lines(&self, cx: &App) -> bool {
        // A comment is posted against the pull request's head diff, so a line number taken from
        // one commit's own diff would land somewhere else entirely.
        if self.commit.is_some() || self.draft.is_some() {
            return false;
        }
        let Some(snapshot) = self.ctx.snapshot(cx) else {
            return false;
        };
        let Loadable::Ready(detail) = &snapshot.detail else {
            return false;
        };
        detail.capabilities.review.inline_comment && detail.viewer_permissions.comment
    }

    // ── Rows ──

    #[allow(clippy::too_many_arguments)]
    fn gutter(
        &self,
        ix: usize,
        half: &str,
        number: Option<i64>,
        kind: DiffRowKind,
        width: Pixels,
        comment: Option<(usize, usize, PullRequestDiffSide)>,
        metrics: &Metrics,
        palette: &Palette,
        theme: &AppTheme,
        group: &SharedString,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let bar = palette.bar(kind);
        div()
            .relative()
            .flex()
            .flex_shrink_0()
            .w(width)
            .justify_end()
            .pr(px(12.))
            .border_l(px(4.))
            .border_color(bar.unwrap_or(gpui_kit::transparent_black()))
            .bg(palette.gutter(kind).unwrap_or(palette.bg))
            .font_family(metrics.font.clone())
            .text_size(metrics.size)
            .line_height(metrics.line)
            .text_color(palette.gutter_fg)
            .children(number.map(|number| SharedString::from(number.to_string())))
            .when_some(comment, |this, (file, row, side)| {
                // The gutter's utility button: comment on this line.
                this.child(
                    div()
                        .id(SharedString::from(format!("comment-{ix}-{half}")))
                        .absolute()
                        .left(px(6.))
                        .top(px(2.))
                        .flex()
                        .size(px(16.))
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(theme.primary)
                        .cursor_pointer()
                        .opacity(0.)
                        .group_hover(group.clone(), |style| style.opacity(1.))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.begin_comment(file, row, side, window, cx);
                        }))
                        .child(
                            Icon::new(IconName::Plus)
                                .size(rems(0.75))
                                .color(theme.primary_foreground),
                        ),
                )
            })
            .into_any_element()
    }

    fn code_cell(
        &self,
        text: &str,
        tokens: &[(Range<usize>, Hsla)],
        metrics: &Metrics,
        palette: &Palette,
    ) -> AnyElement {
        div()
            .flex_1()
            .min_w_0()
            .px(px(8.))
            .font_family(metrics.font.clone())
            .text_size(metrics.size)
            .line_height(metrics.line)
            .when(!self.wrap, |this| {
                this.whitespace_nowrap().overflow_hidden()
            })
            .child(code_text(text, tokens, palette.fg))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn unified_row(
        &self,
        ix: usize,
        file: usize,
        view: &FileView,
        row_ix: usize,
        width: Pixels,
        metrics: &Metrics,
        palette: &Palette,
        theme: &AppTheme,
        can_comment: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = &view.rows[row_ix];
        let tokens = self.tokens_for(file, view, theme.is_dark());
        let group: SharedString = format!("diff-row-{ix}").into();
        let side = if row.kind == DiffRowKind::Deletion {
            PullRequestDiffSide::Left
        } else {
            PullRequestDiffSide::Right
        };
        let number = if row.kind == DiffRowKind::Deletion {
            row.old_line
        } else {
            row.new_line
        };
        let gutter = self.gutter(
            ix,
            "u",
            number,
            row.kind,
            width,
            can_comment.then_some((file, row_ix, side)),
            metrics,
            palette,
            theme,
            &group,
            cx,
        );
        div()
            .id(SharedString::from(group.to_string()))
            .group(group)
            .flex()
            .w_full()
            .min_h(metrics.line)
            .bg(palette.line(row.kind).unwrap_or(palette.bg))
            .child(gutter)
            .child(self.code_cell(
                row.text.trim_end_matches(['\n', '\r']),
                tokens.get(row_ix).map_or(&[][..], Vec::as_slice),
                metrics,
                palette,
            ))
            .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn split_row(
        &self,
        ix: usize,
        file: usize,
        view: &FileView,
        left: Option<usize>,
        right: Option<usize>,
        width: Pixels,
        metrics: &Metrics,
        palette: &Palette,
        theme: &AppTheme,
        can_comment: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let tokens = self.tokens_for(file, view, theme.is_dark());
        let group: SharedString = format!("diff-row-{ix}").into();
        let half = |index: Option<usize>,
                    side: PullRequestDiffSide,
                    name: &str,
                    cx: &mut Context<Self>| {
            let (kind, number, text, row_tokens) = match index {
                Some(row_ix) => {
                    let row = &view.rows[row_ix];
                    (
                        row.kind,
                        match side {
                            PullRequestDiffSide::Left => row.old_line,
                            PullRequestDiffSide::Right => row.new_line,
                        },
                        row.text.trim_end_matches(['\n', '\r']).to_owned(),
                        tokens.get(row_ix).cloned().unwrap_or_default(),
                    )
                }
                None => (DiffRowKind::Context, None, String::new(), Vec::new()),
            };
            let empty = index.is_none();
            div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h(metrics.line)
                .bg(if empty {
                    palette.separator_bg
                } else {
                    palette.line(kind).unwrap_or(palette.bg)
                })
                .child(
                    self.gutter(
                        ix,
                        name,
                        number,
                        if empty { DiffRowKind::Context } else { kind },
                        width,
                        index
                            .filter(|_| can_comment)
                            .map(|row_ix| (file, row_ix, side)),
                        metrics,
                        palette,
                        theme,
                        &group,
                        cx,
                    ),
                )
                .child(self.code_cell(&text, &row_tokens, metrics, palette))
        };
        let left = half(left, PullRequestDiffSide::Left, "l", cx);
        let right = half(right, PullRequestDiffSide::Right, "r", cx);
        div()
            .id(SharedString::from(group.to_string()))
            .group(group)
            .flex()
            .w_full()
            .child(left)
            .child(
                div()
                    .w(px(1.))
                    .flex_shrink_0()
                    .bg(with_alpha(theme.border, 0.6)),
            )
            .child(right)
            .into_any_element()
    }

    /// `[data-diffs-header]`: sticky upstream; here a plain row.
    #[allow(clippy::too_many_arguments)]
    fn file_header(
        &self,
        ix: usize,
        view: &FileView,
        omitted: Option<(f64, f64)>,
        enabled_viewed: bool,
        palette: &Palette,
        theme: &AppTheme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let collapsed = self.collapsed(&view.key);
        let key = view.key.clone();
        let path = view.path.clone();
        let (mut additions, mut deletions) = (view.additions, view.deletions);
        if additions == 0
            && deletions == 0
            && let Some((added, removed)) = omitted
        {
            additions = added as u64;
            deletions = removed as u64;
        }
        let viewed = self.viewed.is_viewed(&path);
        let stale = self.viewed.is_stale(&path);
        let group: SharedString = format!("diff-header-{ix}").into();
        let title = match (&view.file.prev_name, view.file.change) {
            (Some(previous), FileChange::RenamePure | FileChange::RenameChanged) => {
                format!("{previous} \u{2192} {}", view.path)
            }
            _ => view.path.clone(),
        };
        let toggle_key = key.clone();
        let viewed_key = key.clone();
        let viewed_path = path.clone();
        div()
            .id(SharedString::from(group.to_string()))
            .group(group.clone())
            .relative()
            .flex()
            .w_full()
            .min_h(px(32.))
            .items_center()
            .gap_2()
            .py(px(6.))
            .pl(px(8.))
            .pr(px(12.))
            .bg(palette.bg)
            .text_color(palette.fg)
            .font_family(SharedString::from(theme.fonts.sans.clone()))
            .text_size(px(12.))
            .line_height(relative(1.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.toggle_file(&toggle_key, cx)))
            // The hover cue: an inset edge on the left.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(3.))
                    .bg(with_alpha(palette.fg, 0.24))
                    .opacity(0.)
                    .group_hover(group, |style| style.opacity(1.)),
            )
            .child(
                div().mr_1().child(
                    Button::new(SharedString::from(format!("fold-{ix}")))
                        .size(ButtonSize::IconMicro)
                        .variant(ButtonVariant::GhostMuted)
                        .icon(if collapsed {
                            IconName::ChevronRight
                        } else {
                            IconName::ChevronDown
                        })
                        .icon_size(rems(1.))
                        .tooltip(Tooltip::new(if collapsed {
                            "Expand diff"
                        } else {
                            "Collapse diff"
                        }))
                        .on_click(cx.listener({
                            let key = key.clone();
                            move |this, _, _, cx| this.toggle_file(&key, cx)
                        })),
                ),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .truncate()
                    .child(SharedString::from(title)),
            )
            .child(
                div()
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_3()
                    .child(
                        div()
                            .text_size(TEXT_2XS.size)
                            .child(PullRequestDiffStat::new(additions, deletions).mono()),
                    )
                    .when(enabled_viewed, |this| {
                        this.child(
                            div()
                                .id(SharedString::from(format!("viewed-{ix}")))
                                .flex()
                                .items_center()
                                .gap(rems(0.375))
                                .text_size(TEXT_2XS.size)
                                .text_color(theme.muted_foreground)
                                .cursor_pointer()
                                // The header folds the file; the tick keeps its press to itself.
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    let next = !this.viewed.is_viewed(&viewed_path);
                                    this.set_file_viewed(&viewed_key, &viewed_path, next, cx);
                                }))
                                .child(checkbox(viewed, theme))
                                .child(if stale {
                                    with_tooltip(
                                        SharedString::from(format!("stale-{ix}")),
                                        "This file has been pushed to since you marked it viewed.",
                                        div().text_color(theme.warning_foreground).child("Changed"),
                                    )
                                    .into_any_element()
                                } else {
                                    div().child("Viewed").into_any_element()
                                }),
                        )
                    }),
            )
            .into_any_element()
    }

    fn separator_row(&self, lines: usize, width: Pixels, palette: &Palette) -> AnyElement {
        div()
            .flex()
            .w_full()
            .h(px(24.))
            .items_center()
            .pl(width)
            .px(px(8.))
            .bg(palette.separator_bg)
            .text_size(px(11.))
            .text_color(palette.separator_fg)
            .child(SharedString::from(format!(
                "{lines} unmodified {}",
                if lines == 1 { "line" } else { "lines" }
            )))
            .into_any_element()
    }

    fn thread_props(
        &self,
        snapshot: &PullRequestDetailSnapshot,
        thread: &PullRequestReviewThread,
    ) -> Option<ThreadCardProps> {
        let Loadable::Ready(detail) = &snapshot.detail else {
            return None;
        };
        let hostile = &detail.capabilities.review;
        let viewer = &detail.viewer_permissions;
        Some(ThreadCardProps {
            thread: thread.clone(),
            can_reply: hostile.reply && viewer.comment,
            can_resolve: hostile.resolve
                && can_resolve_pull_request_thread(viewer, thread.can_resolve),
            can_react: detail.capabilities.reactions == Some(true),
        })
    }

    pub(super) fn thread_card(
        &mut self,
        thread: &PullRequestReviewThread,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<ReviewThreadCard>> {
        let props = {
            let snapshot = self.ctx.snapshot(cx)?;
            self.thread_props(snapshot, thread)?
        };
        // Named with the pull request too: a thread's id is the host's own, and two pull requests
        // can hand out the same one.
        let key = format!(
            "{}#{}:{}",
            self.ctx.reference.project_id.as_str(),
            self.ctx.reference.number,
            thread.id.as_str()
        );
        let ctx = self.ctx.clone();
        let card = self
            .thread_cards
            .entry(key)
            .or_insert_with(|| cx.new(|cx| ReviewThreadCard::new(ctx, props.clone(), window, cx)))
            .clone();
        card.update(cx, |card, cx| card.set_props(props, cx));
        Some(card)
    }

    #[allow(clippy::too_many_arguments)]
    fn annotation(
        &mut self,
        file: usize,
        side: PullRequestDiffSide,
        line: i64,
        palette: &Palette,
        theme: &AppTheme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let group = self
            .groups
            .get(&(file, side, line))
            .cloned()
            .unwrap_or_default();
        let mut body = div()
            .w_full()
            .py_1()
            .bg(palette.bg)
            .font_family(SharedString::from(theme.fonts.sans.clone()))
            .text_color(theme.foreground);
        for thread in &group.threads {
            if let Some(card) = self.thread_card(thread, window, cx) {
                body = body.child(card);
            }
        }
        for comment in group.pending {
            let id = comment.id.clone();
            let entity = cx.entity().downgrade();
            body = body.child(PendingReviewCommentCard::new(comment, move |_, _, cx| {
                entity
                    .update(cx, |this, cx| this.remove_pending(&id, cx))
                    .ok();
            }));
        }
        if group.draft
            && let Some(draft) = &self.draft
        {
            body = body.child(draft.card.clone());
        }
        body.into_any_element()
    }

    fn footer_row(&self, cursor_error: bool, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let next = self
            .ctx
            .snapshot(cx)
            .and_then(|snapshot| snapshot.diffs.get(&self.commit))
            .and_then(Loadable::ready)
            .and_then(|result| {
                result
                    .next_cursor
                    .as_ref()
                    .map(|cursor| cursor.as_str().to_owned())
            });
        let _ = cursor_error;
        div()
            .flex()
            .w_full()
            .items_center()
            .justify_center()
            .gap_2()
            .py_2()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(
                Button::new("diff-load-more")
                    .size(ButtonSize::Xs)
                    .variant(ButtonVariant::Outline)
                    .label("Load more files")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let (Some(cursor), Some(source)) = (next.clone(), this.ctx.source(cx)) {
                            source.load_diff_page(
                                &this.ctx.reference,
                                this.commit.clone(),
                                &cursor,
                                cx,
                            );
                        }
                    })),
            )
            .into_any_element()
    }

    pub(super) fn render_item(
        &mut self,
        ix: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self.items.get(ix).cloned() else {
            return div().into_any_element();
        };
        let theme = cx.app_theme().clone();
        let palette = Palette::new(&theme);
        let metrics = self.metrics(&theme);
        let Some(files) = self.files() else {
            return div().into_any_element();
        };
        let can_comment = self.can_comment_on_lines(cx);
        match item {
            Item::FileHeader { file } => {
                let view = &files[file];
                let (enabled, omitted) = self
                    .ctx
                    .snapshot(cx)
                    .map(|snapshot| {
                        let omitted = snapshot
                            .diffs
                            .get(&self.commit)
                            .and_then(Loadable::ready)
                            .and_then(|result| result.omitted_file_stats.as_ref())
                            .and_then(|stats| {
                                stats
                                    .iter()
                                    .find(|stat| stat.path.as_str() == view.path)
                                    .map(|stat| (stat.additions, stat.deletions))
                            });
                        let enabled = matches!(&snapshot.detail, Loadable::Ready(detail) if detail.capabilities.viewed_files.is_some());
                        (enabled, omitted)
                    })
                    .unwrap_or((false, None));
                self.file_header(ix, view, omitted, enabled, &palette, &theme, cx)
            }
            Item::Binary { file } => div()
                .w_full()
                .px_4()
                .py_2()
                .bg(palette.bg)
                .text_size(TEXT_XS.size)
                .text_color(theme.muted_foreground)
                .child(if files[file].file.change == FileChange::New {
                    "Binary file added."
                } else {
                    "Binary file changed."
                })
                .into_any_element(),
            Item::Separator { file, lines } => {
                self.separator_row(lines, gutter_width(&files[file], &metrics), &palette)
            }
            Item::Line { file, row } => {
                let view = &files[file];
                self.unified_row(
                    ix,
                    file,
                    view,
                    row,
                    gutter_width(view, &metrics),
                    &metrics,
                    &palette,
                    &theme,
                    can_comment,
                    cx,
                )
            }
            Item::Split { file, left, right } => {
                let view = &files[file];
                self.split_row(
                    ix,
                    file,
                    view,
                    left,
                    right,
                    gutter_width(view, &metrics),
                    &metrics,
                    &palette,
                    &theme,
                    can_comment,
                    cx,
                )
            }
            Item::Annotation { file, side, line } => {
                self.annotation(file, side, line, &palette, &theme, window, cx)
            }
            Item::Footer => self.footer_row(false, cx),
        }
    }
}

/// `ui/checkbox.tsx`: a `size-4` box, `bg-primary` with a check when ticked.
fn checkbox(checked: bool, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .size(px(16.))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded(px(4.))
        .border_1()
        .border_color(if checked { theme.primary } else { theme.input })
        .bg(if checked {
            theme.primary
        } else {
            theme.background
        })
        .when(checked, |this| {
            this.child(
                Icon::new(IconName::Check)
                    .size(rems(0.75))
                    .color(theme.primary_foreground),
            )
        })
        .into_any_element()
}

// ── File tree ───────────────────────────────────────────────────────────────

/// One row of the file tree: a directory (compacted `a/b/c`) or a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    /// The path to reveal; `None` for a directory.
    pub path: Option<String>,
}

/// `diffFileTreeEntries` as a flat, depth-indented list: directories first, names in natural
/// order, single-child directory chains joined.
pub fn tree_rows(paths: &[String]) -> Vec<TreeRow> {
    #[derive(Default)]
    struct Node {
        children: std::collections::BTreeMap<String, Node>,
        file: Option<String>,
    }
    let mut root = Node::default();
    for path in paths {
        let mut node = &mut root;
        for part in path.split('/') {
            node = node.children.entry(part.to_owned()).or_default();
        }
        node.file = Some(path.clone());
    }
    fn walk(node: &Node, depth: usize, out: &mut Vec<TreeRow>) {
        let mut directories: Vec<(&String, &Node)> = node
            .children
            .iter()
            .filter(|(_, child)| child.file.is_none())
            .collect();
        let mut files: Vec<(&String, &Node)> = node
            .children
            .iter()
            .filter(|(_, child)| child.file.is_some())
            .collect();
        directories.sort_by_key(|(name, _)| name.to_lowercase());
        files.sort_by_key(|(name, _)| name.to_lowercase());
        for (name, child) in directories {
            let (mut label, mut current) = (name.clone(), child);
            while current.children.len() == 1 {
                let (only_name, only) = current.children.iter().next().expect("one child");
                if only.file.is_some() {
                    break;
                }
                label = format!("{label}/{only_name}");
                current = only;
            }
            out.push(TreeRow {
                depth,
                name: label,
                path: None,
            });
            walk(current, depth + 1, out);
        }
        for (name, child) in files {
            out.push(TreeRow {
                depth,
                name: name.clone(),
                path: child.file.clone(),
            });
        }
    }
    let mut rows = Vec::new();
    walk(&root, 0, &mut rows);
    rows
}

impl CodeTab {
    pub(super) fn file_tree(
        &self,
        files: &[FileView],
        more: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.app_theme().clone();
        let paths: Vec<String> = files.iter().map(|view| view.path.clone()).collect();
        let stats: HashMap<&str, (u64, u64)> = files
            .iter()
            .map(|view| (view.path.as_str(), (view.additions, view.deletions)))
            .collect();
        let mut rows = div().flex().flex_col().p_1();
        for (index, row) in tree_rows(&paths).into_iter().enumerate() {
            let is_file = row.path.is_some();
            let path = row.path.clone();
            let mut line = div()
                .id(SharedString::from(format!("tree-{index}")))
                .flex()
                .h(px(24.))
                .items_center()
                .gap(rems(0.375))
                .rounded(px(6.))
                .pl(px(8. + row.depth as f32 * 12.))
                .pr_2()
                .text_size(TEXT_XS.size)
                .text_color(if is_file {
                    theme.foreground
                } else {
                    theme.muted_foreground
                })
                .child(
                    Icon::new(if is_file {
                        IconName::FileCode
                    } else {
                        IconName::Folder
                    })
                    .size(rems(0.875))
                    .color(theme.muted_foreground),
                )
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .truncate()
                        .child(SharedString::from(row.name.clone())),
                );
            if let Some(path) = path {
                let accent = theme.accent;
                if let Some((added, removed)) = stats.get(path.as_str()).copied() {
                    let color = if removed == 0 {
                        theme.diff_addition_foreground
                    } else if added == 0 {
                        theme.diff_deletion_foreground
                    } else {
                        theme.muted_foreground
                    };
                    line = line.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(color));
                }
                line = line
                    .cursor_pointer()
                    .hover(move |style| style.bg(accent))
                    .on_click(cx.listener(move |this, _, _, cx| this.reveal_file(&path, cx)));
            }
            rows = rows.child(line);
        }
        div()
            .flex()
            .flex_col()
            .flex_shrink_0()
            .w(relative(0.4))
            .min_w(px(192.))
            .max_w(px(320.))
            .border_l_1()
            .border_color(with_alpha(theme.border, 0.6))
            .child(
                div()
                    .id("diff-tree-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(rows),
            )
            .when(more, |this| {
                this.child(
                    div()
                        .flex_shrink_0()
                        .border_t_1()
                        .border_color(with_alpha(theme.border, 0.6))
                        .p_2()
                        .child(
                            Button::new("tree-load-more")
                                .size(ButtonSize::Xs)
                                .variant(ButtonVariant::Outline)
                                .label("Load more files")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let next = this
                                        .ctx
                                        .snapshot(cx)
                                        .and_then(|snapshot| snapshot.diffs.get(&this.commit))
                                        .and_then(Loadable::ready)
                                        .and_then(|result| {
                                            result
                                                .next_cursor
                                                .as_ref()
                                                .map(|cursor| cursor.as_str().to_owned())
                                        });
                                    if let (Some(cursor), Some(source)) =
                                        (next, this.ctx.source(cx))
                                    {
                                        source.load_diff_page(
                                            &this.ctx.reference,
                                            this.commit.clone(),
                                            &cursor,
                                            cx,
                                        );
                                    }
                                })),
                        ),
                )
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn row(depth: usize, name: &str, path: Option<&str>) -> TreeRow {
        TreeRow {
            depth,
            name: name.to_owned(),
            path: path.map(str::to_owned),
        }
    }

    #[test]
    fn tree_compacts_single_child_directories_and_lists_directories_first() {
        let paths = vec![
            "README.md".to_owned(),
            "crates/app/src/lib.rs".to_owned(),
            "crates/app/src/mod.rs".to_owned(),
            "crates/app/Cargo.toml".to_owned(),
            "docs/guide.md".to_owned(),
        ];
        assert_eq!(
            tree_rows(&paths),
            vec![
                row(0, "crates/app", None),
                row(1, "src", None),
                row(2, "lib.rs", Some("crates/app/src/lib.rs")),
                row(2, "mod.rs", Some("crates/app/src/mod.rs")),
                row(1, "Cargo.toml", Some("crates/app/Cargo.toml")),
                row(0, "docs", None),
                row(1, "guide.md", Some("docs/guide.md")),
                row(0, "README.md", Some("README.md")),
            ]
        );
    }

    #[test]
    fn tabs_expand_and_tokens_follow() {
        let color = gpui_kit::hsla(0., 0., 1., 1.);
        let (text, tokens) = expand_tabs("\tfoo", &[(1..4, color)]);
        assert_eq!(text, "    foo");
        assert_eq!(tokens, vec![(4..7, color)]);
        let (plain, tokens) = expand_tabs("foo", &[(0..3, color)]);
        assert_eq!((plain.as_str(), tokens.len()), ("foo", 1));
    }

    #[test]
    fn digits_widen_the_gutter() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(999), 3);
        assert_eq!(digits(1000), 4);
    }
}

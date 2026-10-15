//! The Code tab's toolbar (`h-10 border-b`: scope, counts, diff options) and its `Render`.

use super::*;

/// What the toolbar reports about the diff on screen.
struct ToolbarMeta {
    files: usize,
    more: bool,
    withheld: bool,
    all_collapsed: bool,
    viewed: Option<(usize, usize)>,
    viewed_in_environment: bool,
    inline_comment: bool,
}

fn warning_icon(
    id: &str,
    icon: IconName,
    tooltip: &'static str,
    color: Hsla,
) -> AnyElement {
    with_tooltip(
        SharedString::from(id.to_owned()),
        tooltip,
        Icon::new(icon).size(rems(0.875)).color(color),
    )
    .into_any_element()
}

impl CodeTab {
    fn set_layout(&mut self, layout: DiffLayout, cx: &mut Context<Self>) {
        if self.layout != layout {
            self.layout = layout;
            self.dirty = true;
            cx.notify();
        }
    }

    fn set_ignore_whitespace(&mut self, ignore: bool, cx: &mut Context<Self>) {
        self.ignore_whitespace = ignore;
        self.draft = None;
        self.dirty = true;
        cx.notify();
    }

    fn set_wrap(&mut self, wrap: bool, cx: &mut Context<Self>) {
        self.wrap = wrap;
        self.dirty = true;
        cx.notify();
    }

    /// The scope menu: "All commits" and the commits newest first, ten at a time.
    fn scope_menu(&self, commits: &[PullRequestCommit], cx: &mut Context<Self>) -> Option<AnyElement> {
        if commits.is_empty() {
            return None;
        }
        let selected = commits.iter().find(|entry| Some(entry.oid.as_str()) == self.commit.as_deref());
        let label = selected.map_or_else(|| "All commits".to_owned(), |entry| entry.message_headline.clone());
        let trigger = Button::new("diff-scope")
            .size(ButtonSize::Xs)
            .variant(ButtonVariant::Secondary)
            .min_w_0()
            .max_w(rems(16.))
            .child(div().min_w_0().truncate().child(SharedString::from(label)))
            .child(
                Icon::new(IconName::ChevronDown)
                    .size(rems(0.875))
                    .color(with_alpha(cx.app_theme().secondary_foreground, 0.7).into()),
            );
        let entity = cx.entity().downgrade();
        let current = self.commit.clone();
        let visible = self.visible_commits;
        let entries: Vec<(String, String)> = commits
            .iter()
            .map(|entry| (entry.oid.as_str().to_owned(), entry.message_headline.clone()))
            .collect();
        Some(
            dropdown("diff-scope-menu", Anchor::TopLeft, trigger, move |menu, _, cx| {
                let theme = cx.app_theme().clone();
                let mut menu = menu.scrollable(true).max_h(px(360.)).min_w(px(256.)).max_w(px(448.));
                let pick = |entity: gpui_kit::WeakEntity<CodeTab>, commit: Option<String>| {
                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                        entity.update(cx, |this, cx| this.set_commit(commit.clone(), cx)).ok();
                    }
                };
                menu = menu.item(
                    PopupMenuItem::new("All commits")
                        .checked(current.is_none())
                        .on_click(pick(entity.clone(), None)),
                );
                for (oid, headline) in entries.iter().take(visible) {
                    let short: String = oid.chars().take(7).collect();
                    let headline = headline.clone();
                    let mono = theme.fonts.mono.clone();
                    let muted = theme.muted_foreground;
                    menu = menu.item(
                        PopupMenuItem::element(move |_, _| {
                            div()
                                .flex()
                                .w_full()
                                .min_w_0()
                                .items_center()
                                .gap_2()
                                .child(div().min_w_0().truncate().child(SharedString::from(headline.clone())))
                                .child(
                                    div()
                                        .ml_auto()
                                        .flex_shrink_0()
                                        .font_family(SharedString::from(mono.clone()))
                                        .text_size(TEXT_XS.size)
                                        .text_color(muted)
                                        .child(SharedString::from(short.clone())),
                                )
                        })
                        .checked(current.as_deref() == Some(oid.as_str()))
                        .on_click(pick(entity.clone(), Some(oid.clone()))),
                    );
                }
                if entries.len() > visible {
                    let left = entries.len() - visible;
                    let entity = entity.clone();
                    menu = menu.item(
                        PopupMenuItem::new(format!("Show more ({left} left)")).on_click(move |_, _, cx| {
                            entity
                                .update(cx, |this, cx| {
                                    this.visible_commits += COMMIT_PAGE_SIZE;
                                    cx.notify();
                                })
                                .ok();
                        }),
                    );
                }
                menu
            })
            .into_any_element(),
        )
    }

    fn options_menu(&self, cx: &mut Context<Self>) -> AnyElement {
        let entity = cx.entity().downgrade();
        let (ignore, wrap, layout) = (self.ignore_whitespace, self.wrap, self.layout);
        let trigger = Button::new("diff-options")
            .size(ButtonSize::IconSm)
            .variant(ButtonVariant::Ghost)
            .icon(IconName::Settings2)
            .icon_size(rems(0.875))
            .tooltip(Tooltip::new("Diff options"));
        dropdown("diff-options-menu", Anchor::TopRight, trigger, move |menu, _, _| {
            let toggle = |entity: gpui_kit::WeakEntity<CodeTab>, apply: fn(&mut CodeTab, &mut Context<CodeTab>)| {
                move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                    entity.update(cx, |this, cx| apply(this, cx)).ok();
                }
            };
            menu.item(
                PopupMenuItem::new("Hide whitespace changes")
                    .checked(ignore)
                    .on_click(toggle(entity.clone(), |this, cx| {
                        this.set_ignore_whitespace(!this.ignore_whitespace, cx)
                    })),
            )
            .item(
                PopupMenuItem::new("Wrap lines")
                    .checked(wrap)
                    .on_click(toggle(entity.clone(), |this, cx| this.set_wrap(!this.wrap, cx))),
            )
            .item(PopupMenuItem::separator())
            .item(
                PopupMenuItem::new("Stacked")
                    .checked(layout == DiffLayout::Stacked)
                    .on_click(toggle(entity.clone(), |this, cx| this.set_layout(DiffLayout::Stacked, cx))),
            )
            .item(
                PopupMenuItem::new("Split")
                    .checked(layout == DiffLayout::Split)
                    .on_click(toggle(entity.clone(), |this, cx| this.set_layout(DiffLayout::Split, cx))),
            )
        })
        .into_any_element()
    }

    fn icon_toggle(
        &self,
        id: &'static str,
        icon: IconName,
        pressed: bool,
        tooltip: &'static str,
        variant: ToggleVariant,
        size: ToggleSize,
        on_click: impl Fn(&mut CodeTab, &mut Context<CodeTab>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let entity = cx.entity().downgrade();
        Toggle::new(id)
            .variant(variant)
            .size(size)
            .icon(icon)
            .icon_size(rems(0.875))
            .pressed(pressed)
            .tooltip(Tooltip::new(tooltip))
            .on_click(move |_, _, cx| {
                entity.update(cx, |this, cx| on_click(this, cx)).ok();
            })
            .into_any_element()
    }

    fn toolbar(&self, commits: &[PullRequestCommit], meta: &ToolbarMeta, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let wide = self.toolbar_width >= WIDE_TOOLBAR;
        let warning = theme.warning_foreground;

        let mut segments: Vec<Option<AnyElement>> = Vec::new();
        segments.push(Some(
            div()
                .flex_shrink_0()
                .child(SharedString::from(format!(
                    "{} {}{}",
                    meta.files,
                    if meta.files == 1 { "file" } else { "files" },
                    if meta.more { "+" } else { "" }
                )))
                .into_any_element(),
        ));
        if let Some((viewed, total)) = meta.viewed {
            segments.push(Some(
                div()
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_1()
                    .child(div().flex_shrink_0().child(SharedString::from(format!("{viewed} / {total}"))))
                    .child(div().truncate().child(if meta.viewed_in_environment {
                        "viewed in Turin"
                    } else {
                        "viewed"
                    }))
                    .when(meta.viewed_in_environment, |this| {
                        this.child(warning_icon(
                            "viewed-info",
                            IconName::Info,
                            "This host keeps no shared record of which files you have read, so these ticks are kept by this environment. They follow you between the apps connected to it, but the host's own web UI will not show them.",
                            theme.muted_foreground,
                        ))
                    })
                    .into_any_element(),
            ));
        }
        if meta.withheld {
            segments.push(Some(warning_icon(
                "withheld",
                IconName::TriangleAlert,
                "The host withheld part of this diff \u{2014} a binary file, or a change too large to inline.",
                warning,
            )));
        }
        if self.commit.is_some() && meta.inline_comment {
            segments.push(Some(warning_icon(
                "no-line-comments",
                IconName::MessageSquareOff,
                "A comment is anchored to the whole change, so switch to All commits to write one.",
                theme.muted_foreground,
            )));
        }

        let all_collapsed = meta.all_collapsed;
        let controls = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .child(if wide {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(self.icon_toggle(
                        "diff-ws",
                        IconName::Pilcrow,
                        self.ignore_whitespace,
                        if self.ignore_whitespace {
                            "Show whitespace changes"
                        } else {
                            "Hide whitespace changes"
                        },
                        ToggleVariant::Ghost,
                        ToggleSize::Sm,
                        |this, cx| this.set_ignore_whitespace(!this.ignore_whitespace, cx),
                        cx,
                    ))
                    .child(
                        segmented_group(cx)
                            .child(self.icon_toggle(
                                "diff-stacked",
                                IconName::Rows3,
                                self.layout == DiffLayout::Stacked,
                                "Stacked diff view",
                                ToggleVariant::Segmented,
                                ToggleSize::Segmented,
                                |this, cx| this.set_layout(DiffLayout::Stacked, cx),
                                cx,
                            ))
                            .child(self.icon_toggle(
                                "diff-split",
                                IconName::Columns2,
                                self.layout == DiffLayout::Split,
                                "Split diff view",
                                ToggleVariant::Segmented,
                                ToggleSize::Segmented,
                                |this, cx| this.set_layout(DiffLayout::Split, cx),
                                cx,
                            )),
                    )
                    .child(self.icon_toggle(
                        "diff-wrap",
                        IconName::TextWrap,
                        self.wrap,
                        if self.wrap {
                            "Disable line wrapping"
                        } else {
                            "Enable line wrapping"
                        },
                        ToggleVariant::Ghost,
                        ToggleSize::Sm,
                        |this, cx| this.set_wrap(!this.wrap, cx),
                        cx,
                    ))
                    .into_any_element()
            } else {
                self.options_menu(cx)
            })
            .when(meta.files > 0, |this| {
                this.child(
                    Button::new("diff-fold-all")
                        .size(ButtonSize::IconSm)
                        .variant(ButtonVariant::Ghost)
                        .icon(if all_collapsed { IconName::ChevronsUpDown } else { IconName::ChevronsDownUp })
                        .icon_size(rems(0.875))
                        .tooltip(Tooltip::new(if all_collapsed {
                            "Expand all files"
                        } else {
                            "Collapse all files"
                        }))
                        .on_click(cx.listener(move |this, _, _, cx| this.toggle_all_files(all_collapsed, cx))),
                )
                .child(self.icon_toggle(
                    "diff-tree",
                    IconName::FolderTree,
                    self.tree_open,
                    if self.tree_open { "Hide file tree" } else { "Show file tree" },
                    ToggleVariant::Ghost,
                    ToggleSize::Sm,
                    |this, cx| {
                        this.tree_open = !this.tree_open;
                        cx.notify();
                    },
                    cx,
                ))
            });

        let entity = cx.entity().downgrade();
        div()
            .flex_shrink_0()
            .on_children_prepainted(move |bounds, _, cx| {
                let Some(width) = bounds.first().map(|bounds| f32::from(bounds.size.width)) else {
                    return;
                };
                entity
                    .update(cx, |this, cx| {
                        if (this.toolbar_width - width).abs() > 0.5 {
                            this.toolbar_width = width;
                            cx.notify();
                        }
                    })
                    .ok();
            })
            .child(
                div()
                    .flex()
                    .h_10()
                    .min_h_10()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .border_b_1()
                    .border_color(with_alpha(theme.border, 0.6))
                    .bg(theme.background)
                    .px_4()
                    .text_size(TEXT_XS.size)
                    .line_height(TEXT_XS.line_height)
                    .text_color(theme.muted_foreground)
                    .child(
                        div()
                            .flex()
                            .min_w_0()
                            .flex_1()
                            .items_center()
                            .gap_3()
                            .children(self.scope_menu(commits, cx))
                            .child(pull_request_meta_line(segments, &theme)),
                    )
                    .child(controls),
            )
            .into_any_element()
    }

    /// `DiffPanelLoadingState`.
    fn loading_state(&self, cx: &App) -> AnyElement {
        let theme = cx.app_theme();
        div()
            .flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .text_size(TEXT_XS.size)
            .text_color(theme.muted_foreground)
            .child(Spinner::new("code-loading").size(rems(0.875)))
            .child("Loading pull request diff...")
            .into_any_element()
    }

    fn message(&self, text: impl Into<SharedString>, cx: &App) -> AnyElement {
        div()
            .px_4()
            .py(rems(1.25))
            .text_size(TEXT_SM.size)
            .line_height(TEXT_SM.line_height)
            .text_color(cx.app_theme().muted_foreground)
            .child(text.into())
            .into_any_element()
    }

    fn raw_slices(&self, raw: &[(String, String)], cx: &App) -> AnyElement {
        let theme = cx.app_theme();
        let mut stack = div().flex().flex_col().gap_4().px_4().py(rems(1.25));
        for (reason, text) in raw {
            stack = stack.child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(reason.clone())),
                    )
                    .child(
                        div()
                            .font_family(SharedString::from(theme.fonts.mono.clone()))
                            .text_size(TEXT_XS.size)
                            .whitespace_normal()
                            .child(SharedString::from(text.clone())),
                    ),
            );
        }
        div()
            .id("raw-diff-scroll")
            .size_full()
            .overflow_y_scroll()
            .child(stack)
            .into_any_element()
    }

    /// "Conversations not on the current diff": above the code, closed, and counted.
    fn orphans(
        &mut self,
        threads: Vec<PullRequestReviewThread>,
        more: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if threads.is_empty() {
            return None;
        }
        let theme = cx.app_theme().clone();
        let open = self.orphans_open;
        let mut body = None;
        if open {
            let mut by_file: Vec<(String, Vec<PullRequestReviewThread>)> = Vec::new();
            for thread in &threads {
                match by_file.iter_mut().find(|(path, _)| path == thread.path.as_str()) {
                    Some((_, list)) => list.push(thread.clone()),
                    None => by_file.push((thread.path.as_str().to_owned(), vec![thread.clone()])),
                }
            }
            let mut list = div().flex().flex_col().gap_3().px_4().pb_3();
            for (path, threads) in by_file {
                let mut group = div().child(
                    with_tooltip(
                        SharedString::from(format!("orphan-{path}")),
                        path.clone(),
                        div()
                            .truncate()
                            .px_3()
                            .text_size(TEXT_XS.size)
                            .text_color(theme.muted_foreground)
                            .child(SharedString::from(path.clone())),
                    ),
                );
                let mut stack = div().mt_1().flex().flex_col().gap_2();
                for thread in threads {
                    let mut entry = div();
                    if let Some(line) = thread.line {
                        entry = entry.child(
                            div()
                                .px_3()
                                .text_size(TEXT_XS.size)
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(format!("Line {line}"))),
                        );
                    }
                    if let Some(card) = self.thread_card(&thread, window, cx) {
                        entry = entry.child(card);
                    }
                    stack = stack.child(entry);
                }
                group = group.child(stack);
                list = list.child(group);
            }
            body = Some(
                div()
                    .id("orphan-scroll")
                    .max_h(rems(16.))
                    .overflow_y_scroll()
                    .child(list),
            );
        }
        let count = threads.len();
        Some(
            div()
                .flex_shrink_0()
                .border_b_1()
                .border_color(with_alpha(theme.border, 0.6))
                .child(
                    div()
                        .id("orphans-toggle")
                        .flex()
                        .w_full()
                        .items_center()
                        .gap(rems(0.375))
                        .px_4()
                        .py_2()
                        .text_size(TEXT_XS.size)
                        .text_color(theme.muted_foreground)
                        .cursor_pointer()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.orphans_open = !this.orphans_open;
                            cx.notify();
                        }))
                        // While slices are still arriving a conversation may simply belong to a
                        // file that has not landed yet, which is not the same as being off the diff.
                        .child(if more {
                            "Conversations not on the diff loaded so far"
                        } else {
                            "Conversations not on the current diff"
                        })
                        .child(
                            Icon::new(if open { IconName::ChevronDown } else { IconName::ChevronRight })
                                .size(rems(0.875))
                                .color(theme.muted_foreground),
                        )
                        .child(SharedString::from(count.to_string())),
                )
                .children(body)
                .into_any_element(),
        )
    }
}

impl Render for CodeTab {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.request_diff(cx);
        self.sync(cx);
        let theme = cx.app_theme().clone();
        let snapshot = self.ctx.snapshot(cx).cloned();
        let detail = snapshot.as_ref().and_then(|snapshot| match &snapshot.detail {
            Loadable::Ready(detail) => Some(detail.clone()),
            _ => None,
        });
        let diff = snapshot.as_ref().and_then(|snapshot| snapshot.diffs.get(&self.commit)).cloned();
        let next_cursor = diff
            .as_ref()
            .and_then(Loadable::ready)
            .and_then(|result| result.next_cursor.clone());
        let truncated = diff.as_ref().and_then(Loadable::ready).is_some_and(|result| result.truncated);

        // Newest first: the last commit is the one a reader coming back to a change is looking for.
        let mut commits: Vec<PullRequestCommit> = detail.as_ref().map(|d| d.commits.clone()).unwrap_or_default();
        commits.sort_by_key(|commit| {
            std::cmp::Reverse(crate::state::time::parse_ms(&commit.committed_date).unwrap_or(0))
        });

        let files = self.files();
        let file_views: &[FileView] = files.as_deref().map_or(&[], Vec::as_slice);
        let paths: Vec<String> = file_views.iter().map(|view| view.path.clone()).collect();
        let viewed_enabled = detail.as_ref().is_some_and(|d| d.capabilities.viewed_files.is_some());
        let collapsed_keys: Vec<bool> = file_views.iter().map(|view| self.collapsed(&view.key)).collect();
        let meta = ToolbarMeta {
            files: file_views.len(),
            more: next_cursor.is_some(),
            withheld: truncated || matches!(self.parsed, Parsed::Raw(_)),
            all_collapsed: !collapsed_keys.is_empty() && collapsed_keys.iter().all(|c| *c),
            viewed: (viewed_enabled && !paths.is_empty()).then(|| (self.viewed.viewed_count(&paths), paths.len())),
            viewed_in_environment: detail.as_ref().is_some_and(|d| {
                d.capabilities.viewed_files
                    == Some(turin_contracts::pull_request::PullRequestViewedFilesStore::Environment)
            }),
            inline_comment: detail
                .as_ref()
                .is_some_and(|d| d.capabilities.review.inline_comment && d.viewer_permissions.comment),
        };
        let toolbar = self.toolbar(&commits, &meta, cx);

        let body: AnyElement = match (&diff, &self.parsed) {
            _ if detail.is_none() && !matches!(snapshot.as_ref().map(|s| &s.detail), Some(Loadable::Failed(_))) => {
                self.loading_state(cx)
            }
            (None | Some(Loadable::Pending), _) => self.loading_state(cx),
            (Some(Loadable::Failed(error)), Parsed::Empty) => self.message(error.clone(), cx),
            (_, Parsed::Raw(raw)) if file_views.is_empty() && next_cursor.is_none() => {
                self.raw_slices(&raw.clone(), cx)
            }
            (_, Parsed::Empty) | (_, Parsed::Files(_)) if self.items.is_empty() && next_cursor.is_none() => self.message(
                if self.commit.is_none() {
                    "This pull request has no file changes."
                } else {
                    "This commit has no file changes."
                },
                cx,
            ),
            _ => {
                let orphan_threads: Vec<PullRequestReviewThread> = detail
                    .as_ref()
                    .map(|d| {
                        d.review_threads
                            .iter()
                            .filter(|thread| !self.placed.contains(thread.id.as_str()))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let orphans = self.orphans(orphan_threads, next_cursor.is_some(), window, cx);
                let tree = (self.tree_open && !file_views.is_empty())
                    .then(|| self.file_tree(file_views, next_cursor.is_some(), cx));
                div()
                    .flex()
                    .flex_col()
                    .size_full()
                    .min_h_0()
                    .children(orphans)
                    .child(
                        div()
                            .flex()
                            .flex_1()
                            .min_h_0()
                            .overflow_hidden()
                            .child(
                                div()
                                    .relative()
                                    .flex_1()
                                    .min_w_0()
                                    .min_h_0()
                                    .bg(theme.code_background)
                                    .child(
                                        list(
                                            self.list.clone(),
                                            cx.processor(|this, ix, window, cx| this.render_item(ix, window, cx)),
                                        )
                                        .size_full(),
                                    )
                                    .vertical_scrollbar(&self.list),
                            )
                            .children(tree),
                    )
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_h_0()
            .child(toolbar)
            .child(div().flex_1().min_h_0().child(body))
    }
}

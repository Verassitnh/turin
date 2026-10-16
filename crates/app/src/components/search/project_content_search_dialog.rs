//! Port of upstream/apps/web/src/components/search/ProjectContentSearchDialog.tsx
#![allow(dead_code)]

use std::collections::HashMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div, rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::project::ProjectContentMatch;

use super::highlighted_search_line::HighlightedSearchLine;
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::{TEXT_SM, TEXT_XS};
use crate::ui::{IconName, Toggle, ToggleSize, ToggleVariant};

pub const VISIBLE_MATCH_WINDOW: usize = 100;

#[derive(Clone, Debug)]
pub struct IndexedContentMatch {
    pub match_: ProjectContentMatch,
    pub result_index: usize,
}

#[derive(Clone, Debug)]
pub struct ContentSearchMatchGroup {
    pub path: String,
    pub matches: Vec<IndexedContentMatch>,
}

pub fn split_search_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((directory, name)) => (name.to_owned(), directory.to_owned()),
        None => (path.to_owned(), String::new()),
    }
}

pub fn group_content_matches(matches: &[ProjectContentMatch]) -> Vec<ContentSearchMatchGroup> {
    let mut groups: HashMap<String, Vec<IndexedContentMatch>> = HashMap::new();
    for (result_index, match_) in matches.iter().enumerate() {
        let path = match_.path.as_str().to_owned();
        groups
            .entry(path.clone())
            .or_default()
            .push(IndexedContentMatch {
                match_: match_.clone(),
                result_index,
            });
    }
    groups
        .into_iter()
        .map(|(path, matches)| ContentSearchMatchGroup { path, matches })
        .collect()
}

pub fn visible_content_matches(
    matches: &[ProjectContentMatch],
    visible_count: usize,
) -> Vec<ProjectContentMatch> {
    matches.iter().take(visible_count).cloned().collect()
}

#[derive(Clone, Debug, Default)]
pub struct ProjectContentSearchQuery {
    pub query: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub use_regex: bool,
}

#[derive(Clone, Debug)]
pub struct ProjectContentSearchBackendInput {
    pub environment_id: EnvironmentId,
    pub cwd: String,
    pub query: ProjectContentSearchQuery,
}

#[derive(Clone, Debug)]
pub struct ProjectContentSearchResults {
    pub matches: Vec<ProjectContentMatch>,
    pub can_read_files: bool,
    pub is_pending: bool,
    pub error: Option<String>,
}

#[derive(IntoElement)]
pub struct ProjectContentSearchDialog {
    pub target_cwd: Option<String>,
    pub environment_id: Option<EnvironmentId>,
    pub query: ProjectContentSearchQuery,
    pub results: ProjectContentSearchResults,
    pub selected_index: usize,
    pub visible_count: usize,
    pub dark: bool,
}

impl RenderOnce for ProjectContentSearchDialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.app_theme();
        let Some(cwd) = self.target_cwd else {
            return div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p_6()
                .text_size(TEXT_SM.size)
                .text_color(theme.muted_foreground)
                .child("Open a project to search its files.")
                .into_any_element();
        };
        let visible = visible_content_matches(&self.results.matches, self.visible_count);
        let groups = group_content_matches(&visible);
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_3()
                    .py_2()
                    .child(
                        Toggle::new("content-search-case")
                            .variant(ToggleVariant::Segmented)
                            .size(ToggleSize::Sm)
                            .label("Aa")
                            .pressed(self.query.case_sensitive),
                    )
                    .child(
                        Toggle::new("content-search-whole-word")
                            .variant(ToggleVariant::Segmented)
                            .size(ToggleSize::Sm)
                            .label("W")
                            .pressed(self.query.whole_word),
                    )
                    .child(
                        Toggle::new("content-search-regex")
                            .variant(ToggleVariant::Segmented)
                            .size(ToggleSize::Sm)
                            .label(".*")
                            .pressed(self.query.use_regex),
                    ),
            )
            .child(
                crate::ui::scroll_area::scroll_area("project-content-search-results").child(
                    div().flex_1().children(groups.into_iter().map(|group| {
                        let (name, directory) = split_search_path(&group.path);
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .py_2()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_3()
                                    .child(crate::ui::Icon::new(IconName::FileCode).size(rems(1.)))
                                    .child(
                                        div()
                                            .text_size(TEXT_SM.size)
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(name),
                                    )
                                    .when(!directory.is_empty(), |this| {
                                        this.child(
                                            div()
                                                .text_size(TEXT_XS.size)
                                                .text_color(theme.muted_foreground)
                                                .child(directory),
                                        )
                                    }),
                            )
                            .children(group.matches.into_iter().map(|indexed| {
                                div()
                                    .px_3()
                                    .py(rems(0.125))
                                    .when(indexed.result_index == self.selected_index, |this| {
                                        this.bg(theme.accent)
                                    })
                                    .child(HighlightedSearchLine {
                                        match_: indexed.match_,
                                        path: group.path.clone(),
                                        dark: self.dark,
                                    })
                            }))
                    })),
                ),
            )
            .child(
                div()
                    .px_3()
                    .py_2()
                    .text_size(TEXT_XS.size)
                    .text_color(theme.muted_foreground)
                    .child(format!("Searching {cwd}")),
            )
            .into_any_element()
    }
}

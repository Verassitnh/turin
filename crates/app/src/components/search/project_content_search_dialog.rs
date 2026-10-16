//! Port of upstream/apps/web/src/components/search/ProjectContentSearchDialog.tsx
#![allow(dead_code)]

use indexmap::IndexMap;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Entity, FontWeight, IntoElement, ParentElement as _, RenderOnce, Styled as _, Window, div,
    rems,
};
use turin_contracts::EnvironmentId;
use turin_contracts::project::ProjectContentMatch;

use super::highlighted_search_line::{HighlightedSearchLine, SearchLineHighlightController};
use crate::pull_request::presentation_view::Spinner;
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
    let mut groups: IndexMap<String, Vec<IndexedContentMatch>> = IndexMap::new();
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

#[derive(Clone, Debug, Default)]
pub struct ProjectContentSearchResults {
    pub matches: Vec<ProjectContentMatch>,
    pub can_read_files: bool,
    pub is_pending: bool,
    pub is_checking_access: bool,
    pub invalid_regex: bool,
    pub truncated: bool,
    pub error: Option<String>,
}

pub fn show_content_search_status_bar(
    results: &ProjectContentSearchResults,
    has_query: bool,
) -> bool {
    results.can_read_files
        && (has_query || results.is_pending || results.error.is_some() || results.invalid_regex)
}

pub fn content_search_center_message(
    results: &ProjectContentSearchResults,
    has_query: bool,
) -> String {
    if results.is_checking_access {
        "Checking file access…".to_owned()
    } else if !results.can_read_files {
        results.error.clone().unwrap_or_default()
    } else if has_query && !results.is_pending && results.error.is_none() {
        "No results found.".to_owned()
    } else {
        "Type to search across your project.".to_owned()
    }
}

pub fn content_search_file_count(matches: &[ProjectContentMatch]) -> usize {
    matches
        .iter()
        .map(|match_| match_.path.as_str())
        .collect::<indexmap::IndexSet<_>>()
        .len()
}

pub fn content_search_status_label(
    results: &ProjectContentSearchResults,
    match_count: usize,
    file_count: usize,
) -> String {
    if results.is_pending {
        "Searching…".to_owned()
    } else if let Some(error) = &results.error {
        error.clone()
    } else if results.invalid_regex {
        "Invalid regular expression".to_owned()
    } else {
        let suffix = if results.truncated { "+" } else { "" };
        format!("{match_count}{suffix} results in {file_count} files")
    }
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
    pub highlight: Entity<SearchLineHighlightController>,
}

impl RenderOnce for ProjectContentSearchDialog {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let Some(_cwd) = self.target_cwd else {
            let theme = cx.app_theme();
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
        let has_query = !self.query.query.trim().is_empty();
        let visible = visible_content_matches(&self.results.matches, self.visible_count);
        let highlight = self.highlight.clone();
        let dark = self.dark;
        let all_matches = self.results.matches.clone();
        highlight.update(cx, |controller, cx| {
            controller.warm(&visible, &all_matches, dark, cx);
        });
        let theme = cx.app_theme();
        let groups = group_content_matches(&visible);
        let file_count = content_search_file_count(&self.results.matches);
        let show_status = show_content_search_status_bar(&self.results, has_query);
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
                            .pressed(self.query.case_sensitive)
                            .disabled(!self.results.can_read_files),
                    )
                    .child(
                        Toggle::new("content-search-whole-word")
                            .variant(ToggleVariant::Segmented)
                            .size(ToggleSize::Sm)
                            .pressed(self.query.whole_word)
                            .disabled(!self.results.can_read_files)
                            .child(div().underline().child("ab")),
                    )
                    .child(
                        Toggle::new("content-search-regex")
                            .variant(ToggleVariant::Segmented)
                            .size(ToggleSize::Sm)
                            .label(".*")
                            .pressed(self.query.use_regex)
                            .disabled(!self.results.can_read_files),
                    ),
            )
            .when(show_status, |this| {
                let status = content_search_status_label(
                    &self.results,
                    self.results.matches.len(),
                    file_count,
                );
                this.child(
                    div()
                        .flex()
                        .h(rems(2.25))
                        .flex_shrink_0()
                        .items_center()
                        .border_b_1()
                        .border_color(theme.border)
                        .px_3()
                        .text_size(TEXT_XS.size)
                        .text_color(
                            if self.results.error.is_some() || self.results.invalid_regex {
                                theme.destructive
                            } else {
                                theme.muted_foreground
                            },
                        )
                        .when(self.results.is_pending, |row| {
                            row.gap_2()
                                .child(Spinner::new("content-search-spinner").size(rems(0.875)))
                        })
                        .child(status),
                )
            })
            .child(if self.results.matches.is_empty() {
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px_6()
                    .text_center()
                    .text_size(TEXT_SM.size)
                    .text_color(theme.muted_foreground)
                    .child(content_search_center_message(&self.results, has_query))
                    .into_any_element()
            } else {
                crate::ui::scroll_area::scroll_area("project-content-search-results")
                    .child(div().flex_1().children(groups.into_iter().map(|group| {
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
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px_3()
                                    .py(rems(0.125))
                                    .when(indexed.result_index == self.selected_index, |this| {
                                        this.bg(theme.accent)
                                    })
                                    .child(
                                        div()
                                            .w(rems(2.5))
                                            .flex_shrink_0()
                                            .text_right()
                                            .text_size(TEXT_XS.size)
                                            .text_color(theme.muted_foreground)
                                            .child(indexed.match_.line_number.to_string()),
                                    )
                                    .child(div().flex_1().min_w_0().child(HighlightedSearchLine {
                                        highlight: highlight.clone(),
                                        match_: indexed.match_,
                                        path: group.path.clone(),
                                        dark: self.dark,
                                    }))
                            }))
                    })))
                    .into_any_element()
            })
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::PositiveInt;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::project::ProjectContentMatch;

    fn match_at(path: &str) -> ProjectContentMatch {
        ProjectContentMatch {
            path: TrimmedNonEmptyString::new(path).unwrap(),
            line_number: PositiveInt::new(1).unwrap(),
            line_content: "needle".to_owned(),
            match_ranges: vec![],
        }
    }

    #[test]
    fn groups_matches_in_first_seen_path_order() {
        let grouped =
            group_content_matches(&[match_at("b.rs"), match_at("a.rs"), match_at("b.rs")]);
        assert_eq!(grouped.len(), 2);
        assert_eq!(grouped[0].path, "b.rs");
        assert_eq!(grouped[1].path, "a.rs");
    }

    #[test]
    fn counts_files_across_the_full_result_set() {
        assert_eq!(
            content_search_file_count(&[match_at("a.rs"), match_at("b.rs"), match_at("a.rs"),]),
            2
        );
    }

    #[test]
    fn center_message_follows_search_state() {
        assert_eq!(
            content_search_center_message(
                &ProjectContentSearchResults {
                    is_checking_access: true,
                    ..Default::default()
                },
                false,
            ),
            "Checking file access…"
        );
        assert_eq!(
            content_search_center_message(
                &ProjectContentSearchResults {
                    can_read_files: false,
                    error: Some("denied".into()),
                    ..Default::default()
                },
                false,
            ),
            "denied"
        );
        assert!(
            content_search_center_message(
                &ProjectContentSearchResults {
                    can_read_files: false,
                    ..Default::default()
                },
                false,
            )
            .is_empty()
        );
        assert_eq!(
            content_search_center_message(
                &ProjectContentSearchResults {
                    can_read_files: true,
                    ..Default::default()
                },
                true,
            ),
            "No results found."
        );
    }
}

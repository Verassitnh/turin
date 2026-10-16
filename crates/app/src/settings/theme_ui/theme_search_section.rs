//! Port of upstream/apps/web/src/components/settings/ThemeSearchSection.tsx
//!
//! The community-theme search (Open VSX) as a state machine: React's pair of
//! effects, abort controllers and `lastSearchKeyRef` bookkeeping become
//! [`ThemeSearchState`]. The view feeds it input and, after each change, asks
//! [`ThemeSearchState::after_update`] which request (if any) to start; results
//! come back through `finish_search` / `finish_install`. Network sits behind
//! [`ThemeSearchBackend`] (live Open VSX or fixtures).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::theme::open_vsx::{OpenVsxClient, ReqwestOpenVsxClient};
use crate::theme::open_vsx_themes::{
    OpenVsxThemeExtension, OpenVsxThemeSearchOptions, OpenVsxThemeSort,
    import_open_vsx_theme_extension, search_open_vsx_themes,
};
use crate::theme::theme_palette_library::{
    get_custom_themes, get_stored_custom_theme_collection, replace_custom_theme_collection,
};
use crate::theme::theme_palettes::ThemeDefinition;

pub const SUGGESTED_SEARCHES: [&str; 4] = ["Dracula", "Catppuccin", "Nord", "Tokyo Night"];
pub const SORT_OPTIONS: [(OpenVsxThemeSort, &str); 4] = [
    (OpenVsxThemeSort::DownloadCount, "Most downloaded"),
    (OpenVsxThemeSort::Rating, "Best rated"),
    (OpenVsxThemeSort::Timestamp, "Newest"),
    (OpenVsxThemeSort::Relevance, "Most relevant"),
];
pub const SEARCH_DEBOUNCE_MS: u64 = 350;

pub fn sort_label(sort: OpenVsxThemeSort) -> &'static str {
    SORT_OPTIONS
        .iter()
        .find(|(candidate, _)| *candidate == sort)
        .map_or("", |(_, label)| label)
}

/// `Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 })`
/// in en-US: `999`, `1.2K`, `15K`, `3.4M`, `1B`.
pub fn format_download_count(count: u64) -> String {
    const UNITS: [(f64, &str); 4] = [(1e12, "T"), (1e9, "B"), (1e6, "M"), (1e3, "K")];
    let value = count as f64;
    for (index, (scale, suffix)) in UNITS.iter().enumerate() {
        if value >= *scale {
            let rounded = (value / scale * 10.0).round() / 10.0;
            // 999.95K rounds up to 1000K: carry into the next unit.
            if rounded >= 1000.0 && index > 0 {
                return format!("1{}", UNITS[index - 1].1);
            }
            return if rounded.fract() == 0.0 {
                format!("{}{suffix}", rounded as u64)
            } else {
                format!("{rounded:.1}{suffix}")
            };
        }
    }
    count.to_string()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceLinkIcon {
    GitHub,
    GitLab,
    External,
}

/// `SourceLinkIcon`: the host decides the glyph.
pub fn source_link_icon(url: &str) -> SourceLinkIcon {
    let Some(host) = url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_lowercase))
    else {
        return SourceLinkIcon::External;
    };
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    if on("github.com") {
        SourceLinkIcon::GitHub
    } else if on("gitlab.com") {
        SourceLinkIcon::GitLab
    } else {
        SourceLinkIcon::External
    }
}

type BoxFut<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Where search results and extension files come from.
pub trait ThemeSearchBackend: Send + Sync {
    fn search(
        &self,
        query: String,
        sort_by: OpenVsxThemeSort,
        cancel: CancellationToken,
    ) -> BoxFut<Result<Vec<OpenVsxThemeExtension>, String>>;

    fn import(
        &self,
        extension: OpenVsxThemeExtension,
        cancel: CancellationToken,
    ) -> BoxFut<Result<Vec<ThemeDefinition>, String>>;
}

/// An `OpenVsxClient` (live or fixture) as a backend.
pub struct ClientBackend<C: OpenVsxClient + 'static>(pub Arc<C>);

impl ClientBackend<ReqwestOpenVsxClient> {
    pub fn live() -> Self {
        Self(Arc::new(ReqwestOpenVsxClient::new()))
    }
}

impl<C: OpenVsxClient + 'static> ThemeSearchBackend for ClientBackend<C> {
    fn search(
        &self,
        query: String,
        sort_by: OpenVsxThemeSort,
        cancel: CancellationToken,
    ) -> BoxFut<Result<Vec<OpenVsxThemeExtension>, String>> {
        let client = Arc::clone(&self.0);
        Box::pin(async move {
            search_open_vsx_themes(
                client.as_ref(),
                &query,
                OpenVsxThemeSearchOptions {
                    cancel: Some(&cancel),
                    sort_by,
                },
            )
            .await
        })
    }

    fn import(
        &self,
        extension: OpenVsxThemeExtension,
        cancel: CancellationToken,
    ) -> BoxFut<Result<Vec<ThemeDefinition>, String>> {
        let client = Arc::clone(&self.0);
        Box::pin(async move {
            import_open_vsx_theme_extension(client.as_ref(), &extension, Some(&cancel)).await
        })
    }
}

/// One in-flight search.
#[derive(Clone, Debug)]
pub struct SearchRequest {
    pub id: u64,
    pub text: String,
    pub sort_by: OpenVsxThemeSort,
    pub cancel: CancellationToken,
}

/// One in-flight install.
#[derive(Clone, Debug)]
pub struct InstallRequest {
    pub id: u64,
    pub extension: OpenVsxThemeExtension,
    pub cancel: CancellationToken,
    /// The collection as installed when the install began, so a concurrent
    /// edit elsewhere is not overwritten.
    pub expected_collection: Vec<ThemeDefinition>,
    pub updated: bool,
}

/// What a finished install tells the caller (`onInstalled`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstalledThemes {
    pub themes: Vec<ThemeDefinition>,
    pub updated: bool,
}

#[derive(Debug)]
pub struct ThemeSearchState {
    pub query: String,
    pub sort_by: OpenVsxThemeSort,
    pub results: Option<Vec<OpenVsxThemeExtension>>,
    pub error: Option<String>,
    pub is_searching: bool,
    pub installing_id: Option<String>,
    pub pending_update: Option<OpenVsxThemeExtension>,
    request: Option<SearchRequest>,
    install: Option<InstallRequest>,
    next_request_id: u64,
    /// The (query, sort) pair the last search actually ran, so an install
    /// finishing can tell a same-key rerun (which must not wipe an install
    /// error) from a query that changed mid-install (which must be searched).
    last_search_key: Option<String>,
    /// The (query, sort) pair from the previous effect run, so a search error
    /// that belongs to an older key can be cleared when the user returns to
    /// already-shown results without clearing a fresh install error.
    prev_search_key: Option<String>,
}

fn search_key(query: &str, sort_by: OpenVsxThemeSort) -> String {
    format!("{query}\u{0}{}", sort_label(sort_by))
}

impl Default for ThemeSearchState {
    fn default() -> Self {
        Self {
            query: String::new(),
            sort_by: OpenVsxThemeSort::DownloadCount,
            results: None,
            error: None,
            is_searching: false,
            installing_id: None,
            pending_update: None,
            request: None,
            install: None,
            next_request_id: 0,
            last_search_key: None,
            prev_search_key: None,
        }
    }
}

impl ThemeSearchState {
    fn abort_request(&mut self) {
        if let Some(request) = self.request.take() {
            request.cancel.cancel();
        }
    }

    /// The `[open]` effect: every open starts from a blank dialog; closing
    /// aborts whatever is in flight.
    pub fn set_open(&mut self, open: bool) {
        self.abort_request();
        if let Some(install) = self.install.take() {
            install.cancel.cancel();
        }
        if open {
            *self = Self {
                next_request_id: self.next_request_id,
                ..Self::default()
            };
        }
    }

    /// `runSearch`.
    pub fn run_search(
        &mut self,
        search_text: &str,
        sort_by: OpenVsxThemeSort,
    ) -> Option<SearchRequest> {
        let trimmed = search_text.trim();
        if trimmed.is_empty() {
            return None;
        }
        self.abort_request();
        self.next_request_id += 1;
        let request = SearchRequest {
            id: self.next_request_id,
            text: trimmed.to_string(),
            sort_by,
            cancel: CancellationToken::new(),
        };
        self.request = Some(request.clone());
        self.error = None;
        self.is_searching = true;
        Some(request)
    }

    /// The search finished (or failed) in the background.
    pub fn finish_search(
        &mut self,
        request: &SearchRequest,
        result: Result<Vec<OpenVsxThemeExtension>, String>,
    ) {
        if request.cancel.is_cancelled() {
            return;
        }
        match result {
            Ok(results) => {
                self.results = Some(results);
                self.last_search_key = Some(search_key(&request.text, request.sort_by));
            }
            Err(message) => {
                self.results = None;
                self.last_search_key = None;
                self.error = Some(if message.is_empty() {
                    "Open VSX search failed.".to_string()
                } else {
                    message
                });
            }
        }
        if self
            .request
            .as_ref()
            .is_some_and(|current| current.id == request.id)
        {
            self.request = None;
            self.is_searching = false;
        }
    }

    /// Both effects of `ThemeSearchSection` that react to the query: call after
    /// any change to `query`, `sort_by`, `installing_id` or the debounced
    /// query. Returns the search to start, if any.
    pub fn after_update(&mut self, open: bool, debounced_query: &str) -> Option<SearchRequest> {
        // Effect 1: an empty box with nothing installing drops all results.
        if self.query.trim().is_empty() && self.installing_id.is_none() {
            self.abort_request();
            self.last_search_key = None;
            self.results = None;
            self.error = None;
            self.is_searching = false;
        }

        // Effect 2: the debounced search.
        if !open {
            return None;
        }
        let key = search_key(debounced_query, self.sort_by);
        let key_changed = self.prev_search_key.as_deref() != Some(key.as_str());
        self.prev_search_key = Some(key.clone());
        if self.installing_id.is_some() {
            return None;
        }
        if debounced_query.is_empty() {
            self.last_search_key = None;
            self.abort_request();
            self.results = None;
            self.error = None;
            self.is_searching = false;
            return None;
        }
        if debounced_query != self.query.trim() {
            // The debounced value still trails the input (dialog reopened with
            // the box reset, or the user is mid-keystroke). Searching it would
            // hit Open VSX for a query that is no longer visible; wait for the
            // debounce to catch up to the current input instead.
            return None;
        }
        if self.last_search_key.as_deref() == Some(key.as_str()) {
            // The results already match this query. A request for a newer key
            // may still be in flight (typed and then undone); abort it so it
            // cannot overwrite the results. Only a genuine key change makes a
            // stale search error irrelevant, so an install error on an
            // unchanged query survives.
            self.abort_request();
            self.is_searching = false;
            if key_changed {
                self.error = None;
            }
            return None;
        }
        let sort_by = self.sort_by;
        self.run_search(debounced_query, sort_by)
    }

    /// Enter in the box (an immediate search, skipping the debounce).
    pub fn submit_query(&mut self) -> Option<SearchRequest> {
        if self.is_searching || self.installing_id.is_some() {
            return None;
        }
        let query = self.query.trim().to_string();
        let sort_by = self.sort_by;
        self.run_search(&query, sort_by)
    }

    /// A suggestion chip: re-run an identical query, else type it.
    pub fn pick_suggestion(&mut self, suggestion: &str) -> Option<SearchRequest> {
        if self.installing_id.is_some() {
            return None;
        }
        if self.query.trim() == suggestion {
            let sort_by = self.sort_by;
            self.run_search(suggestion, sort_by)
        } else {
            self.query = suggestion.to_string();
            None
        }
    }

    /// `handleInstall`. `Some` starts the install; `None` means nothing to run
    /// (either the update confirmation is now pending or the call was ignored).
    pub fn begin_install(
        &mut self,
        extension: &OpenVsxThemeExtension,
        allow_update: bool,
    ) -> Option<InstallRequest> {
        self.error = None;
        let installed_collection = get_stored_custom_theme_collection(&extension.collection_id);
        let updated = !installed_collection.is_empty();
        if updated && !allow_update {
            self.pending_update = Some(extension.clone());
            return None;
        }

        self.abort_request();
        self.is_searching = false;
        self.next_request_id += 1;
        let request = InstallRequest {
            id: self.next_request_id,
            extension: extension.clone(),
            cancel: CancellationToken::new(),
            expected_collection: installed_collection,
            updated,
        };
        self.installing_id = Some(extension.id.clone());
        self.install = Some(request.clone());
        Some(request)
    }

    /// The import finished: replaces the collection atomically and reports
    /// the installed themes, or records the error.
    pub fn finish_install(
        &mut self,
        request: &InstallRequest,
        result: Result<Vec<ThemeDefinition>, String>,
    ) -> Option<InstalledThemes> {
        let installed = if request.cancel.is_cancelled() {
            None
        } else {
            match result.and_then(|themes| {
                replace_custom_theme_collection(
                    &request.extension.collection_id,
                    themes,
                    Some(&request.expected_collection),
                )
            }) {
                Ok(themes) => Some(InstalledThemes {
                    themes,
                    updated: request.updated,
                }),
                Err(message) => {
                    self.error = Some(if message.is_empty() {
                        "That theme could not be added.".to_string()
                    } else {
                        message
                    });
                    None
                }
            }
        };
        if self
            .install
            .as_ref()
            .is_some_and(|current| current.id == request.id)
        {
            self.install = None;
            self.installing_id = None;
        }
        installed
    }

    /// Confirms the pending update dialog.
    pub fn confirm_pending_update(&mut self) -> Option<InstallRequest> {
        let extension = self.pending_update.take()?;
        self.begin_install(&extension, true)
    }

    pub fn dismiss_pending_update(&mut self) {
        self.pending_update = None;
    }

    pub fn is_installed(extension: &OpenVsxThemeExtension) -> bool {
        get_custom_themes().iter().any(|theme| {
            theme
                .collection
                .as_ref()
                .is_some_and(|collection| collection.id.as_str() == extension.collection_id)
        })
    }

    /// The live-region text.
    pub fn status_message(&self) -> String {
        if self.is_searching {
            return "Searching themes...".to_string();
        }
        match &self.results {
            Some(results) => format!(
                "{} supported {} found.",
                results.len(),
                if results.len() == 1 {
                    "theme"
                } else {
                    "themes"
                }
            ),
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::theme::THEME_GLOBALS_TEST_LOCK;
    use crate::theme::theme_palette_library::{
        InMemoryThemeLibraryStorage, invalidate_custom_themes, set_theme_library_storage,
    };

    fn library() -> parking_lot::MutexGuard<'static, ()> {
        let guard = THEME_GLOBALS_TEST_LOCK.lock();
        set_theme_library_storage(Arc::new(InMemoryThemeLibraryStorage::new()));
        invalidate_custom_themes();
        guard
    }

    fn extension(name: &str) -> OpenVsxThemeExtension {
        let id = format!("pub.{}", name.to_lowercase());
        OpenVsxThemeExtension {
            collection_id: format!("open-vsx:{id}"),
            id,
            name: name.to_string(),
            publisher: "pub".to_string(),
            description: String::new(),
            download_count: 1200,
            icon_url: None,
            source_url: None,
            manifest_url: "https://open-vsx.org/m".to_string(),
            sha256_url: "https://open-vsx.org/s".to_string(),
            vsix_url: "https://open-vsx.org/v".to_string(),
            version: "1.0.0".to_string(),
            license: "MIT".to_string(),
        }
    }

    fn open_state() -> ThemeSearchState {
        let mut state = ThemeSearchState::default();
        state.set_open(true);
        state
    }

    #[test]
    fn formats_download_counts_like_compact_en_us() {
        for (count, text) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1K"),
            (1_234, "1.2K"),
            (15_400, "15.4K"),
            (999_949, "999.9K"),
            (999_950, "1M"),
            (3_400_000, "3.4M"),
            (2_000_000_000, "2B"),
        ] {
            assert_eq!(format_download_count(count), text, "{count}");
        }
    }

    #[test]
    fn picks_the_source_link_glyph_from_the_host() {
        assert_eq!(
            source_link_icon("https://github.com/a/b"),
            SourceLinkIcon::GitHub
        );
        assert_eq!(
            source_link_icon("https://www.github.com/a"),
            SourceLinkIcon::GitHub
        );
        assert_eq!(
            source_link_icon("https://gitlab.com/a/b"),
            SourceLinkIcon::GitLab
        );
        assert_eq!(
            source_link_icon("https://example.com"),
            SourceLinkIcon::External
        );
        assert_eq!(source_link_icon("not a url"), SourceLinkIcon::External);
        assert_eq!(
            source_link_icon("https://notgithub.com"),
            SourceLinkIcon::External
        );
    }

    #[test]
    fn typing_waits_for_the_debounce_to_catch_up_before_searching() {
        let mut state = open_state();
        state.query = "dra".into();
        // The debounced value still trails the box.
        assert!(state.after_update(true, "dr").is_none());
        let request = state.after_update(true, "dra").expect("search");
        assert_eq!(request.text, "dra");
        assert!(state.is_searching);

        state.finish_search(&request, Ok(vec![extension("Dracula")]));
        assert!(!state.is_searching);
        assert_eq!(state.results.as_ref().map(Vec::len), Some(1));
        assert_eq!(state.status_message(), "1 supported theme found.");
        // The same key does not search again.
        assert!(state.after_update(true, "dra").is_none());
    }

    #[test]
    fn changing_the_sort_reruns_the_search() {
        let mut state = open_state();
        state.query = "nord".into();
        let first = state.after_update(true, "nord").expect("search");
        state.finish_search(&first, Ok(vec![]));
        assert_eq!(state.status_message(), "0 supported themes found.");
        state.sort_by = OpenVsxThemeSort::Rating;
        let second = state.after_update(true, "nord").expect("rerun");
        assert_eq!(second.sort_by, OpenVsxThemeSort::Rating);
        assert!(first.cancel.is_cancelled() || state.request.is_some());
    }

    #[test]
    fn an_aborted_request_cannot_overwrite_newer_results() {
        let mut state = open_state();
        state.query = "a".into();
        let stale = state.after_update(true, "a").expect("search");
        state.query = "ab".into();
        let fresh = state.after_update(true, "ab").expect("search");
        assert!(stale.cancel.is_cancelled());
        state.finish_search(&stale, Ok(vec![extension("Stale")]));
        assert!(state.results.is_none());
        assert!(state.is_searching);
        state.finish_search(&fresh, Ok(vec![extension("Fresh")]));
        assert_eq!(state.results.as_ref().expect("results")[0].name, "Fresh");
    }

    #[test]
    fn clearing_the_box_drops_results_and_errors() {
        let mut state = open_state();
        state.query = "nord".into();
        let request = state.after_update(true, "nord").expect("search");
        state.finish_search(&request, Err(String::new()));
        assert_eq!(state.error.as_deref(), Some("Open VSX search failed."));
        state.query.clear();
        assert!(state.after_update(true, "").is_none());
        assert!(state.results.is_none() && state.error.is_none());
        assert!(!state.is_searching);
    }

    #[test]
    fn an_install_error_survives_a_rerun_of_the_same_key() {
        let _guard = library();
        let mut state = open_state();
        state.query = "pub".into();
        let request = state.after_update(true, "pub").expect("search");
        state.finish_search(&request, Ok(vec![extension("Pub")]));

        let install = state
            .begin_install(&extension("Pub"), false)
            .expect("install starts");
        assert_eq!(state.installing_id.as_deref(), Some("pub.pub"));
        assert!(state.after_update(true, "pub").is_none());
        assert!(
            state
                .finish_install(&install, Err("Download failed".into()))
                .is_none()
        );
        assert_eq!(state.error.as_deref(), Some("Download failed"));
        assert_eq!(state.installing_id, None);
        // Same query and sort as the last successful search: the error stays.
        assert!(state.after_update(true, "pub").is_none());
        assert_eq!(state.error.as_deref(), Some("Download failed"));
    }

    #[test]
    fn installing_replaces_the_collection_and_a_second_install_asks_to_update() {
        let _guard = library();
        let mut state = open_state();
        let extension = extension("Demo");
        let install = state.begin_install(&extension, false).expect("install");
        assert!(!install.updated);

        let theme = crate::theme::theme_palette_library::parse_theme_file(serde_json::json!({
            "version": 1, "id": "demo-dark", "name": "Demo Dark", "appearance": "dark",
            "colors": { "canvas": "#101010" },
        }))
        .expect("theme");
        let theme = ThemeDefinition {
            collection: Some(crate::theme::theme_palettes::ThemeCollection {
                id: extension.collection_id.clone().into(),
                label: "Demo".into(),
            }),
            ..theme
        };
        let installed = state
            .finish_install(&install, Ok(vec![theme.clone()]))
            .expect("installed");
        assert_eq!(installed.themes, vec![theme.clone()]);
        assert!(!installed.updated);
        assert!(ThemeSearchState::is_installed(&extension));

        // A second install needs a confirmation first.
        assert!(state.begin_install(&extension, false).is_none());
        assert_eq!(
            state.pending_update.as_ref().map(|e| e.id.as_str()),
            Some("pub.demo")
        );
        let update = state.confirm_pending_update().expect("update");
        assert!(update.updated);
        assert_eq!(update.expected_collection.len(), 1);
        let again = state
            .finish_install(&update, Ok(vec![theme]))
            .expect("updated");
        assert!(again.updated);
    }

    #[test]
    fn closing_the_dialog_cancels_requests_and_reopening_starts_blank() {
        let mut state = open_state();
        state.query = "nord".into();
        let request = state.after_update(true, "nord").expect("search");
        state.set_open(false);
        assert!(request.cancel.is_cancelled());
        state.finish_search(&request, Ok(vec![extension("Nord")]));
        assert!(state.results.is_none());
        state.set_open(true);
        assert_eq!(state.query, "");
        assert!(state.after_update(false, "nord").is_none());
    }

    #[test]
    fn suggestions_type_the_query_or_rerun_an_identical_one() {
        let mut state = open_state();
        assert!(state.pick_suggestion("Nord").is_none());
        assert_eq!(state.query, "Nord");
        let rerun = state.pick_suggestion("Nord").expect("rerun");
        assert_eq!(rerun.text, "Nord");
        // Enter while searching is ignored.
        assert!(state.submit_query().is_none());
    }
}

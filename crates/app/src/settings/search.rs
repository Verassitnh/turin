//! Port of upstream/apps/web/src/components/settings/settingsSearch.ts
//! (and settingsSearch.test.ts).
//!
//! The catalog keeps the rows of kept panels only. Dropped with their
//! features (PORTING.md Deviations): Diagnostics/telemetry, SnapShots,
//! Integrations (browser, devices), Scheduled Tasks, Connections (remote
//! access, T3 Connect, WSL, load balancing, GitHub sharing), the app updater
//! rows and the `t3` CLI install row. The desktop/cloud/WSL availability flags
//! go with them: Turin is always the desktop app.

use std::sync::LazyLock;

use turin_contracts::keybindings::STATIC_KEYBINDING_COMMANDS;
use turin_shared::keybindings::{default_keybinding_rules, is_mac_platform};

use super::keybindings_logic::command_label;
use super::locale::{locale_compare, normalize_search_text};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsPath {
    Projects,
    General,
    Appearance,
    Keybindings,
    Providers,
    SourceControl,
    Storage,
    Archived,
}

impl SettingsPath {
    pub const ALL: [SettingsPath; 8] = [
        Self::Projects,
        Self::General,
        Self::Appearance,
        Self::Keybindings,
        Self::Providers,
        Self::SourceControl,
        Self::Storage,
        Self::Archived,
    ];

    pub fn path(self) -> &'static str {
        match self {
            Self::Projects => "/settings/projects",
            Self::General => "/settings/general",
            Self::Appearance => "/settings/appearance",
            Self::Keybindings => "/settings/keybindings",
            Self::Providers => "/settings/providers",
            Self::SourceControl => "/settings/source-control",
            Self::Storage => "/settings/storage",
            Self::Archived => "/settings/archived",
        }
    }

    /// `SETTINGS_SECTION_LABELS`: the sidebar nav and the search-result
    /// subtitles both render from this, so each label exists once.
    pub fn label(self) -> &'static str {
        match self {
            Self::Projects => "Project",
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Keybindings => "Keybindings",
            Self::Providers => "Providers",
            Self::SourceControl => "Source Control",
            Self::Storage => "Storage",
            Self::Archived => "Archive",
        }
    }

    pub fn from_path(path: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.path() == path)
    }
}

/// Where a setting can be edited. Device-local rows have no scope: they render
/// at every selection. `ProjectDefaults` rows accept project overrides, so
/// they are reachable from any server-backed selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsSearchScope {
    Environment,
    EnvironmentDefaults,
    ProjectDefaults,
    Project,
    Checkout,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSearchItem {
    pub id: String,
    pub title: String,
    pub to: SettingsPath,
    pub target_id: Option<String>,
    /// Descriptions, option labels, and aliases people may remember instead of the title.
    pub search_terms: Vec<String>,
    pub scope: Option<SettingsSearchScope>,
    /// Its row only renders on macOS.
    pub mac_only: bool,
    pub mac_provider_settings_only: bool,
    /// Sorts after every other match. Keybinding commands mirror rows on other
    /// surfaces, so "model" must still lead with Default model, not Model Picker.
    pub secondary: bool,
    pub requires_thread_auto_settlement: bool,
}

impl SettingsSearchItem {
    fn new(id: &str, title: &str, to: SettingsPath) -> Self {
        Self {
            id: id.to_owned(),
            title: title.to_owned(),
            to,
            target_id: None,
            search_terms: Vec::new(),
            scope: None,
            mac_only: false,
            mac_provider_settings_only: false,
            secondary: false,
            requires_thread_auto_settlement: false,
        }
    }

    fn terms(mut self, terms: &[&str]) -> Self {
        self.search_terms = terms.iter().map(|term| (*term).to_owned()).collect();
        self
    }

    fn target(mut self, target_id: &str) -> Self {
        self.target_id = Some(target_id.to_owned());
        self
    }

    fn scope(mut self, scope: SettingsSearchScope) -> Self {
        self.scope = Some(scope);
        self
    }

    fn mac_only(mut self) -> Self {
        self.mac_only = true;
        self
    }

    fn auto_settlement(mut self) -> Self {
        self.requires_thread_auto_settlement = true;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettingsSearchAvailability {
    pub has_mac_provider_settings_environment: bool,
    pub has_thread_auto_settlement: bool,
}

impl Default for SettingsSearchAvailability {
    fn default() -> Self {
        Self {
            has_mac_provider_settings_environment: cfg!(target_os = "macos"),
            has_thread_auto_settlement: true,
        }
    }
}

/// Anchor id of the first row bound to `command` on the Keybindings page.
pub fn keybinding_search_anchor_id(command: &str) -> String {
    format!("keybinding-{command}")
}

/// One result per built-in command, alphabetical by label. The anchor is
/// the command's first row; default keys are searchable so "mod+b" lands on
/// Sidebar: Toggle. A command with no default binding may have no row, so it
/// points at the section instead.
fn keybinding_search_items() -> Vec<SettingsSearchItem> {
    let defaults = default_keybinding_rules();
    let mut commands: Vec<&str> = STATIC_KEYBINDING_COMMANDS.to_vec();
    commands.sort_by(|left, right| locale_compare(&command_label(left), &command_label(right)));
    commands
        .into_iter()
        .map(|command| {
            let default_keys: Vec<String> = defaults
                .iter()
                .filter(|binding| binding.command.as_str() == command)
                .map(|binding| binding.key.as_str().to_owned())
                .collect();
            let mut item = SettingsSearchItem::new(
                &keybinding_search_anchor_id(command),
                &command_label(command),
                SettingsPath::Keybindings,
            );
            item.search_terms = std::iter::once(command.to_owned())
                .chain(default_keys.iter().cloned())
                .collect();
            item.secondary = true;
            if default_keys.is_empty() {
                item.target_id = Some("keybindings".to_owned());
            }
            item
        })
        .collect()
}

use SettingsPath as P;
use SettingsSearchScope as S;

/// Searchable settings and stable destinations, in result order. Rows with a
/// dedicated anchor render their id and title via [`searchable_setting`]; items
/// that may not be mounted point at their nearest stable section instead.
pub static SETTINGS_SEARCH_ITEMS: LazyLock<Vec<SettingsSearchItem>> = LazyLock::new(|| {
    let item = SettingsSearchItem::new;
    let mut items = vec![
        item("storage-worktree-keep-when", "Keep worktrees with local changes", P::Storage)
            .target("storage-worktrees")
            .scope(S::ProjectDefaults)
            .terms(&["any local files uncommitted changes edited tracked files ignored env build output cleanup"]),
        item("storage-delete-now", "Delete now", P::Storage)
            .scope(S::EnvironmentDefaults)
            .terms(&["storage cleanup run now results report removed kept failed"]),
        item("storage-worktrees", "Worktree cleanup", P::Storage)
            .scope(S::ProjectDefaults)
            .terms(&["disk storage delete deleted archived threads old inactive merged unchanged worktrees retention days project inherit off custom"]),
        item("storage-worktrees-location", "Worktree location", P::Storage)
            .scope(S::EnvironmentDefaults)
            .terms(&["worktree location folder directory path drive external disk"]),
        item("storage-artifacts", "Artifacts and logs", P::Storage)
            .scope(S::EnvironmentDefaults)
            .terms(&["disk storage browser screenshots captures rotated logs cleanup retention"]),
        item("project-defaults", "Project defaults and overrides", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["model workspace environments projects inheritance checkout"]),
        item("project-overview", "Project overview", P::Projects)
            .terms(&["name icon emoji image checkout remove delete"]),
        item("default-model", "Default model", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["new thread project provider reasoning effort"]),
        item("default-permissions", "Permissions", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["new thread default runtime mode supervised approvals auto accept edits full access"]),
        // The scheme tiles sit at the top of the Appearance section.
        item("color-scheme", "Color scheme", P::Appearance)
            .terms(&["appearance light dark system mode"])
            .target("appearance"),
        // Theme cards live directly under the scheme tiles; the section is the
        // stable scroll destination for both.
        item("theme", "Themes", P::Appearance)
            .terms(&["appearance colors palette custom import"])
            .target("appearance"),
        // Prefixed because the slider control already owns the `appearance-contrast` id.
        item("setting-appearance-contrast", "Contrast", P::Appearance)
            .terms(&["colors borders interface"]),
        // Prefixed because the slider control already owns the `glass-opacity` id.
        item("setting-glass-opacity", "Glass opacity", P::Appearance)
            .terms(&["transparent transparency solid menus dialogs composer"]),
        item("diff-color-scheme", "Diff colors", P::Appearance)
            .terms(&["red green blue orange additions deletions changes counts palette colorblind"]),
        item("chat-width", "Chat width", P::Appearance)
            .terms(&["wide full width column layout messages composer monitor"]),
        item("panel-animations", "Panel animations", P::Appearance),
        // The setting is stage-dependent, so its parent section is the stable destination.
        item("environment-identification", "Environment identification", P::Appearance)
            .terms(&["dev nightly artwork pill label hide none"])
            .target("appearance-interface"),
        item("interface-font", "Interface font", P::Appearance)
            .terms(&["typography family size system sans"]),
        item("prompt-font", "Prompt font", P::Appearance)
            .terms(&["typography family size composer input"]),
        item("code-font", "Code font", P::Appearance)
            .terms(&["typography family size monospace code blocks diffs file previews"]),
        item("font-smoothing", "Font smoothing", P::Appearance)
            .terms(&["typography text grayscale anti aliasing macos thin"])
            .mac_only(),
        item("word-wrap", "Word wrap", P::Appearance)
            .terms(&["long lines code blocks tables diffs file previews"]),
        item("composer-context", "Composer context", P::Appearance),
        item("project-grouping", "Project grouping", P::General)
            .terms(&["combine matching repositories environments sidebar"]),
        item("project-order", "Project order", P::General)
            .terms(&["sort projects sidebar manual created recent"]),
        item("snooze-limited-threads", "Snooze limited threads", P::General)
            .terms(&["usage quota rate limit reset wake recover continue"]),
        item("auto-resume-limited-threads", "Auto-resume limited threads", P::General)
            .terms(&["usage quota rate limit reset recover continue"]),
        item("working-shelf", "Working section (beta)", P::General)
            .terms(&["hide fold running monitoring threads inbox sidebar shelf"]),
        item("auto-settle-inactive-threads", "Auto-settle inactive threads", P::General)
            .terms(&["sidebar inactivity days no activity automatically"])
            .auto_settlement()
            .scope(S::ProjectDefaults),
        item("auto-settle-merged-threads", "Auto-settle merged threads", P::General)
            .terms(&["pull request merge closed automatically sidebar"])
            .auto_settlement()
            .scope(S::ProjectDefaults),
        item("days-before-auto-settle", "Days of inactivity before auto-settle", P::General)
            .target("auto-settle-inactive-threads")
            .terms(&["thread timeout activity sidebar"])
            .auto_settlement()
            .scope(S::ProjectDefaults),
        item("thread-notifications", "Thread notifications", P::General)
            .terms(&["notification sound alert completion input approval desktop"]),
        item("in-app-notifications", "In-app notifications", P::General)
            .terms(&["notification toast popup completion input approval failure"]),
        item("time-format", "Time format", P::General)
            .terms(&["timestamp clock locale system browser os 12 hour 24 hour"]),
        item("response-streaming", "Response streaming", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["output token paragraph buffered wait turn legacy"]),
        item("hide-whitespace-changes", "Hide whitespace changes", P::General)
            .terms(&["diff ignore spaces edits default"]),
        item("default-diff-file-state", "Default diff file state", P::General)
            .terms(&["collapsed expanded collapse expand files pull request pr code tab"]),
        item("diff-layout", "Diff layout", P::General)
            .terms(&["stacked split side by side unified inline view"]),
        item("proactive-panels", "Proactive panels", P::General)
            .terms(&["automatically open diff pull request pr right panel agent completion"]),
        item("skills-in-slash-menu", "Show skills in slash menu", P::General)
            .terms(&["command menu dollar $ slash /"]),
        item("composer-rich-text", "Rich text composer", P::General)
            .terms(&["composer rich text tiptap bold italic markdown styled wysiwyg"]),
        item("composer-collapse", "Collapse composer on scroll", P::General)
            .terms(&["composer rest resting scroll wheel conversation timeline shrink minimize"]),
        item("send-shortcut", "Send shortcut", P::General)
            .terms(&["enter return command ctrl multiline prompt new line composer"]),
        item("follow-up-behavior", "Follow-up behavior", P::General)
            .terms(&["queue steer running turn send default behavior composer"]),
        item("provider-update-checks", "Provider update checks", P::General)
            .terms(&["installed cli versions newer available codex claude cursor opencode"])
            .scope(S::EnvironmentDefaults),
        item("continue-threads-after-server-update", "Continue threads after restarts", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["resume running active interrupted work restart reboot machine crash desktop update automatically"]),
        item("background-activity", "Background activity", P::General)
            .scope(S::EnvironmentDefaults)
            .terms(&["balanced performance battery saver advanced git fetch provider health refresh host power monitor idle policy"]),
        item("new-threads", "New threads", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["default workspace mode draft local worktree"]),
        item("worktree-submodules", "Submodules", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["git submodule init recursive top-level none worktree t3.json"]),
        item("start-from-origin", "Start from origin", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["new worktrees latest matching remote branch local"]),
        item("add-project-starts-in", "Add project starts in", P::General)
            .scope(S::EnvironmentDefaults)
            .terms(&["base directory folder browser path home"]),
        item("unpin-confirmation", "Unpin confirmation", P::General)
            .terms(&["ask before thread pinned section"]),
        item("archive-confirmation", "Archive confirmation", P::General)
            .terms(&["ask before thread second click inline action"]),
        item("delete-confirmation", "Delete confirmation", P::General)
            .terms(&["ask before thread chat history"]),
        item("quit-confirmation", "Quit shortcut", P::General)
            .terms(&["confirmation desktop app exit direct hold double click press twice"]),
        item("text-generation-model", "Text generation model", P::General)
            .scope(S::ProjectDefaults)
            .terms(&["generated thread titles source control content default provider"]),
        item("app-version", "Version", P::General).terms(&["about release"]),
        item("open-source-licenses", "Open source licenses", P::General),
        item("legacy-plan-mode", "Plan mode (legacy)", P::General)
            .terms(&["build plan composer old"]),
        item("legacy-context-window-indicator", "Context window indicator (legacy)", P::General)
            .terms(&["composer meter usage tokens circle old"]),
        item("legacy-sidebar", "Sidebar (legacy)", P::General)
            .terms(&["project thread tree old flat list"]),
        item("keybindings", "Keybindings", P::Keybindings)
            .terms(&["keyboard shortcuts hotkeys commands bindings json"]),
    ];
    items.extend(keybinding_search_items());
    items.extend([
        item("providers", "Providers", P::Providers).terms(&[
            "agents cli codex claude cursor opencode sign in sign out install subscription instances authentication api key models configuration binary path config directory endpoint arguments environment variables display name accent color custom favorite hidden auto compact",
        ]),
        item("usage-providers", "Usage providers", P::Providers)
            .terms(&["usage sources CLIProxyAPI CLI proxy hub quota subscription limits management key add remove"]),
        {
            let mut cursor = item("cursor-keychain-usage", "Cursor account usage", P::Providers)
                .terms(&["cursor macOS keychain usage tokens cost limits permission"]);
            cursor.mac_provider_settings_only = true;
            cursor
        },
        item("provider-health-check-interval", "Health check interval", P::Providers)
            .terms(&["refresh availability versions auth state models background probes seconds off"]),
        item("automatic-pull", "Automatically pull", P::SourceControl)
            .scope(S::ProjectDefaults)
            .terms(&["auto pull default branch current checkout fast forward upstream"]),
        item("remove-agent-credits-on-merge", "Remove agent credits when merging", P::SourceControl)
            .scope(S::ProjectDefaults)
            .terms(&["pull request github squash co-authored-by attribution claude codex generated"]),
        item("pull-request-merge-method", "Default merge method", P::SourceControl)
            .scope(S::ProjectDefaults)
            .terms(&["pull request merge squash rebase last selected"]),
        item("source-control", "Source control", P::SourceControl)
            .scope(S::EnvironmentDefaults)
            .terms(&["version control git github hosting integrations credentials scan server environment"]),
        item("git-fetch-interval", "Git fetch interval", P::SourceControl)
            .terms(&["automatic remote branch refresh background credentials security keys seconds off"])
            .scope(S::EnvironmentDefaults),
        item("worktree-branch-naming", "Worktree branch naming", P::SourceControl)
            .terms(&["static semantic prefix custom prompt instructions feat fix refactor chore"])
            .scope(S::ProjectDefaults),
        item("github-accounts", "GitHub accounts and token", P::SourceControl)
            .terms(&["github gh account login user host enterprise ghes switch multiple accounts disable sign in token personal access token pat api key credential"])
            .scope(S::EnvironmentDefaults),
        item("source-control-writing-style", "Source control writing style", P::SourceControl)
            .terms(&["repository conventions conventional commits custom instructions change descriptions request titles"]),
        item("follow-change-request-templates", "Follow change request templates", P::SourceControl)
            .terms(&["repository pr pull request description structure"]),
        item("source-control-writer-model", "Source control writer model", P::SourceControl)
            .terms(&["override generated commit change request pr titles descriptions branch bookmark"])
            .scope(S::ProjectDefaults),
        item("review-guides", "Review guides (beta)", P::SourceControl)
            .terms(&["pull request pr guided review guide tour beta experimental"]),
        // Hidden until review guides are on, so a search lands on the switch that shows it.
        item("review-guide-model", "Review guide model", P::SourceControl)
            .target("review-guides")
            .terms(&["pull request pr guided review guide brief maintainer"])
            .scope(S::ProjectDefaults),
        item("review-guide-instructions", "Review guide instructions", P::SourceControl)
            .target("review-guides")
            .terms(&["pull request pr guided review guide preferences taste prompt custom"])
            .scope(S::ProjectDefaults),
        item("project-actions", "Actions", P::Projects)
            .terms(&["commands scripts setup run dev server checkout worktree t3.json import"]),
        item("archive", "Archived threads", P::Archived)
            .terms(&["restore reopen deleted history projects"]),
    ]);
    items
});

/// What the settings scope resolves to (`ResolvedSettingsScope["kind"]`).
/// Turin has one environment, so a selection is "all projects" or a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsScopeKind {
    All,
    Environment,
    Project,
    Checkout,
    Unavailable,
}

fn category_scope(path: SettingsPath) -> Option<SettingsSearchScope> {
    match path {
        SettingsPath::Projects => Some(SettingsSearchScope::Project),
        SettingsPath::General | SettingsPath::Appearance | SettingsPath::Keybindings => None,
        // Providers shows the representative environment at any selection.
        SettingsPath::Providers => None,
        SettingsPath::SourceControl => Some(SettingsSearchScope::EnvironmentDefaults),
        SettingsPath::Storage | SettingsPath::Archived => {
            Some(SettingsSearchScope::ProjectDefaults)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsSearchTargetScope {
    pub title: String,
    pub scope: Option<SettingsSearchScope>,
    pub requires_thread_auto_settlement: bool,
}

/// Search keeps the selected target. A missing row can explain its owning scope instead.
pub fn get_settings_search_target_scope(target_id: &str) -> Option<SettingsSearchTargetScope> {
    let items = &*SETTINGS_SEARCH_ITEMS;
    let item = items
        .iter()
        .find(|candidate| candidate.id == target_id)
        .or_else(|| {
            items
                .iter()
                .find(|candidate| candidate.target_id.as_deref() == Some(target_id))
        })?;
    Some(SettingsSearchTargetScope {
        title: item.title.clone(),
        scope: item.scope.or_else(|| category_scope(item.to)),
        requires_thread_auto_settlement: item.requires_thread_auto_settlement,
    })
}

pub fn is_settings_search_scope_available(
    required: Option<SettingsSearchScope>,
    kind: SettingsScopeKind,
) -> bool {
    use SettingsScopeKind as K;
    match required {
        None => true,
        Some(SettingsSearchScope::Environment) => kind == K::Environment,
        Some(SettingsSearchScope::Checkout) => kind == K::Checkout,
        Some(SettingsSearchScope::Project) => matches!(kind, K::Project | K::Checkout),
        Some(SettingsSearchScope::EnvironmentDefaults) => matches!(kind, K::Environment | K::All),
        Some(SettingsSearchScope::ProjectDefaults) => {
            matches!(kind, K::Environment | K::All | K::Project | K::Checkout)
        }
    }
}

/// `isSettingsOverviewVisible`: the Project page appears only for project targets.
pub fn is_settings_overview_visible(kind: SettingsScopeKind) -> bool {
    matches!(
        kind,
        SettingsScopeKind::Project | SettingsScopeKind::Checkout
    )
}

/// `id` and `title` for the element a search item anchors to. Panels use this
/// instead of restating the strings, so the catalog and the rendered settings
/// cannot drift apart.
pub fn searchable_setting(id: &str) -> (&'static str, &'static str) {
    let item = SETTINGS_SEARCH_ITEMS
        .iter()
        .find(|item| item.id == id)
        .unwrap_or_else(|| panic!("unknown settings search item {id}"));
    // The catalog is a process-lifetime static; hand out `'static` views.
    (
        Box::leak(item.id.clone().into_boxed_str()),
        Box::leak(item.title.clone().into_boxed_str()),
    )
}

pub fn filter_available_settings_search_items(
    availability: &SettingsSearchAvailability,
) -> Vec<SettingsSearchItem> {
    SETTINGS_SEARCH_ITEMS
        .iter()
        .filter(|item| {
            (!item.mac_provider_settings_only || availability.has_mac_provider_settings_environment)
                && (!item.requires_thread_auto_settlement
                    || availability.has_thread_auto_settlement)
        })
        .cloned()
        .collect()
}

pub fn search_settings(
    query: &str,
    items: &[SettingsSearchItem],
    platform: &str,
) -> Vec<SettingsSearchItem> {
    let normalized_query = normalize_search_text(query);
    if normalized_query.is_empty() {
        return Vec::new();
    }
    let query_tokens: Vec<&str> = normalized_query.split(' ').collect();

    struct Ranked<'a> {
        item: &'a SettingsSearchItem,
        index: usize,
        rank: u8,
    }
    let mut matches: Vec<Ranked> = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            if item.mac_only && !is_mac_platform(platform) {
                return None;
            }
            let title = normalize_search_text(&item.title);
            let fields: Vec<String> = [title.clone(), normalize_search_text(item.to.label())]
                .into_iter()
                .chain(
                    item.search_terms
                        .iter()
                        .map(|term| normalize_search_text(term)),
                )
                .collect();
            if !query_tokens
                .iter()
                .all(|token| fields.iter().any(|field| field.contains(token)))
            {
                return None;
            }
            let exact_phrase = fields.iter().any(|field| field.contains(&normalized_query));
            let rank = if title == normalized_query {
                5
            } else if title.starts_with(&normalized_query) {
                4
            } else if title.contains(&normalized_query) {
                3
            } else if query_tokens.iter().all(|token| title.contains(token)) {
                2
            } else if exact_phrase {
                1
            } else {
                0
            };
            Some(Ranked { item, index, rank })
        })
        .collect();
    matches.sort_by(|left, right| {
        left.item
            .secondary
            .cmp(&right.item.secondary)
            .then(right.rank.cmp(&left.rank))
            .then(left.index.cmp(&right.index))
    });
    matches
        .into_iter()
        .map(|ranked| ranked.item.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    fn items() -> Vec<SettingsSearchItem> {
        vec![
            SettingsSearchItem::new("word-wrap", "Word wrap", P::General)
                .terms(&["long lines in code previews"]),
            SettingsSearchItem::new("network-access", "Network access", P::Storage)
                .terms(&["remote pairing backend"]),
            SettingsSearchItem::new("providers", "Providers", P::Providers)
                .terms(&["claude codex agents"]),
            SettingsSearchItem::new("provider-updates", "Update checks", P::General),
            SettingsSearchItem::new("automatic-updates", "Automatic updates", P::General),
        ]
    }

    fn ids(query: &str, items: &[SettingsSearchItem]) -> Vec<String> {
        search_settings(query, items, "MacIntel")
            .into_iter()
            .map(|item| item.id)
            .collect()
    }

    fn catalog_ids(query: &str) -> Vec<String> {
        ids(query, &SETTINGS_SEARCH_ITEMS)
    }

    #[test]
    fn finds_send_shortcut() {
        for query in ["send shortcut", "multiline", "new line"] {
            assert!(
                catalog_ids(query).contains(&"send-shortcut".to_owned()),
                "{query}"
            );
        }
    }

    #[test]
    fn matches_titles_sections_and_remembered_setting_details() {
        let items = items();
        assert_eq!(ids("word", &items), vec!["word-wrap"]);
        assert_eq!(ids("network", &items), vec!["network-access"]);
        // The section label is searchable (here "Storage" stands in for the
        // upstream fixture's Connections page).
        assert_eq!(ids("storage", &items), vec!["network-access"]);
        assert_eq!(ids("claude", &items), vec!["providers"]);
        assert_eq!(ids("long lines", &items), vec!["word-wrap"]);
    }

    #[test]
    fn matches_normalized_title_substrings() {
        let items = items();
        assert_eq!(ids("  WORD   WRAP  ", &items), vec!["word-wrap"]);
        assert_eq!(catalog_ids("glass"), vec!["setting-glass-opacity"]);
        assert_eq!(catalog_ids("panel animations"), vec!["panel-animations"]);
        assert_eq!(
            search_settings("thè\u{1ab0}mes", &SETTINGS_SEARCH_ITEMS, "MacIntel")[0].id,
            "theme"
        );
        assert_eq!(catalog_ids("xyzzy"), Vec::<String>::new());
    }

    #[test]
    fn keeps_catalog_order_for_multiple_title_matches() {
        assert_eq!(
            ids("update", &items()),
            vec!["provider-updates", "automatic-updates"]
        );
    }

    #[test]
    fn matches_query_words_across_fields_and_ranks_the_strongest_result_first() {
        assert_eq!(ids("pairing remote", &items()), vec!["network-access"]);
    }

    #[test]
    fn finds_settings_that_used_to_be_reachable_only_through_their_section() {
        assert_eq!(
            catalog_ids("pull request template")[0],
            "follow-change-request-templates"
        );
        assert_eq!(catalog_ids("git security keys")[0], "git-fetch-interval");
        assert_eq!(catalog_ids("battery saver")[0], "background-activity");
        assert_eq!(catalog_ids("binary path")[0], "providers");
    }

    #[test]
    fn lists_thread_confirmations_in_panel_order() {
        assert_eq!(
            catalog_ids("confirmation"),
            // Upstream's test runs in a browser, where the desktop-only Quit
            // shortcut row is hidden; Turin is the desktop app.
            vec![
                "unpin-confirmation",
                "archive-confirmation",
                "delete-confirmation",
                "quit-confirmation"
            ]
        );
    }

    #[test]
    fn finds_usage_provider_management() {
        for query in [
            "usage providers",
            "CLIProxyAPI",
            "CLI proxy hub",
            "management key",
        ] {
            let first = &search_settings(query, &SETTINGS_SEARCH_ITEMS, "MacIntel")[0];
            assert_eq!(first.id, "usage-providers", "{query}");
            assert_eq!(first.to, SettingsPath::Providers);
        }
    }

    #[test]
    fn returns_no_results_for_an_empty_query() {
        assert_eq!(ids("   ", &items()), Vec::<String>::new());
    }

    #[test]
    fn hides_macos_only_settings_on_other_platforms() {
        assert!(search_settings("font smoothing", &SETTINGS_SEARCH_ITEMS, "Win32").is_empty());
        assert!(!search_settings("font smoothing", &SETTINGS_SEARCH_ITEMS, "MacIntel").is_empty());
    }

    #[test]
    fn hides_settings_whose_controls_are_unavailable() {
        let available = filter_available_settings_search_items(&SettingsSearchAvailability {
            has_mac_provider_settings_environment: false,
            has_thread_auto_settlement: false,
        });
        let gated = [
            "cursor-keychain-usage",
            "auto-settle-inactive-threads",
            "auto-settle-merged-threads",
            "days-before-auto-settle",
        ];
        assert!(
            !available
                .iter()
                .any(|item| gated.contains(&item.id.as_str()))
        );
        let with_mac = filter_available_settings_search_items(&SettingsSearchAvailability {
            has_mac_provider_settings_environment: true,
            has_thread_auto_settlement: false,
        });
        assert!(
            with_mac
                .iter()
                .any(|item| item.id == "cursor-keychain-usage")
        );
    }

    #[test]
    fn shows_automatic_settlement_settings_when_supported() {
        let available = filter_available_settings_search_items(&SettingsSearchAvailability {
            has_mac_provider_settings_environment: false,
            has_thread_auto_settlement: true,
        });
        assert_eq!(
            ids("auto-settle", &available),
            vec![
                "auto-settle-inactive-threads",
                "auto-settle-merged-threads",
                "days-before-auto-settle"
            ]
        );
    }

    #[test]
    fn finds_keybinding_commands_by_label_command_id_and_default_key() {
        assert_eq!(
            catalog_ids("toggle sidebar")[0],
            "keybinding-sidebar.toggle"
        );
        assert_eq!(
            catalog_ids("sidebar.toggle")[0],
            "keybinding-sidebar.toggle"
        );
        assert_eq!(catalog_ids("mod+b")[0], "keybinding-sidebar.toggle");
        let copy = &search_settings("copy link", &SETTINGS_SEARCH_ITEMS, "MacIntel")[0];
        assert_eq!(copy.id, "keybinding-thread.copyReference");
        assert_eq!(copy.to, SettingsPath::Keybindings);
    }

    #[test]
    fn ranks_keybinding_commands_after_other_settings() {
        let ids = catalog_ids("model");
        assert_eq!(ids[0], "default-model");
        let position = |id: &str| ids.iter().position(|candidate| candidate == id).unwrap();
        assert!(position("keybinding-modelPicker.toggle") > position("text-generation-model"));
    }

    #[test]
    fn sends_commands_without_a_default_binding_to_the_section() {
        let stop = &search_settings("thread.stop", &SETTINGS_SEARCH_ITEMS, "MacIntel")[0];
        assert_eq!(stop.id, "keybinding-thread.stop");
        assert_eq!(stop.target_id.as_deref(), Some("keybindings"));
        assert_eq!(
            search_settings("sidebar.toggle", &SETTINGS_SEARCH_ITEMS, "MacIntel")[0].target_id,
            None
        );
    }

    #[test]
    fn keeps_catalog_result_ids_unique() {
        let mut seen = std::collections::HashSet::new();
        for item in SETTINGS_SEARCH_ITEMS.iter() {
            assert!(seen.insert(item.id.clone()), "{} is duplicated", item.id);
        }
    }

    #[test]
    fn serves_anchor_props_to_panels_from_the_catalog() {
        assert_eq!(searchable_setting("word-wrap"), ("word-wrap", "Word wrap"));
        assert_eq!(
            searchable_setting("archive"),
            ("archive", "Archived threads")
        );
    }

    #[test]
    fn routes_appearance_settings_to_their_current_section() {
        let first =
            |query: &str| search_settings(query, &SETTINGS_SEARCH_ITEMS, "MacIntel")[0].clone();
        assert_eq!(first("theme").id, "theme");
        assert_eq!(first("theme").to, SettingsPath::Appearance);
        assert_eq!(first("word wrap").to, SettingsPath::Appearance);
        assert_eq!(first("composer context").id, "composer-context");
        let environment = first("environment identification");
        assert_eq!(environment.to, SettingsPath::Appearance);
        assert_eq!(
            environment.target_id.as_deref(),
            Some("appearance-interface")
        );
    }

    #[test]
    fn routes_settings_to_their_owning_category() {
        for (query, id, to) in [
            ("default model", "default-model", SettingsPath::General),
            ("new threads", "new-threads", SettingsPath::General),
            (
                "automatically pull",
                "automatic-pull",
                SettingsPath::SourceControl,
            ),
            ("actions", "project-actions", SettingsPath::Projects),
            (
                "project overview",
                "project-overview",
                SettingsPath::Projects,
            ),
        ] {
            let first = &search_settings(query, &SETTINGS_SEARCH_ITEMS, "MacIntel")[0];
            assert_eq!((first.id.as_str(), first.to), (id, to), "{query}");
        }
    }

    #[test]
    fn keeps_environment_settings_discoverable() {
        let available = filter_available_settings_search_items(&Default::default());
        assert_eq!(
            ids("writing style", &available)[0],
            "source-control-writing-style"
        );
        assert_eq!(ids("auto-settle", &available).len(), 3);
    }

    #[test]
    fn retains_the_capability_requirement_for_auto_settle_targets() {
        for target in [
            "auto-settle-inactive-threads",
            "auto-settle-merged-threads",
            "days-before-auto-settle",
        ] {
            let scope = get_settings_search_target_scope(target).unwrap();
            assert_eq!(scope.scope, Some(SettingsSearchScope::ProjectDefaults));
            assert!(scope.requires_thread_auto_settlement);
        }
    }

    #[test]
    fn treats_device_local_rows_as_reachable_from_every_selection() {
        let setting = get_settings_search_target_scope("time-format").unwrap();
        assert_eq!(
            setting,
            SettingsSearchTargetScope {
                title: "Time format".into(),
                scope: None,
                requires_thread_auto_settlement: false
            }
        );
        assert!(is_settings_search_scope_available(
            setting.scope,
            SettingsScopeKind::Project
        ));
        assert!(is_settings_search_scope_available(
            setting.scope,
            SettingsScopeKind::All
        ));
        assert_eq!(
            get_settings_search_target_scope("appearance")
                .unwrap()
                .scope,
            None
        );
        assert_eq!(get_settings_search_target_scope("missing-setting"), None);
    }

    #[test]
    fn lets_project_scopable_rows_resolve_at_every_server_backed_scope() {
        let model = get_settings_search_target_scope("text-generation-model").unwrap();
        for kind in [
            SettingsScopeKind::All,
            SettingsScopeKind::Environment,
            SettingsScopeKind::Project,
        ] {
            assert!(is_settings_search_scope_available(model.scope, kind));
        }
        assert!(!is_settings_search_scope_available(
            model.scope,
            SettingsScopeKind::Unavailable
        ));
    }

    #[test]
    fn reaches_source_control_discovery_and_git_fetch_interval_from_the_default_scope() {
        for id in ["source-control", "git-fetch-interval"] {
            let item = get_settings_search_target_scope(id).unwrap();
            assert!(is_settings_search_scope_available(
                item.scope,
                SettingsScopeKind::All
            ));
            assert!(is_settings_search_scope_available(
                item.scope,
                SettingsScopeKind::Environment
            ));
            assert!(!is_settings_search_scope_available(
                item.scope,
                SettingsScopeKind::Project
            ));
        }
    }

    #[test]
    fn keeps_environment_wide_settings_out_of_project_scopes() {
        let updates = get_settings_search_target_scope("provider-update-checks").unwrap();
        assert_eq!(
            updates.scope,
            Some(SettingsSearchScope::EnvironmentDefaults)
        );
        assert!(is_settings_search_scope_available(
            updates.scope,
            SettingsScopeKind::Environment
        ));
        assert!(is_settings_search_scope_available(
            updates.scope,
            SettingsScopeKind::All
        ));
        assert!(!is_settings_search_scope_available(
            updates.scope,
            SettingsScopeKind::Project
        ));
        let streaming = get_settings_search_target_scope("response-streaming").unwrap();
        assert_eq!(streaming.scope, Some(SettingsSearchScope::ProjectDefaults));
        assert!(is_settings_search_scope_available(
            streaming.scope,
            SettingsScopeKind::Project
        ));
        for id in [
            "legacy-plan-mode",
            "legacy-context-window-indicator",
            "legacy-sidebar",
        ] {
            assert_eq!(get_settings_search_target_scope(id).unwrap().scope, None);
        }
    }

    #[test]
    fn shows_overview_only_for_project_and_checkout_targets() {
        assert!(!is_settings_overview_visible(SettingsScopeKind::All));
        assert!(!is_settings_overview_visible(
            SettingsScopeKind::Environment
        ));
        assert!(is_settings_overview_visible(SettingsScopeKind::Project));
        assert!(is_settings_overview_visible(SettingsScopeKind::Checkout));
    }
}

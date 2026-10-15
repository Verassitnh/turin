//! The settings pages. Each panel is an `impl SettingsView` block that returns
//! the page's sections; [`SettingsView::render_page`] routes and frames them
//! (`SettingsPageContainer`).

mod general;

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use super::search::SettingsPath;
use super::view::{SettingsRoute, SettingsView, Which};
use crate::theme::ActiveAppTheme;
use crate::ui::scroll_area;

/// Per-page UI state that is not part of the settings.
#[derive(Default)]
pub struct PanelState {
    pub legacy_open: bool,
}

/// `max-w-4xl`.
const READABLE_WIDTH: f32 = 896.;

impl SettingsView {
    pub(super) fn render_page(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sections: Vec<AnyElement> = match self.route {
            SettingsRoute::Section(SettingsPath::General) => self.render_general(window, cx),
            _ => vec![div()
                .text_color(cx.app_theme().muted_foreground)
                .child(SharedString::from(format!(
                    "{} is not ported yet.",
                    self.route.breadcrumb_label()
                )))
                .into_any_element()],
        };
        self.page_container(sections, cx)
    }

    /// `SettingsPageContainer` + `WorkspacePageContainer`: a scrolling column,
    /// `mx-auto max-w-4xl gap-8 px-6 pt-6 pb-12`.
    pub(super) fn page_container(&mut self, children: Vec<AnyElement>, _cx: &mut Context<Self>) -> AnyElement {
        div()
            .flex_1()
            .min_h_0()
            .child(
                scroll_area("settings-page").child(
                    div()
                        .mx_auto()
                        .flex()
                        .flex_col()
                        .gap_8()
                        .w_full()
                        .max_w(px(READABLE_WIDTH))
                        .px_6()
                        .pt_6()
                        .pb(px(48.))
                        .children(children),
                ),
            )
            .into_any_element()
    }

    /// Labels of every device-level setting that differs from its default
    /// (`useSettingsRestore().changedSettingLabels`).
    pub(super) fn changed_setting_labels(&self, cx: &gpui_kit::App) -> Vec<&'static str> {
        let vm = self.vm(cx);
        let json = self.json(cx);
        let mut labels: Vec<&'static str> = Vec::new();
        for (label, which, key) in RESTORABLE {
            if json.changed(*which, key) {
                labels.push(label);
            }
        }
        for label in super::panels_logic::get_changed_typography_setting_labels(&vm.client) {
            labels.push(label);
        }
        if super::panels_logic::has_changed_background_activity_settings(&vm.server) {
            labels.push("Background activity");
        }
        labels
    }

    pub(super) fn restore_defaults(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let labels = self.changed_setting_labels(cx);
        if labels.is_empty() {
            return;
        }
        let description = format!("This will reset: {}.", labels.join(", "));
        self.dialog = Some(super::dialog::Dialog::Confirm(super::dialog::ConfirmSpec {
            title: "Restore default settings?".into(),
            description: description.into(),
            confirm_label: "Restore".into(),
            destructive: true,
            on_confirm: std::rc::Rc::new(|this, _, cx| this.apply_restore_defaults(cx)),
        }));
        cx.notify();
    }

    fn apply_restore_defaults(&mut self, cx: &mut Context<Self>) {
        let json = self.json(cx);
        let mut client = serde_json::Map::new();
        let mut server = serde_json::Map::new();
        for (_, which, key) in RESTORABLE.iter().chain(TYPOGRAPHY_KEYS.iter()) {
            let target = match which {
                Which::Client => &mut client,
                Which::Server => &mut server,
            };
            target.insert((*key).to_owned(), json.default_of(*which, key).clone());
        }
        for key in ["backgroundActivity", "backgroundActivityProfile", "automaticGitFetchInterval", "providerHealthRefreshInterval"] {
            server.insert(key.to_owned(), json.default_of(Which::Server, key).clone());
        }
        self.update_client(serde_json::Value::Object(client), cx);
        self.update_server(serde_json::Value::Object(server), cx);
    }
}

use Which::{Client, Server};

/// The settings `restoreDefaults` resets, in upstream's label order. Fonts,
/// background activity and the theme are handled apart.
const RESTORABLE: &[(&str, Which, &str)] = &[
    ("Contrast", Client, "appearanceContrast"),
    ("Glass opacity", Client, "glassOpacity"),
    ("Diff colors", Client, "diffColorScheme"),
    ("Chat width", Client, "chatWidth"),
    ("Panel animations", Client, "panelAnimationDurationMs"),
    ("Environment identification", Client, "environmentIdentificationMode"),
    ("Time format", Client, "timestampFormat"),
    ("Thread notifications", Client, "notificationMode"),
    ("In-app notifications", Client, "inAppNotificationsEnabled"),
    ("Visible threads", Client, "sidebarThreadPreviewCount"),
    ("Project Grouping", Client, "sidebarProjectGroupingMode"),
    ("Project order", Client, "sidebarProjectSortOrder"),
    ("Working section", Client, "sidebarWorkingShelfEnabled"),
    ("Auto-settle inactive threads", Server, "sidebarAutoSettleAfterDays"),
    ("Keep threads with a PR", Server, "sidebarAutoSettleKeepPullRequests"),
    ("Auto-settle merged threads", Server, "sidebarAutoSettleOnMerge"),
    ("Auto-resume limited threads", Server, "autoResumeLimitedThreads"),
    ("Snooze limited threads", Server, "snoozeLimitedThreads"),
    ("Word wrap", Client, "wordWrap"),
    ("Composer context", Client, "persistComposerContextStrip"),
    ("Default diff file state", Client, "diffFilesCollapsed"),
    ("Diff whitespace changes", Client, "diffIgnoreWhitespace"),
    ("Diff layout", Client, "diffLayout"),
    ("Proactive panels", Client, "proactivePanelsEnabled"),
    ("Show skills in slash menu", Client, "showSkillsInSlashMenu"),
    ("Collapse composer on scroll", Client, "composerCollapseOnScroll"),
    ("Rich text composer", Client, "composerRichTextEnabled"),
    ("Send shortcut", Client, "sendShortcut"),
    ("Follow-up behavior", Client, "followUpBehavior"),
    ("Context window indicator", Client, "contextWindowMeterEnabled"),
    ("Response streaming", Server, "responseStreamingMode"),
    ("Provider update checks", Server, "enableProviderUpdateChecks"),
    ("Continue threads after restarts", Server, "continueThreadsAfterServerUpdate"),
    ("New thread mode", Server, "defaultThreadEnvMode"),
    ("New worktrees start from origin", Server, "newWorktreesStartFromOrigin"),
    ("Add project base directory", Server, "addProjectBaseDirectory"),
    ("Unpin confirmation", Client, "confirmThreadUnpin"),
    ("Archive confirmation", Client, "confirmThreadArchive"),
    ("Delete confirmation", Client, "confirmThreadDelete"),
    ("Quit shortcut", Client, "confirmQuit"),
    ("Text generation model", Server, "textGenerationModelSelection"),
];

/// Reset together with the restore button; labelled by `getChangedTypographySettingLabels`.
const TYPOGRAPHY_KEYS: &[(&str, Which, &str)] = &[
    ("", Client, "fontFamilySans"),
    ("", Client, "fontFamilyComposer"),
    ("", Client, "fontFamilyCode"),
    ("", Client, "fontFamilyTerminal"),
    ("", Client, "fontSizeInterface"),
    ("", Client, "fontSizePrompt"),
    ("", Client, "fontSizeCode"),
    ("", Client, "fontSizeTerminal"),
];

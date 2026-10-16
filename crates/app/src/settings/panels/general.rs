//! Port of `GeneralSettingsPanel` (upstream SettingsPanels.tsx) and
//! ProjectDefaultsSettings.tsx / NotificationSettings.tsx.

use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement as _, SharedString, Styled as _, Window, div,
    rems,
};
use serde_json::{Value, json};

use super::super::controls::{SelectWidth, switch};
use super::super::layout::{SettingsRow, SettingsSection};
use super::super::panels_logic::{
    BackgroundActivityProfileOption, is_project_grouping_enabled,
    project_grouping_mode_from_toggle, read_last_enabled_project_grouping_mode,
    remember_enabled_project_grouping_mode, resolve_background_activity_profile_option,
};
use super::super::project_defaults_settings::ProjectDefaultsCategory;
use super::super::rows::{BoolRow, Json, SelectRowSpec};
use super::super::search::searchable_setting;
use super::super::view::{SettingsView, Which};
use crate::theme::ActiveAppTheme;
use crate::theme::tokens::TEXT_SM;
use turin_contracts::settings::SidebarProjectGroupingMode;

type Opts = Vec<(Value, SharedString)>;

fn opts(items: &[(&str, &str)]) -> Opts {
    items
        .iter()
        .map(|(value, label)| (json!(value), SharedString::from(label.to_string())))
        .collect()
}

impl SettingsView {
    pub(super) fn render_general(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let json = self.json(cx);
        let vm = self.vm(cx);
        let modifier = vm.modifier_label();
        let _ = window;

        vec![
            self.project_defaults_section(ProjectDefaultsCategory::General, cx),
            self.organization_section(&json, window, cx),
            self.behavior_section(&json, modifier, cx),
            self.projects_and_threads_section(&json, window, cx),
            self.confirmations_section(&json, cx),
            self.text_generation_section(&json, cx),
            self.about_section(cx),
            self.legacy_features_section(&json, cx),
        ]
    }

    fn organization_section(
        &mut self,
        json: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mode_value = json
            .get(Which::Client, "sidebarProjectGroupingMode")
            .clone();
        let mode: SidebarProjectGroupingMode =
            serde_json::from_value(mode_value).unwrap_or(SidebarProjectGroupingMode::Repository);
        let grouping_switch = {
            let entity = cx.entity();
            switch(
                "switch-project-grouping",
                is_project_grouping_enabled(mode),
                false,
                cx,
                move |checked, _, cx| {
                    entity.update(cx, |this, cx| {
                        if !checked && mode != SidebarProjectGroupingMode::Separate {
                            remember_enabled_project_grouping_mode(mode);
                        }
                        let next = project_grouping_mode_from_toggle(
                            checked,
                            read_last_enabled_project_grouping_mode(),
                        );
                        this.update_client(json!({ "sidebarProjectGroupingMode": next }), cx);
                    });
                },
            )
        };
        let grouping = Self::base_row("project-grouping")
            .description("Combine matching repositories across environments.")
            .reset_action(self.reset_button(
                cx,
                json,
                Which::Client,
                "sidebarProjectGroupingMode",
                "project grouping",
            ))
            .control(grouping_switch)
            .into_any_element();

        let order = self.select_row(
            cx,
            json,
            SelectRowSpec {
                search_id: "project-order",
                description: "Order of projects in the sidebar project picker and command palette."
                    .into(),
                which: Which::Client,
                key: "sidebarProjectSortOrder",
                reset_label: "project order",
                options: opts(&[
                    ("updated_at", "Last user message"),
                    ("created_at", "Created at"),
                    ("manual", "Manual"),
                ]),
                width: SelectWidth::Fixed(11.),
                check: false,
            },
        );
        let mut section = SettingsSection::new("Organization")
            .id("organization")
            .child(grouping)
            .child(order);
        section = section
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "auto-resume-limited-threads",
                title: None,
                description: "Resume usage-limit stops at the reported reset time. Each thread can cancel its scheduled continuation.".into(),
                which: Which::Server,
                key: "autoResumeLimitedThreads",
                label: "Auto-resume limited threads",
                reset_label: "auto-resume limited threads",
            }))
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "snooze-limited-threads",
                title: None,
                description: "Snooze usage-limit stops until the reported reset time. Combine with auto-resume to continue when they wake.".into(),
                which: Which::Server,
                key: "snoozeLimitedThreads",
                label: "Snooze limited threads",
                reset_label: "snooze limited threads",
            }))
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "working-shelf",
                title: None,
                description: "Fold working and monitoring threads into a Working section. They return to the top of the inbox when they need you.".into(),
                which: Which::Client,
                key: "sidebarWorkingShelfEnabled",
                label: "Working section (beta)",
                reset_label: "working section",
            }))
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "auto-settle-merged-threads",
                title: None,
                description: "Settle a thread when its pull request merges. Closed pull requests still settle automatically.".into(),
                which: Which::Server,
                key: "sidebarAutoSettleOnMerge",
                label: "Auto-settle merged threads",
                reset_label: "auto-settle on merge",
            }));

        let auto_settle_days = json
            .get(Which::Server, "sidebarAutoSettleAfterDays")
            .clone();
        let auto_settle_on = !auto_settle_days.is_null();
        let days_text = match &auto_settle_days {
            Value::Number(n) => n.to_string(),
            _ => String::new(),
        };
        let days_input = self.input(
            "auto-settle-days",
            &days_text,
            "",
            window,
            cx,
            |this, text, _, cx| {
                // Number(), not parseInt: "3.5" must be rejected; the field snaps back on blur.
                if let Ok(days) = text.trim().parse::<f64>()
                    && days.fract() == 0.
                    && (1.0..=90.0).contains(&days)
                {
                    this.update(
                        Which::Server,
                        json!({ "sidebarAutoSettleAfterDays": days }),
                        cx,
                    );
                } else {
                    cx.notify();
                }
            },
        );
        let days_control = super::super::controls::text_field(
            &days_input,
            Some(rems(6.).to_pixels(gpui_kit::px(16.))),
            super::super::controls::FieldSize::Sm,
            false,
            cx,
        );
        let entity = cx.entity();
        let default_days = json
            .default_of(Which::Server, "sidebarAutoSettleAfterDays")
            .as_f64()
            .unwrap_or(3.);
        let inactive_switch = switch(
            "switch-auto-settle-inactive-threads",
            auto_settle_on,
            false,
            cx,
            move |checked, _, cx| {
                entity.update(cx, |this, cx| {
                this.update(Which::Server, json!({ "sidebarAutoSettleAfterDays": if checked { json!(default_days) } else { Value::Null } }), cx)
            });
            },
        );
        section = section.child(
            Self::base_row("auto-settle-inactive-threads")
                .description("Sidebar threads with no activity for this long settle automatically.")
                .reset_action(self.reset_button(
                    cx,
                    json,
                    Which::Server,
                    "sidebarAutoSettleAfterDays",
                    "auto-settle",
                ))
                .control(inactive_switch),
        );
        if auto_settle_on {
            section = section
                .child(
                    SettingsRow::new(searchable_setting("days-before-auto-settle").1)
                        .description("Any new activity un-settles a thread automatically.")
                        .control(days_control),
                )
                .child(self.bool_row(cx, json, BoolRow {
                    search_id: "auto-settle-keep-pull-requests",
                    title: Some("Keep threads with a PR"),
                    description: "Inactivity does not settle threads with a pull request. Merge and close still do.".into(),
                    which: Which::Server,
                    key: "sidebarAutoSettleKeepPullRequests",
                    label: "Keep threads with a PR",
                    reset_label: "keep threads with a PR",
                }));
        }
        section.into_any_element()
    }

    fn behavior_section(
        &mut self,
        json: &Json,
        modifier: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let send_shortcut = json.str(Which::Client, "sendShortcut").to_owned();
        let follow_up = format!(
            "Queue follow-ups while the agent runs or steer the current run. {}",
            if send_shortcut == "mod-enter-multiline" {
                format!(
                    "Press {modifier} + Enter for single-line prompts or {modifier} + Shift + Enter for multiline prompts to do the opposite for one message."
                )
            } else {
                format!(
                    "Press {modifier}{} + Enter to do the opposite for one message.",
                    if send_shortcut == "mod-enter" {
                        " + Shift"
                    } else {
                        ""
                    }
                )
            }
        );
        let streaming = json.str(Which::Server, "responseStreamingMode").to_owned();
        let streaming_description = match streaming.as_str() {
            "turn" => "Text appears once the agent finishes its turn.",
            _ => "Each paragraph or code block appears as soon as it is complete.",
        };
        let diff_collapsed = json.bool(Which::Client, "diffFilesCollapsed");
        let _ = diff_collapsed;

        let notifications = self.select_row(cx, json, SelectRowSpec {
            search_id: "thread-notifications",
            description: "System alerts when a thread finishes, fails, or needs input or approval. Applies to this device while T3 Code is open.".into(),
            which: Which::Client,
            key: "notificationMode",
            reset_label: "thread notifications",
            options: opts(&[
                ("off", "Off"),
                ("notifications", "Notifications only"),
                ("sound", "Sound only"),
                ("notifications-and-sound", "Notifications with sound"),
            ]),
            width: SelectWidth::Fixed(14.),
            check: false,
        });
        // NotificationSettings has no reset arrow of its own.
        let _ = notifications;
        let notification_row = {
            let current = json.str(Which::Client, "notificationMode").to_owned();
            let options = [
                ("off", "Off"),
                ("notifications", "Notifications only"),
                ("sound", "Sound only"),
                ("notifications-and-sound", "Notifications with sound"),
            ];
            let label = options
                .iter()
                .find(|(v, _)| *v == current)
                .map(|(_, l)| *l)
                .unwrap_or("");
            let control = self.select_control(
                cx,
                "thread-notifications",
                label.into(),
                options
                    .iter()
                    .map(|(v, l)| (json!(v), SharedString::from(*l), *v == current))
                    .collect(),
                SelectWidth::Fixed(14.),
                false,
                |this, value, _, cx| this.update_client(json!({ "notificationMode": value }), cx),
            );
            Self::base_row("thread-notifications")
                .description("System alerts when a thread finishes, fails, or needs input or approval. Applies to this device while T3 Code is open.")
                .control(control)
                .into_any_element()
        };

        let rows: Vec<AnyElement> = vec![
            notification_row,
            self.bool_row(cx, json, BoolRow {
                search_id: "in-app-notifications",
                title: None,
                description: "Show a toast when another thread finishes, fails, or needs input or approval while this app has focus.".into(),
                which: Which::Client,
                key: "inAppNotificationsEnabled",
                label: "In-app notifications",
                reset_label: "in-app notifications",
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "time-format",
                description: "System default follows your browser or OS clock preference.".into(),
                which: Which::Client,
                key: "timestampFormat",
                reset_label: "time format",
                options: opts(&[("locale", "System default"), ("12-hour", "12-hour"), ("24-hour", "24-hour")]),
                width: SelectWidth::Fixed(10.),
                check: false,
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "response-streaming",
                description: streaming_description.into(),
                which: Which::Server,
                key: "responseStreamingMode",
                reset_label: "response streaming",
                options: opts(&[("turn", "Wait for the full response"), ("paragraph", "Show finished paragraphs")]),
                width: SelectWidth::Fixed(14.),
                check: false,
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "hide-whitespace-changes",
                title: None,
                description: "Set whether the diff panel ignores whitespace-only edits by default.".into(),
                which: Which::Client,
                key: "diffIgnoreWhitespace",
                label: "Hide whitespace changes by default",
                reset_label: "diff whitespace changes",
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "default-diff-file-state",
                description: "Start with files expanded or collapsed when opening diffs or a pull request's Code tab.".into(),
                which: Which::Client,
                key: "diffFilesCollapsed",
                reset_label: "default diff file state",
                options: vec![(json!(false), "Expanded".into()), (json!(true), "Collapsed".into())],
                width: SelectWidth::Fixed(10.),
                check: false,
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "diff-layout",
                description: "Show diffs stacked or side by side. The toggle in the diff toolbar changes this too.".into(),
                which: Which::Client,
                key: "diffLayout",
                reset_label: "diff layout",
                options: opts(&[("stacked", "Stacked"), ("split", "Split")]),
                width: SelectWidth::Fixed(10.),
                check: false,
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "proactive-panels",
                title: None,
                description: "Open linked pull requests first. Otherwise, open Changes for edits to at least 3 files or 50 lines.".into(),
                which: Which::Client,
                key: "proactivePanelsEnabled",
                label: "Proactive panels",
                reset_label: "proactive panels",
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "skills-in-slash-menu",
                title: None,
                description: "Also include skills in the / command menu. Skills always appear when you type $.".into(),
                which: Which::Client,
                key: "showSkillsInSlashMenu",
                label: "Show skills in slash menu",
                reset_label: "skills in slash menu",
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "composer-rich-text",
                title: None,
                description: "Show formatted Markdown as you type.".into(),
                which: Which::Client,
                key: "composerRichTextEnabled",
                label: "Rich text composer",
                reset_label: "rich text composer",
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "composer-collapse",
                title: None,
                description: "Rest the composer of an existing thread into a single line when you scroll the conversation. Focus the composer or start typing to expand it again.".into(),
                which: Which::Client,
                key: "composerCollapseOnScroll",
                label: "Collapse composer on scroll",
                reset_label: "collapse composer on scroll",
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "send-shortcut",
                description: "Choose when Enter sends a prompt or inserts a new line".into(),
                which: Which::Client,
                key: "sendShortcut",
                reset_label: "send shortcut",
                options: vec![
                    (json!("enter"), "Enter".into()),
                    (json!("mod-enter-multiline"), format!("{modifier} + Enter for multiline prompts").into()),
                    (json!("mod-enter"), format!("{modifier} + Enter always").into()),
                ],
                width: SelectWidth::Auto,
                check: true,
            }),
            self.select_row(cx, json, SelectRowSpec {
                search_id: "follow-up-behavior",
                description: follow_up.into(),
                which: Which::Client,
                key: "followUpBehavior",
                reset_label: "follow-up behavior",
                options: opts(&[("queue", "Queue"), ("steer", "Steer")]),
                width: SelectWidth::Auto,
                check: false,
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "provider-update-checks",
                title: None,
                description: "Check installed provider CLIs for newer available versions and fetch model catalog updates. When off, new models may not appear.".into(),
                which: Which::Server,
                key: "enableProviderUpdateChecks",
                label: "Check provider versions",
                reset_label: "provider update checks",
            }),
            self.bool_row(cx, json, BoolRow {
                search_id: "continue-threads-after-server-update",
                title: None,
                description: "Automatically resume interrupted threads after an update, crash, or machine restart on the selected environments.".into(),
                which: Which::Server,
                key: "continueThreadsAfterServerUpdate",
                label: "Continue threads after restarts",
                reset_label: "continue threads after restarts",
            }),
            self.background_activity_row(json, cx),
        ];
        let mut section = SettingsSection::new("Behavior").id("behavior");
        for row in rows {
            section = section.child(row);
        }
        section.into_any_element()
    }

    fn background_activity_row(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let vm = self.vm(cx);
        let option = resolve_background_activity_profile_option(&vm.server);
        let resolved =
            turin_shared::server_settings::resolve_server_background_activity_settings(&vm.server);
        let label_of = |profile: turin_contracts::settings::BackgroundActivityProfile| match profile
        {
            turin_contracts::settings::BackgroundActivityProfile::Balanced => "Balanced",
            turin_contracts::settings::BackgroundActivityProfile::Performance => "Performance",
            turin_contracts::settings::BackgroundActivityProfile::BatterySaver => "Battery saver",
        };
        let description = match option {
            BackgroundActivityProfileOption::Advanced => format!(
                "Uses custom intervals. Shared policy: {}.",
                label_of(resolved.profile)
            ),
            BackgroundActivityProfileOption::Profile(profile) => match profile {
                turin_contracts::settings::BackgroundActivityProfile::Balanced => {
                    "Pauses probes for idle clients, locked hosts, or low power mode."
                }
                turin_contracts::settings::BackgroundActivityProfile::Performance => {
                    "Allows scoped background probes while any subscribed client remains connected."
                }
                turin_contracts::settings::BackgroundActivityProfile::BatterySaver => {
                    "Also pauses background probes when the host or client is on battery."
                }
            }
            .to_owned(),
        };
        let selected = match option {
            BackgroundActivityProfileOption::Advanced => "advanced".to_owned(),
            BackgroundActivityProfileOption::Profile(profile) => serde_json::to_value(profile)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default(),
        };
        let current_label = match option {
            BackgroundActivityProfileOption::Advanced => "Advanced",
            BackgroundActivityProfileOption::Profile(profile) => label_of(profile),
        };
        let control = self.select_control(
            cx,
            "background-activity",
            current_label.into(),
            vec![
                (json!("balanced"), "Balanced".into(), selected == "balanced"),
                (json!("performance"), "Performance".into(), selected == "performance"),
                (json!("battery-saver"), "Battery saver".into(), selected == "battery-saver"),
                (json!("advanced"), "Advanced".into(), selected == "advanced"),
            ],
            SelectWidth::Fixed(10.),
            false,
            |this, value, _, cx| {
                if value == json!("advanced") {
                    this.dialog = Some(super::super::dialog::Dialog::BackgroundActivity);
                    cx.notify();
                    return;
                }
                this.update_server(
                    json!({ "backgroundActivity": { "schemaVersion": 1, "profile": value, "overrides": {} } }),
                    cx,
                );
            },
        );
        let configure = matches!(option, BackgroundActivityProfileOption::Advanced).then(|| {
            crate::ui::Button::new("configure-background-activity")
                .variant(crate::ui::ButtonVariant::Outline)
                .size(crate::ui::ButtonSize::IconSm)
                .icon(crate::ui::IconName::Settings2)
                .tooltip(crate::ui::Tooltip::new("Configure background activity"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.dialog = Some(super::super::dialog::Dialog::BackgroundActivity);
                    cx.notify();
                }))
        });
        let control = div()
            .flex()
            .items_center()
            .gap_2()
            .child(control)
            .children(configure);
        let changed =
            super::super::panels_logic::has_changed_background_activity_settings(&vm.server);
        let reset = changed.then(|| {
            let action = self.cb(cx, |this, _, cx| {
                let default = this
                    .json(cx)
                    .default_of(Which::Server, "backgroundActivity")
                    .clone();
                this.update_server(json!({ "backgroundActivity": default }), cx);
            });
            super::super::layout::setting_reset_button(
                "reset-background-activity",
                "Reset to default",
                move |w, c| action(w, c),
            )
            .into_any_element()
        });
        let _ = json;
        let (id, title) = searchable_setting("background-activity");
        SettingsRow::new(title)
            .id(id)
            .description(description)
            .reset_action(reset)
            .control(control)
            .into_any_element()
    }

    fn projects_and_threads_section(
        &mut self,
        json: &Json,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let base_dir = json
            .str(Which::Server, "addProjectBaseDirectory")
            .to_owned();
        let input = self.input(
            "add-project-base-directory",
            &base_dir,
            "~/",
            window,
            cx,
            |this, text, _, cx| {
                this.update_server(json!({ "addProjectBaseDirectory": text.trim() }), cx);
            },
        );
        let input_control = super::super::controls::text_field(
            &input,
            Some(rems(18.).to_pixels(gpui_kit::px(16.))),
            super::super::controls::FieldSize::Sm,
            false,
            cx,
        );
        SettingsSection::new("Projects & threads")
            .id("projects-and-threads")
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "start-from-origin",
                title: None,
                description: "Creates the worktree from the latest matching branch on origin instead of your local branch.".into(),
                which: Which::Server,
                key: "newWorktreesStartFromOrigin",
                label: "Start new worktrees from origin by default",
                reset_label: "new worktrees start from origin",
            }))
            .child(
                Self::base_row("add-project-starts-in")
                    .description("Leave empty to use \"~/\" when the Add Project browser opens.")
                    .reset_action(self.reset_button(cx, json, Which::Server, "addProjectBaseDirectory", "add project base directory"))
                    .control(input_control),
            )
            .into_any_element()
    }

    fn confirmations_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let quit = self.select_row(
            cx,
            json,
            SelectRowSpec {
                search_id: "quit-confirmation",
                description: "Hold mode also quits on two quick presses.".into(),
                which: Which::Client,
                key: "confirmQuit",
                reset_label: "quit shortcut behavior",
                options: opts(&[
                    ("direct", "Direct"),
                    ("hold", "Hold"),
                    ("double-click", "Double press"),
                ]),
                width: SelectWidth::Fixed(10.),
                check: false,
            },
        );
        SettingsSection::new("Confirmations")
            .id("confirmations")
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "unpin-confirmation",
                title: None,
                description: "Ask before unpinning a thread from the pinned section.".into(),
                which: Which::Client,
                key: "confirmThreadUnpin",
                label: "Confirm thread unpinning",
                reset_label: "unpin confirmation",
            }))
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "archive-confirmation",
                title: None,
                description: "Require a second click on the inline archive action before a thread is archived.".into(),
                which: Which::Client,
                key: "confirmThreadArchive",
                label: "Confirm thread archiving",
                reset_label: "archive confirmation",
            }))
            .child(self.bool_row(cx, json, BoolRow {
                search_id: "delete-confirmation",
                title: None,
                description: "Ask before deleting a thread and its chat history.".into(),
                which: Which::Client,
                key: "confirmThreadDelete",
                label: "Confirm thread deletion",
                reset_label: "delete confirmation",
            }))
            .child(quit)
            .into_any_element()
    }

    fn text_generation_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let _ = json;
        SettingsSection::new("Text generation")
            .id("text-generation")
            .child(self.text_generation_model_row(cx))
            .into_any_element()
    }

    fn about_section(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let vm = self.vm(cx);
        let version = vm.environment.app_version.clone();
        let (id, title) = searchable_setting("app-version");
        let title = div().flex().items_baseline().gap_2().child(title).child(
            div()
                .font_family(theme.fonts.mono.clone())
                .text_size(crate::theme::tokens::TEXT_2XS.size)
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(theme.muted_foreground)
                .child(version),
        );
        let licenses = {
            use crate::ui::{Button, ButtonSize, ButtonVariant};
            Self::base_row("open-source-licenses")
                .description(
                    "Notices for dependencies, assets, and optional tools used by T3 Code.",
                )
                .control(
                    Button::new("view-licenses")
                        .variant(ButtonVariant::Outline)
                        .size(ButtonSize::Sm)
                        .label("View licenses")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.set_route(
                                super::super::view::SettingsRoute::OpenSourceLicenses,
                                cx,
                            )
                        })),
                )
        };
        SettingsSection::new("About")
            .id("about")
            .child(
                SettingsRow::new(title)
                    .id(id)
                    .description("Current version of the application."),
            )
            .child(licenses)
            .into_any_element()
    }

    fn legacy_features_section(&mut self, json: &Json, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.app_theme().clone();
        let open = self.panels.legacy_open;
        let header = {
            use gpui_kit::{InteractiveElement as _, StatefulInteractiveElement as _};
            div()
                .id("legacy-features-trigger")
                .group("legacy-features")
                .flex()
                .min_h_8()
                .w_full()
                .items_center()
                .gap_2()
                .px_4()
                .cursor_pointer()
                .on_click(cx.listener(|this, _, _, cx| {
                    this.panels.legacy_open = !this.panels.legacy_open;
                    cx.notify();
                }))
                .child(
                    div()
                        .text_size(TEXT_SM.size)
                        .text_color(gpui_kit::Hsla::from(crate::theme::color::with_alpha(
                            theme.foreground,
                            0.7,
                        )))
                        .group_hover("legacy-features", |style| {
                            style.text_color(theme.foreground)
                        })
                        .child("Legacy features"),
                )
                .child(
                    crate::ui::Icon::new(if open {
                        crate::ui::IconName::ChevronDown
                    } else {
                        crate::ui::IconName::ChevronRight
                    })
                    .color(theme.muted_foreground),
                )
        };
        let mut section = div().flex().flex_col().gap(rems(0.625)).child(header);
        if open {
            section = section.child(super::super::layout::settings_group(
                vec![
                    self.bool_row(cx, json, BoolRow {
                        search_id: "legacy-plan-mode",
                        title: None,
                        description: "Restore Build/Plan, /plan, /default, and Shift+Tab. Off uses build mode.".into(),
                        which: Which::Client,
                        key: "planModeEnabled",
                        label: "Plan mode (legacy)",
                        reset_label: "",
                    }),
                    self.bool_row(cx, json, BoolRow {
                        search_id: "legacy-context-window-indicator",
                        title: None,
                        description: "Shows context window usage as a circular indicator in the composer.".into(),
                        which: Which::Client,
                        key: "contextWindowMeterEnabled",
                        label: "Context window indicator (legacy)",
                        reset_label: "",
                    }),
                    self.bool_row(cx, json, BoolRow {
                        search_id: "legacy-sidebar",
                        title: None,
                        description: "Restore per-project thread trees instead of the default flat sidebar.".into(),
                        which: Which::Client,
                        key: "legacySidebarEnabled",
                        label: "Sidebar (legacy)",
                        reset_label: "",
                    }),
                ],
                cx,
            ));
        }
        section.into_any_element()
    }
}

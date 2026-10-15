//! Fixture-backed [`ComposerSource`]: a provider catalogue, workspace paths
//! and threads, and local handling of every action, so the composer can be
//! run and screenshotted before T14's reducers land.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui_kit::{App, AppContext as _, Entity};

use crate::composer::catalog::{ProviderInstanceEntry, ProviderSkill, ProviderSlashCommand};
use crate::composer::provider_state::ProviderModel;
use crate::composer::command_items::PathKind;
use crate::composer::model_picker::provider_model_key;
use crate::composer::prompt_history::ComposerPromptHistoryMessage;
use crate::composer::provider_options::{ModelCapabilities, OptionChoice, OptionDescriptor, OptionSelection, OptionValue};
use crate::composer::view_model::{
    ComposerSource, ComposerStore, ComposerViewModel, ContextWindowSnapshot, InteractionMode, PathEntry, RuntimeMode,
    SendRequest, SessionPhase, ThreadRef,
};

fn choice(id: &str, label: &str, is_default: bool) -> OptionChoice {
    OptionChoice {
        id: id.into(),
        label: label.into(),
        description: None,
        is_default,
    }
}

fn effort(options: &[(&str, &str)], default: &str) -> OptionDescriptor {
    OptionDescriptor::Select {
        id: "effort".into(),
        label: "Reasoning".into(),
        description: None,
        options: options.iter().map(|(id, label)| choice(id, label, *id == default)).collect(),
        current_value: None,
        prompt_injected_values: Vec::new(),
    }
}

fn fast_mode() -> OptionDescriptor {
    OptionDescriptor::Boolean {
        id: "fastMode".into(),
        label: "Fast Mode".into(),
        description: None,
        current_value: None,
    }
}

fn model(slug: &str, name: &str, descriptors: Vec<OptionDescriptor>) -> ProviderModel {
    ProviderModel {
        capabilities: Some(ModelCapabilities { option_descriptors: descriptors }),
        ..ProviderModel::simple(slug, name)
    }
}

fn skills() -> Vec<ProviderSkill> {
    vec![
        ProviderSkill {
            short_description: Some("Review the current diff for bugs".into()),
            scope: Some("user".into()),
            ..ProviderSkill::new("code-review")
        },
        ProviderSkill {
            short_description: Some("Cut AI tells from writing".into()),
            scope: Some("user".into()),
            ..ProviderSkill::new("unslop")
        },
        ProviderSkill {
            short_description: Some("Distinctive, production-grade interfaces".into()),
            scope: Some("repo".into()),
            ..ProviderSkill::new("frontend-design")
        },
    ]
}

/// The fixture catalogue.
pub fn fixture_providers() -> Vec<ProviderInstanceEntry> {
    let codex_effort = effort(&[("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Extra High")], "medium");
    let mut codex = ProviderInstanceEntry::new("codex", "codex", "Codex");
    codex.models = vec![
        ProviderModel {
            is_default: true,
            ..model("gpt-5.4", "GPT-5.4", vec![codex_effort.clone(), fast_mode()])
        },
        ProviderModel {
            is_new: true,
            ..model("gpt-5.3-codex", "GPT-5.3 Codex", vec![codex_effort.clone()])
        },
        model("gpt-5.3-codex-spark", "GPT-5.3 Codex Spark", vec![codex_effort.clone()]),
        ProviderModel {
            is_legacy: true,
            ..model("gpt-5.2", "GPT-5.2", vec![codex_effort])
        },
    ];
    codex.skills = skills();
    codex.slash_commands = vec![
        ProviderSlashCommand {
            name: "compact".into(),
            description: Some("Compact the conversation".into()),
            input_hint: None,
        },
        ProviderSlashCommand {
            name: "review".into(),
            description: Some("Review the working tree".into()),
            input_hint: None,
        },
    ];
    codex.reports_context_window = true;
    codex.show_interaction_mode_toggle = true;

    let claude_effort = effort(&[("low", "Low"), ("medium", "Medium"), ("high", "High"), ("max", "Max")], "high");
    let mut claude = ProviderInstanceEntry::new("claudeAgent", "claudeAgent", "Claude");
    claude.models = vec![
        ProviderModel {
            is_default: true,
            ..model("claude-opus-5-5", "Claude Opus 5.5", vec![claude_effort.clone()])
        },
        model("claude-sonnet-5-5", "Claude Sonnet 5.5", vec![claude_effort.clone()]),
        model("claude-haiku-5-5", "Claude Haiku 5.5", vec![]),
    ];
    claude.skills = skills();
    claude.reports_context_window = true;
    claude.show_interaction_mode_toggle = true;

    let mut opencode = ProviderInstanceEntry::new("opencode", "opencode", "OpenCode");
    opencode.models = vec![
        ProviderModel {
            sub_provider: Some("OpenCode Zen".into()),
            ..model("opencode/big-pickle", "OpenCode Zen: Big Pickle", vec![])
        },
        ProviderModel {
            sub_provider: Some("GitHub Copilot".into()),
            ..model("github-copilot/claude-opus-4.7", "GitHub Copilot: Claude Opus 4.7", vec![])
        },
    ];

    let mut cursor = ProviderInstanceEntry::new("cursor", "cursor", "Cursor");
    cursor.models = vec![model("auto", "Auto", vec![]), model("composer-2", "Composer 2", vec![fast_mode()])];
    vec![codex, claude, opencode, cursor]
}

fn fixture_paths() -> Vec<PathEntry> {
    [
        ("crates/app/src/main.rs", PathKind::File),
        ("crates/app/src/composer", PathKind::Directory),
        ("crates/app/src/composer/editor_state.rs", PathKind::File),
        ("crates/app/Cargo.toml", PathKind::File),
        ("README.md", PathKind::File),
        ("AGENTS.md", PathKind::File),
        ("PORTING.md", PathKind::File),
        ("docs/screenshots", PathKind::Directory),
    ]
    .into_iter()
    .map(|(path, kind)| PathEntry { path: path.into(), kind })
    .collect()
}

pub fn fixture_view_model() -> ComposerViewModel {
    let providers = fixture_providers();
    ComposerViewModel {
        thread_key: "local:thread-1".into(),
        selected_instance_id: "codex".into(),
        selected_model: "gpt-5.4".into(),
        providers,
        favorite_models: [provider_model_key("claudeAgent", "claude-opus-5-5")].into_iter().collect(),
        prompt_history_messages: vec![
            ComposerPromptHistoryMessage {
                id: "m1".into(),
                role: "user".into(),
                text: "Port the design system".into(),
            },
            ComposerPromptHistoryMessage {
                id: "m2".into(),
                role: "user".into(),
                text: "Now the sidebar, with snooze and drag".into(),
            },
        ],
        context_window: Some(ContextWindowSnapshot {
            used_tokens: 61_000.,
            max_tokens: Some(272_000.),
            total_processed_tokens: Some(143_000.),
            compacts_automatically: true,
            auto_compact_threshold: None,
            cost: None,
        }),
        reserve_context_window_meter: true,
        active_thread_model_display_name: Some("GPT-5.4".into()),
        path_entries: fixture_paths(),
        threads: vec![
            ThreadRef {
                thread_id: "thread-2".into(),
                title: "Port design system and app shell".into(),
            },
            ThreadRef {
                thread_id: "thread-3".into(),
                title: "Sidebar status indicators".into(),
            },
        ],
        shortcut_labels: [
            ("composer.sendAlternate".to_owned(), "⌘⇧↵".to_owned()),
            ("composer.stash".to_owned(), "⌘S".to_owned()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    }
}

/// Applies the user's intent to a local copy of the view model.
pub struct MockComposerSource {
    store: Entity<ComposerStore>,
    sent: Arc<Mutex<Vec<SendRequest>>>,
}

impl MockComposerSource {
    pub fn start(cx: &mut App) -> Entity<ComposerStore> {
        let store = cx.new(|_| ComposerStore::new(fixture_view_model()));
        let source = Arc::new(Self {
            store: store.clone(),
            sent: Arc::default(),
        });
        store.update(cx, |store, _| store.set_source(source));
        store
    }

    pub fn sent(&self) -> Vec<SendRequest> {
        self.sent.lock().map(|sent| sent.clone()).unwrap_or_default()
    }

    fn update(&self, cx: &mut App, edit: impl FnOnce(&mut ComposerViewModel)) {
        self.store.update(cx, |store, cx| {
            let mut view_model = (**store.view_model()).clone();
            edit(&mut view_model);
            store.publish(view_model, cx);
        });
    }
}

impl ComposerSource for MockComposerSource {
    fn send(&self, request: SendRequest, cx: &mut App) -> bool {
        if let Ok(mut sent) = self.sent.lock() {
            sent.push(request.clone());
        }
        if request.answering_pending_user_input || request.editing_queued_run.is_some() {
            return true;
        }
        // The turn runs for a moment, then settles.
        self.update(cx, |vm| {
            vm.phase = SessionPhase::Running;
            vm.can_interrupt = true;
        });
        let store = self.store.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = store.update(cx, |store, cx| {
                let mut view_model = (**store.view_model()).clone();
                view_model.phase = SessionPhase::Ready;
                view_model.can_interrupt = false;
                store.publish(view_model, cx);
            });
        })
        .detach();
        true
    }

    fn interrupt(&self, cx: &mut App) {
        self.update(cx, |vm| {
            vm.phase = SessionPhase::Ready;
            vm.can_interrupt = false;
        });
    }

    fn resume(&self, _cx: &mut App) {}

    fn implement_plan_in_new_thread(&self, _cx: &mut App) {}

    fn select_model(&self, instance_id: &str, model: &str, cx: &mut App) {
        self.update(cx, |vm| {
            vm.selected_instance_id = instance_id.to_owned();
            vm.selected_model = model.to_owned();
            vm.selected_model_options.clear();
        });
    }

    fn set_model_options(&self, instance_id: &str, model: &str, options: Option<Vec<OptionSelection>>, cx: &mut App) {
        self.update(cx, |vm| {
            vm.selected_instance_id = instance_id.to_owned();
            vm.selected_model = model.to_owned();
            vm.selected_model_options = options.unwrap_or_default();
        });
    }

    fn set_runtime_mode(&self, mode: RuntimeMode, cx: &mut App) {
        self.update(cx, |vm| vm.runtime_mode = mode);
    }

    fn set_interaction_mode(&self, mode: InteractionMode, cx: &mut App) {
        self.update(cx, |vm| vm.interaction_mode = mode);
    }

    fn toggle_favorite_model(&self, instance_id: &str, model: &str, cx: &mut App) {
        let key = provider_model_key(instance_id, model);
        self.update(cx, |vm| {
            if !vm.favorite_models.remove(&key) {
                vm.favorite_models.insert(key);
            }
        });
    }

    fn open_provider_setup(&self, _instance_id: &str, _cx: &mut App) {}

    fn toggle_keep_full_history(&self, cx: &mut App) {
        self.update(cx, |vm| vm.keep_full_history = !vm.keep_full_history);
    }

    fn compact_context(&self, _cx: &mut App) {}

    fn dismiss_banner(&self, id: &str, cx: &mut App) {
        let id = id.to_owned();
        self.update(cx, |vm| vm.banners.retain(|banner| banner.id != id));
    }

    fn banner_action(&self, _banner_id: &str, _action_id: &str, _cx: &mut App) {}

    fn edit_queued_run(&self, run_id: &str, cx: &mut App) {
        let run_id = run_id.to_owned();
        self.update(cx, |vm| {
            if let Some(queue) = &mut vm.queue {
                queue.editing_run_id = Some(run_id);
            }
        });
    }

    fn cancel_queued_edit(&self, cx: &mut App) {
        self.update(cx, |vm| {
            if let Some(queue) = &mut vm.queue {
                queue.editing_run_id = None;
            }
        });
    }

    fn cancel_queued_run(&self, run_id: &str, cx: &mut App) {
        let run_id = run_id.to_owned();
        self.update(cx, |vm| {
            if let Some(queue) = &mut vm.queue {
                queue.queued.retain(|run| run.run_id != run_id);
                if queue.queued.is_empty() {
                    vm.queue = None;
                }
            }
        });
    }

    fn promote_queued_run(&self, _run_id: &str, _cx: &mut App) {}

    fn reorder_queued_run(&self, run_id: &str, to_index: usize, cx: &mut App) {
        let run_id = run_id.to_owned();
        self.update(cx, |vm| {
            if let Some(queue) = &mut vm.queue
                && let Some(from) = queue.queued.iter().position(|run| run.run_id == run_id)
            {
                let run = queue.queued.remove(from);
                queue.queued.insert(to_index.min(queue.queued.len()), run);
            }
        });
    }

    fn respond_to_pending_input(&self, _request_id: &str, _answer: &str, _cx: &mut App) {}

    fn previous_pending_question(&self, _cx: &mut App) {}

    fn search_paths(&self, _query: &str, _cx: &mut App) {}
}

#[allow(dead_code)]
fn _option_value(_: OptionValue) {}

/// Debug scenarios (`TURIN_COMPOSER_DEMO`): extra state layered over the
/// fixture so each drawer can be screenshotted.
pub fn apply_demo_scenario(scenario: &str, vm: &mut ComposerViewModel) {
    use crate::composer::view_model::{BannerItem, BannerPriority, BannerVariant, QueueState, QueuedRun};
    use crate::ui::IconName;
    match scenario {
        "banners" => {
            vm.banners = vec![
                BannerItem {
                    id: "usage".into(),
                    variant: BannerVariant::Info,
                    priority: Some(BannerPriority::Notice),
                    icon: IconName::Gauge,
                    title: "Usage limits".into(),
                    description: Some("Codex · Pro".into()),
                    dismissible: true,
                    dismiss_label: Some("Dismiss usage limits".into()),
                    actions: Vec::new(),
                },
                BannerItem {
                    id: "update".into(),
                    variant: BannerVariant::Warning,
                    priority: Some(BannerPriority::Urgent),
                    icon: IconName::TriangleAlert,
                    title: "Claude needs an update".into(),
                    description: Some("Update the CLI to keep using this provider.".into()),
                    dismissible: true,
                    dismiss_label: None,
                    actions: vec![("Provider settings".into(), "open-settings".into())],
                },
            ];
        }
        "queue" => {
            vm.phase = SessionPhase::Running;
            vm.can_interrupt = true;
            vm.queue = Some(QueueState {
                queued: vec![
                    QueuedRun {
                        run_id: "run-1".into(),
                        message_id: "m1".into(),
                        text: "Also add a test for the empty-prompt case".into(),
                        image_names: Vec::new(),
                        file_names: Vec::new(),
                        pending: false,
                    },
                    QueuedRun {
                        run_id: "run-2".into(),
                        message_id: "m2".into(),
                        text: "Then update the changelog".into(),
                        image_names: Vec::new(),
                        file_names: Vec::new(),
                        pending: true,
                    },
                ],
                can_reorder: true,
                editing_run_id: None,
            });
        }
        "plan" => {
            vm.show_plan_follow_up_prompt = true;
            vm.plan_follow_up_title = Some("Port the composer to GPUI".into());
        }
        _ => {}
    }
}

//! The seam between the orchestration read model and the composer.
//!
//! [`ComposerStore`] is a GPUI model holding the current
//! [`ComposerViewModel`] (upstream's `ChatComposerProps`); the composer view
//! observes it. Whatever feeds the store implements [`ComposerSource`]: it
//! publishes view models and receives the user's intent (send, stop, model
//! picks, queue edits…) — the callbacks of `ChatComposerProps`.
//!
//! T14 replaces [`MockComposerSource`](super::mock_source::MockComposerSource)
//! with the real reducers: build the view model from the thread projection,
//! provider snapshots and settings, and call [`ComposerStore::publish`]; route
//! the `ComposerSource` calls to the orchestrator.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui_kit::{App, Context};

use crate::composer::catalog::ProviderInstanceEntry;
use crate::composer::command_items::{PathKind, PullRequestMeta};
use crate::composer::dispatch::{ActiveTurnComposerAction, ComposerDispatchMode};
use crate::composer::draft_store::{ComposerFileAttachment, ComposerImageAttachment};
use crate::composer::logic::{ComposerSubmissionIntent, SendShortcut};
use crate::composer::prompt_history::ComposerPromptHistoryMessage;
use crate::composer::provider_options::OptionSelection;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SessionPhase {
    Disconnected,
    Connecting,
    #[default]
    Ready,
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    ApprovalRequired,
    AutoAcceptEdits,
    #[default]
    FullAccess,
}

impl RuntimeMode {
    pub fn wire(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval-required",
            Self::AutoAcceptEdits => "auto-accept-edits",
            Self::FullAccess => "full-access",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        Some(match value {
            "approval-required" => Self::ApprovalRequired,
            "auto-accept-edits" => Self::AutoAcceptEdits,
            "full-access" => Self::FullAccess,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InteractionMode {
    #[default]
    Default,
    Plan,
}

/// The settings the composer reads (`UnifiedSettings` subset).
#[derive(Debug, Clone)]
pub struct ComposerSettings {
    pub send_shortcut: SendShortcut,
    pub follow_up_behavior: ActiveTurnComposerAction,
    pub composer_rich_text_enabled: bool,
    pub context_window_meter_enabled: bool,
    pub show_skills_in_slash_menu: bool,
    pub plan_mode_enabled: bool,
}

impl Default for ComposerSettings {
    fn default() -> Self {
        Self {
            send_shortcut: SendShortcut::Enter,
            follow_up_behavior: ActiveTurnComposerAction::Steer,
            composer_rich_text_enabled: true,
            context_window_meter_enabled: true,
            show_skills_in_slash_menu: true,
            plan_mode_enabled: true,
        }
    }
}

/// `ContextWindowSnapshot` (lib/contextWindow.ts), the fields the meter shows.
#[derive(Debug, Clone, PartialEq)]
pub struct ContextWindowSnapshot {
    pub used_tokens: f64,
    pub max_tokens: Option<f64>,
    pub total_processed_tokens: Option<f64>,
    pub compacts_automatically: bool,
}

impl ContextWindowSnapshot {
    /// Used share of the window (0–100), when the window size is known.
    pub fn used_percentage(&self) -> Option<f64> {
        self.max_tokens.filter(|max| *max > 0.).map(|max| (self.used_tokens / max * 100.).clamp(0., 100.))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerVariant {
    Default,
    Error,
    Info,
    Success,
    Warning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerPriority {
    Urgent,
    Activity,
    Notice,
}

/// `ComposerBannerStackItem` (data only; the view renders icon and actions).
#[derive(Debug, Clone, PartialEq)]
pub struct BannerItem {
    pub id: String,
    pub variant: BannerVariant,
    pub priority: Option<BannerPriority>,
    pub icon: crate::ui::IconName,
    pub title: String,
    pub description: Option<String>,
    pub dismissible: bool,
    pub dismiss_label: Option<String>,
    /// Action buttons (label, id) the source resolves on click.
    pub actions: Vec<(String, String)>,
}

/// A queued run behind the active turn (`QueuedRunsControl`).
#[derive(Debug, Clone, PartialEq)]
pub struct QueuedRun {
    pub run_id: String,
    pub message_id: String,
    pub text: String,
    pub image_names: Vec<String>,
    pub file_names: Vec<String>,
    /// Sent but not yet acknowledged by the server.
    pub pending: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct QueueState {
    pub queued: Vec<QueuedRun>,
    pub can_reorder: bool,
    pub editing_run_id: Option<String>,
}

/// A workspace entry offered for `@` completion.
#[derive(Debug, Clone, PartialEq)]
pub struct PathEntry {
    pub path: String,
    pub kind: PathKind,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRef {
    pub thread_id: String,
    pub title: String,
}

/// The pending user-input answer the composer is writing (`activePendingProgress`).
#[derive(Debug, Clone, PartialEq)]
pub struct PendingAnswer {
    pub request_id: String,
    pub custom_answer: String,
    pub is_choice_only: bool,
    pub is_responding: bool,
    pub question_index: usize,
    pub is_last_question: bool,
    pub can_advance: bool,
    pub is_complete: bool,
}

/// What `ChatComposerProps` carries into the composer.
#[derive(Debug, Clone)]
pub struct ComposerViewModel {
    // Identity.
    pub thread_key: String,
    pub environment_id: String,
    pub is_draft_thread: bool,
    pub is_existing_thread: bool,

    // Session phase.
    pub phase: SessionPhase,
    pub can_operate_thread: bool,
    pub can_interrupt: bool,
    pub is_connecting: bool,
    pub is_send_busy: bool,
    pub can_resume: bool,
    pub send_disabled_reason: Option<String>,
    pub is_preparing_worktree: bool,
    pub project_selection_required: bool,
    pub environment_unavailable: Option<String>,

    // Modes.
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,

    // Provider / model.
    pub providers: Vec<ProviderInstanceEntry>,
    pub provider_catalog_known: bool,
    pub locked_provider: Option<String>,
    pub locked_continuation_group_key: Option<String>,
    pub selected_instance_id: String,
    pub selected_model: String,
    pub selected_model_options: Vec<OptionSelection>,
    /// Display-only values the running session reports.
    pub reported_model_options: Option<Vec<OptionSelection>>,
    /// `provider_model_key`s of the user's favourite models.
    pub favorite_models: HashSet<String>,

    pub settings: ComposerSettings,
    pub prompt_history_messages: Vec<ComposerPromptHistoryMessage>,
    pub context_window: Option<ContextWindowSnapshot>,
    pub reserve_context_window_meter: bool,
    pub active_thread_model_display_name: Option<String>,

    // Drawers and strips.
    pub banners: Vec<BannerItem>,
    pub queue: Option<QueueState>,
    pub show_plan_follow_up_prompt: bool,
    pub plan_follow_up_title: Option<String>,
    pub pending_answer: Option<PendingAnswer>,
    pub has_pending_approval: bool,
    pub resume_compaction_tokens: Option<f64>,
    pub keep_full_history: bool,

    // Attachment capabilities.
    pub attachment_uploads_capability_known: bool,
    pub supports_attachment_uploads: bool,
    pub max_file_attachment_bytes: Option<u64>,

    // Completion sources.
    pub path_entries: Vec<PathEntry>,
    pub path_search_pending: bool,
    pub threads: Vec<ThreadRef>,
    pub pull_requests: Vec<PullRequestMeta>,

    /// Shortcut labels the composer shows, keyed by keybinding command.
    pub shortcut_labels: HashMap<String, String>,
}

impl Default for ComposerViewModel {
    fn default() -> Self {
        Self {
            thread_key: String::new(),
            environment_id: "local".into(),
            is_draft_thread: false,
            is_existing_thread: true,
            phase: SessionPhase::Ready,
            can_operate_thread: true,
            can_interrupt: false,
            is_connecting: false,
            is_send_busy: false,
            can_resume: false,
            send_disabled_reason: None,
            is_preparing_worktree: false,
            project_selection_required: false,
            environment_unavailable: None,
            runtime_mode: RuntimeMode::FullAccess,
            interaction_mode: InteractionMode::Default,
            providers: Vec::new(),
            provider_catalog_known: true,
            locked_provider: None,
            locked_continuation_group_key: None,
            selected_instance_id: String::new(),
            selected_model: String::new(),
            selected_model_options: Vec::new(),
            reported_model_options: None,
            favorite_models: HashSet::new(),
            settings: ComposerSettings::default(),
            prompt_history_messages: Vec::new(),
            context_window: None,
            reserve_context_window_meter: false,
            active_thread_model_display_name: None,
            banners: Vec::new(),
            queue: None,
            show_plan_follow_up_prompt: false,
            plan_follow_up_title: None,
            pending_answer: None,
            has_pending_approval: false,
            resume_compaction_tokens: None,
            keep_full_history: false,
            attachment_uploads_capability_known: true,
            supports_attachment_uploads: true,
            max_file_attachment_bytes: Some(50 * 1024 * 1024),
            path_entries: Vec::new(),
            path_search_pending: false,
            threads: Vec::new(),
            pull_requests: Vec::new(),
            shortcut_labels: HashMap::new(),
        }
    }
}

impl ComposerViewModel {
    pub fn selected_entry(&self) -> Option<&ProviderInstanceEntry> {
        self.providers.iter().find(|entry| entry.instance_id == self.selected_instance_id)
    }

    pub fn no_provider_available(&self) -> bool {
        self.provider_catalog_known && self.selected_entry().is_none_or(|entry| !entry.picker_ready() && entry.models.is_empty())
    }
}

/// Everything `onSend` needs (`getSendContext`).
#[derive(Debug, Clone)]
pub struct SendRequest {
    pub prompt: String,
    pub images: Vec<ComposerImageAttachment>,
    pub files: Vec<ComposerFileAttachment>,
    pub instance_id: String,
    pub model: String,
    pub model_options: Option<Vec<OptionSelection>>,
    pub interaction_mode: InteractionMode,
    pub runtime_mode: RuntimeMode,
    pub dispatch_mode: ComposerDispatchMode,
    pub submission_intent: ComposerSubmissionIntent,
    /// The send answers the pending user input instead of starting a turn.
    pub answering_pending_user_input: bool,
    /// Edits the queued message instead of sending.
    pub editing_queued_run: Option<String>,
}

/// The user's intent, routed to whatever owns the thread.
pub trait ComposerSource: 'static {
    /// `onSend`. Returns false when the send boundary rejects the draft
    /// (the composer keeps it).
    fn send(&self, request: SendRequest, cx: &mut App) -> bool;
    fn interrupt(&self, cx: &mut App);
    fn resume(&self, cx: &mut App);
    fn implement_plan_in_new_thread(&self, cx: &mut App);
    fn select_model(&self, instance_id: &str, model: &str, cx: &mut App);
    fn set_model_options(&self, instance_id: &str, model: &str, options: Option<Vec<OptionSelection>>, cx: &mut App);
    fn set_runtime_mode(&self, mode: RuntimeMode, cx: &mut App);
    fn set_interaction_mode(&self, mode: InteractionMode, cx: &mut App);
    fn toggle_favorite_model(&self, instance_id: &str, model: &str, cx: &mut App);
    fn open_provider_setup(&self, instance_id: &str, cx: &mut App);
    fn toggle_keep_full_history(&self, cx: &mut App);
    fn compact_context(&self, cx: &mut App);
    fn dismiss_banner(&self, id: &str, cx: &mut App);
    fn banner_action(&self, banner_id: &str, action_id: &str, cx: &mut App);
    fn edit_queued_run(&self, run_id: &str, cx: &mut App);
    fn cancel_queued_edit(&self, cx: &mut App);
    fn cancel_queued_run(&self, run_id: &str, cx: &mut App);
    fn promote_queued_run(&self, run_id: &str, cx: &mut App);
    fn reorder_queued_run(&self, run_id: &str, to_index: usize, cx: &mut App);
    fn respond_to_pending_input(&self, request_id: &str, answer: &str, cx: &mut App);
    fn previous_pending_question(&self, cx: &mut App);
    /// Debounced path search for `@` completion.
    fn search_paths(&self, query: &str, cx: &mut App);
}

/// The observable view-model holder.
pub struct ComposerStore {
    view_model: Arc<ComposerViewModel>,
    source: Option<Arc<dyn ComposerSource>>,
}

impl ComposerStore {
    pub fn new(view_model: ComposerViewModel) -> Self {
        Self {
            view_model: Arc::new(view_model),
            source: None,
        }
    }

    pub fn view_model(&self) -> &Arc<ComposerViewModel> {
        &self.view_model
    }

    pub fn source(&self) -> Option<Arc<dyn ComposerSource>> {
        self.source.clone()
    }

    pub fn set_source(&mut self, source: Arc<dyn ComposerSource>) {
        self.source = Some(source);
    }

    /// Replace the view model and notify observers.
    pub fn publish(&mut self, view_model: ComposerViewModel, cx: &mut Context<Self>) {
        self.view_model = Arc::new(view_model);
        cx.notify();
    }
}

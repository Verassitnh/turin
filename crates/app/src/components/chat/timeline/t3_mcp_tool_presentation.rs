//! Port of upstream/packages/shared/src/t3McpToolPresentation.ts
//!
//! The tool table keeps every T3 MCP tool upstream ships, including the ones
//! for features Turin cuts (preview browser, devices, scheduled tasks, HTML
//! render). They are only labels here, and keeping them means a T3 database
//! imported into Turin still brands those historic calls correctly.

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::base_schemas::js_trim;

/// `T3McpToolLogo`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum T3McpToolLogo {
    T3Code,
}

impl T3McpToolLogo {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::T3Code => "t3-code",
        }
    }
}

/// `T3McpToolPresentation`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct T3McpToolPresentation {
    pub display_name: &'static str,
    pub logo: T3McpToolLogo,
}

/// `T3McpToolSummaryAction`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum T3McpToolSummaryAction {
    Capabilities,
    Delegate,
    TaskStatus,
    TaskCancel,
    ScheduleRun,
    ScheduleCreate,
    ScheduleList,
    ScheduleUpdate,
    ScheduleDelete,
    ThreadCreate,
    ThreadList,
    ThreadRead,
    ThreadSend,
    ThreadWait,
    ThreadInterrupt,
    WorkflowStop,
    ThreadConfiguration,
    ThreadConfigure,
    ThreadFork,
    ThreadMerge,
    ThreadSearch,
    ThreadTransfers,
    ThreadOrganize,
    ThreadUpdate,
    QueueList,
    QueueRead,
    QueueEdit,
    QueueCancel,
    QueueReorder,
    QueueSteer,
    QuestionList,
    QuestionRead,
    QuestionRespond,
    SecretRequest,
    WorktreeHandoff,
    WorktreeList,
    WorktreeStatus,
    ProjectList,
    ProjectRead,
    ProjectCreate,
    ProjectUpdate,
    ProjectDelete,
    ProjectClone,
    EnvironmentRead,
    EnvironmentUpdate,
    AttachmentPrepare,
    AttachmentDiscard,
    AttachmentSend,
    LinkPr,
    UnlinkPr,
    ListPrs,
    WatchPr,
    UnwatchPr,
    WritePrGuides,
    ReadPrGuide,
    Browser,
    Device,
    HtmlPreview,
    HtmlRender,
}

impl T3McpToolSummaryAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Capabilities => "capabilities",
            Self::Delegate => "delegate",
            Self::TaskStatus => "task-status",
            Self::TaskCancel => "task-cancel",
            Self::ScheduleRun => "schedule-run",
            Self::ScheduleCreate => "schedule-create",
            Self::ScheduleList => "schedule-list",
            Self::ScheduleUpdate => "schedule-update",
            Self::ScheduleDelete => "schedule-delete",
            Self::ThreadCreate => "thread-create",
            Self::ThreadList => "thread-list",
            Self::ThreadRead => "thread-read",
            Self::ThreadSend => "thread-send",
            Self::ThreadWait => "thread-wait",
            Self::ThreadInterrupt => "thread-interrupt",
            Self::WorkflowStop => "workflow-stop",
            Self::ThreadConfiguration => "thread-configuration",
            Self::ThreadConfigure => "thread-configure",
            Self::ThreadFork => "thread-fork",
            Self::ThreadMerge => "thread-merge",
            Self::ThreadSearch => "thread-search",
            Self::ThreadTransfers => "thread-transfers",
            Self::ThreadOrganize => "thread-organize",
            Self::ThreadUpdate => "thread-update",
            Self::QueueList => "queue-list",
            Self::QueueRead => "queue-read",
            Self::QueueEdit => "queue-edit",
            Self::QueueCancel => "queue-cancel",
            Self::QueueReorder => "queue-reorder",
            Self::QueueSteer => "queue-steer",
            Self::QuestionList => "question-list",
            Self::QuestionRead => "question-read",
            Self::QuestionRespond => "question-respond",
            Self::SecretRequest => "secret-request",
            Self::WorktreeHandoff => "worktree-handoff",
            Self::WorktreeList => "worktree-list",
            Self::WorktreeStatus => "worktree-status",
            Self::ProjectList => "project-list",
            Self::ProjectRead => "project-read",
            Self::ProjectCreate => "project-create",
            Self::ProjectUpdate => "project-update",
            Self::ProjectDelete => "project-delete",
            Self::ProjectClone => "project-clone",
            Self::EnvironmentRead => "environment-read",
            Self::EnvironmentUpdate => "environment-update",
            Self::AttachmentPrepare => "attachment-prepare",
            Self::AttachmentDiscard => "attachment-discard",
            Self::AttachmentSend => "attachment-send",
            Self::LinkPr => "link-pr",
            Self::UnlinkPr => "unlink-pr",
            Self::ListPrs => "list-prs",
            Self::WatchPr => "watch-pr",
            Self::UnwatchPr => "unwatch-pr",
            Self::WritePrGuides => "write-pr-guides",
            Self::ReadPrGuide => "read-pr-guide",
            Self::Browser => "browser",
            Self::Device => "device",
            Self::HtmlPreview => "html-preview",
            Self::HtmlRender => "html-render",
        }
    }
}

/// `T3McpToolDefinition["icon"]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum T3McpToolIcon {
    T3Code,
    Browser,
    Device,
    PullRequest,
}

impl T3McpToolIcon {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::T3Code => "t3-code",
            Self::Browser => "browser",
            Self::Device => "device",
            Self::PullRequest => "pull-request",
        }
    }
}

/// `T3McpToolDefinition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct T3McpToolDefinition {
    pub display_name: &'static str,
    /// `[action, running, completed, detail]`.
    pub labels: [&'static str; 4],
    pub icon: T3McpToolIcon,
    pub summary_action: T3McpToolSummaryAction,
}

/// Upstream's `tool(labels, summaryAction, icon = "t3-code", displayName =
/// `${labels[0]} ${labels[3]}`)` helper, evaluated at compile time.
macro_rules! tool {
    ($name:literal, [$a:literal, $r:literal, $c:literal, $d:literal], $action:ident) => {
        tool!($name, [$a, $r, $c, $d], $action, T3Code)
    };
    ($name:literal, [$a:literal, $r:literal, $c:literal, $d:literal], $action:ident, $icon:ident) => {
        tool!(
            $name,
            [$a, $r, $c, $d],
            $action,
            $icon,
            concat!($a, " ", $d)
        )
    };
    (
        $name:literal,
        [$a:literal, $r:literal, $c:literal, $d:literal],
        $action:ident,
        $icon:ident,
        $display:expr
    ) => {
        (
            $name,
            T3McpToolDefinition {
                display_name: $display,
                labels: [$a, $r, $c, $d],
                icon: T3McpToolIcon::$icon,
                summary_action: T3McpToolSummaryAction::$action,
            },
        )
    };
}

const T3_MCP_SERVER_ALIASES: [&str; 3] = ["t3-code", "t3_code", "t3code"];

// Cards, activity rows, summaries, and provider identity recovery share this inventory.
const T3_MCP_TOOLS: &[(&str, T3McpToolDefinition)] = &[
    tool!(
        "link_pull_request",
        ["Link", "Linking", "Linked", "a pull request"],
        LinkPr,
        PullRequest
    ),
    tool!(
        "unlink_pull_request",
        ["Unlink", "Unlinking", "Unlinked", "a pull request"],
        UnlinkPr,
        PullRequest
    ),
    tool!(
        "list_thread_pull_requests",
        ["Check", "Checking", "Checked", "linked pull requests"],
        ListPrs,
        PullRequest
    ),
    tool!(
        "watch_pull_request",
        ["Watch", "Watching", "Watching", "a pull request"],
        WatchPr,
        PullRequest
    ),
    tool!(
        "unwatch_pull_request",
        [
            "Stop watching",
            "Stopping watching",
            "Stopped watching",
            "a pull request"
        ],
        UnwatchPr,
        PullRequest
    ),
    tool!(
        "write_pull_request_guides",
        ["Write", "Writing", "Started", "review guides"],
        WritePrGuides,
        PullRequest
    ),
    tool!(
        "read_pull_request_guide",
        ["Read", "Reading", "Read", "a review guide"],
        ReadPrGuide,
        PullRequest
    ),
    tool!(
        "orchestrator_capabilities",
        ["Get", "Getting", "Got", "orchestration capabilities"],
        Capabilities
    ),
    tool!(
        "delegate_task",
        ["Delegate", "Delegating", "Delegated", "a child task"],
        Delegate
    ),
    tool!(
        "task_status",
        ["Get", "Getting", "Got", "delegated task status"],
        TaskStatus
    ),
    tool!(
        "task_cancel",
        [
            "Cancel",
            "Canceling",
            "Requested cancellation of",
            "delegated task"
        ],
        TaskCancel
    ),
    tool!(
        "schedule_task",
        ["Schedule", "Scheduling", "Scheduled", "a recurring task"],
        ScheduleCreate
    ),
    tool!(
        "list_scheduled_tasks",
        ["List", "Listing", "Listed", "scheduled tasks"],
        ScheduleList
    ),
    tool!(
        "update_scheduled_task",
        ["Update", "Updating", "Updated", "a scheduled task"],
        ScheduleUpdate
    ),
    tool!(
        "delete_scheduled_task",
        [
            "Delete",
            "Deleting",
            "Requested deletion of",
            "a scheduled task"
        ],
        ScheduleDelete
    ),
    tool!(
        "request_secret",
        ["Ask for", "Asking for", "Asked for", "a secret"],
        SecretRequest
    ),
    tool!(
        "create_threads",
        ["Create", "Creating", "Created", "T3 threads"],
        ThreadCreate
    ),
    tool!(
        "t3_thread_start",
        ["Start", "Starting", "Started", "a T3 thread"],
        ThreadCreate
    ),
    tool!(
        "t3_thread_list",
        ["List", "Listing", "Listed", "T3 threads"],
        ThreadList
    ),
    tool!(
        "t3_thread_read",
        ["Read", "Reading", "Read", "a T3 thread"],
        ThreadRead
    ),
    tool!(
        "t3_thread_send",
        ["Send", "Sending", "Sent", "to a T3 thread"],
        ThreadSend
    ),
    tool!(
        "t3_thread_wait",
        ["Wait", "Waiting", "Waited", "for a T3 thread"],
        ThreadWait
    ),
    tool!(
        "t3_workflow_stop",
        ["Stop", "Stopping", "Requested a stop for", "a workflow"],
        WorkflowStop
    ),
    tool!(
        "t3_thread_interrupt",
        [
            "Interrupt",
            "Interrupting",
            "Requested an interrupt of",
            "a T3 thread"
        ],
        ThreadInterrupt
    ),
    tool!(
        "t3_worktree_handoff",
        [
            "Hand off",
            "Handing off",
            "Handed off",
            "thread to a git worktree"
        ],
        WorktreeHandoff
    ),
    tool!(
        "t3_worktree_status",
        ["Get", "Getting", "Got", "thread worktree status"],
        WorktreeStatus
    ),
    tool!(
        "preview_status",
        ["Get", "Getting", "Got", "preview browser status"],
        Browser,
        Browser
    ),
    tool!(
        "preview_open",
        ["Open", "Opening", "Opened", "a page in the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_navigate",
        ["Navigate", "Navigating", "Navigated", "the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_dialog",
        [
            "Respond",
            "Responding",
            "Responded",
            "to a preview browser dialog"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_snapshot",
        [
            "Take a snapshot of",
            "Taking a snapshot of",
            "Took a snapshot of",
            "the preview page"
        ],
        Browser,
        Browser,
        "Snapshot the preview page"
    ),
    tool!(
        "preview_click",
        ["Click", "Clicking", "Clicked", "in the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_press",
        [
            "Press",
            "Pressing",
            "Pressed",
            "a key in the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_type",
        ["Type", "Typing", "Typed", "in the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_hover",
        ["Hover", "Hovering", "Hovered", "in the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_select",
        [
            "Choose",
            "Choosing",
            "Chose",
            "an option in the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_drag",
        ["Drag", "Dragging", "Dragged", "in the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_upload",
        [
            "Upload",
            "Uploading",
            "Uploaded",
            "files to the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_scroll",
        ["Scroll", "Scrolling", "Scrolled", "the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_resize",
        ["Resize", "Resizing", "Resized", "the preview browser"],
        Browser,
        Browser
    ),
    tool!(
        "preview_evaluate",
        [
            "Evaluate",
            "Evaluating",
            "Evaluated",
            "script in the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_wait_for",
        ["Wait", "Waiting", "Waited", "for the preview page"],
        Browser,
        Browser
    ),
    tool!(
        "preview_set_appearance",
        ["Set", "Setting", "Set", "preview browser appearance"],
        Browser,
        Browser
    ),
    tool!(
        "preview_recording_start",
        [
            "Start",
            "Starting",
            "Started",
            "recording the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "preview_recording_stop",
        [
            "Stop",
            "Stopping",
            "Stopped",
            "recording the preview browser"
        ],
        Browser,
        Browser
    ),
    tool!(
        "device_list",
        ["List", "Listing", "Listed", "simulators and emulators"],
        Device,
        Device
    ),
    tool!(
        "device_open",
        ["Open", "Opening", "Opened", "a device in the Device panel"],
        Device,
        Device
    ),
    tool!(
        "device_screenshot",
        [
            "Take a screenshot of",
            "Taking a screenshot of",
            "Took a screenshot of",
            "the device"
        ],
        Device,
        Device
    ),
    tool!(
        "device_close",
        ["Close", "Closing", "Closed", "a device"],
        Device,
        Device
    ),
    tool!(
        "run_scheduled_task_now",
        ["Run", "Running", "Requested a run of", "a scheduled task"],
        ScheduleRun
    ),
    tool!(
        "t3_queue_list",
        ["List", "Listing", "Listed", "queued messages"],
        QueueList
    ),
    tool!(
        "t3_queue_read",
        ["Read", "Reading", "Read", "a queued message"],
        QueueRead
    ),
    tool!(
        "t3_queue_edit",
        ["Edit", "Editing", "Edited", "a queued message"],
        QueueEdit
    ),
    tool!(
        "t3_queue_cancel",
        [
            "Cancel",
            "Canceling",
            "Requested cancellation of",
            "a queued run"
        ],
        QueueCancel
    ),
    tool!(
        "t3_queue_reorder",
        ["Reorder", "Reordering", "Reordered", "a queued run"],
        QueueReorder
    ),
    tool!(
        "t3_queue_promote_to_steer",
        [
            "Steer with",
            "Steering with",
            "Requested steering with",
            "a queued message"
        ],
        QueueSteer
    ),
    tool!(
        "t3_pending_request_list",
        ["List", "Listing", "Listed", "pending questions"],
        QuestionList
    ),
    tool!(
        "t3_pending_request_read",
        ["Read", "Reading", "Read", "pending questions"],
        QuestionRead
    ),
    tool!(
        "t3_pending_request_respond",
        ["Answer", "Answering", "Answered", "pending questions"],
        QuestionRespond
    ),
    tool!(
        "t3_thread_configuration",
        ["Read", "Reading", "Read", "thread configuration"],
        ThreadConfiguration
    ),
    tool!(
        "t3_thread_configure",
        ["Set", "Setting", "Set", "thread model"],
        ThreadConfigure
    ),
    tool!(
        "t3_thread_fork",
        ["Fork", "Forking", "Requested a fork of", "this thread"],
        ThreadFork
    ),
    tool!(
        "t3_thread_merge_back",
        ["Merge", "Merging", "Requested a merge of", "thread context"],
        ThreadMerge
    ),
    tool!(
        "t3_thread_search",
        ["Search", "Searching", "Searched", "thread content"],
        ThreadSearch
    ),
    tool!(
        "t3_thread_transfers",
        ["Read", "Reading", "Read", "thread transfers"],
        ThreadTransfers
    ),
    tool!(
        "t3_thread_organize",
        ["Organize", "Organizing", "Organized", "a thread"],
        ThreadOrganize
    ),
    tool!(
        "t3_thread_update",
        ["Update", "Updating", "Updated", "T3 thread metadata"],
        ThreadUpdate
    ),
    tool!(
        "t3_worktree_list",
        ["List", "Listing", "Listed", "workspace branches"],
        WorktreeList
    ),
    tool!(
        "t3_preview_list",
        ["List", "Listing", "Listed", "preview tabs"],
        Browser,
        Browser
    ),
    tool!(
        "t3_preview_close",
        ["Close", "Closing", "Closed", "a preview tab"],
        Browser,
        Browser
    ),
    tool!(
        "t3_environment_read",
        ["Read", "Reading", "Read", "environment preferences"],
        EnvironmentRead
    ),
    tool!(
        "t3_environment_preferences_update",
        ["Update", "Updating", "Updated", "environment preferences"],
        EnvironmentUpdate
    ),
    tool!(
        "t3_thread_launch",
        ["Launch", "Launching", "Launched", "a project thread"],
        ThreadCreate
    ),
    tool!(
        "t3_project_list",
        ["List", "Listing", "Listed", "projects"],
        ProjectList
    ),
    tool!(
        "t3_project_read",
        ["Read", "Reading", "Read", "a project"],
        ProjectRead
    ),
    tool!(
        "t3_project_create",
        ["Register", "Registering", "Registered", "a project"],
        ProjectCreate
    ),
    tool!(
        "t3_project_update",
        ["Update", "Updating", "Updated", "a project"],
        ProjectUpdate
    ),
    tool!(
        "t3_project_delete",
        ["Delete", "Deleting", "Deleted", "a project"],
        ProjectDelete
    ),
    tool!(
        "t3_project_clone",
        ["Clone", "Cloning", "Cloned", "a repository"],
        ProjectClone
    ),
    tool!(
        "t3_attachment_prepare_upload",
        ["Prepare", "Preparing", "Prepared", "an attachment upload"],
        AttachmentPrepare
    ),
    tool!(
        "t3_attachment_discard",
        ["Discard", "Discarding", "Discarded", "a pending attachment"],
        AttachmentDiscard
    ),
    tool!(
        "t3_thread_send_attachments",
        ["Send", "Sending", "Sent", "attachments"],
        AttachmentSend
    ),
    tool!(
        "html_preview",
        ["Preview", "Previewing", "Previewed", "an HTML page"],
        HtmlPreview
    ),
    tool!(
        "html_render",
        ["Render", "Rendering", "Rendered", "an HTML page"],
        HtmlRender
    ),
];

const T3_MCP_TOOL_COUNT: usize = T3_MCP_TOOLS.len();

/// The T3 orchestration tool inventory, used to gate loose name matching on
/// both the server (ACP MCP identity recovery) and the client (logo branding).
///
/// Upstream is a `ReadonlySet`; here it is the table's keys in table order
/// (use `.contains(&name)` for `.has(name)`).
pub const T3_MCP_TOOL_NAMES: [&str; T3_MCP_TOOL_COUNT] = {
    let mut names = [""; T3_MCP_TOOL_COUNT];
    let mut index = 0;
    while index < T3_MCP_TOOL_COUNT {
        names[index] = T3_MCP_TOOLS[index].0;
        index += 1;
    }
    names
};

/// `Object.hasOwn(T3_MCP_TOOLS, name) ? T3_MCP_TOOLS[name] : …`.
fn lookup_t3_mcp_tool(name: &str) -> Option<&'static (&'static str, T3McpToolDefinition)> {
    T3_MCP_TOOLS.iter().find(|(tool, _)| *tool == name)
}

static COMPLETION_SUFFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\s+(?:complete|completed)\s*$").expect("valid completion suffix regex")
});
static MCP_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^mcp__(?P<server>.+?)__(?P<tool>.+)$").expect("valid mcp name regex")
});
static NAMESPACED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?P<server>t3-code|t3_code|t3code)(?:[.:/]|\s*·\s*)(?P<tool>.+)$")
        .expect("valid namespaced name regex")
});
static PREFIXED_NAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:mcp[-_]{1,2})?t3[-_ ]?code(?:__|[-_.:/ ])(?P<tool>.+)$")
        .expect("valid prefixed name regex")
});

fn normalize_t3_mcp_tool_label(value: &str) -> &str {
    let without_suffix = match COMPLETION_SUFFIX.find(value) {
        Some(found) => &value[..found.start()],
        None => value,
    };
    js_trim(without_suffix)
}

/// ACP agents disagree on how the injected T3 server prefixes its tools:
/// `mcp__t3-code__x` (Claude/Cursor), `t3-code.x` (Codex), plus single
/// underscore, colon, slash, dash, and space separators seen from registry
/// agents. The prefix match is deliberately loose because the display-name
/// inventory is the real gate; unknown tools stay on the generic renderer.
fn resolve_t3_mcp_tool_name(value: &str) -> Option<&str> {
    let label = normalize_t3_mcp_tool_label(value);
    if let Some(captures) = MCP_NAME.captures(label) {
        let server = captures.name("server")?.as_str();
        let tool = captures.name("tool")?.as_str();
        return T3_MCP_SERVER_ALIASES
            .contains(&server.to_lowercase().as_str())
            .then_some(tool);
    }

    if let Some(captures) = NAMESPACED_NAME.captures(label) {
        return captures.name("tool").map(|tool| tool.as_str());
    }

    let candidate = PREFIXED_NAME
        .captures(label)
        .and_then(|captures| captures.name("tool"))
        .map_or(label, |tool| tool.as_str());
    if lookup_t3_mcp_tool(candidate).is_some() {
        return Some(candidate);
    }
    // OpenCode 2 registers one server per thread, `t3-code-<thread>`, and joins
    // it to the tool with `_`. Thread ids can hold `_` too, so take the longest
    // known tool name that ends the label.
    if !label
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("t3-code-"))
    {
        return None;
    }
    let mut longest: Option<&'static str> = None;
    for (tool, _) in T3_MCP_TOOLS {
        let ends_label = label
            .strip_suffix(tool)
            .is_some_and(|rest| rest.ends_with('_'));
        if ends_label && tool.len() > longest.map_or(0, str::len) {
            longest = Some(tool);
        }
    }
    longest
}

fn resolve_t3_mcp_tool_entry(
    tool_name: Option<&str>,
) -> Option<&'static (&'static str, T3McpToolDefinition)> {
    lookup_t3_mcp_tool(resolve_t3_mcp_tool_name(tool_name?)?)
}

/// The bare T3 tool name (`html_render`) for any provider's spelling of it.
pub fn resolve_t3_mcp_tool_id(tool_name: Option<&str>) -> Option<&'static str> {
    resolve_t3_mcp_tool_entry(tool_name).map(|(name, _)| *name)
}

pub fn resolve_t3_mcp_tool_definition(
    tool_name: Option<&str>,
) -> Option<&'static T3McpToolDefinition> {
    resolve_t3_mcp_tool_entry(tool_name).map(|(_, definition)| definition)
}

pub fn resolve_t3_mcp_tool_presentation(tool_name: Option<&str>) -> Option<T3McpToolPresentation> {
    resolve_t3_mcp_tool_definition(tool_name).map(|definition| T3McpToolPresentation {
        display_name: definition.display_name,
        logo: T3McpToolLogo::T3Code,
    })
}

pub fn resolve_t3_mcp_tool_summary_action(
    tool_name: Option<&str>,
) -> Option<T3McpToolSummaryAction> {
    resolve_t3_mcp_tool_definition(tool_name).map(|definition| definition.summary_action)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn presentation(display_name: &'static str) -> Option<T3McpToolPresentation> {
        Some(T3McpToolPresentation {
            display_name,
            logo: T3McpToolLogo::T3Code,
        })
    }

    #[test]
    fn recognizes_every_t3_tool_across_provider_prefixes_and_completion_suffixes() {
        for tool in T3_MCP_TOOL_NAMES {
            let expected = resolve_t3_mcp_tool_presentation(Some(tool));
            assert!(expected.is_some(), "{tool}");
            for prefix in [
                "mcp__t3-code__",
                "mcp__t3_code__",
                "mcp__t3code__",
                "T3-code.",
                "t3_code/",
                "t3code:",
                "mcp_t3-code_",
                "T3 Code ",
                "t3-code · ",
            ] {
                assert_eq!(
                    resolve_t3_mcp_tool_presentation(Some(&format!("{prefix}{tool} completed"))),
                    expected,
                    "{tool}"
                );
            }
            assert_eq!(
                resolve_t3_mcp_tool_presentation(Some(&format!("mcp__another-server__{tool}"))),
                None,
                "{tool}"
            );
        }
    }

    #[test]
    fn pretty_prints_claude_and_cursor_t3_mcp_tool_names() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("mcp__t3-code__t3_thread_read")),
            presentation("Read a T3 thread")
        );
    }

    #[test]
    fn pretty_prints_codex_t3_mcp_tool_names() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("t3-code.create_threads")),
            presentation("Create T3 threads")
        );
    }

    #[test]
    fn pretty_prints_thread_metadata_updates() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("mcp__t3-code__t3_thread_update")),
            presentation("Update T3 thread metadata")
        );
    }

    #[test]
    fn pretty_prints_bare_t3_mcp_toolkit_names() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("list_scheduled_tasks")),
            presentation("List scheduled tasks")
        );
    }

    #[test]
    fn pretty_prints_worktree_t3_mcp_tool_names() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("mcp__t3-code__t3_worktree_handoff")),
            presentation("Hand off thread to a git worktree")
        );
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("t3-code.t3_worktree_status")),
            presentation("Get thread worktree status")
        );
    }

    #[test]
    fn pretty_prints_preview_t3_mcp_tool_names() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("T3-code.preview_open")),
            presentation("Open a page in the preview browser")
        );
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("mcp__t3-code__preview_status")),
            presentation("Get preview browser status")
        );
    }

    #[test]
    fn matches_the_separator_variants_acp_registry_agents_emit() {
        for name in [
            "mcp_t3-code_delegate_task",
            "t3_code:delegate_task",
            "t3code/delegate_task",
            "t3-code delegate_task",
            "T3 Code delegate_task",
            "t3-code__delegate_task",
        ] {
            assert_eq!(
                resolve_t3_mcp_tool_presentation(Some(name)).map(|p| p.display_name),
                Some("Delegate a child task"),
                "{name}"
            );
        }
    }

    #[test]
    fn matches_opencode_2_per_thread_server_names_whose_thread_ids_hold_underscores() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some(
                "t3-code-thread_opencode2-adapter_delegate_task"
            ))
            .map(|p| p.display_name),
            Some("Delegate a child task")
        );
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("t3-code-thread_opencode2-adapter_not_a_tool")),
            None
        );
    }

    #[test]
    fn keeps_unknown_mcp_tools_on_the_generic_renderer_path() {
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("mcp__github__search_issues")),
            None
        );
        assert_eq!(
            resolve_t3_mcp_tool_presentation(Some("t3-code.not_a_real_tool")),
            None
        );
    }
}

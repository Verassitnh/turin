//! Port of upstream/apps/web/src/rightPanelStore.ts (+ test): the thread-scoped
//! right-panel surface state, its user-versus-automatic revision guard, the
//! inline/popover visibility pair, the closed-tab log, and the v1–v14
//! persisted-shape migration.
//!
//! Upstream is a zustand store. Here it is a plain struct whose mutators take
//! `&mut self`; the zustand selectors are methods. Upstream returns the same
//! object when nothing changed and tests check identity. Value equality stands
//! in for that. Persistence I/O (`persist` to localStorage under
//! `t3code:right-panel-state:v2`) belongs to the settings task. The struct
//! exposes `to_persisted_value` and `load_persisted` so the owner can read and
//! write the JSON shape in one call.
//!
//! Surfaces are cut to the kept set (PORTING.md cut list): `diff`,
//! `pull-requests`, `pull-request:*`, and `file`/`attachment`. The browser
//! (`preview`), device, and terminal surfaces and the `files` explorer are
//! dropped, both in the actions and in the migration. The closed-tab log keeps
//! upstream's `closedViewStore.remember` (newest first, same target replaced,
//! capped at 20). Its persistence and its browser and terminal entries are not
//! ported.

// Consumers are wired by the shell and settings tasks (T20/T22).
#![allow(dead_code)]

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use turin_contracts::chat_attachment::ChatFileAttachment;
use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ThreadId};

use crate::state::helpers::shared_support::encode_uri_component;

/// `RIGHT_PANEL_STORAGE_VERSION`. Versions v1–v14 are read by
/// `migrate_persisted_right_panel_state`.
pub const RIGHT_PANEL_STORAGE_VERSION: u32 = 14;
/// `RIGHT_PANEL_STORAGE_KEY`. Kept for the settings task that owns the file.
pub const RIGHT_PANEL_STORAGE_KEY: &str = "t3code:right-panel-state:v2";
const PULL_REQUESTS_PANEL_ID: &str = "pull-requests-panel";
const CLOSED_VIEW_LIMIT: usize = 20;

/// `PULL_REQUESTS_PANEL_REF`: a fixed workspace-level ref. Each PR surface
/// carries its own real environment.
pub fn pull_requests_panel_ref() -> ScopedThreadRef {
    ScopedThreadRef {
        environment_id: EnvironmentId::new(PULL_REQUESTS_PANEL_ID).expect("static id is non-blank"),
        thread_id: ThreadId::new(PULL_REQUESTS_PANEL_ID).expect("static id is non-blank"),
    }
}

/// `scopedThreadKey`.
pub fn thread_key(thread: &ScopedThreadRef) -> String {
    format!("{}:{}", thread.environment_id, thread.thread_id)
}

/// `isPullRequestsPanelKey`: the list's shared panel is session state.
fn is_pull_requests_panel_key(key: &str) -> bool {
    key.ends_with(":pull-requests-panel")
}

/// `RightPanelKind` restricted to the kept kinds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RightPanelKind {
    Diff,
    PullRequests,
    PullRequest,
    File,
}

impl RightPanelKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::PullRequests => "pull-requests",
            Self::PullRequest => "pull-request",
            Self::File => "file",
        }
    }
}

/// The singleton kinds `open` and `toggle` take (`Exclude<RightPanelKind, ...>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SingletonKind {
    Diff,
    PullRequests,
}

/// `PullRequestTarget`: the fields of a change-request reference, without the
/// surface id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestTarget {
    /// Which server the change request was read from. Left out beside a thread,
    /// where the environment comes from the thread's own ref.
    pub environment_id: Option<String>,
    pub project_id: String,
    pub host: Option<String>,
    pub repository: String,
    pub number: u64,
    pub url: Option<String>,
}

/// `PullRequestSurface`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestSurface {
    pub id: String,
    pub target: PullRequestTarget,
}

/// `file` and `attachment` surfaces. `attachment` is present when the file
/// lives in the thread's attachment store rather than at a workspace path.
#[derive(Clone, Debug, PartialEq)]
pub struct FileSurface {
    pub id: String,
    /// Workspace-relative, or the attachment name.
    pub relative_path: String,
    pub reveal_line: Option<f64>,
    pub reveal_request_id: u64,
    pub attachment: Option<ChatFileAttachment>,
}

/// `RightPanelSurface`, restricted to the kept kinds.
#[derive(Clone, Debug, PartialEq)]
pub enum RightPanelSurface {
    Diff,
    PullRequests,
    PullRequest(PullRequestSurface),
    File(FileSurface),
}

impl RightPanelSurface {
    pub fn id(&self) -> String {
        match self {
            Self::Diff => "diff".to_owned(),
            Self::PullRequests => "pull-requests".to_owned(),
            Self::PullRequest(surface) => surface.id.clone(),
            Self::File(surface) => surface.id.clone(),
        }
    }

    pub fn kind(&self) -> RightPanelKind {
        match self {
            Self::Diff => RightPanelKind::Diff,
            Self::PullRequests => RightPanelKind::PullRequests,
            Self::PullRequest(_) => RightPanelKind::PullRequest,
            Self::File(_) => RightPanelKind::File,
        }
    }
}

/// The surfaces `openProactive` takes (`Extract<RightPanelSurface, { kind:
/// "diff" | "pull-request" | "pull-requests" }>`).
#[derive(Clone, Debug, PartialEq)]
pub enum ProactiveSurface {
    Diff,
    PullRequest(PullRequestSurface),
    PullRequests,
}

impl ProactiveSurface {
    fn into_surface(self) -> RightPanelSurface {
        match self {
            Self::Diff => RightPanelSurface::Diff,
            Self::PullRequest(surface) => RightPanelSurface::PullRequest(surface),
            Self::PullRequests => RightPanelSurface::PullRequests,
        }
    }
}

/// `ThreadRightPanelState`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadRightPanelState {
    pub is_open: bool,
    pub active_surface_id: Option<String>,
    pub surfaces: Vec<RightPanelSurface>,
    /// Kept with the thread so the layout survives a remount or a reload.
    /// Upstream stores `maximized?: true`, so `false` here means absent.
    pub maximized: bool,
}

pub use crate::state::right_panel_layout::ThreadPanelPresentation;

/// `ThreadPanelVisibility`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadPanelVisibility {
    pub inline_open: bool,
    pub popover_open: bool,
}

impl Default for ThreadPanelVisibility {
    /// `DEFAULT_THREAD_PANEL_VISIBILITY`.
    fn default() -> Self {
        Self {
            inline_open: true,
            popover_open: false,
        }
    }
}

/// `ClosedView` (panel-tab entries only).
#[derive(Clone, Debug, PartialEq)]
pub struct ClosedPanelTab {
    pub id: String,
    pub thread_ref: ScopedThreadRef,
    pub surface: RightPanelSurface,
}

impl ClosedPanelTab {
    pub fn new(thread_ref: ScopedThreadRef, surface: RightPanelSurface) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            thread_ref,
            surface,
        }
    }
}

/// `PersistedRightPanelState`: what `migratePersistedRightPanelState` returns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PersistedRightPanelState {
    pub by_thread_key: IndexMap<String, ThreadRightPanelState>,
    pub thread_panel_visibility_by_thread_key: IndexMap<String, ThreadPanelVisibility>,
}

/// The right-panel store.
#[derive(Clone, Debug, Default)]
pub struct RightPanelStore {
    pub by_thread_key: IndexMap<String, ThreadRightPanelState>,
    /// A thread whose right panel should open maximized the next time it shows.
    pub pending_maximize_thread_key: Option<String>,
    pub thread_panel_visibility_by_thread_key: IndexMap<String, ThreadPanelVisibility>,
    /// Session-only count of user panel choices per thread. Automatic updates
    /// do not advance it.
    pub user_action_revision_by_thread_key: IndexMap<String, u64>,
    /// Newest first. Stands in for `useClosedViewStore`'s panel-tab entries.
    pub closed_panel_tabs: Vec<ClosedPanelTab>,
}

/// `pullRequestSurfaceId`.
pub fn pull_request_surface_id(target: &PullRequestTarget) -> String {
    // The environment leads the id where there is one, so the same change
    // request read from two servers is two tabs, not one tab that changes its
    // mind about which server it is on.
    let scope = match &target.environment_id {
        None => String::new(),
        Some(environment_id) => format!("{}:", encode_uri_component(environment_id)),
    };
    let host = match &target.host {
        None => String::new(),
        Some(host) => format!("{}:", encode_uri_component(&host.to_lowercase())),
    };
    format!(
        "pull-request:{scope}{}:{host}{}:{}",
        encode_uri_component(&target.project_id),
        encode_uri_component(&target.repository),
        target.number,
    )
}

/// `pullRequestSurface`: the host is lowercased.
pub fn pull_request_surface(target: &PullRequestTarget) -> PullRequestSurface {
    let mut target = target.clone();
    target.host = target.host.map(|host| host.to_lowercase());
    PullRequestSurface {
        id: pull_request_surface_id(&target),
        target,
    }
}

fn singleton_surface(kind: SingletonKind) -> RightPanelSurface {
    match kind {
        SingletonKind::Diff => RightPanelSurface::Diff,
        SingletonKind::PullRequests => RightPanelSurface::PullRequests,
    }
}

fn file_surface(
    relative_path: &str,
    reveal_line: Option<f64>,
    reveal_request_id: u64,
) -> RightPanelSurface {
    RightPanelSurface::File(FileSurface {
        id: format!("file:{relative_path}"),
        relative_path: relative_path.to_owned(),
        reveal_line,
        reveal_request_id,
        attachment: None,
    })
}

fn attachment_surface(attachment: ChatFileAttachment) -> RightPanelSurface {
    RightPanelSurface::File(FileSurface {
        id: format!("attachment:{}", attachment.id.as_str()),
        relative_path: attachment.name.as_str().to_owned(),
        reveal_line: None,
        reveal_request_id: 0,
        attachment: Some(attachment),
    })
}

/// `upsertSurface`: an existing surface keeps its place and its data.
fn upsert_surface(
    current: &ThreadRightPanelState,
    surface: RightPanelSurface,
    activate: bool,
) -> ThreadRightPanelState {
    let mut next = current.clone();
    next.is_open = true;
    let id = surface.id();
    if !next.surfaces.iter().any(|entry| entry.id() == id) {
        next.surfaces.push(surface);
    }
    if activate {
        next.active_surface_id = Some(id);
    }
    next
}

/// `normalizeRevealLine`.
fn normalize_reveal_line(line: Option<f64>) -> Option<f64> {
    let line = line?;
    if !line.is_finite() {
        return None;
    }
    Some(line.trunc().max(1.0))
}

/// `/^[A-Za-z]:\/+$/`: a Windows drive root, which keeps its slashes.
fn is_windows_drive_root(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2..].iter().all(|byte| *byte == b'/')
}

/// `JS_SAFE_INTEGER`: `Number.isSafeInteger` accepts up to 2^53 - 1.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

fn pull_request_surface_from_value(value: &Map<String, Value>) -> Option<RightPanelSurface> {
    let project_id = value.get("projectId")?.as_str()?;
    let repository = value.get("repository")?.as_str()?;
    let number = value.get("number")?.as_u64()?;
    if !(1..=MAX_SAFE_INTEGER).contains(&number) {
        return None;
    }
    let target = PullRequestTarget {
        environment_id: value
            .get("environmentId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        project_id: project_id.to_owned(),
        host: value.get("host").and_then(Value::as_str).map(str::to_owned),
        repository: repository.to_owned(),
        number,
        url: value.get("url").and_then(Value::as_str).map(str::to_owned),
    };
    Some(RightPanelSurface::PullRequest(pull_request_surface(
        &target,
    )))
}

fn file_surface_from_value(value: &Map<String, Value>) -> Option<RightPanelSurface> {
    let attachment = match value.get("attachment") {
        None | Some(Value::Null) => None,
        Some(raw) => Some(serde_json::from_value::<ChatFileAttachment>(raw.clone()).ok()?),
    };
    if let Some(attachment) = attachment {
        let surface = attachment_surface(attachment);
        return Some(with_reveal(surface, value));
    }
    let relative_path = value.get("relativePath")?.as_str()?;
    Some(with_reveal(file_surface(relative_path, None, 0), value))
}

/// Reveal state of a persisted file surface. Non-finite lines become `null`,
/// and an unsafe request id becomes `0`.
fn with_reveal(surface: RightPanelSurface, value: &Map<String, Value>) -> RightPanelSurface {
    match surface {
        RightPanelSurface::File(mut file) => {
            file.reveal_line = value
                .get("revealLine")
                .and_then(Value::as_f64)
                .filter(|line| line.is_finite())
                .map(|line| line.trunc().max(1.0));
            file.reveal_request_id = value
                .get("revealRequestId")
                .and_then(Value::as_u64)
                .filter(|id| *id <= MAX_SAFE_INTEGER)
                .unwrap_or(0);
            RightPanelSurface::File(file)
        }
        other => other,
    }
}

/// One persisted surface, or `None` for a dropped one. Dropped kinds are
/// `plan` and `agents` (removed in v9 and v14), and `preview`, `device`,
/// `terminal`, and `files` (cut).
fn surface_from_value(value: &Value) -> Option<RightPanelSurface> {
    let object = value.as_object()?;
    match object.get("kind")?.as_str()? {
        "diff" => Some(RightPanelSurface::Diff),
        "pull-requests" => Some(RightPanelSurface::PullRequests),
        "pull-request" => pull_request_surface_from_value(object),
        "file" => file_surface_from_value(object),
        _ => None,
    }
}

fn surface_to_value(surface: &RightPanelSurface) -> Value {
    match surface {
        RightPanelSurface::Diff => json!({ "id": "diff", "kind": "diff" }),
        RightPanelSurface::PullRequests => {
            json!({ "id": "pull-requests", "kind": "pull-requests" })
        }
        RightPanelSurface::PullRequest(surface) => {
            let target = &surface.target;
            let mut object = Map::new();
            object.insert("id".into(), json!(surface.id));
            object.insert("kind".into(), json!("pull-request"));
            if let Some(environment_id) = &target.environment_id {
                object.insert("environmentId".into(), json!(environment_id));
            }
            object.insert("projectId".into(), json!(target.project_id));
            if let Some(host) = &target.host {
                object.insert("host".into(), json!(host));
            }
            object.insert("repository".into(), json!(target.repository));
            object.insert("number".into(), json!(target.number));
            if let Some(url) = &target.url {
                object.insert("url".into(), json!(url));
            }
            Value::Object(object)
        }
        RightPanelSurface::File(file) => {
            let mut object = Map::new();
            object.insert("id".into(), json!(file.id));
            object.insert("kind".into(), json!("file"));
            object.insert("relativePath".into(), json!(file.relative_path));
            object.insert("revealLine".into(), json!(file.reveal_line));
            object.insert("revealRequestId".into(), json!(file.reveal_request_id));
            if let Some(attachment) = &file.attachment {
                object.insert(
                    "attachment".into(),
                    serde_json::to_value(attachment).unwrap_or(Value::Null),
                );
            }
            Value::Object(object)
        }
    }
}

/// `migratePersistedRightPanelState`. Accepts any persisted value, and an
/// unreadable one yields empty maps.
pub fn migrate_persisted_right_panel_state(persisted: &Value) -> PersistedRightPanelState {
    let Some(object) = persisted.as_object() else {
        return PersistedRightPanelState::default();
    };

    let by_thread_key = match object.get("byThreadKey").and_then(Value::as_object) {
        None => IndexMap::new(),
        Some(entries) => entries
            .iter()
            .filter(|(key, _)| !is_pull_requests_panel_key(key))
            .map(|(key, thread)| (key.clone(), migrate_thread(thread)))
            .collect(),
    };

    let thread_panel_visibility_by_thread_key = match object
        .get("threadPanelVisibilityByThreadKey")
        .and_then(Value::as_object)
    {
        None => IndexMap::new(),
        Some(entries) => entries
            .iter()
            .filter_map(|(key, value)| {
                let visibility = value.as_object()?;
                // Only a hidden inline column survives a reload.
                let inline_open = visibility.get("inlineOpen")?;
                (inline_open == &Value::Bool(false)).then(|| {
                    (
                        key.clone(),
                        ThreadPanelVisibility {
                            inline_open: false,
                            popover_open: false,
                        },
                    )
                })
            })
            .collect(),
    };

    PersistedRightPanelState {
        by_thread_key,
        thread_panel_visibility_by_thread_key,
    }
}

fn migrate_thread(thread: &Value) -> ThreadRightPanelState {
    let thread = thread.as_object();
    let surfaces: Vec<RightPanelSurface> = thread
        .and_then(|thread| thread.get("surfaces"))
        .and_then(Value::as_array)
        .map(|surfaces| surfaces.iter().filter_map(surface_from_value).collect())
        .unwrap_or_default();

    let raw_active = thread.and_then(|thread| thread.get("activeSurfaceId"));
    let raw_active_id = raw_active.and_then(Value::as_str);
    // A legacy singleton `pull-request` id points at the first PR tab.
    let persisted_active_id = match raw_active_id {
        Some(id) if surfaces.iter().any(|surface| surface.id() == id) => Some(id.to_owned()),
        Some("pull-request") => surfaces
            .iter()
            .find(|surface| surface.kind() == RightPanelKind::PullRequest)
            .map(RightPanelSurface::id),
        _ => None,
    };

    // A migration that dropped every surface must not reopen an empty panel.
    let is_open = !surfaces.is_empty()
        && match thread.and_then(|thread| thread.get("isOpen")) {
            Some(Value::Bool(open)) => *open,
            _ => persisted_active_id.is_some(),
        };
    // An open panel needs an active surface. A removed `agents` selection also
    // keeps a survivor ready while the panel is closed.
    let active_surface_id = persisted_active_id.or_else(|| {
        if is_open || raw_active_id == Some("agents") {
            surfaces.first().map(RightPanelSurface::id)
        } else {
            None
        }
    });

    ThreadRightPanelState {
        is_open,
        active_surface_id,
        surfaces,
        maximized: thread
            .and_then(|thread| thread.get("maximized"))
            .and_then(Value::as_bool)
            == Some(true),
    }
}

impl RightPanelStore {
    // ----- selectors -----------------------------------------------------

    /// `selectThreadRightPanelState`.
    pub fn thread_state(&self, thread: &ScopedThreadRef) -> ThreadRightPanelState {
        self.by_thread_key
            .get(&thread_key(thread))
            .cloned()
            .unwrap_or_default()
    }

    /// `selectThreadPanelVisibility`.
    pub fn thread_panel_visibility(&self, thread: &ScopedThreadRef) -> ThreadPanelVisibility {
        self.thread_panel_visibility_by_thread_key
            .get(&thread_key(thread))
            .copied()
            .unwrap_or_default()
    }

    /// `selectThreadPanelOpen`.
    pub fn thread_panel_open(
        &self,
        thread: &ScopedThreadRef,
        presentation: ThreadPanelPresentation,
    ) -> bool {
        let visibility = self.thread_panel_visibility(thread);
        match presentation {
            ThreadPanelPresentation::Inline => visibility.inline_open,
            ThreadPanelPresentation::Popover => visibility.popover_open,
        }
    }

    /// `selectActiveRightPanel`: the kind of the active surface while open.
    pub fn active_panel(&self, thread: &ScopedThreadRef) -> Option<RightPanelKind> {
        self.active_panel_surface(thread)
            .map(|surface| surface.kind())
    }

    /// `selectActiveRightPanelSurface`.
    pub fn active_panel_surface(&self, thread: &ScopedThreadRef) -> Option<RightPanelSurface> {
        let state = self.thread_state(thread);
        if !state.is_open {
            return None;
        }
        self.selected_panel_surface(thread)
    }

    /// `selectSelectedRightPanelSurface`: the selected surface even while the
    /// panel is hidden, so a layout control can restore it.
    pub fn selected_panel_surface(&self, thread: &ScopedThreadRef) -> Option<RightPanelSurface> {
        let state = self.thread_state(thread);
        state
            .surfaces
            .into_iter()
            .find(|surface| Some(surface.id()) == state.active_surface_id)
    }

    /// `getUserActionRevision`.
    pub fn user_action_revision(&self, thread: &ScopedThreadRef) -> u64 {
        self.user_action_revision_by_thread_key
            .get(&thread_key(thread))
            .copied()
            .unwrap_or(0)
    }

    // ----- maximize request ----------------------------------------------

    /// `requestMaximize`.
    pub fn request_maximize(&mut self, thread: &ScopedThreadRef) {
        self.pending_maximize_thread_key = Some(thread_key(thread));
    }

    /// `consumeMaximizeRequest`: true once, for the thread that asked.
    pub fn consume_maximize_request(&mut self, thread: &ScopedThreadRef) -> bool {
        if self.pending_maximize_thread_key.as_deref() != Some(thread_key(thread).as_str()) {
            return false;
        }
        self.pending_maximize_thread_key = None;
        true
    }

    // ----- proactive and user-driven opens --------------------------------

    /// `openProactive`. Refused when the user made a panel choice after
    /// `expected_user_action_revision` was read. A linked PR beats a completed
    /// turn's diff.
    pub fn open_proactive(
        &mut self,
        thread: &ScopedThreadRef,
        surface: ProactiveSurface,
        expected_user_action_revision: u64,
    ) -> bool {
        let key = thread_key(thread);
        if self.user_action_revision(thread) != expected_user_action_revision {
            return false;
        }
        if matches!(surface, ProactiveSurface::Diff)
            && matches!(
                self.active_panel(thread),
                Some(RightPanelKind::PullRequest | RightPanelKind::PullRequests)
            )
        {
            return false;
        }
        self.automatic_update(&key, |current| {
            upsert_surface(current, surface.into_surface(), true)
        });
        true
    }

    /// `open`.
    pub fn open(&mut self, thread: &ScopedThreadRef, kind: SingletonKind) {
        self.user_action(thread, |current| {
            upsert_surface(current, singleton_surface(kind), true)
        });
    }

    /// `openFile`. A path of `.` opened the file explorer, which is cut, so it
    /// changes nothing.
    pub fn open_file(&mut self, thread: &ScopedThreadRef, requested_path: &str, line: Option<f64>) {
        if requested_path == "." {
            self.user_action(thread, |current| current.clone());
            return;
        }
        // Workspace entry paths use '/', including on Windows.
        let relative_path = if is_windows_drive_root(requested_path) {
            requested_path.to_owned()
        } else {
            let trimmed = requested_path.trim_end_matches('/');
            if trimmed.is_empty() {
                requested_path.to_owned()
            } else {
                trimmed.to_owned()
            }
        };
        let surface_id = format!("file:{relative_path}");
        self.user_action(thread, |current| {
            let existing = current.surfaces.iter().find_map(|entry| match entry {
                RightPanelSurface::File(file) if file.id == surface_id => Some(file),
                _ => None,
            });
            let reveal_request_id = existing.map_or(0, |file| file.reveal_request_id) + 1;
            let surface = file_surface(
                &relative_path,
                normalize_reveal_line(line),
                reveal_request_id,
            );
            let surfaces = match current
                .surfaces
                .iter()
                .position(|entry| entry.id() == surface_id)
            {
                Some(index) => {
                    let mut surfaces = current.surfaces.clone();
                    surfaces[index] = surface;
                    surfaces
                }
                None => {
                    let mut surfaces = current.surfaces.clone();
                    surfaces.push(surface);
                    surfaces
                }
            };
            ThreadRightPanelState {
                is_open: true,
                active_surface_id: Some(surface_id.clone()),
                surfaces,
                maximized: current.maximized,
            }
        });
    }

    /// `openAttachment`.
    pub fn open_attachment(&mut self, thread: &ScopedThreadRef, attachment: ChatFileAttachment) {
        self.user_action(thread, |current| {
            upsert_surface(current, attachment_surface(attachment.clone()), true)
        });
    }

    /// `openPullRequest`. A url given with the target replaces the stored
    /// surface (the same reference reads the same tab, but the url is new).
    pub fn open_pull_request(&mut self, thread: &ScopedThreadRef, target: &PullRequestTarget) {
        let surface = pull_request_surface(target);
        self.user_action(thread, |current| {
            let next = upsert_surface(
                current,
                RightPanelSurface::PullRequest(surface.clone()),
                true,
            );
            if target.url.is_none() {
                return next;
            }
            let mut next = next;
            for entry in &mut next.surfaces {
                if entry.id() == surface.id {
                    *entry = RightPanelSurface::PullRequest(surface.clone());
                }
            }
            next
        });
    }

    /// `activateSurface`.
    pub fn activate_surface(&mut self, thread: &ScopedThreadRef, surface_id: &str) {
        self.user_action(thread, |current| {
            if current
                .surfaces
                .iter()
                .any(|surface| surface.id() == surface_id)
            {
                ThreadRightPanelState {
                    is_open: true,
                    active_surface_id: Some(surface_id.to_owned()),
                    ..current.clone()
                }
            } else {
                current.clone()
            }
        });
    }

    // ----- closes ---------------------------------------------------------

    /// `closeSurface`.
    pub fn close_surface(&mut self, thread: &ScopedThreadRef, surface_id: &str) {
        self.close_action(thread, |current| {
            let Some(index) = current
                .surfaces
                .iter()
                .position(|surface| surface.id() == surface_id)
            else {
                return current.clone();
            };
            let mut surfaces = current.surfaces.clone();
            surfaces.remove(index);
            let is_open = !surfaces.is_empty() && current.is_open;
            let active_surface_id = if current.active_surface_id.as_deref() != Some(surface_id) {
                current.active_surface_id.clone()
            } else {
                // Falls back to the neighbor at the same index, clamped to the end.
                surfaces
                    .get(index.min(surfaces.len().saturating_sub(1)))
                    .map(RightPanelSurface::id)
            };
            ThreadRightPanelState {
                is_open,
                active_surface_id,
                surfaces,
                maximized: current.maximized,
            }
        });
    }

    /// `closeOtherSurfaces`.
    pub fn close_other_surfaces(&mut self, thread: &ScopedThreadRef, surface_id: &str) {
        self.close_action(thread, |current| {
            let Some(surface) = current
                .surfaces
                .iter()
                .find(|surface| surface.id() == surface_id)
            else {
                return current.clone();
            };
            if current.surfaces.len() == 1 {
                return current.clone();
            }
            ThreadRightPanelState {
                is_open: true,
                active_surface_id: Some(surface.id()),
                surfaces: vec![surface.clone()],
                maximized: current.maximized,
            }
        });
    }

    /// `closeSurfacesToRight`.
    pub fn close_surfaces_to_right(&mut self, thread: &ScopedThreadRef, surface_id: &str) {
        self.close_action(thread, |current| {
            let Some(index) = current
                .surfaces
                .iter()
                .position(|surface| surface.id() == surface_id)
            else {
                return current.clone();
            };
            if index == current.surfaces.len() - 1 {
                return current.clone();
            }
            let surfaces = current.surfaces[..=index].to_vec();
            let active_still_exists = surfaces
                .iter()
                .any(|surface| Some(surface.id()) == current.active_surface_id);
            ThreadRightPanelState {
                surfaces,
                active_surface_id: if active_still_exists {
                    current.active_surface_id.clone()
                } else {
                    Some(surface_id.to_owned())
                },
                ..current.clone()
            }
        });
    }

    /// `closeAllSurfaces`.
    pub fn close_all_surfaces(&mut self, thread: &ScopedThreadRef) {
        self.close_action(thread, |current| {
            if current.surfaces.is_empty() {
                return current.clone();
            }
            ThreadRightPanelState {
                is_open: false,
                active_surface_id: None,
                surfaces: Vec::new(),
                maximized: current.maximized,
            }
        });
    }

    // ----- reorder and reconcile ------------------------------------------

    /// `moveSurface`. Reordering is not a choice about what the panel shows,
    /// so it leaves the revision alone.
    pub fn move_surface(&mut self, thread: &ScopedThreadRef, surface_id: &str, to_index: usize) {
        self.update_thread(&thread_key(thread), |current| {
            let Some(from_index) = current
                .surfaces
                .iter()
                .position(|surface| surface.id() == surface_id)
            else {
                return current.clone();
            };
            if from_index == to_index {
                return current.clone();
            }
            let mut surfaces = current.surfaces.clone();
            let surface = surfaces.remove(from_index);
            // JS `splice` clamps an index past the end to the end.
            surfaces.insert(to_index.min(surfaces.len()), surface);
            ThreadRightPanelState {
                surfaces,
                ..current.clone()
            }
        });
    }

    /// `reconcileFileSurfaces`: workspace file tabs go when the workspace is
    /// gone. Attachment previews stay.
    pub fn reconcile_file_surfaces(&mut self, thread: &ScopedThreadRef, workspace_available: bool) {
        self.automatic_update(&thread_key(thread), |current| {
            if workspace_available {
                return current.clone();
            }
            let surfaces: Vec<RightPanelSurface> = current
                .surfaces
                .iter()
                .filter(|surface| match surface {
                    RightPanelSurface::File(file) => file.attachment.is_some(),
                    _ => true,
                })
                .cloned()
                .collect();
            if surfaces.len() == current.surfaces.len() {
                return current.clone();
            }
            let active_still_exists = surfaces
                .iter()
                .any(|surface| Some(surface.id()) == current.active_surface_id);
            ThreadRightPanelState {
                is_open: !surfaces.is_empty() && current.is_open,
                active_surface_id: if active_still_exists {
                    current.active_surface_id.clone()
                } else {
                    surfaces.last().map(RightPanelSurface::id)
                },
                surfaces,
                maximized: current.maximized,
            }
        });
    }

    // ----- panel visibility ------------------------------------------------

    /// `show`.
    pub fn show(&mut self, thread: &ScopedThreadRef) {
        self.user_action(thread, |current| {
            if current.is_open {
                current.clone()
            } else {
                ThreadRightPanelState {
                    is_open: true,
                    ..current.clone()
                }
            }
        });
    }

    /// `close`: hides the panel and keeps its selected surface.
    pub fn close(&mut self, thread: &ScopedThreadRef) {
        self.user_action(thread, |current| {
            if current.is_open {
                ThreadRightPanelState {
                    is_open: false,
                    ..current.clone()
                }
            } else {
                current.clone()
            }
        });
    }

    /// `setMaximized`. Layout only, so proactive panels still apply.
    pub fn set_maximized(&mut self, thread: &ScopedThreadRef, maximized: bool) {
        self.automatic_update(&thread_key(thread), |current| {
            if current.maximized == maximized {
                return current.clone();
            }
            ThreadRightPanelState {
                maximized,
                ..current.clone()
            }
        });
    }

    /// `toggleVisibility`.
    pub fn toggle_visibility(&mut self, thread: &ScopedThreadRef) {
        self.user_action(thread, |current| ThreadRightPanelState {
            is_open: !current.is_open,
            ..current.clone()
        });
    }

    /// `toggle`: closes when `kind` is already the open, active panel. Otherwise
    /// opens `kind`.
    pub fn toggle(&mut self, thread: &ScopedThreadRef, kind: SingletonKind) {
        self.user_action(thread, |current| {
            let active = current
                .surfaces
                .iter()
                .find(|surface| Some(surface.id()) == current.active_surface_id);
            if current.is_open
                && active.map(RightPanelSurface::kind)
                    == Some(match kind {
                        SingletonKind::Diff => RightPanelKind::Diff,
                        SingletonKind::PullRequests => RightPanelKind::PullRequests,
                    })
            {
                return ThreadRightPanelState {
                    is_open: false,
                    ..current.clone()
                };
            }
            upsert_surface(current, singleton_surface(kind), true)
        });
    }

    /// `setThreadPanelOpen`.
    pub fn set_thread_panel_open(
        &mut self,
        thread: &ScopedThreadRef,
        presentation: ThreadPanelPresentation,
        open: bool,
    ) {
        self.update_visibility(&thread_key(thread), |visibility| {
            let current = match presentation {
                ThreadPanelPresentation::Inline => visibility.inline_open,
                ThreadPanelPresentation::Popover => visibility.popover_open,
            };
            if current == open {
                return *visibility;
            }
            match presentation {
                ThreadPanelPresentation::Inline => ThreadPanelVisibility {
                    inline_open: open,
                    ..*visibility
                },
                ThreadPanelPresentation::Popover => ThreadPanelVisibility {
                    popover_open: open,
                    ..*visibility
                },
            }
        });
    }

    /// `toggleThreadPanel`.
    pub fn toggle_thread_panel(
        &mut self,
        thread: &ScopedThreadRef,
        presentation: ThreadPanelPresentation,
    ) {
        self.update_visibility(&thread_key(thread), |visibility| match presentation {
            ThreadPanelPresentation::Inline => ThreadPanelVisibility {
                inline_open: !visibility.inline_open,
                ..*visibility
            },
            ThreadPanelPresentation::Popover => ThreadPanelVisibility {
                popover_open: !visibility.popover_open,
                ..*visibility
            },
        });
    }

    /// `removeThread`: drops every per-thread entry.
    pub fn remove_thread(&mut self, thread: &ScopedThreadRef) {
        let key = thread_key(thread);
        self.by_thread_key.shift_remove(&key);
        self.thread_panel_visibility_by_thread_key
            .shift_remove(&key);
        self.user_action_revision_by_thread_key.shift_remove(&key);
    }

    // ----- persisted shape -------------------------------------------------

    /// The `partialize` output: the pull-request list's shared panel is
    /// dropped, and only a hidden inline column is kept.
    pub fn to_persisted_value(&self) -> Value {
        let by_thread_key: Map<String, Value> = self
            .by_thread_key
            .iter()
            .filter(|(key, _)| !is_pull_requests_panel_key(key))
            .map(|(key, state)| (key.clone(), thread_state_to_value(state)))
            .collect();
        let visibility: Map<String, Value> = self
            .thread_panel_visibility_by_thread_key
            .iter()
            .filter(|(_, visibility)| !visibility.inline_open)
            .map(|(key, _)| {
                (
                    key.clone(),
                    json!({ "inlineOpen": false, "popoverOpen": false }),
                )
            })
            .collect();
        json!({
            "byThreadKey": by_thread_key,
            "threadPanelVisibilityByThreadKey": visibility,
        })
    }

    /// Replaces the persisted maps with a migrated value. The per-session maps
    /// (revisions, closed tabs, pending maximize) are untouched, as upstream's
    /// `setState(migrate(...))` leaves them.
    pub fn load_persisted(&mut self, persisted: &Value) {
        let migrated = migrate_persisted_right_panel_state(persisted);
        self.by_thread_key = migrated.by_thread_key;
        self.thread_panel_visibility_by_thread_key = migrated.thread_panel_visibility_by_thread_key;
    }

    // ----- internals -------------------------------------------------------

    /// `userAction`: a panel choice. It always advances the thread's revision,
    /// even when the updater changes nothing.
    fn user_action(
        &mut self,
        thread: &ScopedThreadRef,
        updater: impl FnOnce(&ThreadRightPanelState) -> ThreadRightPanelState,
    ) {
        let key = thread_key(thread);
        self.update_thread(&key, updater);
        self.bump_revision(&key);
    }

    /// `closeAction`: a user choice that also hides or removes tabs. Each
    /// removed tab goes to the closed log, the active one last, so it reopens
    /// first.
    fn close_action(
        &mut self,
        thread: &ScopedThreadRef,
        updater: impl FnOnce(&ThreadRightPanelState) -> ThreadRightPanelState,
    ) {
        let key = thread_key(thread);
        let before = self.by_thread_key.get(&key).cloned().unwrap_or_default();
        self.user_action(thread, updater);
        let after = self.by_thread_key.get(&key).cloned().unwrap_or_default();

        let removed: Vec<RightPanelSurface> = before
            .surfaces
            .iter()
            .filter(|surface| {
                !after
                    .surfaces
                    .iter()
                    .any(|entry| entry.id() == surface.id())
            })
            .cloned()
            .collect();
        let (active, others): (Vec<_>, Vec<_>) = removed
            .into_iter()
            .partition(|surface| Some(surface.id()) == before.active_surface_id);
        for surface in others.into_iter().chain(active) {
            self.remember_closed(thread, surface);
        }
    }

    /// `closedViewStore.remember`, panel-tab entries only: the same target is
    /// replaced, newest first, capped at `CLOSED_VIEW_LIMIT`.
    fn remember_closed(&mut self, thread: &ScopedThreadRef, surface: RightPanelSurface) {
        let key = thread_key(thread);
        let surface_id = surface.id();
        self.closed_panel_tabs.retain(|entry| {
            !(thread_key(&entry.thread_ref) == key && entry.surface.id() == surface_id)
        });
        self.closed_panel_tabs
            .insert(0, ClosedPanelTab::new(thread.clone(), surface));
        self.closed_panel_tabs.truncate(CLOSED_VIEW_LIMIT);
    }

    pub fn remove_closed_id(&mut self, id: &str) {
        self.closed_panel_tabs.retain(|entry| entry.id != id);
    }

    /// `defer`: move a failed restore to the end so the next entry is tried.
    pub fn defer_closed_id(&mut self, id: &str) {
        let Some(index) = self
            .closed_panel_tabs
            .iter()
            .position(|entry| entry.id == id)
        else {
            return;
        };
        let entry = self.closed_panel_tabs.remove(index);
        self.closed_panel_tabs.push(entry);
    }

    /// `automaticUpdate`: the app's change, not the user's. It leaves the
    /// revision alone.
    fn automatic_update(
        &mut self,
        key: &str,
        updater: impl FnOnce(&ThreadRightPanelState) -> ThreadRightPanelState,
    ) {
        self.update_thread(key, updater);
    }

    fn bump_revision(&mut self, key: &str) {
        let revision = self
            .user_action_revision_by_thread_key
            .get(key)
            .copied()
            .unwrap_or(0);
        self.user_action_revision_by_thread_key
            .insert(key.to_owned(), revision + 1);
    }

    /// `updateThread`: applies a panel mutation and its visibility transition
    /// together. Opening a panel closes its popover. Closing one reopens the
    /// inline column, so a popover that stays open is not lost.
    fn update_thread(
        &mut self,
        key: &str,
        updater: impl FnOnce(&ThreadRightPanelState) -> ThreadRightPanelState,
    ) {
        let current = self.by_thread_key.get(key).cloned().unwrap_or_default();
        let next = updater(&current);
        self.store_thread_state(key, next);
        let stored = self.by_thread_key.get(key).cloned().unwrap_or_default();

        if !current.is_open && stored.is_open {
            self.update_visibility(key, |visibility| {
                if visibility.popover_open {
                    ThreadPanelVisibility {
                        popover_open: false,
                        ..*visibility
                    }
                } else {
                    *visibility
                }
            });
        } else if current.is_open && !stored.is_open {
            self.update_visibility(key, |visibility| {
                if visibility.popover_open && !visibility.inline_open {
                    ThreadPanelVisibility {
                        inline_open: true,
                        ..*visibility
                    }
                } else {
                    *visibility
                }
            });
        }
    }

    /// `updateThreadStateMap`: a thread with nothing to show is removed.
    fn store_thread_state(&mut self, key: &str, next: ThreadRightPanelState) {
        if !next.is_open && next.active_surface_id.is_none() && next.surfaces.is_empty() {
            self.by_thread_key.shift_remove(key);
            return;
        }
        self.by_thread_key.insert(key.to_owned(), next);
    }

    /// `updateThreadPanelVisibilityMap`: the default pair is not stored.
    fn update_visibility(
        &mut self,
        key: &str,
        updater: impl FnOnce(&ThreadPanelVisibility) -> ThreadPanelVisibility,
    ) {
        let current = self
            .thread_panel_visibility_by_thread_key
            .get(key)
            .copied()
            .unwrap_or_default();
        let next = updater(&current);
        if next.inline_open && !next.popover_open {
            self.thread_panel_visibility_by_thread_key.shift_remove(key);
            return;
        }
        self.thread_panel_visibility_by_thread_key
            .insert(key.to_owned(), next);
    }
}

fn thread_state_to_value(state: &ThreadRightPanelState) -> Value {
    let mut object = Map::new();
    object.insert("isOpen".into(), json!(state.is_open));
    object.insert("activeSurfaceId".into(), json!(state.active_surface_id));
    object.insert(
        "surfaces".into(),
        Value::Array(state.surfaces.iter().map(surface_to_value).collect()),
    );
    if state.maximized {
        object.insert("maximized".into(), json!(true));
    }
    Value::Object(object)
}

#[cfg(test)]
#[path = "right_panel_store_tests.rs"]
mod tests;

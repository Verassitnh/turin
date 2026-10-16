//! Port of upstream/apps/web/src/lib/attachmentUploadQueue.ts
//!
//! Per-environment upload queue (at most three in flight), reconnect retry,
//! and the rules for which server id a cancel may delete. Upstream stores
//! this in module globals and a zustand atom, and reaches the server through
//! RPC. Turin keeps the same control flow on [`AttachmentUploadQueue`];
//! mint, verify, delete, the byte transfer and the connection phase are
//! [`AttachmentUploadServices`]. Draft stamping is [`ComposerDraftUploads`].
//!
//! `resolveAssetUrl` (client-runtime `state/assets.ts`) is [`resolve_asset_url`].
//! The upload cycle (`runAttachmentUploadCycle` / `verifyPersistedAttachmentUpload`
//! / `deletePendingAttachmentUpload`) lives here because this queue is its
//! only caller: one process, so there is no atom registry.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::watch;
use turin_contracts::chat_attachment::PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES;

use crate::composer::attachment_upload_state::{AttachmentUploadState, ReadyAttachmentUpload};

const MAX_UPLOADS_PER_ENVIRONMENT: usize = 3;
const UPLOAD_TIMEOUT_MS: u64 = 5 * 60_000;

/// `new URL(relativeUrl, httpBaseUrl)` — `null` when either side is not a URL.
pub fn resolve_asset_url(http_base_url: &str, relative_url: &str) -> Option<String> {
    let base = url::Url::parse(http_base_url).ok()?;
    base.join(relative_url).ok().map(|url| url.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapShotAttachmentSource {
    pub kind: &'static str,
    pub captured_at: String,
    pub app_name: String,
    pub window_title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PastedTextAttachmentSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerThreadTarget {
    Draft(String),
    Thread {
        environment_id: String,
        thread_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposerUploadAttachment {
    Image {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
        bytes: Option<Vec<u8>>,
        source: Option<SnapShotAttachmentSource>,
    },
    File {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
        bytes: Option<Vec<u8>>,
        uploaded_attachment_id: Option<String>,
        upload_environment_id: Option<String>,
        source: Option<PastedTextAttachmentSource>,
    },
}

impl ComposerUploadAttachment {
    fn id(&self) -> &str {
        match self {
            Self::Image { id, .. } | Self::File { id, .. } => id,
        }
    }

    fn is_file(&self) -> bool {
        matches!(self, Self::File { .. })
    }

    fn name(&self) -> &str {
        match self {
            Self::Image { name, .. } | Self::File { name, .. } => name,
        }
    }

    fn mime_type(&self) -> &str {
        match self {
            Self::Image { mime_type, .. } | Self::File { mime_type, .. } => mime_type,
        }
    }

    fn size_bytes(&self) -> u64 {
        match self {
            Self::Image { size_bytes, .. } | Self::File { size_bytes, .. } => *size_bytes,
        }
    }

    fn has_bytes(&self) -> bool {
        match self {
            Self::Image { bytes, .. } | Self::File { bytes, .. } => bytes.is_some(),
        }
    }

    /// Persisted id owned by the draft, and only for the environment that stored it.
    fn persisted_id_for(&self, environment_id: &str) -> Option<&str> {
        match self {
            Self::File {
                uploaded_attachment_id: Some(id),
                upload_environment_id: Some(env),
                ..
            } if env == environment_id && !id.is_empty() => Some(id),
            _ => None,
        }
    }

    fn upload_mime(&self) -> Option<String> {
        let mime = self.mime_type().to_lowercase();
        match self {
            Self::File { .. } => Some(mime),
            Self::Image { .. } => PROVIDER_SEND_TURN_SUPPORTED_IMAGE_MIME_TYPES
                .iter()
                .find(|supported| **supported == mime)
                .map(|supported| (*supported).to_owned()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UploadedAttachment {
    Image {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
        source: Option<SnapShotAttachmentSource>,
    },
    File {
        id: String,
        name: String,
        mime_type: String,
        size_bytes: u64,
        source: Option<PastedTextAttachmentSource>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadClaim {
    pub is_file: bool,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MintedUpload {
    pub attachment_id: String,
    pub relative_url: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MintResult {
    Success(MintedUpload),
    Failure { message: String },
}

/// Outcome of the asset-url query `verifyPersistedAttachmentUpload` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssetQueryResult {
    Success,
    Failure {
        tag: Option<String>,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersistedAttachmentVerification {
    Verified,
    Missing,
    Failed { message: String },
}

impl PersistedAttachmentVerification {
    fn from_query(result: AssetQueryResult) -> Self {
        match result {
            AssetQueryResult::Success => Self::Verified,
            AssetQueryResult::Failure { tag, message } => {
                if tag.as_deref() == Some("AssetAttachmentNotFoundError") {
                    Self::Missing
                } else {
                    Self::Failed { message }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferRequest {
    pub url: String,
    pub mime_type: String,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransferError {
    Rejected(u16),
    Failed,
    TimedOut,
    Cancelled,
}

impl std::fmt::Display for TransferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rejected(status) => write!(f, "Upload rejected ({status})"),
            Self::Failed => f.write_str("Upload failed"),
            Self::TimedOut => f.write_str("Upload timed out"),
            Self::Cancelled => f.write_str("Upload cancelled"),
        }
    }
}

pub struct BegunTransfer {
    pub done:
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), TransferError>> + Send>>,
    pub abort: Arc<dyn Fn() + Send + Sync>,
}

/// Network and connection surface. `can_upload` is upstream's
/// `readEnvironmentScope(..., AuthOrchestrationOperateScope)`.
#[async_trait]
pub trait AttachmentUploadServices: Send + Sync {
    fn can_upload(&self, environment_id: &str) -> bool;
    fn is_connected(&self, environment_id: &str) -> bool;
    fn watch_connection(
        &self,
        environment_id: &str,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send>;
    fn http_base_url(&self, environment_id: &str) -> Option<String>;
    async fn query_attachment(&self, environment_id: &str, attachment_id: &str)
    -> AssetQueryResult;
    async fn mint_upload(&self, environment_id: &str, claim: &UploadClaim) -> MintResult;
    fn delete_pending(&self, environment_id: &str, attachment_id: &str);
    fn begin_transfer(
        &self,
        request: TransferRequest,
        on_progress: Arc<dyn Fn(f64) + Send + Sync>,
    ) -> BegunTransfer;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRow {
    pub key: String,
    pub session_environment_id: Option<String>,
    pub file_ids: Vec<String>,
}

pub trait ComposerDraftUploads: Send + Sync {
    fn rows(&self) -> Vec<DraftRow>;
    fn set_file_upload(
        &self,
        thread_key: &str,
        file_id: &str,
        environment_id: &str,
        attachment_id: &str,
    );
    fn mark_file_upload_missing(
        &self,
        thread_key: &str,
        file_id: &str,
        environment_id: &str,
        attachment_id: &str,
    ) -> bool;
}

struct JobInner {
    cancelled: bool,
    attachment_id: Option<String>,
    abort: Option<Arc<dyn Fn() + Send + Sync>>,
    stop_watch: Option<Box<dyn FnOnce() + Send>>,
}

struct Job {
    image: ComposerUploadAttachment,
    environment_id: String,
    draft_target: Option<ComposerThreadTarget>,
    previous: Option<ReadyAttachmentUpload>,
    persisted_attachment_id: Option<String>,
    inner: Mutex<JobInner>,
    settled_tx: watch::Sender<bool>,
    settled_rx: watch::Receiver<bool>,
}

struct QueueState {
    uploads: HashMap<String, AttachmentUploadState>,
    jobs: HashMap<String, Arc<Job>>,
    queue: Vec<Arc<Job>>,
    active: HashMap<String, usize>,
}

struct Inner {
    services: Arc<dyn AttachmentUploadServices>,
    drafts: Arc<dyn ComposerDraftUploads>,
    state: Mutex<QueueState>,
}

#[derive(Clone)]
pub struct AttachmentUploadQueue {
    inner: Arc<Inner>,
}

impl AttachmentUploadQueue {
    pub fn new(
        services: Arc<dyn AttachmentUploadServices>,
        drafts: Arc<dyn ComposerDraftUploads>,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                services,
                drafts,
                state: Mutex::new(QueueState {
                    uploads: HashMap::new(),
                    jobs: HashMap::new(),
                    queue: Vec::new(),
                    active: HashMap::new(),
                }),
            }),
        }
    }

    pub fn read(&self, image_id: &str) -> Option<AttachmentUploadState> {
        self.inner.state.lock().uploads.get(image_id).cloned()
    }

    pub fn start(
        &self,
        environment_id: &str,
        image: ComposerUploadAttachment,
        draft_target: Option<ComposerThreadTarget>,
    ) {
        if !self.inner.services.can_upload(environment_id) {
            return;
        }
        let image_id = image.id().to_owned();
        {
            let state = self.inner.state.lock();
            if state
                .jobs
                .get(&image_id)
                .is_some_and(|job| job.environment_id == environment_id)
            {
                return;
            }
        }
        if let Some(existing) = self.read(&image_id) {
            if existing.is_status_ready_same(environment_id)
                || existing.is_failed_same(environment_id)
            {
                return;
            }
            if let Some(previous) = existing
                .previous()
                .filter(|previous| previous.environment_id == environment_id)
                .cloned()
            {
                let failed_mint = match &existing {
                    AttachmentUploadState::Failed {
                        environment_id: failed_env,
                        attachment_id: Some(attachment_id),
                        ..
                    } => Some((failed_env.clone(), attachment_id.clone())),
                    _ => None,
                };
                self.cancel(&image_id);
                if let Some((failed_env, attachment_id)) = failed_mint {
                    self.delete_pending(&failed_env, &attachment_id);
                }
                self.set_state(&image_id, AttachmentUploadState::Ready(previous));
                return;
            }
        }
        if self.inner.state.lock().jobs.contains_key(&image_id) {
            self.cancel(&image_id);
        }

        let previous = self.read(&image_id).and_then(|existing| match existing {
            AttachmentUploadState::Ready(ready) => Some(ready),
            other => other.previous().cloned(),
        });
        let persisted_attachment_id = image.persisted_id_for(environment_id).map(str::to_owned);
        let (settled_tx, settled_rx) = watch::channel(false);
        let job = Arc::new(Job {
            image,
            environment_id: environment_id.to_owned(),
            draft_target,
            previous: previous.clone(),
            persisted_attachment_id,
            inner: Mutex::new(JobInner {
                cancelled: false,
                attachment_id: None,
                abort: None,
                stop_watch: None,
            }),
            settled_tx,
            settled_rx,
        });
        {
            let mut state = self.inner.state.lock();
            state.jobs.insert(image_id.clone(), Arc::clone(&job));
        }
        self.watch_reconnect(Arc::clone(&job));
        self.inner.state.lock().queue.push(Arc::clone(&job));
        self.set_state(
            &image_id,
            AttachmentUploadState::Uploading {
                environment_id: environment_id.to_owned(),
                progress: 0.0,
                previous,
            },
        );
        self.pump();
    }

    pub fn release(&self, image_id: &str) {
        let upload = self.read(image_id);
        self.cancel(image_id);
        if let Some(upload) = upload {
            match upload {
                AttachmentUploadState::Ready(ready) => {
                    self.delete_pending(&ready.environment_id, &ready.attachment_id);
                }
                AttachmentUploadState::Failed {
                    environment_id,
                    attachment_id,
                    previous,
                    ..
                } => {
                    if let Some(attachment_id) = attachment_id {
                        self.delete_pending(&environment_id, &attachment_id);
                    }
                    if let Some(previous) = previous {
                        self.delete_pending(&previous.environment_id, &previous.attachment_id);
                    }
                }
                AttachmentUploadState::Uploading { previous, .. } => {
                    if let Some(previous) = previous {
                        self.delete_pending(&previous.environment_id, &previous.attachment_id);
                    }
                }
            }
        }
        self.clear_state(image_id);
    }

    pub fn release_persisted(&self, id: &str, environment_id: &str, attachment_id: &str) {
        if let Some(AttachmentUploadState::Ready(ready)) = self.read(id)
            && ready.environment_id == environment_id
            && ready.attachment_id == attachment_id
        {
            self.release(id);
            return;
        }
        let job_matches = self.inner.state.lock().jobs.get(id).is_some_and(|job| {
            job.environment_id == environment_id
                && job.persisted_attachment_id.as_deref() == Some(attachment_id)
        });
        if job_matches {
            self.release(id);
        }
        self.delete_pending(environment_id, attachment_id);
    }

    pub fn release_draft_attachment(&self, attachment: &ComposerUploadAttachment) {
        if let ComposerUploadAttachment::File {
            id,
            uploaded_attachment_id: Some(attachment_id),
            upload_environment_id: Some(environment_id),
            ..
        } = attachment
        {
            self.release_persisted(id, environment_id, attachment_id);
            let still_tracked = {
                let state = self.inner.state.lock();
                state.jobs.contains_key(id) || state.uploads.contains_key(id)
            };
            if still_tracked {
                self.release(id);
            }
            return;
        }
        self.release(attachment.id());
    }

    pub fn release_draft_attachments(&self, attachments: &[ComposerUploadAttachment]) {
        for attachment in attachments {
            self.release_draft_attachment(attachment);
        }
    }

    pub fn retry(
        &self,
        environment_id: &str,
        image: ComposerUploadAttachment,
        draft_target: Option<ComposerThreadTarget>,
    ) {
        if !self.inner.services.can_upload(environment_id) {
            return;
        }
        let image_id = image.id().to_owned();
        let previous = self.read(&image_id);
        self.cancel(&image_id);
        if let Some(AttachmentUploadState::Failed {
            environment_id: failed_env,
            attachment_id: Some(attachment_id),
            previous: carried,
            ..
        }) = &previous
        {
            self.delete_pending(failed_env, attachment_id);
            if let Some(carried) = carried.clone() {
                self.set_state(&image_id, AttachmentUploadState::Ready(carried));
            } else {
                self.clear_state(&image_id);
            }
        } else if let Some(carried) = previous.as_ref().and_then(AttachmentUploadState::previous) {
            self.set_state(&image_id, AttachmentUploadState::Ready(carried.clone()));
        } else {
            self.clear_state(&image_id);
        }
        self.start(environment_id, image, draft_target);
    }

    pub async fn verify_stashed(
        &self,
        environment_id: &str,
        attachment_id: &str,
    ) -> PersistedAttachmentVerification {
        let result = self
            .inner
            .services
            .query_attachment(environment_id, attachment_id)
            .await;
        PersistedAttachmentVerification::from_query(result)
    }

    pub async fn await_attachment_uploads(&self, image_ids: Vec<String>) {
        let receivers: Vec<watch::Receiver<bool>> = {
            let state = self.inner.state.lock();
            image_ids
                .iter()
                .filter_map(|id| state.jobs.get(id).map(|job| job.settled_rx.clone()))
                .collect()
        };
        for mut receiver in receivers {
            let _ = receiver.wait_for(|settled| *settled).await;
        }
    }

    pub fn get_uploaded_attachments(
        &self,
        environment_id: &str,
        images: &[ComposerUploadAttachment],
    ) -> Option<Vec<UploadedAttachment>> {
        let mut attachments = Vec::with_capacity(images.len());
        for image in images {
            let AttachmentUploadState::Ready(ready) = self.read(image.id())? else {
                return None;
            };
            if ready.environment_id != environment_id {
                return None;
            }
            attachments.push(uploaded_from(image, &ready.attachment_id));
        }
        Some(attachments)
    }

    fn delete_pending(&self, environment_id: &str, attachment_id: &str) {
        if !self.inner.services.can_upload(environment_id) {
            return;
        }
        self.inner
            .services
            .delete_pending(environment_id, attachment_id);
    }

    fn set_state(&self, image_id: &str, upload: AttachmentUploadState) {
        self.inner
            .state
            .lock()
            .uploads
            .insert(image_id.to_owned(), upload);
    }

    fn clear_state(&self, image_id: &str) {
        self.inner.state.lock().uploads.remove(image_id);
    }

    fn cancel(&self, image_id: &str) {
        let Some(job) = self.inner.state.lock().jobs.remove(image_id) else {
            return;
        };
        let (abort, stop, attachment_id) = {
            let mut inner = job.inner.lock();
            inner.cancelled = true;
            (
                inner.abort.take(),
                inner.stop_watch.take(),
                inner.attachment_id.clone(),
            )
        };
        self.inner
            .state
            .lock()
            .queue
            .retain(|queued| !Arc::ptr_eq(queued, &job));
        if let Some(abort) = abort {
            abort();
        }
        if let Some(stop) = stop {
            stop();
        }
        if let Some(attachment_id) = attachment_id {
            self.delete_pending(&job.environment_id, &attachment_id);
        }
        let _ = job.settled_tx.send(true);
    }

    fn pump(&self) {
        let mut starting = Vec::new();
        {
            let mut state = self.inner.state.lock();
            let mut index = 0;
            while index < state.queue.len() {
                let environment_id = state.queue[index].environment_id.clone();
                let active = state.active.get(&environment_id).copied().unwrap_or(0);
                if active >= MAX_UPLOADS_PER_ENVIRONMENT {
                    index += 1;
                    continue;
                }
                let job = state.queue.remove(index);
                if job.inner.lock().cancelled {
                    continue;
                }
                state.active.insert(environment_id, active + 1);
                starting.push(job);
            }
        }
        for job in starting {
            let queue = self.clone();
            tokio::spawn(async move {
                queue.run_upload(&job).await;
                queue.finish(&job);
            });
        }
    }

    fn finish(&self, job: &Arc<Job>) {
        let failed = matches!(
            self.read(job.image.id()),
            Some(AttachmentUploadState::Failed { .. })
        );
        let stop = {
            let mut state = self.inner.state.lock();
            let still_ours = state
                .jobs
                .get(job.image.id())
                .is_some_and(|current| Arc::ptr_eq(current, job));
            let stop = if still_ours && !failed {
                state.jobs.remove(job.image.id());
                job.inner.lock().stop_watch.take()
            } else {
                None
            };
            let remaining = state
                .active
                .get(&job.environment_id)
                .copied()
                .unwrap_or(1)
                .saturating_sub(1);
            if remaining > 0 {
                state.active.insert(job.environment_id.clone(), remaining);
            } else {
                state.active.remove(&job.environment_id);
            }
            stop
        };
        if let Some(stop) = stop {
            stop();
        }
        let _ = job.settled_tx.send(true);
        self.pump();
    }

    fn watch_reconnect(&self, job: Arc<Job>) {
        let queue = self.clone();
        let watched = Arc::clone(&job);
        let environment_id = job.environment_id.clone();
        let was_connected = Mutex::new(self.inner.services.is_connected(&environment_id));
        let on_change: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let connected = queue.inner.services.is_connected(&environment_id);
            let mut was = was_connected.lock();
            let reconnected = connected && !*was;
            *was = connected;
            drop(was);
            if !reconnected {
                return;
            }
            let queue = queue.clone();
            let job = Arc::clone(&watched);
            tokio::spawn(async move {
                let _ = job.settled_rx.clone().wait_for(|settled| *settled).await;
                let still_failed = {
                    let state = queue.inner.state.lock();
                    state.jobs.get(job.image.id()).is_some_and(|current| {
                        Arc::ptr_eq(current, &job)
                            && matches!(
                                state.uploads.get(job.image.id()),
                                Some(AttachmentUploadState::Failed { .. })
                            )
                    })
                };
                if still_failed && queue.inner.services.is_connected(&job.environment_id) {
                    queue.retry(
                        &job.environment_id,
                        job.image.clone(),
                        job.draft_target.clone(),
                    );
                }
            });
        });
        let stop = self
            .inner
            .services
            .watch_connection(&job.environment_id, on_change);
        job.inner.lock().stop_watch = Some(stop);
    }

    async fn run_upload(&self, job: &Arc<Job>) {
        if !self.inner.services.can_upload(&job.environment_id) {
            self.fail(job, "This connection cannot upload attachments.", None);
            return;
        }
        if let Some(persisted) = job.persisted_attachment_id.clone() {
            let verification = self.verify_persisted(&job.environment_id, &persisted).await;
            if job.inner.lock().cancelled {
                return;
            }
            match verification {
                PersistedAttachmentVerification::Verified => {
                    self.set_state(
                        job.image.id(),
                        AttachmentUploadState::Ready(ReadyAttachmentUpload {
                            environment_id: job.environment_id.clone(),
                            attachment_id: persisted.clone(),
                        }),
                    );
                    self.stamp(job, &persisted);
                    return;
                }
                PersistedAttachmentVerification::Missing
                    if !job.image.has_bytes() && job.image.is_file() =>
                {
                    if self.mark_missing(job, &persisted) {
                        self.clear_state(job.image.id());
                        return;
                    }
                    self.fail(
                        job,
                        "Uploaded file expired. Remove it and attach it again.",
                        None,
                    );
                    return;
                }
                PersistedAttachmentVerification::Missing => {
                    if !job.image.has_bytes() {
                        self.fail(
                            job,
                            "Uploaded file expired. Remove it and attach it again.",
                            None,
                        );
                        return;
                    }
                }
                PersistedAttachmentVerification::Failed { .. } => {
                    self.fail(
                        job,
                        "Uploaded file could not be verified. Retry when the server reconnects.",
                        None,
                    );
                    return;
                }
            }
        }

        let Some(mime_type) = job.image.upload_mime() else {
            self.fail(job, "Unsupported image type", None);
            return;
        };
        if !job.image.has_bytes() {
            self.fail(job, "Original file is no longer available", None);
            return;
        }
        if !self.inner.services.can_upload(&job.environment_id) {
            self.fail(job, "This connection cannot upload attachments.", None);
            return;
        }

        let claim = UploadClaim {
            is_file: job.image.is_file(),
            name: job.image.name().to_owned(),
            mime_type: mime_type.clone(),
            size_bytes: job.image.size_bytes(),
        };
        let result = self.run_cycle(job, &claim, &mime_type).await;
        if job.inner.lock().cancelled || matches!(result, CycleResult::Cancelled) {
            return;
        }
        match result {
            CycleResult::Uploaded { attachment_id } => {
                self.set_state(
                    job.image.id(),
                    AttachmentUploadState::Ready(ReadyAttachmentUpload {
                        environment_id: job.environment_id.clone(),
                        attachment_id: attachment_id.clone(),
                    }),
                );
                self.stamp(job, &attachment_id);
                if let Some(previous) = &job.previous {
                    self.delete_pending(&previous.environment_id, &previous.attachment_id);
                }
            }
            CycleResult::Failed {
                step,
                attachment_id,
                message,
            } => {
                let reason = match step {
                    FailStep::Mint => "Upload could not start".to_owned(),
                    FailStep::ResolveUrl => {
                        if self.inner.services.can_upload(&job.environment_id) {
                            "Not connected".to_owned()
                        } else {
                            "This connection cannot upload attachments.".to_owned()
                        }
                    }
                    FailStep::Transfer => message,
                };
                self.fail(job, &reason, attachment_id);
            }
            CycleResult::Cancelled => {}
        }
    }

    async fn verify_persisted(
        &self,
        environment_id: &str,
        attachment_id: &str,
    ) -> PersistedAttachmentVerification {
        let result = self
            .inner
            .services
            .query_attachment(environment_id, attachment_id)
            .await;
        PersistedAttachmentVerification::from_query(result)
    }

    async fn run_cycle(&self, job: &Arc<Job>, claim: &UploadClaim, mime_type: &str) -> CycleResult {
        let minted = self
            .inner
            .services
            .mint_upload(&job.environment_id, claim)
            .await;
        let minted = match minted {
            MintResult::Success(minted) => minted,
            MintResult::Failure { message } => {
                return CycleResult::Failed {
                    step: FailStep::Mint,
                    attachment_id: None,
                    message,
                };
            }
        };
        let cancelled = job.inner.lock().cancelled;
        if cancelled && self.inner.services.can_upload(&job.environment_id) {
            self.delete_pending(&job.environment_id, &minted.attachment_id);
            return CycleResult::Cancelled;
        }
        job.inner.lock().attachment_id = Some(minted.attachment_id.clone());

        let url = self.resolve_upload_url(job, &minted.relative_url);
        let Some(url) = url else {
            return CycleResult::Failed {
                step: FailStep::ResolveUrl,
                attachment_id: Some(minted.attachment_id),
                message: "The environment is not connected.".to_owned(),
            };
        };

        let last_step = Arc::new(std::sync::atomic::AtomicI32::new(-1));
        let queue = self.clone();
        let image_id = job.image.id().to_owned();
        let environment_id = job.environment_id.clone();
        let previous = job.previous.clone();
        let progress_job = Arc::clone(job);
        let on_progress: Arc<dyn Fn(f64) + Send + Sync> = Arc::new(move |progress| {
            let step = (progress * 20.0).floor() as i32;
            if progress_job.inner.lock().cancelled || last_step.load(Ordering::Relaxed) == step {
                return;
            }
            last_step.store(step, Ordering::Relaxed);
            queue.set_state(
                &image_id,
                AttachmentUploadState::Uploading {
                    environment_id: environment_id.clone(),
                    progress,
                    previous: previous.clone(),
                },
            );
        });
        let begun = self.inner.services.begin_transfer(
            TransferRequest {
                url,
                mime_type: mime_type.to_owned(),
                timeout_ms: UPLOAD_TIMEOUT_MS,
            },
            on_progress,
        );
        {
            let mut inner = job.inner.lock();
            if inner.cancelled {
                let abort = begun.abort.clone();
                drop(inner);
                abort();
            } else {
                inner.abort = Some(begun.abort);
            }
        }
        match begun.done.await {
            Ok(()) => CycleResult::Uploaded {
                attachment_id: minted.attachment_id,
            },
            Err(error) => CycleResult::Failed {
                step: FailStep::Transfer,
                attachment_id: Some(minted.attachment_id),
                message: error.to_string(),
            },
        }
    }

    fn resolve_upload_url(&self, job: &Job, relative_url: &str) -> Option<String> {
        if job.inner.lock().cancelled || !self.inner.services.can_upload(&job.environment_id) {
            return None;
        }
        let base = self.inner.services.http_base_url(&job.environment_id)?;
        resolve_asset_url(&base, relative_url)
    }

    fn stamp(&self, job: &Job, attachment_id: &str) {
        let Some(key) = self.resolve_draft_key(job) else {
            return;
        };
        self.inner
            .drafts
            .set_file_upload(&key, job.image.id(), &job.environment_id, attachment_id);
    }

    fn mark_missing(&self, job: &Job, attachment_id: &str) -> bool {
        let Some(key) = self.resolve_draft_key(job) else {
            return false;
        };
        self.inner.drafts.mark_file_upload_missing(
            &key,
            job.image.id(),
            &job.environment_id,
            attachment_id,
        )
    }

    fn resolve_draft_key(&self, job: &Job) -> Option<String> {
        let target = job.draft_target.as_ref()?;
        if !job.image.is_file() {
            return None;
        }
        for row in self.inner.drafts.rows() {
            if !row.file_ids.iter().any(|id| id == job.image.id()) {
                continue;
            }
            if let Some(session_env) = &row.session_environment_id {
                if session_env == &job.environment_id {
                    return Some(row.key);
                }
                continue;
            }
            if let ComposerThreadTarget::Draft(draft_id) = target
                && draft_id == &row.key
            {
                return Some(row.key);
            }
            if parse_scoped_thread_key(&row.key).is_some_and(|(env, _)| env == job.environment_id) {
                return Some(row.key);
            }
        }
        None
    }

    fn fail(&self, job: &Job, reason: &str, attachment_id: Option<String>) {
        self.set_state(
            job.image.id(),
            AttachmentUploadState::Failed {
                environment_id: job.environment_id.clone(),
                reason: reason.to_owned(),
                attachment_id,
                previous: job.previous.clone(),
            },
        );
    }
}

enum FailStep {
    Mint,
    ResolveUrl,
    Transfer,
}

enum CycleResult {
    Uploaded {
        attachment_id: String,
    },
    Cancelled,
    Failed {
        step: FailStep,
        attachment_id: Option<String>,
        message: String,
    },
}

fn uploaded_from(image: &ComposerUploadAttachment, attachment_id: &str) -> UploadedAttachment {
    match image {
        ComposerUploadAttachment::Image {
            name,
            mime_type,
            size_bytes,
            source,
            ..
        } => UploadedAttachment::Image {
            id: attachment_id.to_owned(),
            name: name.clone(),
            mime_type: mime_type.clone(),
            size_bytes: *size_bytes,
            source: source.clone(),
        },
        ComposerUploadAttachment::File {
            name,
            mime_type,
            size_bytes,
            source,
            ..
        } => UploadedAttachment::File {
            id: attachment_id.to_owned(),
            name: name.clone(),
            mime_type: mime_type.clone(),
            size_bytes: *size_bytes,
            source: *source,
        },
    }
}

fn parse_scoped_thread_key(key: &str) -> Option<(String, String)> {
    let separator = key.find(':')?;
    if separator == 0 || separator + 1 >= key.len() {
        return None;
    }
    Some((key[..separator].to_owned(), key[separator + 1..].to_owned()))
}

trait UploadStatusExt {
    fn is_status_ready_same(&self, environment_id: &str) -> bool;
    fn is_failed_same(&self, environment_id: &str) -> bool;
}

impl UploadStatusExt for AttachmentUploadState {
    fn is_status_ready_same(&self, environment_id: &str) -> bool {
        self.is_ready_for(environment_id)
    }

    fn is_failed_same(&self, environment_id: &str) -> bool {
        matches!(self, Self::Failed { environment_id: env, .. } if env == environment_id)
    }
}

#[cfg(test)]
#[path = "attachment_upload_queue_tests.rs"]
mod tests;

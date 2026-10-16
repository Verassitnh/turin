//! Port of upstream/apps/web/src/lib/attachmentUploadQueue.test.ts

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use tokio::sync::{Notify, oneshot};

use super::*;
use crate::composer::attachment_files::video_mime_type;

const ENV1: &str = "environment-1";
const ENV2: &str = "environment-2";

type ConnectionListener = Arc<dyn Fn() + Send + Sync>;

struct FakeInner {
    allow_all: bool,
    allowed: HashSet<String>,
    connected: HashMap<String, bool>,
    base_url: Option<String>,
    listeners: HashMap<String, Vec<(u64, ConnectionListener)>>,
    next_listener: u64,
    verifications: VecDeque<ScriptedQuery>,
    held_mint: Option<oneshot::Receiver<MintResult>>,
    calls: Vec<ServiceCall>,
    xhrs: Vec<Arc<TestXhr>>,
    delete_tx: Option<oneshot::Sender<()>>,
}

enum ScriptedQuery {
    Immediate(AssetQueryResult),
    Wait(oneshot::Receiver<AssetQueryResult>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ServiceCall {
    Mint {
        environment_id: String,
        is_file: bool,
        name: String,
        mime_type: String,
        size_bytes: u64,
    },
    Delete {
        environment_id: String,
        attachment_id: String,
    },
    Asset {
        environment_id: String,
        attachment_id: String,
    },
}

struct FakeServices {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeServices {
    fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeInner {
                allow_all: true,
                allowed: HashSet::new(),
                connected: HashMap::new(),
                base_url: Some("https://environment.test/".to_owned()),
                listeners: HashMap::new(),
                next_listener: 1,
                verifications: VecDeque::new(),
                held_mint: None,
                calls: Vec::new(),
                xhrs: Vec::new(),
                delete_tx: None,
            })),
        }
    }

    fn allow_all(&self) {
        let mut inner = self.inner.lock();
        inner.allow_all = true;
        inner.allowed.clear();
    }

    fn allow_only(&self, environment_id: &str) {
        let mut inner = self.inner.lock();
        inner.allow_all = false;
        inner.allowed.clear();
        inner.allowed.insert(environment_id.to_owned());
    }

    fn deny_all(&self) {
        let mut inner = self.inner.lock();
        inner.allow_all = false;
        inner.allowed.clear();
    }

    fn set_connected(&self, environment_id: &str, connected: bool) {
        let listeners = {
            let mut inner = self.inner.lock();
            inner.connected.insert(environment_id.to_owned(), connected);
            inner
                .listeners
                .get(environment_id)
                .cloned()
                .unwrap_or_default()
        };
        for (_, listener) in listeners {
            listener();
        }
    }

    fn hold_next_mint(&self) -> oneshot::Sender<MintResult> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().held_mint = Some(rx);
        tx
    }

    fn script_verification(&self, result: AssetQueryResult) {
        self.inner
            .lock()
            .verifications
            .push_back(ScriptedQuery::Immediate(result));
    }

    fn hold_next_verification(&self) -> oneshot::Sender<AssetQueryResult> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .lock()
            .verifications
            .push_back(ScriptedQuery::Wait(rx));
        tx
    }

    fn notify_on_delete(&self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.inner.lock().delete_tx = Some(tx);
        rx
    }

    fn xhrs(&self) -> Vec<Arc<TestXhr>> {
        self.inner.lock().xhrs.clone()
    }

    fn mint_count(&self) -> usize {
        self.inner
            .lock()
            .calls
            .iter()
            .filter(|call| matches!(call, ServiceCall::Mint { .. }))
            .count()
    }

    fn command_count(&self) -> usize {
        self.inner
            .lock()
            .calls
            .iter()
            .filter(|call| !matches!(call, ServiceCall::Asset { .. }))
            .count()
    }

    fn mints(&self) -> Vec<ServiceCall> {
        self.inner
            .lock()
            .calls
            .iter()
            .filter(|call| matches!(call, ServiceCall::Mint { .. }))
            .cloned()
            .collect()
    }

    fn deletes(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .calls
            .iter()
            .filter_map(|call| match call {
                ServiceCall::Delete {
                    environment_id,
                    attachment_id,
                } => Some((environment_id.clone(), attachment_id.clone())),
                _ => None,
            })
            .collect()
    }

    fn assets(&self) -> Vec<(String, String)> {
        self.inner
            .lock()
            .calls
            .iter()
            .filter_map(|call| match call {
                ServiceCall::Asset {
                    environment_id,
                    attachment_id,
                } => Some((environment_id.clone(), attachment_id.clone())),
                _ => None,
            })
            .collect()
    }
}

#[async_trait]
impl AttachmentUploadServices for FakeServices {
    fn can_upload(&self, environment_id: &str) -> bool {
        let inner = self.inner.lock();
        inner.allow_all || inner.allowed.contains(environment_id)
    }

    fn is_connected(&self, environment_id: &str) -> bool {
        self.inner
            .lock()
            .connected
            .get(environment_id)
            .copied()
            .unwrap_or(false)
    }

    fn watch_connection(
        &self,
        environment_id: &str,
        on_change: Arc<dyn Fn() + Send + Sync>,
    ) -> Box<dyn FnOnce() + Send> {
        let id = {
            let mut inner = self.inner.lock();
            let id = inner.next_listener;
            inner.next_listener += 1;
            inner
                .listeners
                .entry(environment_id.to_owned())
                .or_default()
                .push((id, on_change));
            id
        };
        let inner = Arc::clone(&self.inner);
        let environment_id = environment_id.to_owned();
        Box::new(move || {
            if let Some(listeners) = inner.lock().listeners.get_mut(&environment_id) {
                listeners.retain(|(listener_id, _)| *listener_id != id);
            }
        })
    }

    fn http_base_url(&self, _environment_id: &str) -> Option<String> {
        self.inner.lock().base_url.clone()
    }

    async fn query_attachment(
        &self,
        environment_id: &str,
        attachment_id: &str,
    ) -> AssetQueryResult {
        let script = {
            let mut inner = self.inner.lock();
            inner.calls.push(ServiceCall::Asset {
                environment_id: environment_id.to_owned(),
                attachment_id: attachment_id.to_owned(),
            });
            inner.verifications.pop_front()
        };
        match script {
            Some(ScriptedQuery::Immediate(result)) => result,
            Some(ScriptedQuery::Wait(rx)) => rx.await.unwrap_or(AssetQueryResult::Failure {
                tag: None,
                message: "verification dropped".to_owned(),
            }),
            None => AssetQueryResult::Success,
        }
    }

    async fn mint_upload(&self, environment_id: &str, claim: &UploadClaim) -> MintResult {
        let held = {
            let mut inner = self.inner.lock();
            inner.calls.push(ServiceCall::Mint {
                environment_id: environment_id.to_owned(),
                is_file: claim.is_file,
                name: claim.name.clone(),
                mime_type: claim.mime_type.clone(),
                size_bytes: claim.size_bytes,
            });
            inner.held_mint.take()
        };
        if let Some(rx) = held {
            return rx.await.unwrap_or(MintResult::Failure {
                message: "mint dropped".to_owned(),
            });
        }
        let attachment_id = format!("pending-{environment_id}-{}", claim.name);
        MintResult::Success(MintedUpload {
            relative_url: format!("/api/attachments/upload/{attachment_id}"),
            attachment_id,
        })
    }

    fn delete_pending(&self, environment_id: &str, attachment_id: &str) {
        let mut inner = self.inner.lock();
        inner.calls.push(ServiceCall::Delete {
            environment_id: environment_id.to_owned(),
            attachment_id: attachment_id.to_owned(),
        });
        if let Some(tx) = inner.delete_tx.take() {
            let _ = tx.send(());
        }
    }

    fn begin_transfer(
        &self,
        request: TransferRequest,
        on_progress: Arc<dyn Fn(f64) + Send + Sync>,
    ) -> BegunTransfer {
        let xhr = Arc::new(TestXhr::new(request));
        self.inner.lock().xhrs.push(Arc::clone(&xhr));
        let waiting = Arc::clone(&xhr);
        let done = Box::pin(async move {
            loop {
                let (progress, outcome) = waiting.take_events();
                for ratio in progress {
                    on_progress(ratio);
                }
                if let Some(outcome) = outcome {
                    return outcome;
                }
                waiting.notify.notified().await;
            }
        });
        let aborting = Arc::clone(&xhr);
        BegunTransfer {
            done,
            abort: Arc::new(move || aborting.abort()),
        }
    }
}

struct XhrInner {
    progress: Vec<f64>,
    outcome: Option<Result<(), TransferError>>,
}

struct TestXhr {
    method: String,
    url: String,
    timeout_ms: u64,
    headers: HashMap<String, String>,
    inner: Mutex<XhrInner>,
    notify: Notify,
}

impl TestXhr {
    fn new(request: TransferRequest) -> Self {
        let mut headers = HashMap::new();
        headers.insert("Content-Type".to_owned(), request.mime_type);
        Self {
            method: "POST".to_owned(),
            url: request.url,
            timeout_ms: request.timeout_ms,
            headers,
            inner: Mutex::new(XhrInner {
                progress: Vec::new(),
                outcome: None,
            }),
            notify: Notify::new(),
        }
    }

    fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).cloned()
    }

    fn progress(&self, loaded: u64, total: u64) {
        if total == 0 {
            return;
        }
        self.inner
            .lock()
            .progress
            .push(loaded as f64 / total as f64);
        self.notify.notify_one();
    }

    fn complete(&self, status: u16) {
        let outcome = if (200..300).contains(&status) {
            Ok(())
        } else {
            Err(TransferError::Rejected(status))
        };
        self.finish(outcome);
    }

    fn abort(&self) {
        let mut inner = self.inner.lock();
        if inner.outcome.is_none() {
            inner.outcome = Some(Err(TransferError::Cancelled));
            drop(inner);
            self.notify.notify_one();
        }
    }

    fn finish(&self, outcome: Result<(), TransferError>) {
        self.inner.lock().outcome = Some(outcome);
        self.notify.notify_one();
    }

    fn take_events(&self) -> (Vec<f64>, Option<Result<(), TransferError>>) {
        let mut inner = self.inner.lock();
        (std::mem::take(&mut inner.progress), inner.outcome.take())
    }
}

#[derive(Clone, Debug)]
struct DraftFile {
    id: String,
    name: String,
    mime_type: String,
    size_bytes: u64,
    has_bytes: bool,
    uploaded_attachment_id: Option<String>,
    upload_environment_id: Option<String>,
}

struct MemoryRow {
    key: String,
    session_environment_id: Option<String>,
    files: Vec<DraftFile>,
}

#[derive(Default)]
struct MemoryDrafts {
    rows: Mutex<Vec<MemoryRow>>,
}

impl MemoryDrafts {
    fn add_file(&self, key: &str, file: DraftFile) {
        let mut rows = self.rows.lock();
        if !rows.iter().any(|row| row.key == key) {
            rows.push(MemoryRow {
                key: key.to_owned(),
                session_environment_id: None,
                files: Vec::new(),
            });
        }
        let row = rows.iter_mut().find(|row| row.key == key).expect("row");
        if row.files.iter().any(|existing| existing.id == file.id) {
            return;
        }
        if let Some(index) = row
            .files
            .iter()
            .position(|existing| needs_reattach(existing) && matches_reattach(existing, &file))
        {
            row.files[index] = file;
            return;
        }
        if row
            .files
            .iter()
            .any(|existing| dedup_key(existing) == dedup_key(&file))
        {
            return;
        }
        row.files.push(file);
    }

    fn files(&self, key: &str) -> Vec<DraftFile> {
        self.rows
            .lock()
            .iter()
            .find(|row| row.key == key)
            .map(|row| row.files.clone())
            .unwrap_or_default()
    }
}

fn needs_reattach(file: &DraftFile) -> bool {
    !file.has_bytes && file.uploaded_attachment_id.is_none()
}

fn dedup_key(file: &DraftFile) -> String {
    format!("{}\0{}\0{}", file.mime_type, file.size_bytes, file.name)
}

fn matches_reattach(marker: &DraftFile, file: &DraftFile) -> bool {
    if marker.name != file.name || marker.size_bytes != file.size_bytes {
        return false;
    }
    if marker.mime_type == file.mime_type {
        return true;
    }
    let marker_mime = marker.mime_type.to_lowercase();
    (marker_mime.is_empty() || marker_mime == "application/octet-stream")
        && video_mime_type(&marker.name, &marker.mime_type).is_some()
        && video_mime_type(&file.name, &file.mime_type).is_some()
}

impl ComposerDraftUploads for MemoryDrafts {
    fn rows(&self) -> Vec<DraftRow> {
        self.rows
            .lock()
            .iter()
            .map(|row| DraftRow {
                key: row.key.clone(),
                session_environment_id: row.session_environment_id.clone(),
                file_ids: row.files.iter().map(|file| file.id.clone()).collect(),
            })
            .collect()
    }

    fn set_file_upload(
        &self,
        thread_key: &str,
        file_id: &str,
        environment_id: &str,
        attachment_id: &str,
    ) {
        let mut rows = self.rows.lock();
        let Some(file) = rows
            .iter_mut()
            .find(|row| row.key == thread_key)
            .and_then(|row| row.files.iter_mut().find(|file| file.id == file_id))
        else {
            return;
        };
        if file.upload_environment_id.as_deref() == Some(environment_id)
            && file.uploaded_attachment_id.as_deref() == Some(attachment_id)
        {
            return;
        }
        file.uploaded_attachment_id = Some(attachment_id.to_owned());
        file.upload_environment_id = Some(environment_id.to_owned());
    }

    fn mark_file_upload_missing(
        &self,
        thread_key: &str,
        file_id: &str,
        environment_id: &str,
        attachment_id: &str,
    ) -> bool {
        let mut rows = self.rows.lock();
        let Some(file) = rows
            .iter_mut()
            .find(|row| row.key == thread_key)
            .and_then(|row| row.files.iter_mut().find(|file| file.id == file_id))
        else {
            return false;
        };
        if file.has_bytes
            || file.upload_environment_id.as_deref() != Some(environment_id)
            || file.uploaded_attachment_id.as_deref() != Some(attachment_id)
        {
            return false;
        }
        file.uploaded_attachment_id = None;
        file.upload_environment_id = None;
        true
    }
}

struct Harness {
    queue: AttachmentUploadQueue,
    services: Arc<FakeServices>,
    drafts: Arc<MemoryDrafts>,
}

impl Harness {
    fn new() -> Self {
        let services = Arc::new(FakeServices::new());
        services.set_connected(ENV1, true);
        services.set_connected(ENV2, true);
        let drafts = Arc::new(MemoryDrafts::default());
        let queue = AttachmentUploadQueue::new(
            Arc::clone(&services) as Arc<dyn AttachmentUploadServices>,
            Arc::clone(&drafts) as Arc<dyn ComposerDraftUploads>,
            tokio::runtime::Handle::current(),
        );
        Self {
            queue,
            services,
            drafts,
        }
    }
}

fn image(id: &str) -> ComposerUploadAttachment {
    ComposerUploadAttachment::Image {
        id: id.to_owned(),
        name: format!("{id}.png"),
        mime_type: "image/png".to_owned(),
        size_bytes: 3,
        bytes: Some(vec![1, 2, 3]),
        source: None,
    }
}

fn file(id: &str) -> ComposerUploadAttachment {
    ComposerUploadAttachment::File {
        id: id.to_owned(),
        name: format!("{id}.pdf"),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: Some(vec![1, 2, 3]),
        uploaded_attachment_id: None,
        upload_environment_id: None,
        source: None,
    }
}

fn draft_file(attachment: &ComposerUploadAttachment) -> DraftFile {
    match attachment {
        ComposerUploadAttachment::File {
            id,
            name,
            mime_type,
            size_bytes,
            bytes,
            uploaded_attachment_id,
            upload_environment_id,
            ..
        } => DraftFile {
            id: id.clone(),
            name: name.clone(),
            mime_type: mime_type.clone(),
            size_bytes: *size_bytes,
            has_bytes: bytes.is_some(),
            uploaded_attachment_id: uploaded_attachment_id.clone(),
            upload_environment_id: upload_environment_id.clone(),
        },
        ComposerUploadAttachment::Image { .. } => panic!("images are not draft files"),
    }
}

async fn flush() {
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
}

async fn wait_for_xhrs(services: &FakeServices, count: usize) {
    for _ in 0..40 {
        if services.xhrs().len() >= count {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!(
        "expected at least {count} uploads, saw {}",
        services.xhrs().len()
    );
}

fn not_found() -> AssetQueryResult {
    AssetQueryResult::Failure {
        tag: Some("AssetAttachmentNotFoundError".to_owned()),
        message: "missing".to_owned(),
    }
}

fn socket_closed() -> AssetQueryResult {
    AssetQueryResult::Failure {
        tag: None,
        message: "socket closed".to_owned(),
    }
}

async fn retries_a_failed_file_once_after_reconnect(late_failure: bool) {
    let h = Harness::new();
    let attachment = file("reconnect");
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.set_connected(ENV1, false);
    if late_failure {
        h.services.set_connected(ENV1, true);
    }
    h.services.xhrs()[0].complete(503);
    settled.await;
    if !late_failure {
        h.services.set_connected(ENV1, true);
    }
    flush().await;
    assert_eq!(h.services.xhrs().len(), 2);
    let retry_settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[1].complete(503);
    retry_settled.await;
    h.services.set_connected(ENV1, true);
    flush().await;
    assert_eq!(h.services.xhrs().len(), 2);
    assert!(matches!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Failed { .. })
    ));

    h.services.set_connected(ENV1, false);
    h.services.set_connected(ENV1, true);
    flush().await;
    let final_settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[2].complete(204);
    final_settled.await;
    assert!(
        h.queue
            .get_uploaded_attachments(ENV1, &[attachment])
            .is_some()
    );
    h.services.set_connected(ENV1, false);
    h.services.set_connected(ENV1, true);
    flush().await;
    assert_eq!(h.services.xhrs().len(), 3);
}

#[tokio::test(flavor = "current_thread")]
async fn retries_after_reconnect_including_a_late_http_failure() {
    retries_a_failed_file_once_after_reconnect(true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn retries_after_reconnect_when_the_socket_drops_first() {
    retries_a_failed_file_once_after_reconnect(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_retry_for_another_environment_or_after_the_attachment_is_removed() {
    let h = Harness::new();
    let attachment = file("removed");
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(503);
    settled.await;
    h.services.set_connected(ENV2, false);
    h.services.set_connected(ENV2, true);
    flush().await;
    assert_eq!(h.services.xhrs().len(), 1);
    h.services.set_connected(ENV1, false);
    h.services.set_connected(ENV1, true);
    h.queue.release(attachment.id());
    flush().await;
    assert_eq!(h.services.xhrs().len(), 1);
    assert_eq!(h.queue.read(attachment.id()), None);
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_an_attachment_local_until_its_own_environment_grants_write_access() {
    let h = Harness::new();
    let attachment = image("permission-gain");
    h.services.allow_only(ENV1);
    h.queue.start(ENV2, attachment.clone(), None);
    assert_eq!(h.services.command_count(), 0);
    assert!(h.services.xhrs().is_empty());
    assert_eq!(h.queue.read(attachment.id()), None);
    assert!(matches!(
        attachment,
        ComposerUploadAttachment::Image { bytes: Some(_), .. }
    ));

    h.services.allow_all();
    h.queue.start(ENV2, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Ready(ready)) => assert_eq!(ready.environment_id, ENV2),
        other => panic!("expected ready, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_transfer_bytes_or_retry_when_access_is_revoked_while_minting() {
    let h = Harness::new();
    let attachment = image("revoke-before-bytes");
    let finish_mint = h.services.hold_next_mint();
    h.queue.start(ENV1, attachment.clone(), None);
    // The mint is already in flight (upstream runs until its first await
    // before the test revokes access).
    flush().await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.deny_all();
    finish_mint
        .send(MintResult::Success(MintedUpload {
            attachment_id: "pending-revoked".to_owned(),
            relative_url: "/api/attachments/upload/pending-revoked".to_owned(),
        }))
        .unwrap();
    settled.await;
    assert!(h.services.xhrs().is_empty());
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Failed {
            reason,
            attachment_id,
            ..
        }) => {
            assert_eq!(reason, "This connection cannot upload attachments.");
            assert_eq!(attachment_id.as_deref(), Some("pending-revoked"));
        }
        other => panic!("expected failed, got {other:?}"),
    }
    h.queue.retry(ENV1, attachment.clone(), None);
    assert!(matches!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Failed { .. })
    ));
    h.queue.release(attachment.id());
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(h.services.command_count(), 1);
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_mint_queued_uploads_after_access_is_revoked() {
    let h = Harness::new();
    let images: Vec<_> = (0..4)
        .map(|index| image(&format!("queued-{index}")))
        .collect();
    for attachment in &images {
        h.queue.start(ENV1, attachment.clone(), None);
    }
    wait_for_xhrs(&h.services, 3).await;
    let ids: Vec<_> = images
        .iter()
        .map(|attachment| attachment.id().to_owned())
        .collect();
    let settled = h.queue.await_attachment_uploads(ids);
    h.services.deny_all();
    for xhr in h.services.xhrs() {
        xhr.complete(204);
    }
    settled.await;
    assert_eq!(h.services.mint_count(), 3);
    assert_eq!(h.services.xhrs().len(), 3);
    match h.queue.read(images[3].id()) {
        Some(AttachmentUploadState::Failed { reason, .. }) => {
            assert_eq!(reason, "This connection cannot upload attachments.");
        }
        other => panic!("expected failed, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_delete_a_persisted_upload_using_another_environments_grant() {
    let h = Harness::new();
    h.services.allow_only(ENV1);
    h.queue
        .release_persisted("local-file", ENV2, "persisted-file");
    assert_eq!(h.services.command_count(), 0);

    h.services.allow_all();
    h.queue
        .release_persisted("local-file", ENV2, "persisted-file");
    assert_eq!(
        h.services.deletes(),
        vec![(ENV2.to_owned(), "persisted-file".to_owned())]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn uploads_images_immediately_and_sends_attachment_references() {
    let h = Harness::new();
    let mut attachment = image("image-1");
    if let ComposerUploadAttachment::Image { source, .. } = &mut attachment {
        *source = Some(SnapShotAttachmentSource {
            kind: "snap-shot",
            captured_at: "2026-08-24T11:00:00.000Z".to_owned(),
            app_name: "Terminal".to_owned(),
            window_title: "Tests".to_owned(),
        });
    }
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let request = &h.services.xhrs()[0];
    assert_eq!(request.method, "POST");
    assert_eq!(
        request.url,
        "https://environment.test/api/attachments/upload/pending-environment-1-image-1.png"
    );
    assert_eq!(request.timeout_ms, 5 * 60_000);
    request.progress(1, 3);
    flush().await;
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Uploading { progress, .. }) => assert_eq!(progress, 1.0 / 3.0),
        other => panic!("expected uploading, got {other:?}"),
    }
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    request.complete(204);
    settled.await;
    assert_eq!(
        h.queue
            .get_uploaded_attachments(ENV1, &[attachment.clone()]),
        Some(vec![UploadedAttachment::Image {
            id: "pending-environment-1-image-1.png".to_owned(),
            name: "image-1.png".to_owned(),
            mime_type: "image/png".to_owned(),
            size_bytes: 3,
            source: Some(SnapShotAttachmentSource {
                kind: "snap-shot",
                captured_at: "2026-08-24T11:00:00.000Z".to_owned(),
                app_name: "Terminal".to_owned(),
                window_title: "Tests".to_owned(),
            }),
        }])
    );
    h.queue.release_draft_attachments(&[attachment.clone()]);
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(
        h.services.deletes(),
        vec![(
            ENV1.to_owned(),
            "pending-environment-1-image-1.png".to_owned()
        )]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn uploads_generic_files_and_sends_file_attachment_references() {
    let h = Harness::new();
    let mut attachment = file("report");
    if let ComposerUploadAttachment::File { source, .. } = &mut attachment {
        *source = Some(PastedTextAttachmentSource);
    }
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    assert_eq!(
        h.services.mints(),
        vec![ServiceCall::Mint {
            environment_id: ENV1.to_owned(),
            is_file: true,
            name: "report.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 3,
        }]
    );
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
    assert_eq!(
        h.queue.get_uploaded_attachments(ENV1, &[attachment]),
        Some(vec![UploadedAttachment::File {
            id: "pending-environment-1-report.pdf".to_owned(),
            name: "report.pdf".to_owned(),
            mime_type: "application/pdf".to_owned(),
            size_bytes: 3,
            source: Some(PastedTextAttachmentSource),
        }])
    );
}

#[tokio::test(flavor = "current_thread")]
async fn uses_the_fallback_mime_type_for_the_upload_claim_and_request_header() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "file-without-browser-mime".to_owned(),
        name: "unknown.bin".to_owned(),
        mime_type: "application/octet-stream".to_owned(),
        size_bytes: 3,
        bytes: Some(vec![1, 2, 3]),
        uploaded_attachment_id: None,
        upload_environment_id: None,
        source: None,
    };
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    assert_eq!(
        h.services.mints(),
        vec![ServiceCall::Mint {
            environment_id: ENV1.to_owned(),
            is_file: true,
            name: "unknown.bin".to_owned(),
            mime_type: "application/octet-stream".to_owned(),
            size_bytes: 3,
        }]
    );
    assert_eq!(
        h.services.xhrs()[0].header("Content-Type").as_deref(),
        Some("application/octet-stream")
    );
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
}

#[tokio::test(flavor = "current_thread")]
async fn persists_a_background_completions_ids_with_no_composer_mounted() {
    let h = Harness::new();
    let draft_id = "draft-background-upload";
    let attachment = file("background");
    h.drafts.add_file(draft_id, draft_file(&attachment));
    h.queue.start(
        ENV1,
        attachment.clone(),
        Some(ComposerThreadTarget::Draft(draft_id.to_owned())),
    );
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
    let files = h.drafts.files(draft_id);
    assert_eq!(files[0].id, attachment.id());
    assert_eq!(
        files[0].uploaded_attachment_id.as_deref(),
        Some("pending-environment-1-background.pdf")
    );
    assert_eq!(files[0].upload_environment_id.as_deref(), Some(ENV1));
}

#[tokio::test(flavor = "current_thread")]
async fn persists_a_retried_question_upload_without_a_mounted_composer() {
    let h = Harness::new();
    let draft_id = "question-retry-upload";
    let attachment = file("question-retry");
    h.drafts.add_file(draft_id, draft_file(&attachment));
    let target = Some(ComposerThreadTarget::Draft(draft_id.to_owned()));
    h.queue.start(ENV1, attachment.clone(), target.clone());
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(500);
    settled.await;
    assert_eq!(h.drafts.files(draft_id)[0].uploaded_attachment_id, None);
    h.queue.retry(ENV1, attachment.clone(), target);
    wait_for_xhrs(&h.services, 2).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[1].complete(204);
    settled.await;
    let files = h.drafts.files(draft_id);
    assert_eq!(
        files[0].uploaded_attachment_id.as_deref(),
        Some("pending-environment-1-question-retry.pdf")
    );
    assert_eq!(files[0].upload_environment_id.as_deref(), Some(ENV1));
}

#[tokio::test(flavor = "current_thread")]
async fn verifies_an_uploaded_file_reference_before_restoring_it() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "restored".to_owned(),
        name: "restored.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-restored-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.queue.start(ENV1, attachment.clone(), None);
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    assert_eq!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: ENV1.to_owned(),
            attachment_id: "pending-restored-pdf".to_owned(),
        }))
    );
    assert_eq!(
        h.services.assets(),
        vec![(ENV1.to_owned(), "pending-restored-pdf".to_owned())]
    );
    assert!(h.services.xhrs().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn turns_an_expired_persisted_file_into_a_marker_a_repick_can_replace() {
    let h = Harness::new();
    let draft_id = "draft-expired-upload";
    let attachment = ComposerUploadAttachment::File {
        id: "expired".to_owned(),
        name: "expired.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-expired-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.services.script_verification(not_found());
    h.drafts.add_file(draft_id, draft_file(&attachment));
    h.queue.start(
        ENV1,
        attachment.clone(),
        Some(ComposerThreadTarget::Draft(draft_id.to_owned())),
    );
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    let marker = &h.drafts.files(draft_id)[0];
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(marker.uploaded_attachment_id, None);
    assert_eq!(marker.upload_environment_id, None);
    assert!(needs_reattach(marker));

    let replacement = ComposerUploadAttachment::File {
        id: "expired-repicked".to_owned(),
        name: "expired.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: Some(vec![1, 2, 3]),
        uploaded_attachment_id: None,
        upload_environment_id: None,
        source: None,
    };
    h.drafts.add_file(draft_id, draft_file(&replacement));
    let files = h.drafts.files(draft_id);
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].id, replacement.id());
    assert!(files[0].has_bytes);

    h.queue.start(
        ENV1,
        replacement.clone(),
        Some(ComposerThreadTarget::Draft(draft_id.to_owned())),
    );
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![replacement.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
    let files = h.drafts.files(draft_id);
    assert_eq!(files[0].id, replacement.id());
    assert_eq!(
        files[0].uploaded_attachment_id.as_deref(),
        Some("pending-environment-1-expired.pdf")
    );
    assert_eq!(files[0].upload_environment_id.as_deref(), Some(ENV1));
    assert!(h.services.deletes().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn uploads_the_original_file_again_when_its_persisted_server_upload_expired() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "recoverable".to_owned(),
        name: "recoverable.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: Some(vec![1, 2, 3]),
        uploaded_attachment_id: Some("pending-expired-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.services.script_verification(not_found());
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Ready(ready)) => {
            assert_eq!(ready.attachment_id, "pending-environment-1-recoverable.pdf");
        }
        other => panic!("expected ready, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn removes_a_persisted_upload_when_its_draft_is_discarded_during_verification() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "checking".to_owned(),
        name: "checking.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-checking-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    let resolve = h.services.hold_next_verification();
    h.queue.start(ENV1, attachment.clone(), None);
    flush().await;
    h.queue.release_draft_attachment(&attachment);
    resolve.send(AssetQueryResult::Success).unwrap();
    flush().await;
    assert_eq!(
        h.services.deletes(),
        vec![(ENV1.to_owned(), "pending-checking-pdf".to_owned())]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_the_persisted_upload_when_retrying_after_a_transient_verification_failure() {
    let h = Harness::new();
    let draft_id = "draft-transient-verification";
    let attachment = ComposerUploadAttachment::File {
        id: "flaky".to_owned(),
        name: "flaky.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-flaky-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.services.script_verification(socket_closed());
    h.drafts.add_file(draft_id, draft_file(&attachment));
    let target = Some(ComposerThreadTarget::Draft(draft_id.to_owned()));
    h.queue.start(ENV1, attachment.clone(), target.clone());
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Failed {
            reason,
            attachment_id,
            ..
        }) => {
            assert_eq!(
                reason,
                "Uploaded file could not be verified. Retry when the server reconnects."
            );
            assert_eq!(attachment_id, None);
        }
        other => panic!("expected failed, got {other:?}"),
    }
    let files = h.drafts.files(draft_id);
    assert_eq!(
        files[0].uploaded_attachment_id.as_deref(),
        Some("pending-flaky-pdf")
    );
    assert_eq!(files[0].upload_environment_id.as_deref(), Some(ENV1));

    h.queue.retry(ENV1, attachment.clone(), target);
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    assert_eq!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Ready(ReadyAttachmentUpload {
            environment_id: ENV1.to_owned(),
            attachment_id: "pending-flaky-pdf".to_owned(),
        }))
    );
    let files = h.drafts.files(draft_id);
    assert_eq!(
        files[0].uploaded_attachment_id.as_deref(),
        Some("pending-flaky-pdf")
    );
    assert_eq!(files[0].upload_environment_id.as_deref(), Some(ENV1));
    assert!(h.services.deletes().is_empty());
}

#[tokio::test(flavor = "current_thread")]
async fn keeps_the_persisted_upload_when_an_environment_switch_cancels_verification() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "moving".to_owned(),
        name: "moving.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-moving-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    let resolve = h.services.hold_next_verification();
    h.queue.start(ENV1, attachment.clone(), None);
    flush().await;
    h.queue.start(ENV2, attachment.clone(), None);
    resolve.send(AssetQueryResult::Success).unwrap();
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    flush().await;
    assert!(
        !h.services
            .deletes()
            .iter()
            .any(|(_, id)| id == "pending-moving-pdf")
    );
}

#[tokio::test(flavor = "current_thread")]
async fn cancels_persisted_upload_verification_when_a_stash_discards_its_file() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "stashed-checking".to_owned(),
        name: "stashed-checking.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-stashed-checking-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    let resolve = h.services.hold_next_verification();
    h.queue.start(ENV1, attachment.clone(), None);
    flush().await;
    h.queue
        .release_persisted(attachment.id(), ENV1, "pending-stashed-checking-pdf");
    resolve.send(AssetQueryResult::Success).unwrap();
    flush().await;
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(
        h.services.deletes(),
        vec![(ENV1.to_owned(), "pending-stashed-checking-pdf".to_owned())]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn releases_the_persisted_server_upload_when_a_hydrated_draft_is_discarded() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "hydrated".to_owned(),
        name: "hydrated.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-hydrated-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.queue.release_draft_attachment(&attachment);
    assert_eq!(
        h.services.deletes(),
        vec![(ENV1.to_owned(), "pending-hydrated-pdf".to_owned())]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn routes_a_live_persisted_upload_through_the_queue_release_exactly_once() {
    let h = Harness::new();
    let attachment = ComposerUploadAttachment::File {
        id: "live".to_owned(),
        name: "live.pdf".to_owned(),
        mime_type: "application/pdf".to_owned(),
        size_bytes: 3,
        bytes: None,
        uploaded_attachment_id: Some("pending-live-pdf".to_owned()),
        upload_environment_id: Some(ENV1.to_owned()),
        source: None,
    };
    h.queue.start(ENV1, attachment.clone(), None);
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    assert!(matches!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Ready(_))
    ));
    h.queue.release_draft_attachment(&attachment);
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(
        h.services.deletes(),
        vec![(ENV1.to_owned(), "pending-live-pdf".to_owned())]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn deletes_a_persisted_server_upload_even_when_browser_upload_state_is_gone() {
    let h = Harness::new();
    h.queue.release_persisted(
        "stashed-report",
        ENV1,
        "pending-00000000-0000-4000-8000-000000000001-pdf",
    );
    assert_eq!(
        h.services.deletes(),
        vec![(
            ENV1.to_owned(),
            "pending-00000000-0000-4000-8000-000000000001-pdf".to_owned()
        )]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn retries_rejected_uploads() {
    let h = Harness::new();
    let attachment = image("image-retry");
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(500);
    settled.await;
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Failed { reason, .. }) => {
            assert_eq!(reason, "Upload rejected (500)");
        }
        other => panic!("expected failed, got {other:?}"),
    }
    h.queue.retry(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 2).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[1].complete(204);
    settled.await;
    assert!(matches!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Ready(_))
    ));
}

#[tokio::test(flavor = "current_thread")]
async fn releases_an_upload_url_that_resolves_after_its_image_was_removed() {
    let h = Harness::new();
    let attachment = image("image-cancelled");
    let finish_mint = h.services.hold_next_mint();
    let deleted = h.services.notify_on_delete();
    h.queue.start(ENV1, attachment.clone(), None);
    flush().await;
    h.queue.release(attachment.id());
    finish_mint
        .send(MintResult::Success(MintedUpload {
            attachment_id: "pending-environment-1-image-cancelled.png".to_owned(),
            relative_url: "/api/attachments/upload/cancelled".to_owned(),
        }))
        .unwrap();
    deleted.await.unwrap();
    assert!(h.services.xhrs().is_empty());
    assert_eq!(h.queue.read(attachment.id()), None);
    assert_eq!(
        h.services.deletes(),
        vec![(
            ENV1.to_owned(),
            "pending-environment-1-image-cancelled.png".to_owned()
        )]
    );
}

#[tokio::test(flavor = "current_thread")]
async fn restores_the_previous_environment_after_a_replacement_upload_fails() {
    let h = Harness::new();
    let attachment = image("image-move");
    h.queue.start(ENV1, attachment.clone(), None);
    wait_for_xhrs(&h.services, 1).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[0].complete(204);
    settled.await;

    h.queue.start(ENV2, attachment.clone(), None);
    wait_for_xhrs(&h.services, 2).await;
    let settled = h
        .queue
        .await_attachment_uploads(vec![attachment.id().to_owned()]);
    h.services.xhrs()[1].complete(500);
    settled.await;

    h.queue.start(ENV1, attachment.clone(), None);
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Ready(ready)) => {
            assert_eq!(ready.environment_id, ENV1);
            assert_eq!(ready.attachment_id, "pending-environment-1-image-move.png");
        }
        other => panic!("expected the first environment restored, got {other:?}"),
    }
}

#[tokio::test(flavor = "current_thread")]
async fn does_not_let_stalled_uploads_block_another_environment() {
    let h = Harness::new();
    let images: Vec<_> = ["image-a", "image-b", "image-c", "image-d"]
        .into_iter()
        .map(image)
        .collect();
    for attachment in &images {
        h.queue.start(ENV1, attachment.clone(), None);
    }
    let other = image("image-other");
    h.queue.start(ENV2, other.clone(), None);
    wait_for_xhrs(&h.services, 4).await;
    assert!(
        h.services
            .xhrs()
            .iter()
            .any(|xhr| xhr.url.contains("environment-2"))
    );
    for xhr in h.services.xhrs() {
        xhr.complete(204);
    }
    let mut waiting = Vec::new();
    for attachment in images.iter().take(3) {
        waiting.push(
            h.queue
                .await_attachment_uploads(vec![attachment.id().to_owned()]),
        );
    }
    waiting.push(
        h.queue
            .await_attachment_uploads(vec![other.id().to_owned()]),
    );
    for pending in waiting {
        pending.await;
    }
    flush().await;
    h.services.xhrs()[4].complete(204);
    h.queue
        .await_attachment_uploads(vec![images[3].id().to_owned()])
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn fails_a_file_with_an_empty_mime_type_as_unsupported() {
    let h = Harness::new();
    let attachment = match file("no-mime") {
        ComposerUploadAttachment::File {
            id,
            name,
            size_bytes,
            bytes,
            ..
        } => ComposerUploadAttachment::File {
            id,
            name,
            mime_type: String::new(),
            size_bytes,
            bytes,
            uploaded_attachment_id: None,
            upload_environment_id: None,
            source: None,
        },
        ComposerUploadAttachment::Image { .. } => unreachable!(),
    };
    h.queue.start(ENV1, attachment.clone(), None);
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    assert_eq!(h.services.mint_count(), 0);
    match h.queue.read(attachment.id()) {
        Some(AttachmentUploadState::Failed { reason, .. }) => {
            assert_eq!(reason, "Unsupported image type")
        }
        other => panic!("expected a failed upload, got {other:?}"),
    }
}

/// The GPUI main thread has no reactor entered: the queue spawns through the
/// handle it was given, so `start` works from a plain thread.
#[tokio::test(flavor = "current_thread")]
async fn starts_an_upload_from_a_thread_without_a_reactor() {
    let h = Harness::new();
    let attachment = file("off-runtime");
    let queue = h.queue.clone();
    let starting = attachment.clone();
    std::thread::spawn(move || queue.start(ENV1, starting, None))
        .join()
        .expect("start must not panic without a reactor");
    wait_for_xhrs(&h.services, 1).await;
    h.services.xhrs()[0].complete(200);
    h.queue
        .await_attachment_uploads(vec![attachment.id().to_owned()])
        .await;
    assert!(matches!(
        h.queue.read(attachment.id()),
        Some(AttachmentUploadState::Ready(_))
    ));
}

//! Port of upstream/apps/server/src/orchestration-v2/AttachmentClaims.ts
//! (plus `remapComposerContextAttachments` from
//! packages/shared/src/composerContextReferences.ts).
//!
//! Claims pending uploads into the target thread's attachment store before
//! the command enters the orchestrator: verifies the staged file, copies it
//! under a thread-scoped id, and rewrites the attachment ref. A copy, not a
//! move: the pending file stays behind as the retry source for a failed
//! bootstrap, and the periodic pending sweep reclaims it later. Already
//! claimed attachments pass through untouched.

use super::attachment_store::{
    AttachmentClaimPlan, PENDING_ATTACHMENT_THREAD_SEGMENT, attachment_fields,
    parse_thread_segment_from_attachment_id, plan_attachment_claim, resolve_attachment_path,
};
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use turin_contracts::chat_attachment::{
    ChatAttachment, ChatAttachmentId, get_provider_attachment_limit_error,
};
use turin_contracts::composer_context::{
    ComposerContextRecord, KnownComposerContextRecord, OrchestrationMessageContext,
};

#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct AttachmentClaimError {
    pub message: String,
    #[source]
    pub cause: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl AttachmentClaimError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            cause: None,
        }
    }

    fn with_cause(
        message: impl Into<String>,
        cause: impl Into<Box<dyn std::error::Error + Send + Sync>>,
    ) -> Self {
        Self {
            message: message.into(),
            cause: Some(cause.into()),
        }
    }
}

pub fn validate_attachment_limits(
    attachments: &[ChatAttachment],
) -> Result<(), AttachmentClaimError> {
    match get_provider_attachment_limit_error(attachments) {
        Some(message) => Err(AttachmentClaimError::new(message)),
        None => Ok(()),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ClaimedAttachments {
    pub attachments: Vec<ChatAttachment>,
    pub claimed_paths: Vec<PathBuf>,
}

pub fn attachment_is_pending_upload(attachment: &ChatAttachment) -> bool {
    parse_thread_segment_from_attachment_id(attachment_fields(attachment).id).as_deref()
        == Some(PENDING_ATTACHMENT_THREAD_SEGMENT)
}

/// Remove partial claims only before dispatch, or after proving they were not
/// accepted. Best effort: a path that is already gone is not an error.
pub async fn release_claimed_attachments(claimed_paths: &[PathBuf]) {
    for path in claimed_paths {
        let _ = tokio::fs::remove_file(path).await;
    }
}

type CopyGate = Arc<dyn Fn() -> BoxFuture<'static, ()> + Send + Sync>;

#[derive(Clone)]
pub struct AttachmentClaims {
    attachments_dir: PathBuf,
    /// Runs right before each copy; lets a test hold a claim mid-flight.
    copy_gate: Option<CopyGate>,
}

/// If the caller's future is dropped while the claim task runs, the task
/// still finishes (its copies cannot be cancelled), and whatever it claimed is
/// released afterwards.
struct InFlightClaim {
    task: Option<tokio::task::JoinHandle<Result<ClaimedAttachments, AttachmentClaimError>>>,
}

impl Drop for InFlightClaim {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            // Owned by this cleanup task, which awaits it: not a detached handle.
            tokio::spawn(async move {
                if let Ok(Ok(claimed)) = task.await {
                    release_claimed_attachments(&claimed.claimed_paths).await;
                }
            });
        }
    }
}

impl AttachmentClaims {
    pub fn new(attachments_dir: impl Into<PathBuf>) -> Self {
        Self {
            attachments_dir: attachments_dir.into(),
            copy_gate: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_copy_gate(mut self, gate: CopyGate) -> Self {
        self.copy_gate = Some(gate);
        self
    }

    pub fn attachments_dir(&self) -> &std::path::Path {
        &self.attachments_dir
    }

    /// Claims the pending uploads among `attachments` for `thread_id`.
    ///
    /// The copy and its rollback registration form one uninterruptible region
    /// (upstream: `fs.copyFile` cannot be cancelled, so an interrupt landing
    /// mid-copy still waits for the write to settle and records the path
    /// before cleanup runs). Here the whole claim runs as its own task; a
    /// cancelled caller releases what the task claimed once it finishes.
    pub async fn claim_pending_attachments(
        &self,
        thread_id: &str,
        attachments: &[ChatAttachment],
    ) -> Result<ClaimedAttachments, AttachmentClaimError> {
        validate_attachment_limits(attachments)?;
        let ids: HashSet<&str> = attachments
            .iter()
            .map(|attachment| attachment_fields(attachment).id)
            .collect();
        if ids.len() != attachments.len() {
            return Err(AttachmentClaimError::new(
                "Duplicate attachment ids are not allowed.",
            ));
        }
        if !attachments.iter().any(attachment_is_pending_upload) {
            return Ok(ClaimedAttachments {
                attachments: attachments.to_vec(),
                claimed_paths: vec![],
            });
        }
        let claims = self.clone();
        let thread_id = thread_id.to_owned();
        let attachments = attachments.to_vec();
        let mut in_flight = InFlightClaim {
            task: Some(tokio::spawn(async move {
                claims.claim_all(&thread_id, attachments).await
            })),
        };
        let task = in_flight.task.as_mut().expect("just set");
        let result = task.await;
        in_flight.task = None;
        result.map_err(|join| {
            AttachmentClaimError::with_cause("Attachment claim was interrupted.", join)
        })?
    }

    async fn claim_all(
        &self,
        thread_id: &str,
        attachments: Vec<ChatAttachment>,
    ) -> Result<ClaimedAttachments, AttachmentClaimError> {
        let mut claimed_paths: Vec<PathBuf> = Vec::new();
        let mut claimed = Vec::with_capacity(attachments.len());
        for attachment in attachments {
            match self
                .claim_one(thread_id, attachment, &mut claimed_paths)
                .await
            {
                Ok(attachment) => claimed.push(attachment),
                Err(error) => {
                    release_claimed_attachments(&claimed_paths).await;
                    return Err(error);
                }
            }
        }
        Ok(ClaimedAttachments {
            attachments: claimed,
            claimed_paths,
        })
    }

    async fn claim_one(
        &self,
        thread_id: &str,
        attachment: ChatAttachment,
        claimed_paths: &mut Vec<PathBuf>,
    ) -> Result<ChatAttachment, AttachmentClaimError> {
        if !attachment_is_pending_upload(&attachment) {
            return Ok(attachment);
        }
        let fields = attachment_fields(&attachment);
        let name = fields.name.to_owned();
        let cannot_send = |reason: &str| {
            AttachmentClaimError::new(format!("Attachment '{name}' cannot be sent: {reason}."))
        };
        let (final_id, current_path, final_path) =
            match plan_attachment_claim(&self.attachments_dir, thread_id, fields.id) {
                AttachmentClaimPlan::Ok {
                    final_id,
                    current_path,
                    final_path,
                } => (final_id, current_path, final_path),
                AttachmentClaimPlan::Rejected { reason } => return Err(cannot_send(reason)),
            };
        let info = tokio::fs::metadata(&current_path).await.map_err(|cause| {
            AttachmentClaimError::with_cause(
                format!("Attachment '{name}' cannot be sent: attachment not found."),
                cause,
            )
        })?;
        if info.len() != fields.size_bytes {
            return Err(cannot_send("stored size does not match"));
        }
        let normalized = normalized_attachment(&attachment, &final_id)
            .ok_or_else(|| cannot_send("attachment type does not match the upload"))?;
        let expected_path = resolve_attachment_path(&self.attachments_dir, &normalized);
        if expected_path.as_deref() != Some(final_path.as_path()) {
            return Err(cannot_send("attachment type does not match the upload"));
        }
        if let Some(gate) = &self.copy_gate {
            gate().await;
        }
        // A copy, not a hard link: an agent editing the delivered file in
        // place must not mutate the retry source.
        let copied = tokio::fs::copy(&current_path, &final_path).await;
        match copied {
            Ok(_) => claimed_paths.push(final_path),
            Err(cause) => {
                // A partial copy may have landed; remove it with the rest.
                claimed_paths.push(final_path);
                return Err(AttachmentClaimError::with_cause(
                    format!("Failed to claim attachment '{name}' for this thread."),
                    cause,
                ));
            }
        }
        Ok(normalized)
    }
}

/// The claimed attachment: the thread-scoped id and a lower-cased mime type.
fn normalized_attachment(attachment: &ChatAttachment, final_id: &str) -> Option<ChatAttachment> {
    let id = ChatAttachmentId::new(final_id).ok()?;
    let mut value = serde_json::to_value(attachment).ok()?;
    value["id"] = serde_json::Value::String(id.to_string());
    let mime = value["mimeType"].as_str()?.to_lowercase();
    value["mimeType"] = serde_json::Value::String(mime);
    serde_json::from_value(value).ok()
}

/// Preserve context bindings when uploads become thread-owned attachments.
pub fn remap_composer_context_attachments(
    context: &OrchestrationMessageContext,
    before: &[ChatAttachment],
    after: &[ChatAttachment],
) -> OrchestrationMessageContext {
    let ids: HashMap<&str, &str> = before
        .iter()
        .zip(after)
        .map(|(before, after)| (attachment_fields(before).id, attachment_fields(after).id))
        .collect();
    let records = context
        .records
        .iter()
        .map(|record| {
            let ComposerContextRecord::Known(
                KnownComposerContextRecord::Image(_) | KnownComposerContextRecord::File(_),
            ) = record
            else {
                return record.clone();
            };
            let Ok(mut value) = serde_json::to_value(record) else {
                return record.clone();
            };
            let target = value["attachmentId"]
                .as_str()
                .and_then(|id| ids.get(id))
                .map(|id| (*id).to_owned());
            if let Some(target) = target {
                value["attachmentId"] = serde_json::Value::String(target);
            }
            serde_json::from_value(value).unwrap_or_else(|_| record.clone())
        })
        .collect();
    OrchestrationMessageContext {
        version: context.version,
        records,
    }
}

#[cfg(test)]
mod tests;

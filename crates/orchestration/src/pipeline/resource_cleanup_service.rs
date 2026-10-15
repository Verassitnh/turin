//! Port of upstream/apps/server/src/orchestration-v2/ResourceCleanupService.ts
//!
//! Terminals and previews are cut features (PORTING cut list), so their
//! cleanup is upstream's `defaultValue` no-op; attachment cleanup is real.

use super::attachment_store::resolve_attachment_path_by_id;
use futures::{StreamExt, TryStreamExt, stream};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceCleanupOperation {
    Terminal,
    Preview,
    Attachment,
}

#[derive(Debug, thiserror::Error)]
#[error("Resource cleanup failed ({operation:?}): {cause}")]
pub struct ResourceCleanupError {
    pub operation: ResourceCleanupOperation,
    pub thread_id: Option<String>,
    pub attachment_id: Option<String>,
    #[source]
    pub cause: io::Error,
}

pub struct ResourceCleanupService {
    attachments_dir: Arc<PathBuf>,
}

impl ResourceCleanupService {
    pub fn new(attachments_dir: PathBuf) -> Self {
        Self {
            attachments_dir: Arc::new(attachments_dir),
        }
    }

    pub async fn cleanup_terminals(&self, _thread_id: &str) -> Result<(), ResourceCleanupError> {
        Ok(())
    }

    /// Closes every preview session of the thread; server browser tabs end
    /// with them.
    pub async fn cleanup_previews(&self, _thread_id: &str) -> Result<(), ResourceCleanupError> {
        Ok(())
    }

    pub async fn cleanup_attachments(
        &self,
        attachment_ids: &[String],
    ) -> Result<(), ResourceCleanupError> {
        stream::iter(attachment_ids.iter().cloned())
            .map(|attachment_id| self.remove_attachment(attachment_id))
            .buffer_unordered(4)
            .try_for_each(|()| async { Ok(()) })
            .await
    }

    async fn remove_attachment(&self, attachment_id: String) -> Result<(), ResourceCleanupError> {
        let error = |attachment_id: String, cause: io::Error| ResourceCleanupError {
            operation: ResourceCleanupOperation::Attachment,
            thread_id: None,
            attachment_id: Some(attachment_id),
            cause,
        };
        let dir = Arc::clone(&self.attachments_dir);
        let id = attachment_id.clone();
        let path = tokio::task::spawn_blocking(move || resolve_attachment_path_by_id(&dir, &id))
            .await
            .map_err(|join| error(attachment_id.clone(), io::Error::other(join)))?;
        let Some(path) = path else {
            return Ok(());
        };
        // `remove(path, { force: true })`: a file already gone is not an error.
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(cause) if cause.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(cause) => Err(error(attachment_id, cause)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::attachment_store::create_attachment_id;

    #[tokio::test]
    async fn removes_stored_attachments_and_ignores_missing_or_invalid_ids() {
        let dir = tempfile::tempdir().unwrap();
        let kept = create_attachment_id("thread-1", Some("txt")).unwrap();
        let removed = create_attachment_id("thread-1", Some("txt")).unwrap();
        let missing = create_attachment_id("thread-1", Some("png")).unwrap();
        for id in [&kept, &removed] {
            std::fs::write(dir.path().join(format!("{id}.txt")), b"data").unwrap();
        }
        let service = ResourceCleanupService::new(dir.path().to_path_buf());
        service
            .cleanup_attachments(&[removed.clone(), missing, "../escape".into()])
            .await
            .unwrap();
        assert!(!dir.path().join(format!("{removed}.txt")).exists());
        assert!(dir.path().join(format!("{kept}.txt")).exists());
        service.cleanup_terminals("thread-1").await.unwrap();
        service.cleanup_previews("thread-1").await.unwrap();
    }
}

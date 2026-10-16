//! Port of upstream/apps/server/src/pullRequest/pullRequestViewedFiles.ts
//!
//! The marks a reader has ticked off, for a host that keeps none of its own, and the held record of
//! what the head has of those files. The revisions cache is filed here because this is its only
//! consumer: when a second one appears, export `makeFileRevisions` and split it into its own file.
//!
//! Upstream receives its collaborators as closures from `PullRequestService`; here they are the
//! [`ViewedFilesDependencies`] trait, which the service implements. `SupportedProject` is reduced
//! to the fields this module reads ([`FilesViewedProject`]).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use indexmap::{IndexMap, IndexSet};
use parking_lot::Mutex;
use tokio::task::JoinSet;
use turin_contracts::base_schemas::{ProjectId, TrimmedNonEmptyString};
use turin_contracts::pull_request::{
    PullRequestFileViewed, PullRequestFileViewedState, PullRequestFilesViewedResult,
    PullRequestOperationError, PullRequestRef, PullRequestSetFilesViewedInput,
    PullRequestViewedFilesStore,
};
use turin_persistence::{
    Db, PullRequestFileViewedWrite, PullRequestFilesViewedScope, format_utc_millis,
    list_pull_request_files_viewed, set_pull_request_files_viewed,
};

use crate::source_control_core::ambient;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::{
    GetFileRevisionsInput, ProviderChangeRequestRef, ProviderRepositoryRef, PullRequestOperation,
    PullRequestProviderApi, PullRequestProviderError, SetFilesViewedInput,
};

/// How long the head's version of a file is believed, and how long a held answer stands while the
/// next one is fetched. This is the host call behind the **Changed** badge alone, so a held answer
/// costs a badge that is a minute behind rather than a stale tick.
const FILE_REVISIONS_CACHE_TTL: Duration = Duration::from_secs(60);
const FILE_REVISIONS_STALE_WINDOW: Duration = Duration::from_secs(10 * 60);
pub const FILE_REVISIONS_CACHE_CAPACITY: usize = 64;

/// How many paths one scope's entry carries. Bounds a single scope, not how many scopes are held:
/// a reader ticking one file after another renews the same scope and grows it without limit
/// otherwise.
pub const MAX_FILE_REVISION_PATHS: usize = 1_000;

/// The fields of upstream's `SupportedProject` this module reads.
#[derive(Clone)]
pub struct FilesViewedProject {
    /// `project.project.id`
    pub project_id: ProjectId,
    /// `project.project.workspaceRoot`
    pub workspace_root: String,
    pub api: Arc<dyn PullRequestProviderApi>,
    pub repository: String,
    /// The host the repository lives on, which is the account boundary rather than the kind.
    pub host: String,
    /// The identity's canonical key, which is what this environment's own records are keyed by.
    /// Unique where `repository` is not.
    pub remote: String,
}

/// What `PullRequestService` hands upstream's `ViewedFiles.make`.
#[async_trait]
pub trait ViewedFilesDependencies: Send + Sync + 'static {
    /// `PullRequestError`: the service's unavailable-or-operation error.
    type Error: From<PullRequestOperationError> + Send + 'static;

    fn ref_epoch(&self, reference: &PullRequestRef) -> u64;
    fn file_revisions_epoch(&self) -> u64;
    /// `toPullRequestError(operation)(error)`
    fn to_pull_request_error(
        &self,
        operation: &str,
        error: PullRequestProviderError,
    ) -> Self::Error;
    /// The error's `_tag`, for the warning logged when revisions cannot be read.
    fn error_tag(&self, error: &Self::Error) -> &'static str;
    async fn require_project(
        &self,
        reference: &PullRequestRef,
    ) -> Result<FilesViewedProject, Self::Error>;
    async fn required_viewer_of(
        &self,
        project: &FilesViewedProject,
        operation: &str,
    ) -> Result<Option<String>, Self::Error>;
}

/// What the head has of the files a reader has marked, held between reads. The entry tracks what
/// has been asked as well as what was heard, since a path missing from an answer keeps whatever
/// version was last given for it rather than being cleared.
#[derive(Clone)]
struct HeldFileRevisions {
    at: i64,
    asked: IndexSet<String>,
    revisions: IndexMap<String, String>,
}

struct Gate {
    gate: Arc<tokio::sync::Mutex<()>>,
    pending: usize,
}

struct Inner<D: ViewedFilesDependencies> {
    dependencies: D,
    db: Db,
    clock: Arc<dyn Clock>,
    held_file_revisions: Mutex<IndexMap<String, HeldFileRevisions>>,
    refreshing_file_revisions: Mutex<HashSet<String>>,
    files_viewed_gates: Mutex<HashMap<String, Gate>>,
    /// Background refreshes (upstream's `runFork`), aborted with the service.
    tasks: Mutex<JoinSet<()>>,
}

/// `ViewedFiles.make(...)`: one instance per service, since the held revisions, refresh set and
/// write gates are only correct at one instance per service.
pub struct PullRequestViewedFiles<D: ViewedFilesDependencies> {
    inner: Arc<Inner<D>>,
}

impl<D: ViewedFilesDependencies> Clone for PullRequestViewedFiles<D> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// The environment's record could not be reached.
fn to_files_viewed_store_error(
    operation: &str,
    cause: impl std::fmt::Display,
) -> PullRequestOperationError {
    PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(
            "This environment could not reach its record of which files you have seen.",
        )
        .expect("non-blank literal"),
        reason: None,
        cause: Some(serde_json::Value::String(cause.to_string())),
    }
}

fn unsupported_error(operation: &str) -> PullRequestOperationError {
    PullRequestOperationError::Operation {
        operation: operation.to_owned(),
        detail: TrimmedNonEmptyString::new(
            "This host does not track which files a reader has seen.",
        )
        .expect("non-blank literal"),
        reason: None,
        cause: None,
    }
}

fn change_request_ref(project: &FilesViewedProject, number: u64) -> ProviderChangeRequestRef {
    ProviderChangeRequestRef {
        repo: ProviderRepositoryRef {
            cwd: project.workspace_root.clone(),
            repository: project.repository.clone(),
            host: project.host.clone(),
        },
        number,
    }
}

/// Whether a read of the held entry is a fresh fetch or may be answered from a stale value.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Freshness {
    Held,
    Fresh,
}

impl<D: ViewedFilesDependencies> Inner<D> {
    /// Carries the reference's epoch, so whatever moved the head strands what was held (or in
    /// flight) against the old one. Spelled from the project rather than the reference, since a
    /// reference arrives however the client spelled it while the epoch is bumped against the
    /// remote's own spelling.
    fn file_revisions_key(
        &self,
        project: &FilesViewedProject,
        reference: &PullRequestRef,
    ) -> String {
        let mut spelled = reference.clone();
        if let Ok(host) = TrimmedNonEmptyString::new(&project.host) {
            spelled.host = Some(host);
        }
        if let Ok(repository) = TrimmedNonEmptyString::new(&project.repository) {
            spelled.repository = repository;
        }
        [
            self.dependencies.ref_epoch(&spelled).to_string(),
            self.dependencies.file_revisions_epoch().to_string(),
            reference.project_id.to_string(),
            crate::shared::js::js_trim(&project.repository).to_lowercase(),
            reference.number.get().to_string(),
        ]
        .join(" ")
    }

    /// `paths` are what was asked about, and are held as answered for whether the host had a
    /// version for them or not: that is what stops the same question being asked again. A
    /// `complete` answer adds every other path it carries, since a host that read the whole change
    /// to answer for one file has already paid for all of them, and the tick after this one names
    /// a file nothing has asked about yet.
    fn record_file_revisions(
        &self,
        key: &str,
        paths: &[String],
        answer: &crate::source_control_core::pull_request_provider::ProviderFileRevisions,
    ) -> IndexMap<String, String> {
        let at = self.clock.now_millis();
        let mut held_map = self.held_file_revisions.lock();
        let held = held_map.get(key);
        // Past the stale window the old entry is not worth merging into: it would carry paths
        // nobody has asked about since, at revisions the head has long moved off.
        let carried =
            held.filter(|held| at - held.at <= FILE_REVISIONS_STALE_WINDOW.as_millis() as i64);
        let carried_at = carried.map(|held| held.at);
        let mut revisions = carried
            .map(|held| held.revisions.clone())
            .unwrap_or_default();
        let mut asked = carried.map(|held| held.asked.clone()).unwrap_or_default();
        // The paths asked for go last, so a whole-change answer wider than the cap is trimmed
        // down to the reader's own files rather than over them.
        let learned: Vec<&String> = if answer.complete == Some(true) {
            answer.revisions.keys().chain(paths.iter()).collect()
        } else {
            paths.iter().collect()
        };
        for path in learned {
            // Reinserted rather than added, so what a full entry drops below is the path nobody
            // has asked about in the longest rather than one just asked for.
            asked.shift_remove(path);
            asked.insert(path.clone());
            // A path left out of the answer keeps its last known version rather than being cleared.
            if let Some(revision) = answer.revisions.get(path) {
                revisions.shift_remove(path);
                revisions.insert(path.clone(), revision.clone());
            }
        }
        while asked.len() > MAX_FILE_REVISION_PATHS {
            if let Some(oldest) = asked.shift_remove_index(0) {
                revisions.shift_remove(&oldest);
            }
        }
        held_map.shift_remove(key);
        if held_map.len() >= FILE_REVISIONS_CACHE_CAPACITY {
            held_map.shift_remove_index(0);
        }
        // The entry is only as fresh as its oldest revision: stamping it with `now` on a partial
        // answer would let an old revision ride past the point it should have been re-read.
        let stamped = if revisions
            .keys()
            .all(|path| answer.revisions.contains_key(path))
        {
            at
        } else {
            carried_at.unwrap_or(at)
        };
        held_map.insert(
            key.to_owned(),
            HeldFileRevisions {
                at: stamped,
                asked,
                revisions: revisions.clone(),
            },
        );
        revisions
    }

    /// A held entry that covers every path asked for and is still worth answering from.
    fn held_file_revisions_for(
        &self,
        key: &str,
        paths: &[String],
        now: i64,
    ) -> Option<HeldFileRevisions> {
        let mut held_map = self.held_file_revisions.lock();
        // Put back at the end on every read, so the scope a reader is working through is not the
        // one evicted by a listing walking scopes nobody has open.
        let held = held_map.shift_remove(key)?;
        held_map.insert(key.to_owned(), held.clone());
        if now - held.at > FILE_REVISIONS_STALE_WINDOW.as_millis() as i64 {
            return None;
        }
        paths
            .iter()
            .all(|path| held.asked.contains(path))
            .then_some(held)
    }

    async fn fetch_file_revisions(
        &self,
        project: &FilesViewedProject,
        reference: &PullRequestRef,
        paths: &[String],
        operation: &str,
    ) -> Result<IndexMap<String, String>, D::Error> {
        let key = self.file_revisions_key(project, reference);
        let answer = project
            .api
            .get_file_revisions(GetFileRevisionsInput {
                request: change_request_ref(project, reference.number.get()),
                paths: paths.to_vec(),
            })
            .await
            .map_err(|error| self.dependencies.to_pull_request_error(operation, error))?;
        Ok(self.record_file_revisions(&key, paths, &answer))
    }

    /// What the head has of these files, or `None` where the host cannot say (not an error: the
    /// marks just stop reporting staleness). `Held` answers from a stale value and refetches off
    /// the critical path, since a badge a moment behind beats a page that won't paint until the
    /// host answers; `Fresh` is for the press itself, which must not store a revision the head
    /// already moved off.
    async fn file_revisions_of(
        self: &Arc<Self>,
        project: &FilesViewedProject,
        reference: &PullRequestRef,
        paths: &[String],
        operation: &str,
        freshness: Freshness,
    ) -> Result<Option<IndexMap<String, String>>, D::Error> {
        if !project.api.supports(PullRequestOperation::GetFileRevisions) {
            return Ok(None);
        }
        let now = self.clock.now_millis();
        let key = self.file_revisions_key(project, reference);
        let Some(held) = self.held_file_revisions_for(&key, paths, now) else {
            return self
                .fetch_file_revisions(project, reference, paths, operation)
                .await
                .map(Some);
        };
        if now - held.at <= FILE_REVISIONS_CACHE_TTL.as_millis() as i64 {
            return Ok(Some(held.revisions));
        }
        if freshness == Freshness::Fresh {
            return self
                .fetch_file_revisions(project, reference, paths, operation)
                .await
                .map(Some);
        }
        if !self.refreshing_file_revisions.lock().insert(key.clone()) {
            return Ok(Some(held.revisions));
        }
        // Its own task rather than a child: the caller has been answered and is gone before this
        // lands. One at a time per change request, so a page of files costs one host read.
        let inner = self.clone();
        let (project, reference, paths, operation) = (
            project.clone(),
            reference.clone(),
            paths.to_vec(),
            operation.to_owned(),
        );
        let captured = ambient::capture();
        {
            let mut tasks = self.tasks.lock();
            while tasks.try_join_next().is_some() {}
            tasks.spawn(captured.run(async move {
                struct Ensuring<'a, D: ViewedFilesDependencies>(&'a Arc<Inner<D>>, String);
                impl<D: ViewedFilesDependencies> Drop for Ensuring<'_, D> {
                    fn drop(&mut self) {
                        self.0.refreshing_file_revisions.lock().remove(&self.1);
                    }
                }
                let _ensuring = Ensuring(&inner, key);
                let _ = inner
                    .fetch_file_revisions(&project, &reference, &paths, &operation)
                    .await;
            }));
        }
        Ok(Some(held.revisions))
    }

    /// Which change request's marks, and whose. Provider and host lead the key because the same
    /// repository can exist on more than one install; the reader is part of it because a host's
    /// own record is per-account. A host that names no reader is one reader, not none.
    fn files_viewed_scope(
        project: &FilesViewedProject,
        number: u64,
        viewer: Option<String>,
    ) -> PullRequestFilesViewedScope {
        PullRequestFilesViewedScope {
            provider: project.api.kind().to_string(),
            host: project.host.clone(),
            repository: project.remote.clone(),
            number: number as i64,
            viewer: viewer.unwrap_or_default(),
        }
    }

    /// The marks this environment keeps for a host that keeps none of its own. A file the head
    /// still has at the revision it was cleared at is cleared; one the head has moved on from is
    /// reported as changed. Revisions are asked for the marked paths alone, so a reader who has
    /// marked nothing costs no host call.
    async fn environment_files_viewed(
        self: &Arc<Self>,
        project: &FilesViewedProject,
        reference: &PullRequestRef,
    ) -> Result<PullRequestFilesViewedResult, D::Error> {
        let viewer = self
            .dependencies
            .required_viewer_of(project, "filesViewed")
            .await?;
        let scope = Self::files_viewed_scope(project, reference.number.get(), viewer);
        let held = self
            .db
            .read(move |conn| list_pull_request_files_viewed(conn, &scope))
            .await
            .map_err(|cause| to_files_viewed_store_error("filesViewed", cause))?;
        let marks = held.files;
        if marks.is_empty() {
            return Ok(PullRequestFilesViewedResult {
                files: vec![],
                truncated: held.truncated,
            });
        }
        // A host that won't say what its head has costs the marks their staleness
        // (`file_revisions_of` returns `None`), rather than costing the reader every tick they've made.
        let paths: Vec<String> = marks.iter().map(|mark| mark.path.clone()).collect();
        let revisions = match self
            .file_revisions_of(project, reference, &paths, "filesViewed", Freshness::Held)
            .await
        {
            Ok(revisions) => revisions,
            Err(error) => {
                tracing::warn!(
                    operation = "filesViewed",
                    reason = self.dependencies.error_tag(&error),
                    "reporting viewed files without what the head has of them"
                );
                None
            }
        };
        let files = marks
            .into_iter()
            .map(|mark| {
                // A mark stamped with no baseline holds until the reader presses it again.
                let Some(stamp) = mark.revision.as_deref() else {
                    return PullRequestFileViewed {
                        path: mark.path,
                        state: PullRequestFileViewedState::Viewed,
                    };
                };
                let revision = revisions.as_ref().and_then(|r| r.get(&mark.path));
                // A deleted file is answered as the empty revision, matching its stamp, so it
                // stays cleared; a path the host had no answer for also holds as cleared.
                let state = match revision {
                    None => PullRequestFileViewedState::Viewed,
                    Some(revision) if revision == stamp => PullRequestFileViewedState::Viewed,
                    Some(_) => PullRequestFileViewedState::Dismissed,
                };
                PullRequestFileViewed {
                    path: mark.path,
                    state,
                }
            })
            .collect();
        // The store caps marks per scope; a reader over that cap is told so, like a paginated read.
        Ok(PullRequestFilesViewedResult {
            files,
            truncated: held.truncated,
        })
    }

    async fn environment_set_files_viewed(
        self: &Arc<Self>,
        project: &FilesViewedProject,
        input: &PullRequestSetFilesViewedInput,
    ) -> Result<(), D::Error> {
        let viewer = self
            .dependencies
            .required_viewer_of(project, "setFilesViewed")
            .await?;
        // Only the files being cleared need a revision. An unticked one is about to lose its row,
        // and what the head has of it changes nothing about deleting it.
        let cleared: Vec<String> = input
            .files
            .iter()
            .filter(|file| file.viewed)
            .map(|file| file.path.clone())
            .collect();
        let revisions = if cleared.is_empty() {
            None
        } else {
            let reference = PullRequestRef {
                project_id: input.project_id.clone(),
                host: input.host.clone(),
                expected_account_id: input.expected_account_id.clone(),
                allow_stale: input.allow_stale,
                repository: input.repository.clone(),
                number: input.number,
            };
            // A host that won't say what its head has costs the press its baseline, not the press
            // itself: the mark is stored with none and holds until pressed again.
            match self
                .file_revisions_of(
                    project,
                    &reference,
                    &cleared,
                    "setFilesViewed",
                    Freshness::Fresh,
                )
                .await
            {
                Ok(revisions) => revisions,
                Err(error) => {
                    tracing::warn!(
                        operation = "setFilesViewed",
                        reason = self.dependencies.error_tag(&error),
                        "recording viewed files without what the head has of them"
                    );
                    None
                }
            }
        };
        let viewed_at = format_utc_millis(
            chrono::DateTime::from_timestamp_millis(self.clock.now_millis()).unwrap_or_default(),
        );
        let scope = Self::files_viewed_scope(project, input.number.get(), viewer);
        // A path left out of the answer stores with no baseline, not the empty revision, since
        // the empty revision is itself an answer and would misreport the file once it turns out
        // to have a version after all.
        let files: Vec<PullRequestFileViewedWrite> = input
            .files
            .iter()
            .map(|file| PullRequestFileViewedWrite {
                path: file.path.clone(),
                revision: revisions.as_ref().and_then(|r| r.get(&file.path)).cloned(),
                viewed: file.viewed,
            })
            .collect();
        self.db
            .write(move |tx| set_pull_request_files_viewed(tx, &scope, &files, &viewed_at))
            .await
            .map_err(|cause| to_files_viewed_store_error("setFilesViewed", cause))?;
        Ok(())
    }

    /// One environment-backed write at a time per change request. A tick's host round trip is
    /// slower than an untick's, so unordered presses could finish out of order and leave a stale
    /// tick standing over a later untick.
    async fn in_files_viewed_order<T>(
        &self,
        project: &FilesViewedProject,
        number: u64,
        write: impl std::future::Future<Output = T>,
    ) -> T {
        // Finding the gate, putting it in and taking a place in its queue are one synchronous
        // step: two presses each making a gate of their own would neither wait on the other.
        let key = format!("{} {} {number}", project.project_id, project.remote);
        let gate = {
            let mut gates = self.files_viewed_gates.lock();
            let entry = gates.entry(key.clone()).or_insert_with(|| Gate {
                gate: Arc::new(tokio::sync::Mutex::new(())),
                pending: 0,
            });
            entry.pending += 1;
            entry.gate.clone()
        };
        // Dropped once nobody is queued behind it, so a long-lived server does not keep a gate per
        // change request anyone has ever ticked a file in.
        struct Ensuring<'a, D: ViewedFilesDependencies>(&'a Inner<D>, String);
        impl<D: ViewedFilesDependencies> Drop for Ensuring<'_, D> {
            fn drop(&mut self) {
                let mut gates = self.0.files_viewed_gates.lock();
                if let Some(entry) = gates.get_mut(&self.1) {
                    entry.pending -= 1;
                    if entry.pending == 0 {
                        gates.remove(&self.1);
                    }
                }
            }
        }
        let _ensuring = Ensuring(self, key);
        let _turn = gate.lock().await;
        write.await
    }
}

impl<D: ViewedFilesDependencies> PullRequestViewedFiles<D> {
    pub fn new(dependencies: D, db: Db, clock: Arc<dyn Clock>) -> Self {
        Self {
            inner: Arc::new(Inner {
                dependencies,
                db,
                clock,
                held_file_revisions: Mutex::new(IndexMap::new()),
                refreshing_file_revisions: Mutex::new(HashSet::new()),
                files_viewed_gates: Mutex::new(HashMap::new()),
                tasks: Mutex::new(JoinSet::new()),
            }),
        }
    }

    pub async fn files_viewed(
        &self,
        input: &PullRequestRef,
    ) -> Result<PullRequestFilesViewedResult, D::Error> {
        let project = self.inner.dependencies.require_project(input).await?;
        let store = project.api.capabilities().viewed_files;
        if store == Some(PullRequestViewedFilesStore::Host)
            && project.api.supports(PullRequestOperation::GetFilesViewed)
        {
            let viewed = project
                .api
                .get_files_viewed(change_request_ref(&project, input.number.get()))
                .await
                .map_err(|error| {
                    self.inner
                        .dependencies
                        .to_pull_request_error("filesViewed", error)
                })?;
            return Ok(PullRequestFilesViewedResult {
                files: viewed.files,
                truncated: viewed.truncated,
            });
        }
        if store == Some(PullRequestViewedFilesStore::Environment) {
            return self.inner.environment_files_viewed(&project, input).await;
        }
        Err(unsupported_error("filesViewed").into())
    }

    pub async fn set_files_viewed(
        &self,
        input: &PullRequestSetFilesViewedInput,
    ) -> Result<(), D::Error> {
        let reference = PullRequestRef {
            project_id: input.project_id.clone(),
            host: input.host.clone(),
            expected_account_id: input.expected_account_id.clone(),
            allow_stale: input.allow_stale,
            repository: input.repository.clone(),
            number: input.number,
        };
        let project = self.inner.dependencies.require_project(&reference).await?;
        let store = project.api.capabilities().viewed_files;
        if store == Some(PullRequestViewedFilesStore::Host)
            && project.api.supports(PullRequestOperation::SetFilesViewed)
        {
            return project
                .api
                .set_files_viewed(SetFilesViewedInput {
                    request: change_request_ref(&project, input.number.get()),
                    files: input
                        .files
                        .iter()
                        .map(|f| (f.path.clone(), f.viewed))
                        .collect(),
                })
                .await
                .map_err(|error| {
                    self.inner
                        .dependencies
                        .to_pull_request_error("setFilesViewed", error)
                });
        }
        if store == Some(PullRequestViewedFilesStore::Environment) {
            return self
                .inner
                .in_files_viewed_order(
                    &project,
                    input.number.get(),
                    self.inner.environment_set_files_viewed(&project, input),
                )
                .await;
        }
        Err(unsupported_error("setFilesViewed").into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pull_request::provider_registry::test_support::FakeProvider;
    use crate::pull_request::read_cache::PullRequestReadError;
    use crate::source_control_core::clock::ManualClock;
    use crate::source_control_core::pull_request_provider::{
        ProviderFileRevisions, ProviderFilesViewed, PullRequestProviderErrorReason,
    };
    use pretty_assertions::assert_eq;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use turin_contracts::base_schemas::PositiveInt;
    use turin_contracts::pull_request::PullRequestSetFilesViewedFile;

    const START: i64 = 1_790_812_800_000;

    struct Deps {
        project: FilesViewedProject,
        epoch: Arc<AtomicU64>,
    }

    #[async_trait]
    impl ViewedFilesDependencies for Deps {
        type Error = PullRequestReadError;
        fn ref_epoch(&self, _: &PullRequestRef) -> u64 {
            self.epoch.load(Ordering::SeqCst)
        }
        fn file_revisions_epoch(&self) -> u64 {
            0
        }
        fn to_pull_request_error(
            &self,
            operation: &str,
            error: PullRequestProviderError,
        ) -> Self::Error {
            PullRequestOperationError::Operation {
                operation: operation.to_owned(),
                detail: TrimmedNonEmptyString::new(error.detail).unwrap(),
                reason: None,
                cause: None,
            }
            .into()
        }
        fn error_tag(&self, _: &Self::Error) -> &'static str {
            "PullRequestOperationError"
        }
        async fn require_project(
            &self,
            _: &PullRequestRef,
        ) -> Result<FilesViewedProject, Self::Error> {
            Ok(self.project.clone())
        }
        async fn required_viewer_of(
            &self,
            _: &FilesViewedProject,
            _: &str,
        ) -> Result<Option<String>, Self::Error> {
            Ok(Some("octocat".to_owned()))
        }
    }

    struct Harness {
        files: PullRequestViewedFiles<Deps>,
        clock: Arc<ManualClock>,
        head: Arc<Mutex<HashMap<String, String>>>,
        host_reads: Arc<AtomicUsize>,
        failing: Arc<AtomicBool>,
        epoch: Arc<AtomicU64>,
    }

    async fn harness(store: &str, with_revisions: bool) -> Harness {
        let head: Arc<Mutex<HashMap<String, String>>> = Arc::default();
        let host_reads = Arc::new(AtomicUsize::new(0));
        let failing = Arc::new(AtomicBool::new(false));
        let mut provider = FakeProvider::new("github", Some(store));
        if with_revisions {
            let (head, host_reads, failing) = (head.clone(), host_reads.clone(), failing.clone());
            provider.file_revisions = Some(Arc::new(move |input: GetFileRevisionsInput| {
                host_reads.fetch_add(1, Ordering::SeqCst);
                if failing.load(Ordering::SeqCst) {
                    return Err(PullRequestProviderError::new(
                        input_kind(),
                        "getFileRevisions",
                        PullRequestProviderErrorReason::Failed,
                        "head unavailable",
                    ));
                }
                let head = head.lock();
                Ok(ProviderFileRevisions {
                    revisions: input
                        .paths
                        .iter()
                        .filter_map(|path| head.get(path).map(|r| (path.clone(), r.clone())))
                        .collect(),
                    complete: None,
                })
            }));
        }
        provider.files_viewed = Some(Arc::new(|_| {
            Ok(ProviderFilesViewed {
                files: vec![PullRequestFileViewed {
                    path: "host.rs".to_owned(),
                    state: PullRequestFileViewedState::Viewed,
                }],
                truncated: false,
            })
        }));
        provider.set_files_viewed = Some(Arc::new(|_| Ok(())));
        let epoch = Arc::new(AtomicU64::new(0));
        let project = FilesViewedProject {
            project_id: ProjectId::new("p1").unwrap(),
            workspace_root: "/repo".to_owned(),
            api: Arc::new(provider),
            repository: "acme/web".to_owned(),
            host: "github.com".to_owned(),
            remote: "github.com/acme/web".to_owned(),
        };
        let clock = ManualClock::new(START);
        let files = PullRequestViewedFiles::new(
            Deps {
                project,
                epoch: epoch.clone(),
            },
            Db::open_in_memory().await.unwrap(),
            clock.clone(),
        );
        Harness {
            files,
            clock,
            head,
            host_reads,
            failing,
            epoch,
        }
    }

    fn input_kind() -> turin_contracts::source_control::SourceControlProviderKind {
        turin_contracts::source_control::SourceControlProviderKind::new("github").unwrap()
    }

    fn reference() -> PullRequestRef {
        PullRequestRef {
            project_id: ProjectId::new("p1").unwrap(),
            host: None,
            expected_account_id: None,
            allow_stale: None,
            repository: TrimmedNonEmptyString::new("acme/web").unwrap(),
            number: PositiveInt::new(7).unwrap(),
        }
    }

    fn set(files: &[(&str, bool)]) -> PullRequestSetFilesViewedInput {
        PullRequestSetFilesViewedInput {
            project_id: ProjectId::new("p1").unwrap(),
            host: None,
            expected_account_id: None,
            allow_stale: None,
            repository: TrimmedNonEmptyString::new("acme/web").unwrap(),
            number: PositiveInt::new(7).unwrap(),
            files: files
                .iter()
                .map(|(path, viewed)| PullRequestSetFilesViewedFile {
                    path: (*path).to_owned(),
                    viewed: *viewed,
                })
                .collect(),
        }
    }

    fn states(result: &PullRequestFilesViewedResult) -> Vec<(String, PullRequestFileViewedState)> {
        result
            .files
            .iter()
            .map(|f| (f.path.clone(), f.state))
            .collect()
    }

    fn set_head(harness: &Harness, path: &str, revision: &str) {
        harness
            .head
            .lock()
            .insert(path.to_owned(), revision.to_owned());
    }

    #[tokio::test]
    async fn a_host_that_keeps_marks_answers_them_itself() {
        let h = harness("host", false).await;
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("host.rs".to_owned(), PullRequestFileViewedState::Viewed)]
        );
        h.files
            .set_files_viewed(&set(&[("host.rs", true)]))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_host_that_tracks_nothing_is_refused() {
        let h = harness("host", false).await;
        let mut project = h.files.inner.dependencies.project.clone();
        project.api = Arc::new(FakeProvider::new("github", None));
        let files = PullRequestViewedFiles::new(
            Deps {
                project,
                epoch: h.epoch.clone(),
            },
            Db::open_in_memory().await.unwrap(),
            h.clock.clone(),
        );
        for error in [
            files.files_viewed(&reference()).await.unwrap_err(),
            files
                .set_files_viewed(&set(&[("a", true)]))
                .await
                .unwrap_err(),
        ] {
            assert!(
                error
                    .to_string()
                    .contains("This host does not track which files a reader has seen."),
                "{error}"
            );
        }
    }

    #[tokio::test]
    async fn environment_marks_hold_until_the_head_moves_then_report_changed() {
        let h = harness("environment", true).await;
        set_head(&h, "a.rs", "r1");
        set_head(&h, "b.rs", "r1");
        h.files
            .set_files_viewed(&set(&[("a.rs", true), ("b.rs", true)]))
            .await
            .unwrap();
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![
                ("a.rs".to_owned(), PullRequestFileViewedState::Viewed),
                ("b.rs".to_owned(), PullRequestFileViewedState::Viewed),
            ]
        );
        assert!(!result.truncated);

        // The head moves on b.rs; the held answer is a badge behind until it is fetched again.
        set_head(&h, "b.rs", "r2");
        h.epoch.fetch_add(1, Ordering::SeqCst);
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![
                ("a.rs".to_owned(), PullRequestFileViewedState::Viewed),
                ("b.rs".to_owned(), PullRequestFileViewedState::Dismissed),
            ]
        );

        // Unticking removes the row.
        h.files
            .set_files_viewed(&set(&[("b.rs", false)]))
            .await
            .unwrap();
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("a.rs".to_owned(), PullRequestFileViewedState::Viewed)]
        );
        assert!(h.files.inner.files_viewed_gates.lock().is_empty());
    }

    #[tokio::test]
    async fn a_reader_with_no_marks_costs_no_host_call() {
        let h = harness("environment", true).await;
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert!(result.files.is_empty());
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_press_without_a_head_answer_stores_no_baseline_and_holds() {
        let h = harness("environment", true).await;
        h.failing.store(true, Ordering::SeqCst);
        h.files
            .set_files_viewed(&set(&[("a.rs", true)]))
            .await
            .unwrap();
        h.failing.store(false, Ordering::SeqCst);
        set_head(&h, "a.rs", "anything");
        h.epoch.fetch_add(1, Ordering::SeqCst);
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("a.rs".to_owned(), PullRequestFileViewedState::Viewed)]
        );
    }

    #[tokio::test]
    async fn a_host_that_cannot_say_what_the_head_has_keeps_the_ticks() {
        let h = harness("environment", false).await;
        h.files
            .set_files_viewed(&set(&[("a.rs", true)]))
            .await
            .unwrap();
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("a.rs".to_owned(), PullRequestFileViewedState::Viewed)]
        );
    }

    #[tokio::test]
    async fn held_revisions_answer_within_the_ttl_and_refresh_in_the_background_after_it() {
        let h = harness("environment", true).await;
        set_head(&h, "a.rs", "r1");
        h.files
            .set_files_viewed(&set(&[("a.rs", true)]))
            .await
            .unwrap();
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 1);
        // Within the TTL: answered from what the press recorded.
        h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 1);

        // Past the TTL but inside the stale window: the held answer comes back at once and one
        // refetch lands afterwards.
        set_head(&h, "a.rs", "r2");
        h.clock.advance(61_000);
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("a.rs".to_owned(), PullRequestFileViewedState::Viewed)]
        );
        for _ in 0..100 {
            if h.host_reads.load(Ordering::SeqCst) == 2
                && h.files.inner.refreshing_file_revisions.lock().is_empty()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 2);
        let result = h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(
            states(&result),
            vec![("a.rs".to_owned(), PullRequestFileViewedState::Dismissed)]
        );
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 2);

        // Past the stale window the held entry is dropped and the read waits on the host.
        h.clock.advance(11 * 60_000);
        h.files.files_viewed(&reference()).await.unwrap();
        assert_eq!(h.host_reads.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn complete_answers_are_held_and_the_path_cap_drops_the_least_recently_asked() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let h = harness("environment", true).await;
            let inner = &h.files.inner;
            let mut revisions = IndexMap::new();
            for index in 0..(MAX_FILE_REVISION_PATHS + 5) {
                revisions.insert(format!("f{index}"), "r".to_owned());
            }
            let answer = ProviderFileRevisions {
                revisions,
                complete: Some(true),
            };
            let held = inner.record_file_revisions("k", &["f1004".to_owned()], &answer);
            assert_eq!(held.len(), MAX_FILE_REVISION_PATHS);
            // The path asked about goes last, so it survives the trim; the first ones do not.
            assert!(held.contains_key("f1004"));
            assert!(!held.contains_key("f0"));
            let state = inner.held_file_revisions.lock();
            assert_eq!(state["k"].asked.len(), MAX_FILE_REVISION_PATHS);
        });
    }

    #[tokio::test]
    async fn the_held_cache_evicts_the_oldest_scope_past_its_capacity() {
        let h = harness("environment", true).await;
        let answer = ProviderFileRevisions::default();
        for index in 0..=FILE_REVISIONS_CACHE_CAPACITY {
            h.files
                .inner
                .record_file_revisions(&format!("k{index}"), &["a".to_owned()], &answer);
        }
        let held = h.files.inner.held_file_revisions.lock();
        assert_eq!(held.len(), FILE_REVISIONS_CACHE_CAPACITY);
        assert!(!held.contains_key("k0"));
        assert!(held.contains_key(&format!("k{FILE_REVISIONS_CACHE_CAPACITY}")));
    }

    #[tokio::test]
    async fn environment_writes_for_one_change_request_run_one_at_a_time() {
        let h = harness("environment", true).await;
        let order = Arc::new(Mutex::new(Vec::new()));
        let project = h.files.inner.dependencies.project.clone();
        let first = h.files.inner.in_files_viewed_order(&project, 7, {
            let order = order.clone();
            async move {
                order.lock().push("first:start");
                tokio::time::sleep(Duration::from_millis(30)).await;
                order.lock().push("first:end");
            }
        });
        let second = h.files.inner.in_files_viewed_order(&project, 7, {
            let order = order.clone();
            async move {
                order.lock().push("second");
            }
        });
        let (_, _) = tokio::join!(first, second);
        assert_eq!(*order.lock(), vec!["first:start", "first:end", "second"]);
        assert!(h.files.inner.files_viewed_gates.lock().is_empty());
    }
}

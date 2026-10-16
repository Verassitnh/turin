//! Runs blocking filesystem and process work on tokio's blocking pool, so the
//! async workers never wait on disk, a stalled network mount, or a child
//! process.

/// Runs `work` on the blocking pool and returns its result. A panic in `work`
/// resumes on the caller, as it would have inline.
pub async fn blocking<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

//! Port of upstream/apps/server/src/provider/CodexChatGptSessionLock.ts.
//!
//! Keeps rotating ChatGPT tokens and their active profile atomic across Node
//! and Rust servers sharing a secret directory. This follows `proper-lockfile`:
//! an atomic lock directory `<key>.bin.lock`, taken over once it is stale, and
//! refreshed by a heartbeat. If the heartbeat fails, the lock is compromised
//! and the task is abandoned.

use std::{
    future::Future,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use tokio::{sync::oneshot, task::JoinHandle};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockPolicy {
    pub stale: Duration,
    pub update: Duration,
    pub retries: u32,
    pub retry_delay: Duration,
}

impl Default for LockPolicy {
    /// The upstream options: stale 120 s, update 10 s, 80 retries 500 ms apart.
    fn default() -> Self {
        Self {
            stale: Duration::from_secs(120),
            update: Duration::from_secs(10),
            retries: 80,
            retry_delay: Duration::from_millis(500),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Could not lock the ChatGPT connection for an update. Try again.")]
pub struct CredentialLockError;

/// `withChatGptSessionLock`. With no directory (in-memory stores), the task
/// runs unlocked; the auth controller already serializes its callers.
pub async fn with_chatgpt_session_lock<T>(
    directory: Option<&Path>,
    key: &str,
    task: impl Future<Output = T>,
) -> Result<T, CredentialLockError> {
    with_chatgpt_session_lock_policy(directory, key, LockPolicy::default(), task).await
}

pub async fn with_chatgpt_session_lock_policy<T>(
    directory: Option<&Path>,
    key: &str,
    policy: LockPolicy,
    task: impl Future<Output = T>,
) -> Result<T, CredentialLockError> {
    let Some(directory) = directory else {
        return Ok(task.await);
    };
    let lock_dir = directory.join(format!("{key}.bin.lock"));
    acquire(&lock_dir, policy).await?;
    let (compromised_tx, mut compromised) = oneshot::channel::<()>();
    let heartbeat = spawn_heartbeat(lock_dir.clone(), policy.update, compromised_tx);
    let result = tokio::select! {
        value = task => Ok(value),
        _ = &mut compromised => Err(CredentialLockError),
    };
    heartbeat.abort();
    let _ = tokio::fs::remove_dir_all(&lock_dir).await;
    result
}

/// Creates the lock directory, retrying while another holder has it. A holder
/// whose directory is older than `stale` is taken over.
async fn acquire(lock_dir: &Path, policy: LockPolicy) -> Result<(), CredentialLockError> {
    for attempt in 0..=policy.retries {
        match tokio::fs::create_dir(lock_dir).await {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if is_stale(lock_dir, policy.stale).await {
                    let _ = tokio::fs::remove_dir_all(lock_dir).await;
                    continue;
                }
            }
            Err(_) => return Err(CredentialLockError),
        }
        if attempt < policy.retries {
            tokio::time::sleep(policy.retry_delay).await;
        }
    }
    Err(CredentialLockError)
}

async fn is_stale(lock_dir: &Path, stale: Duration) -> bool {
    let Ok(metadata) = tokio::fs::metadata(lock_dir).await else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age >= stale)
}

/// Refreshes the lock directory's mtime. Creating and removing an entry touches
/// the directory, so the lock stays fresh for other servers.
fn spawn_heartbeat(
    lock_dir: PathBuf,
    update: Duration,
    compromised: oneshot::Sender<()>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let beat = lock_dir.join(".heartbeat");
        loop {
            tokio::time::sleep(update).await;
            let _ = tokio::fs::remove_file(&beat).await;
            if tokio::fs::write(&beat, b"").await.is_err() {
                let _ = compromised.send(());
                return;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_policy() -> LockPolicy {
        LockPolicy {
            stale: Duration::from_secs(120),
            update: Duration::from_millis(10),
            retries: 1,
            retry_delay: Duration::from_millis(5),
        }
    }

    #[tokio::test]
    async fn no_directory_runs_the_task_unlocked() {
        let value = with_chatgpt_session_lock(None, "key", async { 7 }).await;
        assert_eq!(value, Ok(7));
    }

    #[tokio::test]
    async fn the_lock_directory_is_removed_after_the_task() {
        let temp = tempfile::tempdir().unwrap();
        let value =
            with_chatgpt_session_lock_policy(Some(temp.path()), "acct", fast_policy(), async { 1 })
                .await;
        assert_eq!(value, Ok(1));
        assert!(!temp.path().join("acct.bin.lock").exists());
    }

    #[tokio::test]
    async fn a_held_lock_refuses_a_second_holder_until_released() {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().to_path_buf();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let holder_dir = dir.clone();
        let holder = tokio::spawn(async move {
            with_chatgpt_session_lock_policy(Some(&holder_dir), "acct", fast_policy(), async move {
                let _ = release_rx.await;
            })
            .await
        });
        // Wait for the holder to create the directory. This is state-driven, not a timed wait.
        while !dir.join("acct.bin.lock").exists() {
            tokio::task::yield_now().await;
        }

        let second =
            with_chatgpt_session_lock_policy(Some(&dir), "acct", fast_policy(), async { 2 }).await;
        assert_eq!(second, Err(CredentialLockError));

        release_tx.send(()).unwrap();
        assert_eq!(holder.await.unwrap(), Ok(()));
        let third =
            with_chatgpt_session_lock_policy(Some(&dir), "acct", fast_policy(), async { 3 }).await;
        assert_eq!(third, Ok(3));
    }

    #[tokio::test]
    async fn a_stale_holder_is_taken_over() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::create_dir(temp.path().join("acct.bin.lock")).unwrap();
        let policy = LockPolicy {
            stale: Duration::ZERO,
            ..fast_policy()
        };
        let value =
            with_chatgpt_session_lock_policy(Some(temp.path()), "acct", policy, async { 4 }).await;
        assert_eq!(value, Ok(4));
    }

    #[tokio::test]
    async fn losing_the_lock_directory_abandons_the_task() {
        let temp = tempfile::tempdir().unwrap();
        let lock_dir = temp.path().join("acct.bin.lock");
        let removed = lock_dir.clone();
        let policy = fast_policy();
        // The task removes its own lock directory, so the next heartbeat write fails.
        let outcome = tokio::time::timeout(
            Duration::from_secs(5),
            with_chatgpt_session_lock_policy(Some(temp.path()), "acct", policy, async move {
                std::fs::remove_dir_all(&removed).unwrap();
                std::future::pending::<()>().await;
            }),
        )
        .await
        .expect("the heartbeat reports the compromise");
        assert_eq!(outcome, Err::<(), _>(CredentialLockError));
    }
}

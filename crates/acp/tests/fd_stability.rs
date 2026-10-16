//! Dropping an ACP client must release its child's stdin and the connection
//! tasks. Each leaked client used to keep one descriptor open for the life of
//! the process.

#[cfg(unix)]
fn open_fds() -> usize {
    std::fs::read_dir("/dev/fd")
        .expect("list open descriptors")
        .count()
}

/// Abort is asynchronous, so wait until the count stops moving.
#[cfg(unix)]
async fn settled_fds() -> usize {
    let mut last = open_fds();
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let now = open_fds();
        if now == last || tokio::time::Instant::now() > deadline {
            return now;
        }
        last = now;
    }
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_spawn_and_drop_does_not_leak_descriptors() {
    async fn spawn_and_drop() {
        let client = turin_acp::spawn(turin_acp::SpawnCommand::new("/bin/cat", &[]))
            .await
            .expect("spawn cat");
        drop(client);
    }

    // Warm up once so lazily-opened runtime descriptors are part of the baseline.
    spawn_and_drop().await;
    let baseline = settled_fds().await;
    for _ in 0..50 {
        spawn_and_drop().await;
    }
    let after = settled_fds().await;
    assert!(
        after <= baseline,
        "descriptors grew from {baseline} to {after} after 50 spawn/drop cycles"
    );
}

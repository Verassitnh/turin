//! `FileSystem.watch(directory)` filtered to one file, plus `Stream.debounce`,
//! as used by upstream `keybindings.ts` and `serverSettings.ts`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::mpsc;

use super::symlink::lexical_absolute;

/// Upstream debounces watch events by 100ms so the file is fully written
/// before it is read: editors emit several events per save (truncate, write,
/// rename) and `fs.watch` can fire before the content has been flushed.
pub const WATCH_DEBOUNCE: Duration = Duration::from_millis(100);

/// Watches the directory holding `file_path` and sends `()` for every event
/// that names that file. The watch lives as long as the returned watcher.
pub fn watch_file(
    file_path: &Path,
    events: mpsc::UnboundedSender<()>,
) -> notify::Result<RecommendedWatcher> {
    let resolved = lexical_absolute(file_path).map_err(notify::Error::io)?;
    let directory = resolved
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/"));
    let file_name = resolved.file_name().map(|name| name.to_owned());
    // Event paths come back canonical (macOS reports `/private/var/...` for
    // `/var/...`), so the directory is compared in both forms.
    let canonical_directory = std::fs::canonicalize(&directory).ok();
    let watch_directory = canonical_directory
        .clone()
        .unwrap_or_else(|| directory.clone());
    let mut watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        let Ok(event) = result else { return };
        let matches = event.paths.iter().any(|path| {
            if path == &resolved {
                return true;
            }
            if path.file_name() != file_name.as_deref() {
                return false;
            }
            let parent = path.parent();
            parent == Some(directory.as_path())
                || (canonical_directory.is_some() && parent == canonical_directory.as_deref())
        });
        if matches {
            let _ = events.send(());
        }
    })?;
    watcher.watch(&watch_directory, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Waits for the next burst of events and returns once `period` passes with
/// no further event (`Stream.debounce`). `None` when the sender side closed
/// before any event arrived.
pub async fn next_debounced(
    events: &mut mpsc::UnboundedReceiver<()>,
    period: Duration,
) -> Option<()> {
    events.recv().await?;
    loop {
        match tokio::time::timeout(period, events.recv()).await {
            Ok(Some(())) => continue,
            Ok(None) | Err(_) => return Some(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reports_only_the_watched_file_after_a_quiet_period() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("settings.json");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let _watcher = watch_file(&file, tx).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        std::fs::write(root.path().join("other.json"), "{}").unwrap();
        std::fs::write(&file, "{}").unwrap();
        std::fs::write(&file, "{ }").unwrap();

        tokio::time::timeout(
            Duration::from_secs(5),
            next_debounced(&mut rx, WATCH_DEBOUNCE),
        )
        .await
        .expect("a settings event")
        .unwrap();
    }
}

//! Port of upstream/apps/server/src/assets/NativeAppIconResolver.ts.
//!
//! Resolves a macOS application's icon into a 64px PNG in the server's cache
//! directory, so clients receive a local file path and never a host path.
//! Commands (`mdfind`, `mdls`, `plutil`, `sips`) go through [`LaunchHost`].
//!
//! Upstream has no in-flight dedupe for concurrent misses on one key; both
//! run, and the two-permit semaphore bounds the cost. See PORTING.md
//! Deviations.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use indexmap::IndexMap;
use parking_lot::Mutex;
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;
use turin_contracts::provider_runtime::ToolActivityNativeAppReference;
use uuid::Uuid;

use crate::host::LaunchHost;

const ICON_SIZE: &str = "64";
const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);
const RESOLUTION_CACHE_TTL_NANOS: u64 = 3_600 * 1_000_000_000;
const RESOLUTION_CACHE_MAX_ENTRIES: usize = 256;
const RESOLUTION_CONCURRENCY: usize = 2;

#[derive(Clone)]
struct CachedResolution {
    /// `None` is a cached miss: the app has no usable icon.
    value: Option<PathBuf>,
    stored_at_nanos: u64,
}

/// LRU by insertion order: a hit moves its entry to the back.
#[derive(Default)]
struct ResolutionCache {
    entries: IndexMap<String, CachedResolution>,
}

pub struct NativeAppIconResolver {
    host: Arc<dyn LaunchHost>,
    /// Upstream `providerStatusCacheDir`; icons go under `native-app-icons`.
    cache_dir: PathBuf,
    resolutions: Semaphore,
    cache: Mutex<ResolutionCache>,
}

impl NativeAppIconResolver {
    pub fn new(host: Arc<dyn LaunchHost>, cache_dir: PathBuf) -> Self {
        Self {
            host,
            cache_dir,
            resolutions: Semaphore::new(RESOLUTION_CONCURRENCY),
            cache: Mutex::new(ResolutionCache::default()),
        }
    }

    /// The cached icon PNG for the app, or `None` when no icon is available.
    /// Failures are logged and never cached.
    pub async fn resolve(&self, app: &ToolActivityNativeAppReference) -> Option<PathBuf> {
        if let ToolActivityNativeAppReference::DisplayName { display_name } = app {
            if contains_control_character(display_name.as_str()) {
                return None;
            }
        }
        let key = serde_json::to_string(app).ok()?;
        if let Some(cached) = self.cached(&key) {
            match cached {
                None => return None,
                Some(path) if path.is_file() => return Some(path),
                // The icon file was deleted: forget the entry and recompute.
                Some(_) => {
                    self.cache.lock().entries.shift_remove(&key);
                }
            }
        }
        self.compute(&key, app).await
    }

    async fn compute(&self, key: &str, app: &ToolActivityNativeAppReference) -> Option<PathBuf> {
        let _permit = self.resolutions.acquire().await.ok()?;
        match self.resolve_uncached(app).await {
            Ok(value) => {
                self.store(key, value.clone());
                value
            }
            Err(error) => {
                tracing::debug!(%error, "failed to resolve native application icon");
                None
            }
        }
    }

    /// `Some` while a fresh entry exists (the inner `Option` is the cached
    /// value). Refreshes the entry's LRU position on a hit.
    fn cached(&self, key: &str) -> Option<Option<PathBuf>> {
        let now = self.host.monotonic_nanos();
        let mut cache = self.cache.lock();
        let entry = cache.entries.shift_remove(key)?;
        if now.saturating_sub(entry.stored_at_nanos) >= RESOLUTION_CACHE_TTL_NANOS {
            return None;
        }
        let value = entry.value.clone();
        cache.entries.insert(key.to_owned(), entry);
        Some(value)
    }

    fn store(&self, key: &str, value: Option<PathBuf>) {
        let now = self.host.monotonic_nanos();
        let mut cache = self.cache.lock();
        cache.entries.shift_remove(key);
        if cache.entries.len() >= RESOLUTION_CACHE_MAX_ENTRIES {
            cache.entries.shift_remove_index(0);
        }
        cache.entries.insert(
            key.to_owned(),
            CachedResolution {
                value,
                stored_at_nanos: now,
            },
        );
    }

    async fn resolve_uncached(
        &self,
        app: &ToolActivityNativeAppReference,
    ) -> io::Result<Option<PathBuf>> {
        let Some(app_path) = self.resolve_application_path(app).await? else {
            return Ok(None);
        };

        let canonical_app = fs::canonicalize(&app_path)?;
        let info_plist = canonical_app.join("Contents/Info.plist");
        let resources = canonical_app.join("Contents/Resources");
        let mut icon_name = self.plist_value(&info_plist, "CFBundleIconFile").await;
        if icon_name.is_empty() {
            icon_name = self.plist_value(&info_plist, "CFBundleIconName").await;
        }
        if !icon_name.is_empty() && icon_name.contains('/') {
            return Ok(None);
        }
        let icon_file_name = (!icon_name.is_empty()).then(|| {
            if Path::new(&icon_name).extension().is_some() {
                icon_name.clone()
            } else {
                format!("{icon_name}.icns")
            }
        });

        let resource_entries = read_directory_names(&resources);
        let mut source = None;
        if let Some(name) = &icon_file_name {
            source = existing_file(&resources.join(name))?;
        }
        if source.is_none() {
            source = existing_file(&resources.join("AppIcon.icns"))?;
        }
        if source.is_none() {
            if let Some(entry) = resource_entries
                .iter()
                .find(|entry| entry.to_lowercase().ends_with(".icns"))
            {
                source = existing_file(&resources.join(entry))?;
            }
        }
        let Some(source_candidate) = source else {
            return Ok(None);
        };
        let source_icon = fs::canonicalize(&source_candidate)?;
        // The icon must live inside the bundle's Resources directory.
        if source_icon.strip_prefix(&resources).is_err() {
            return Ok(None);
        }

        let mut app_version = self.plist_value(&info_plist, "CFBundleVersion").await;
        if app_version.is_empty() {
            app_version = self
                .plist_value(&info_plist, "CFBundleShortVersionString")
                .await;
        }
        let digest = Sha256::digest(
            format!(
                "{}\0{}\0{}",
                canonical_app.display(),
                app_version,
                source_icon.display()
            )
            .as_bytes(),
        );
        let cache_key: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
        let cache_directory = self.cache_dir.join("native-app-icons");
        let cache_path = cache_directory.join(format!("{cache_key}.png"));
        if existing_file(&cache_path)?.is_some() {
            return Ok(Some(cache_path));
        }

        fs::create_dir_all(&cache_directory)?;
        let temporary_path = cache_directory.join(format!(
            ".{cache_key}-{}-{}.png",
            std::process::id(),
            Uuid::new_v4()
        ));
        let source_arg = source_icon.display().to_string();
        let temporary_arg = temporary_path.display().to_string();
        let args = [
            "-z",
            ICON_SIZE,
            ICON_SIZE,
            "-s",
            "format",
            "png",
            source_arg.as_str(),
            "--out",
            temporary_arg.as_str(),
        ]
        .map(str::to_owned)
        .to_vec();
        let converted = self
            .host
            .command_output("/usr/bin/sips", &args, COMMAND_TIMEOUT)
            .await;
        let renamed = match converted {
            Ok(_) => fs::rename(&temporary_path, &cache_path),
            Err(error) => Err(error),
        };
        // Runs on failure too. After a successful rename there is nothing to remove.
        let _ = fs::remove_file(&temporary_path);
        renamed?;
        existing_file(&cache_path)
    }

    async fn resolve_application_path(
        &self,
        app: &ToolActivityNativeAppReference,
    ) -> io::Result<Option<PathBuf>> {
        let query = match app {
            ToolActivityNativeAppReference::AppId { app_id } => {
                format!("kMDItemCFBundleIdentifier == '{}'", app_id.as_str())
            }
            ToolActivityNativeAppReference::DisplayName { display_name } => format!(
                "kMDItemContentType == 'com.apple.application-bundle' && kMDItemDisplayName == '{}'",
                escape_spotlight_string(display_name.as_str())
            ),
        };
        let spotlight_output = self
            .host
            .command_output("/usr/bin/mdfind", &[query], COMMAND_TIMEOUT)
            .await?;
        let candidates: Vec<&str> = spotlight_output
            .split('\n')
            .map(str::trim)
            .filter(|value| value.ends_with(".app"))
            .collect();
        let matching: Vec<&str> = match app {
            ToolActivityNativeAppReference::AppId { .. } => candidates.clone(),
            ToolActivityNativeAppReference::DisplayName { display_name } => candidates
                .iter()
                .copied()
                .filter(|value| {
                    basename_without_app(value).to_lowercase()
                        == display_name.as_str().to_lowercase()
                })
                .collect(),
        };
        let ranked = if matching.is_empty() {
            candidates
        } else {
            matching
        };

        // The most recently used bundle wins. The timestamp is compared as text,
        // as upstream does; it is ISO-8601 so that orders correctly.
        let mut most_recent: Option<(&str, String)> = None;
        for candidate in ranked {
            let last_used = self
                .host
                .command_output(
                    "/usr/bin/mdls",
                    &[
                        "-raw".to_owned(),
                        "-name".to_owned(),
                        "kMDItemLastUsedDate".to_owned(),
                        candidate.to_owned(),
                    ],
                    COMMAND_TIMEOUT,
                )
                .await
                .map(|value| value.trim().to_owned())
                .unwrap_or_default();
            let is_newer = most_recent
                .as_ref()
                .is_none_or(|(_, previous)| last_used > *previous);
            if is_newer {
                most_recent = Some((candidate, last_used));
            }
        }
        Ok(most_recent.map(|(path, _)| PathBuf::from(path)))
    }

    /// A key from an `Info.plist`, trimmed, or empty on any failure.
    async fn plist_value(&self, info_plist: &Path, key: &str) -> String {
        let args = [
            "-extract".to_owned(),
            key.to_owned(),
            "raw".to_owned(),
            "-o".to_owned(),
            "-".to_owned(),
            info_plist.display().to_string(),
        ];
        self.host
            .command_output("/usr/bin/plutil", &args, COMMAND_TIMEOUT)
            .await
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    }
}

/// `Some(path)` when `path` is a regular file, `None` when it is missing.
/// Any other error fails the resolution, as upstream does.
fn existing_file(path: &Path) -> io::Result<Option<PathBuf>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => Ok(Some(path.to_path_buf())),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// Entry names of a directory. Empty when it cannot be read.
fn read_directory_names(directory: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Spotlight query literal escaping: `\`, `'`, `*` and `?` get a backslash.
fn escape_spotlight_string(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        if matches!(character, '\\' | '\'' | '*' | '?') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

/// Any C0 control character or DEL. Display names with one never reach
/// Spotlight.
fn contains_control_character(value: &str) -> bool {
    value
        .chars()
        .any(|character| u32::from(character) <= 31 || u32::from(character) == 127)
}

/// `path.basename(value, ".app")`.
fn basename_without_app(value: &str) -> &str {
    let name = value.rsplit('/').next().unwrap_or(value);
    name.strip_suffix(".app").unwrap_or(name)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;

    use turin_contracts::base_schemas::TrimmedNonEmptyString;

    use super::*;
    use crate::host::test_support::FakeHost;

    type Output = Box<dyn Fn(&str, &[String]) -> io::Result<String> + Send + Sync>;

    fn display_name(name: &str) -> ToolActivityNativeAppReference {
        ToolActivityNativeAppReference::DisplayName {
            display_name: TrimmedNonEmptyString::new(name).unwrap(),
        }
    }

    fn resolver(host: &Arc<FakeHost>, cache_dir: &Path) -> NativeAppIconResolver {
        NativeAppIconResolver::new(
            Arc::clone(host) as Arc<dyn LaunchHost>,
            cache_dir.to_path_buf(),
        )
    }

    #[tokio::test]
    async fn escapes_spotlight_wildcards_and_caches_misses() {
        let cache = tempfile::tempdir().unwrap();
        let commands: Arc<StdMutex<Vec<(String, Vec<String>)>>> = Arc::default();
        let recorded = Arc::clone(&commands);
        let host = Arc::new(FakeHost::new());
        *host.output.lock().unwrap() = Some(Box::new(move |command: &str, args: &[String]| {
            recorded
                .lock()
                .unwrap()
                .push((command.to_owned(), args.to_vec()));
            Ok(String::new())
        }) as Output);
        let resolver = resolver(&host, cache.path());
        let app = display_name("Review * App");

        assert_eq!(resolver.resolve(&app).await, None);
        assert_eq!(resolver.resolve(&app).await, None);

        {
            let commands = commands.lock().unwrap();
            assert_eq!(commands.len(), 1);
            assert_eq!(commands[0].0, "/usr/bin/mdfind");
            assert!(commands[0].1[0].contains("Review \\* App"));
        }

        for index in 0..256 {
            assert_eq!(
                resolver
                    .resolve(&display_name(&format!("Missing Review App {index}")))
                    .await,
                None
            );
        }
        assert_eq!(commands.lock().unwrap().len(), 257);
        // The 256 newer misses evicted the oldest entry, so it is looked up again.
        assert_eq!(resolver.resolve(&app).await, None);
        assert_eq!(commands.lock().unwrap().len(), 258);
    }

    #[tokio::test]
    async fn converts_an_app_icon_once_into_the_cache_directory() {
        let home = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let app = home.path().join("Foo.app");
        let resources = app.join("Contents/Resources");
        fs::create_dir_all(&resources).unwrap();
        fs::write(app.join("Contents/Info.plist"), "").unwrap();
        fs::write(resources.join("AppIcon.icns"), "icns-bytes").unwrap();

        let app_path = app.display().to_string();
        let converted = Arc::new(StdMutex::new(0usize));
        let counter = Arc::clone(&converted);
        let host = Arc::new(FakeHost::new());
        *host.output.lock().unwrap() =
            Some(
                Box::new(move |command: &str, args: &[String]| match command {
                    "/usr/bin/mdfind" => Ok(format!("{app_path}\n")),
                    "/usr/bin/mdls" => Ok("2026-10-01 00:00:00 +0000".to_owned()),
                    "/usr/bin/plutil" => match args[1].as_str() {
                        "CFBundleIconFile" => Ok("AppIcon\n".to_owned()),
                        "CFBundleVersion" => Ok("42\n".to_owned()),
                        _ => Ok(String::new()),
                    },
                    "/usr/bin/sips" => {
                        *counter.lock().unwrap() += 1;
                        fs::write(&args[args.len() - 1], "png-bytes")?;
                        Ok(String::new())
                    }
                    other => Err(io::Error::other(format!("unexpected command {other}"))),
                }) as Output,
            );
        let resolver = resolver(&host, cache.path());
        let app_ref = ToolActivityNativeAppReference::AppId {
            app_id: TrimmedNonEmptyString::new("com.example.foo").unwrap(),
        };

        let first = resolver.resolve(&app_ref).await.expect("icon resolved");
        assert!(first.starts_with(cache.path().join("native-app-icons")));
        assert_eq!(fs::read_to_string(&first).unwrap(), "png-bytes");
        assert_eq!(resolver.resolve(&app_ref).await, Some(first.clone()));
        assert_eq!(*converted.lock().unwrap(), 1);

        // A deleted cache file is converted again instead of returned.
        fs::remove_file(&first).unwrap();
        assert_eq!(resolver.resolve(&app_ref).await, Some(first.clone()));
        assert_eq!(*converted.lock().unwrap(), 2);
    }

    #[test]
    fn spotlight_escaping_and_control_characters() {
        assert_eq!(escape_spotlight_string(r"a'b*c?d\e"), r"a\'b\*c\?d\\e");
        assert!(contains_control_character("bad\nname"));
        assert!(contains_control_character("bad\u{7f}name"));
        assert!(!contains_control_character("Fine App"));
    }
}

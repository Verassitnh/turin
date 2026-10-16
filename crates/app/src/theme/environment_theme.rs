//! Port of upstream/apps/server/src/environmentTheme.ts, with the
//! `EnvironmentTheme*` contracts from upstream/packages/contracts/src/server.ts
//! (kept here until the contracts crate grows them).
//!
//! EnvironmentTheme - palettes this machine publishes for clients to follow.
//!
//! A desktop that retints its apps when the user switches system theme writes
//! `<stateDir>/themes/<id>.json`; this service watches that directory and
//! publishes the set so a theme change lands without a restart. The filename
//! is the theme id: it stays stable while the machine rewrites the colors
//! underneath, so `defaultTheme` and a client's selection keep pointing at the
//! same theme across recolors. Theming is cosmetic, so every failure here
//! degrades to "not published" rather than propagating.

// Ported ahead of the views that consume it (theme UI, T22 wiring).
#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde_json::{Map, Value};
use tokio::sync::{Mutex as AsyncMutex, mpsc, watch};
use tokio_util::task::AbortOnDropHandle;
use turin_contracts::base_schemas::js_trim;

use super::theme_palettes::{ThemeAppearance, UNPUBLISHABLE_THEME_IDS};

const THEME_FILE_SUFFIX: &str = ".json";

/// Bounds on what a machine can publish. The directory is local, so this is not
/// a trust boundary -- but an accidental dump of large files there would
/// otherwise be read in full, streamed to every client, and repainted, so the
/// cost of a mistake is capped rather than unbounded.
const MAX_THEME_FILES: usize = 32;
/// Exported so the publish path cannot accept a file the watcher will skip.
pub const MAX_THEME_FILE_BYTES: u64 = 32 * 1024;
/// The set travels whole to every subscriber, so the sum matters more than any
/// single file. An exported theme runs a few KB, leaving this far above any
/// real directory while keeping a mistake off the wire.
const MAX_THEME_TOTAL_BYTES: usize = 192 * 1024;

/// Debounced for the same reason settings watching is: a theme script emits
/// several events per save and a watcher can fire before the content is
/// flushed. Every event triggers a full re-read, so no event needs filtering.
const WATCH_DEBOUNCE: Duration = Duration::from_millis(100);

// ── Contracts ────────────────────────────────────────────────────────

/// `EnvironmentThemeColor`: `#rgb` or `#rrggbb`.
pub fn is_environment_theme_color(value: &str) -> bool {
    let Some(hex) = value.strip_prefix('#') else {
        return false;
    };
    matches!(hex.len(), 3 | 6) && hex.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `EnvironmentThemeId`: matches the client-side theme id rule, so a published
/// id is selectable. The appearance keywords are excluded outright: a
/// published `dark.json` would otherwise capture every client whose stored
/// preference is the stock `"dark"`, retinting people who never chose it.
pub fn is_environment_theme_id(value: &str) -> bool {
    if matches!(value, "system" | "light" | "dark") {
        return false;
    }
    let bytes = value.as_bytes();
    let valid_first = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    !bytes.is_empty()
        && bytes.len() <= 48
        && valid_first(bytes[0])
        && bytes[1..]
            .iter()
            .all(|byte| valid_first(*byte) || *byte == b'-')
}

/// Role colors as published. Values are any CSS color the client's theme
/// parser accepts, canonicalized client-side; roles a build does not know are
/// dropped there. Keys must still be role-shaped and values color-sized.
pub type EnvironmentThemeColors = BTreeMap<String, String>;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentThemeVariants {
    pub light: Option<EnvironmentThemeColors>,
    pub dark: Option<EnvironmentThemeColors>,
}

impl EnvironmentThemeVariants {
    pub fn get(&self, appearance: ThemeAppearance) -> Option<&EnvironmentThemeColors> {
        match appearance {
            ThemeAppearance::Light => self.light.as_ref(),
            ThemeAppearance::Dark => self.dark.as_ref(),
        }
    }
}

/// One published theme file. The id is the filename, not part of the content,
/// so a file cannot claim another file's identity; an embedded `id` is ignored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentThemeFile {
    /// Standard exported theme files carry `version: 1`; the seeded short form
    /// a desktop generates has no version.
    pub version: Option<u8>,
    /// Shown on the theme card, e.g. the desktop theme's own name.
    pub name: String,
    pub appearance: ThemeAppearance,
    /// Seed colors. When present, clients derive the full palette from them
    /// and layer `colors` on top; when absent, `colors` is the palette.
    pub canvas: Option<String>,
    pub accent: Option<String>,
    pub colors: Option<EnvironmentThemeColors>,
    /// The other appearance's palette, as exported theme files carry it.
    pub variants: Option<EnvironmentThemeVariants>,
}

fn decode_colors(value: &Value) -> Result<EnvironmentThemeColors, String> {
    let record = value.as_object().ok_or("colors must be an object")?;
    let mut colors = EnvironmentThemeColors::new();
    for (role, color) in record {
        let mut chars = role.chars();
        let role_shaped = role.len() <= 64
            && chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric());
        if !role_shaped {
            return Err(format!("invalid role name {role:?}"));
        }
        let color = color.as_str().ok_or("color must be a string")?;
        let trimmed = js_trim(color);
        if trimmed.is_empty() || trimmed.chars().count() > 64 {
            return Err(format!("invalid color for {role}"));
        }
        colors.insert(role.clone(), trimmed.to_string());
    }
    Ok(colors)
}

fn decode_seed(value: &Value, field: &str) -> Result<String, String> {
    value
        .as_str()
        .filter(|seed| is_environment_theme_color(seed))
        .map(str::to_string)
        .ok_or_else(|| format!("{field} must be a #rgb or #rrggbb color"))
}

impl EnvironmentThemeFile {
    /// `Schema.decodeUnknownExit(Schema.fromJsonString(EnvironmentThemeFile))`.
    /// Unknown fields (including an embedded `id`) are ignored.
    pub fn decode(raw: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
        Self::from_value(&value)
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let record = value.as_object().ok_or("theme file must be an object")?;
        let version = match record.get("version") {
            None => None,
            Some(Value::Number(number)) if number.as_u64() == Some(1) => Some(1),
            Some(_) => return Err("version must be 1".to_string()),
        };
        let name = record
            .get("name")
            .and_then(Value::as_str)
            .map(js_trim)
            .filter(|name| !name.is_empty() && name.chars().count() <= 48)
            .ok_or("name must be a non-empty string of at most 48 characters")?
            .to_string();
        let appearance = match record.get("appearance").and_then(Value::as_str) {
            Some("light") => ThemeAppearance::Light,
            Some("dark") => ThemeAppearance::Dark,
            _ => return Err("appearance must be light or dark".to_string()),
        };
        let canvas = record
            .get("canvas")
            .map(|v| decode_seed(v, "canvas"))
            .transpose()?;
        let accent = record
            .get("accent")
            .map(|v| decode_seed(v, "accent"))
            .transpose()?;
        let colors = record.get("colors").map(decode_colors).transpose()?;
        let variants = record
            .get("variants")
            .map(|variants| {
                let record = variants.as_object().ok_or("variants must be an object")?;
                Ok::<_, String>(EnvironmentThemeVariants {
                    light: record.get("light").map(decode_colors).transpose()?,
                    dark: record.get("dark").map(decode_colors).transpose()?,
                })
            })
            .transpose()?;
        Ok(Self {
            version,
            name,
            appearance,
            canvas,
            accent,
            colors,
            variants,
        })
    }

    /// `Schema.fromJsonString(EnvironmentThemeFile)` encode.
    pub fn encode(&self) -> String {
        let colors_value = |colors: &EnvironmentThemeColors| {
            Value::Object(
                colors
                    .iter()
                    .map(|(role, color)| (role.clone(), Value::String(color.clone())))
                    .collect::<Map<_, _>>(),
            )
        };
        let mut record = Map::new();
        if let Some(version) = self.version {
            record.insert("version".into(), version.into());
        }
        record.insert("name".into(), self.name.clone().into());
        record.insert(
            "appearance".into(),
            match self.appearance {
                ThemeAppearance::Light => "light",
                ThemeAppearance::Dark => "dark",
            }
            .into(),
        );
        if let Some(canvas) = &self.canvas {
            record.insert("canvas".into(), canvas.clone().into());
        }
        if let Some(accent) = &self.accent {
            record.insert("accent".into(), accent.clone().into());
        }
        if let Some(colors) = &self.colors {
            record.insert("colors".into(), colors_value(colors));
        }
        if let Some(variants) = &self.variants {
            let mut encoded = Map::new();
            if let Some(light) = &variants.light {
                encoded.insert("light".into(), colors_value(light));
            }
            if let Some(dark) = &variants.dark {
                encoded.insert("dark".into(), colors_value(dark));
            }
            record.insert("variants".into(), Value::Object(encoded));
        }
        Value::Object(record).to_string()
    }

    /// `environmentThemeFileHasColors`: whether a theme file carries anything
    /// to render. A file with neither seeds nor colors would show as the stock
    /// palette wearing a name, which reads as a bug rather than a theme.
    pub fn has_colors(&self) -> bool {
        (self.canvas.is_some() && self.accent.is_some())
            || self
                .colors
                .as_ref()
                .is_some_and(|colors| !colors.is_empty())
    }
}

/// `EnvironmentTheme`: a published file plus its id (the filename without its
/// extension, stable across recolors).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnvironmentTheme {
    pub id: String,
    pub file: EnvironmentThemeFile,
}

impl std::ops::Deref for EnvironmentTheme {
    type Target = EnvironmentThemeFile;

    fn deref(&self) -> &Self::Target {
        &self.file
    }
}

// ── Reading the directory ────────────────────────────────────────────

/// Reads a theme file through one opened handle, so every check binds to the
/// file actually read rather than to a path that may have been swapped since:
/// `O_NOFOLLOW` rejects a symlink outright (a symlinked themes directory stays
/// usable, a symlinked file inside it does not), `O_NONBLOCK` keeps a FIFO
/// from blocking the open, and the fstat type and size gate examines the open
/// descriptor. Returns `None` for anything that is not a small regular file.
pub fn read_theme_file_guarded(file_path: &Path, max_bytes: u64) -> Option<String> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(not(unix))]
    {
        // No O_NOFOLLOW: the symlink check is an lstat before the open. That
        // leaves a window a swap could slip through, which the descriptor-bound
        // checks below then narrow to "a regular file at that path".
        if std::fs::symlink_metadata(file_path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(true)
        {
            return None;
        }
    }
    let mut file: File = options.open(file_path).ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return None;
    }
    let mut contents = Vec::with_capacity(metadata.len() as usize);
    file.by_ref()
        .take(max_bytes)
        .read_to_end(&mut contents)
        .ok()?;
    String::from_utf8(contents).ok()
}

/// Every theme the directory actually publishes. A file that is missing,
/// unreadable, malformed, colorless, or misnamed is simply skipped; the rest
/// of the set is unaffected. The one place that decides what "published"
/// means, so a caller validating an id cannot disagree with the watcher
/// serving it.
pub fn read_published_themes(themes_dir: &Path) -> Vec<EnvironmentTheme> {
    let mut entries: Vec<String> = std::fs::read_dir(themes_dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    entries.sort();

    let mut themes = Vec::new();
    let mut examined = 0usize;
    let mut total_bytes = 0usize;
    for entry in entries {
        let Some(id) = entry.strip_suffix(THEME_FILE_SUFFIX) else {
            continue;
        };
        // A reserved id is either shadowed by a built-in on the client or
        // captures clients that never chose it, so it is not publishable.
        if !is_environment_theme_id(id) || UNPUBLISHABLE_THEME_IDS.contains(&id) {
            continue;
        }

        // Counts files examined, not themes accepted: capping the output would
        // let a directory of malformed files be opened, read, and decoded in
        // full on every refresh and every client connect.
        examined += 1;
        if examined > MAX_THEME_FILES {
            tracing::warn!(
                path = %themes_dir.display(),
                limit = MAX_THEME_FILES,
                "ignoring environment theme files past the limit"
            );
            break;
        }

        let file_path = themes_dir.join(&entry);
        let Some(raw) = read_theme_file_guarded(&file_path, MAX_THEME_FILE_BYTES) else {
            tracing::warn!(
                path = %file_path.display(),
                limit = MAX_THEME_FILE_BYTES,
                "ignoring unusable environment theme file"
            );
            continue;
        };
        if js_trim(&raw).is_empty() {
            continue;
        }

        let file = match EnvironmentThemeFile::decode(&raw) {
            Ok(file) => file,
            Err(detail) => {
                tracing::warn!(
                    path = %file_path.display(),
                    detail,
                    "ignoring invalid environment theme"
                );
                continue;
            }
        };
        if !file.has_colors() {
            tracing::warn!(
                path = %file_path.display(),
                "ignoring environment theme without colors"
            );
            continue;
        }

        // Counted only once accepted: the cap bounds what travels to clients,
        // so a skipped file must not eat the budget of valid themes sorted
        // after it. Bytes, not string length -- the cap describes wire weight.
        total_bytes += raw.len();
        if total_bytes > MAX_THEME_TOTAL_BYTES {
            tracing::warn!(
                path = %themes_dir.display(),
                limit = MAX_THEME_TOTAL_BYTES,
                "ignoring environment themes past the total size limit"
            );
            break;
        }

        themes.push(EnvironmentTheme {
            id: id.to_string(),
            file,
        });
    }
    themes
}

// ── The service ──────────────────────────────────────────────────────

/// The published set with the sequence number it was observed at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct PublishedThemes {
    seq: u64,
    themes: Vec<EnvironmentTheme>,
}

pub struct EnvironmentThemeService {
    themes_dir: PathBuf,
    /// Latest set only (upstream: `PubSub.sliding(1)`): every update carries
    /// the complete set, so a subscriber that stops consuming holds at most the
    /// newest set rather than an unbounded backlog.
    changes: watch::Sender<PublishedThemes>,
    /// Guards the whole read/compare/publish, not just the state update. The
    /// directory read is async, so two concurrent refreshes can finish out of
    /// order and a slower read of an older set would publish under a higher
    /// sequence.
    refresh_lock: AsyncMutex<()>,
    _watcher: parking_lot::Mutex<Option<notify::RecommendedWatcher>>,
    _watch_task: parking_lot::Mutex<Option<AbortOnDropHandle<()>>>,
}

/// A client's view of the published sets: the current set first, then every
/// change, with repeats dropped.
pub struct EnvironmentThemeSubscription {
    receiver: watch::Receiver<PublishedThemes>,
    snapshot: Option<Vec<EnvironmentTheme>>,
    snapshot_seq: u64,
}

impl EnvironmentThemeSubscription {
    pub async fn next(&mut self) -> Option<Vec<EnvironmentTheme>> {
        if let Some(snapshot) = self.snapshot.take() {
            return Some(snapshot);
        }
        loop {
            self.receiver.changed().await.ok()?;
            let update = self.receiver.borrow_and_update().clone();
            // Drop anything the snapshot already accounts for.
            if update.seq > self.snapshot_seq {
                self.snapshot_seq = update.seq;
                return Some(update.themes);
            }
        }
    }
}

impl EnvironmentThemeService {
    /// Creates the directory, seeds the dedupe with the first read, and starts
    /// watching it.
    pub async fn start(themes_dir: PathBuf) -> Arc<Self> {
        // Created up front so the watcher has something to attach to before
        // the first publisher writes into it.
        if let Err(error) = std::fs::create_dir_all(&themes_dir) {
            tracing::warn!(path = %themes_dir.display(), %error, "could not create the themes directory");
        }
        let (changes, _) = watch::channel(PublishedThemes::default());
        let service = Arc::new(Self {
            themes_dir,
            changes,
            refresh_lock: AsyncMutex::new(()),
            _watcher: parking_lot::Mutex::new(None),
            _watch_task: parking_lot::Mutex::new(None),
        });
        // Seeds the dedupe so a watch event that reports no actual change (a
        // touch, a rewrite with identical contents) does not retint every
        // client.
        service.refresh().await;

        let (events_tx, mut events_rx) = mpsc::unbounded_channel::<()>();
        let watcher = notify::recommended_watcher(move |_event: notify::Result<notify::Event>| {
            let _ = events_tx.send(());
        })
        .and_then(|mut watcher| {
            watcher.watch(&service.themes_dir, RecursiveMode::NonRecursive)?;
            Ok(watcher)
        });
        match watcher {
            Ok(watcher) => *service._watcher.lock() = Some(watcher),
            Err(error) => tracing::warn!(%error, "could not watch the themes directory"),
        }
        let weak = Arc::downgrade(&service);
        let task = tokio::spawn(async move {
            while events_rx.recv().await.is_some() {
                tokio::time::sleep(WATCH_DEBOUNCE).await;
                while events_rx.try_recv().is_ok() {}
                let Some(service) = weak.upgrade() else {
                    break;
                };
                service.refresh().await;
            }
        });
        *service._watch_task.lock() = Some(AbortOnDropHandle::new(task));
        service
    }

    /// Reads the directory and folds it into the sequenced state, publishing
    /// only a genuine change. Every reader goes through here, so the snapshot a
    /// client connects on and the events it then receives come from one
    /// ordered source rather than from disk and the queue independently.
    async fn refresh(&self) -> PublishedThemes {
        let _permit = self.refresh_lock.lock().await;
        let dir = self.themes_dir.clone();
        let themes = tokio::task::spawn_blocking(move || read_published_themes(&dir))
            .await
            .unwrap_or_default();
        let previous = self.changes.borrow().clone();
        if previous.themes == themes {
            return previous;
        }
        let updated = PublishedThemes {
            seq: previous.seq + 1,
            themes,
        };
        self.changes.send_replace(updated.clone());
        updated
    }

    /// The set published right now, read from disk rather than from the
    /// watcher's last observation: a client connecting must see what the
    /// machine actually publishes even if it missed a filesystem event.
    pub async fn current(&self) -> Vec<EnvironmentTheme> {
        self.refresh().await.themes
    }

    /// The current set followed by every change, with repeats dropped. The
    /// subscription is acquired before the current set is read, so a publish
    /// landing while a client connects is delivered rather than lost.
    pub async fn stream_changes(&self) -> EnvironmentThemeSubscription {
        let mut receiver = self.changes.subscribe();
        let snapshot = self.refresh().await;
        receiver.mark_unchanged();
        EnvironmentThemeSubscription {
            receiver,
            snapshot: Some(snapshot.themes),
            snapshot_seq: snapshot.seq,
        }
    }
}

#[cfg(test)]
mod tests {
    // Port of upstream/apps/server/src/environmentTheme.test.ts.
    use super::*;

    fn nightfall() -> EnvironmentThemeFile {
        EnvironmentThemeFile {
            version: None,
            name: "Nightfall".to_string(),
            appearance: ThemeAppearance::Dark,
            canvas: Some("#1a1b26".to_string()),
            accent: Some("#7aa2f7".to_string()),
            colors: None,
            variants: None,
        }
    }

    /// The standard exported form: a full palette, no seeds.
    fn shared_light() -> EnvironmentThemeFile {
        EnvironmentThemeFile {
            version: Some(1),
            name: "Shared Light".to_string(),
            appearance: ThemeAppearance::Light,
            canvas: None,
            accent: None,
            colors: Some(
                [("canvas", "#eff1f5"), ("accent", "#1e66f5")]
                    .into_iter()
                    .map(|(role, color)| (role.to_string(), color.to_string()))
                    .collect(),
            ),
            variants: None,
        }
    }

    /// Seeds theme files before the service starts, as a real machine would.
    fn themes_dir_with(seeds: &[(&str, String)]) -> (tempfile::TempDir, PathBuf) {
        let base = tempfile::Builder::new()
            .prefix("t3code-environment-theme-")
            .tempdir()
            .expect("temp dir");
        let themes_dir = base.path().join("userdata").join("themes");
        std::fs::create_dir_all(&themes_dir).expect("themes dir");
        for (filename, contents) in seeds {
            std::fs::write(themes_dir.join(filename), contents).expect("seed");
        }
        (base, themes_dir)
    }

    fn ids(themes: &[EnvironmentTheme]) -> Vec<&str> {
        themes.iter().map(|theme| theme.id.as_str()).collect()
    }

    #[tokio::test]
    async fn publishes_nothing_when_the_machine_has_no_theme_files() {
        let (_base, dir) = themes_dir_with(&[]);
        let service = EnvironmentThemeService::start(dir).await;
        assert_eq!(service.current().await, vec![]);
    }

    #[tokio::test]
    async fn publishes_each_file_under_its_filename_as_the_id() {
        let (_base, dir) = themes_dir_with(&[
            ("nightfall.json", nightfall().encode()),
            ("shared-light.json", shared_light().encode()),
        ]);
        let service = EnvironmentThemeService::start(dir).await;
        let themes = service.current().await;
        assert_eq!(ids(&themes), ["nightfall", "shared-light"]);
        assert_eq!(
            themes[0],
            EnvironmentTheme {
                id: "nightfall".to_string(),
                file: nightfall()
            }
        );
        assert_eq!(
            themes[1],
            EnvironmentTheme {
                id: "shared-light".to_string(),
                file: shared_light()
            }
        );
    }

    // Read from disk rather than from the watcher's last observation, so a
    // client connecting after a missed filesystem event still sees the truth.
    #[tokio::test]
    async fn follows_the_directory_rather_than_the_set_read_at_start() {
        let (_base, dir) = themes_dir_with(&[("nightfall.json", nightfall().encode())]);
        let service = EnvironmentThemeService::start(dir.clone()).await;

        std::fs::write(dir.join("shared-light.json"), shared_light().encode()).expect("write");
        assert_eq!(service.current().await.len(), 2);

        std::fs::remove_file(dir.join("nightfall.json")).expect("remove");
        assert_eq!(ids(&service.current().await), ["shared-light"]);
    }

    // The subscription is acquired before the current set is read, so nothing
    // published while a client connects can fall between snapshot and stream.
    #[tokio::test]
    async fn streams_the_current_set_first() {
        let (_base, dir) = themes_dir_with(&[("nightfall.json", nightfall().encode())]);
        let service = EnvironmentThemeService::start(dir).await;
        let first = service.stream_changes().await.next().await;
        assert_eq!(
            first,
            Some(vec![EnvironmentTheme {
                id: "nightfall".to_string(),
                file: nightfall()
            }])
        );
    }

    // Subscribing happens before the snapshot read, so a publish landing in
    // between is queued. It must not replay after the newer snapshot and walk
    // clients back onto colors the machine has already moved past.
    #[tokio::test]
    async fn never_replays_a_set_older_than_the_snapshot_it_started_from() {
        let (_base, dir) = themes_dir_with(&[("nightfall.json", nightfall().encode())]);
        let service = EnvironmentThemeService::start(dir.clone()).await;

        // Advance the directory without waiting for the watcher, so the read
        // behind the snapshot is strictly newer than anything observed.
        std::fs::write(dir.join("shared-light.json"), shared_light().encode()).expect("write");
        let first = service.stream_changes().await.next().await;
        assert_eq!(
            ids(&first.expect("snapshot")),
            ["nightfall", "shared-light"]
        );
    }

    #[tokio::test]
    async fn skips_invalid_files_while_keeping_valid_ones() {
        let (_base, dir) = themes_dir_with(&[
            ("nightfall.json", nightfall().encode()),
            (
                "unresolved.json",
                r##"{ "name": "X", "appearance": "dark", "canvas": "{{ background }}", "accent": "#7aa2f7" }"##
                    .to_string(),
            ),
            ("malformed.json", "{ not json".to_string()),
            (
                "no-colors.json",
                r#"{ "name": "Empty", "appearance": "dark" }"#.to_string(),
            ),
            ("Bad Name.json", shared_light().encode()),
            ("ocean.json", shared_light().encode()),
            ("dark.json", shared_light().encode()),
            ("notes.txt", "not a theme".to_string()),
        ]);
        let service = EnvironmentThemeService::start(dir).await;
        assert_eq!(ids(&service.current().await), ["nightfall"]);
    }

    // A symlinked themes directory stays usable, but a symlinked file inside it
    // must not publish whatever it points at.
    #[cfg(unix)]
    #[tokio::test]
    async fn ignores_a_symlinked_theme_file() {
        let (_base, dir) = themes_dir_with(&[]);
        let outside = dir.join("..").join("outside.json");
        std::fs::write(&outside, nightfall().encode()).expect("write");
        std::os::unix::fs::symlink(&outside, dir.join("nightfall.json")).expect("symlink");
        let service = EnvironmentThemeService::start(dir).await;
        assert_eq!(service.current().await, vec![]);
    }

    // The aggregate size cap charges only accepted themes, so a pile of
    // malformed files cannot spend the budget and hide a valid theme sorted
    // after them.
    #[tokio::test]
    async fn does_not_charge_skipped_files_against_the_total_size_limit() {
        let mut seeds: Vec<(String, String)> = (0..7)
            .map(|index| (format!("junk-{index}.json"), "{".repeat(30_000)))
            .collect();
        seeds.push(("zz-valid.json".to_string(), nightfall().encode()));
        let seeds: Vec<(&str, String)> = seeds
            .iter()
            .map(|(name, contents)| (name.as_str(), contents.clone()))
            .collect();
        let (_base, dir) = themes_dir_with(&seeds);
        let service = EnvironmentThemeService::start(dir).await;
        assert_eq!(ids(&service.current().await), ["zz-valid"]);
    }

    #[tokio::test]
    async fn embedded_ids_are_ignored_and_oversized_files_are_skipped() {
        let (_base, dir) = themes_dir_with(&[
            (
                "claimed.json",
                r##"{ "id": "other", "name": "Claimed", "appearance": "dark", "canvas": "#101010", "accent": "#fafafa" }"##
                    .to_string(),
            ),
            (
                "huge.json",
                format!(
                    r##"{{ "name": "Huge", "appearance": "dark", "canvas": "#101010", "accent": "#fafafa", "pad": "{}" }}"##,
                    "x".repeat(40_000)
                ),
            ),
        ]);
        let service = EnvironmentThemeService::start(dir).await;
        assert_eq!(ids(&service.current().await), ["claimed"]);
    }

    #[test]
    fn validates_ids_and_seed_colors() {
        for good in ["a", "nightfall", "shared-light", "0day"] {
            assert!(is_environment_theme_id(good), "{good}");
        }
        for bad in [
            "",
            "-x",
            "Bad",
            "system",
            "light",
            "dark",
            "has space",
            &"a".repeat(49),
        ] {
            assert!(!is_environment_theme_id(bad), "{bad}");
        }
        assert!(is_environment_theme_color("#abc"));
        assert!(is_environment_theme_color("#A1b2C3"));
        assert!(!is_environment_theme_color("#abcd"));
        assert!(!is_environment_theme_color("abc"));
        assert!(!is_environment_theme_color("{{ background }}"));
    }

    fn next_set(
        subscription: &mut EnvironmentThemeSubscription,
    ) -> impl std::future::Future<
        Output = Result<Option<Vec<EnvironmentTheme>>, tokio::time::error::Elapsed>,
    > + '_ {
        tokio::time::timeout(Duration::from_secs(30), subscription.next())
    }

    // The feature's headline claim: rewrite a file and connected clients
    // retint without a restart. Real filesystem events, so this proves the
    // watcher rather than a direct read.
    #[tokio::test]
    async fn streams_a_set_for_every_change_to_the_directory() {
        let (base, dir) = themes_dir_with(&[]);
        let service = EnvironmentThemeService::start(dir.clone()).await;
        let mut subscription = service.stream_changes().await;

        // Empty to start.
        assert_eq!(
            next_set(&mut subscription).await.expect("timeout"),
            Some(vec![])
        );

        // Published atomically, the way a theme hook writes it.
        let staging = base.path().join("staged.json");
        std::fs::write(&staging, nightfall().encode()).expect("stage");
        std::fs::rename(&staging, dir.join("nightfall.json")).expect("rename");
        let published = next_set(&mut subscription)
            .await
            .expect("timeout")
            .expect("set");
        assert_eq!(ids(&published), ["nightfall"]);

        // Removed again, and the set empties without a restart.
        std::fs::remove_file(dir.join("nightfall.json")).expect("remove");
        assert_eq!(
            next_set(&mut subscription).await.expect("timeout"),
            Some(vec![])
        );
    }
}

//! Port of the parts of upstream/apps/server/src/attachmentStore.ts,
//! attachmentPaths.ts and imageMime.ts that the write pipeline uses
//! (attachment ids, claim planning, path resolution, the pending sweep).
//!
//! Not ported: `threadHtmlRenderAttachmentIds` (html_render / MCP apps are
//! cut) and the `Mime.getExtension` registry fallback of `inferImageExtension`
//! (a small table of the extra image types covers the safe-extension list).
//!
//! Port of upstream/apps/server/src/attachmentStore.ts (parts the app needs).

use regex::Regex;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, SystemTime};
use turin_contracts::chat_attachment::ChatAttachment;

const ATTACHMENT_ID_THREAD_SEGMENT_MAX_CHARS: usize = 80;
pub const PENDING_ATTACHMENT_THREAD_SEGMENT: &str = "pending";
const PENDING_ATTACHMENT_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const PARTIAL_UPLOAD_MAX_AGE: Duration = Duration::from_secs(60 * 60);

static ATTACHMENT_ID_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^([a-z0-9_]+(?:-[a-z0-9_]+)*)-([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})(?:-([a-z0-9]{1,10}))?$",
    )
    .expect("attachment id pattern")
});
static EXTENSION_ID_SUFFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9]{1,10}$").expect("extension pattern"));

pub const SAFE_IMAGE_FILE_EXTENSIONS: &[&str] = &[
    ".avif", ".bmp", ".gif", ".heic", ".heif", ".ico", ".jpeg", ".jpg", ".png", ".svg", ".tiff",
    ".webp",
];

fn attachment_filename_extensions() -> impl Iterator<Item = &'static str> {
    SAFE_IMAGE_FILE_EXTENSIONS
        .iter()
        .copied()
        .chain(std::iter::once(".bin"))
}

/// `NodePath.normalize` on a POSIX path, as `normalizeAttachmentRelativePath`
/// uses it, then the leading-slash strip and the `..`/NUL rejection.
pub fn normalize_attachment_relative_path(raw: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    let absolute = raw.starts_with('/');
    for part in raw.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|last| *last != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut normalized = parts.join("/");
    if raw.ends_with('/') && !normalized.is_empty() {
        normalized.push('/');
    }
    let stripped = normalized.trim_start_matches(['/', '\\']);
    if stripped.is_empty() || stripped.starts_with("..") || stripped.contains('\0') {
        return None;
    }
    Some(stripped.replace('\\', "/"))
}

fn lexical_absolute(path: &Path) -> PathBuf {
    let base = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut out = PathBuf::new();
    for component in base.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `resolveAttachmentRelativePath`: the file under `attachments_dir`, or None
/// when the relative path escapes it.
pub fn resolve_attachment_relative_path(
    attachments_dir: &Path,
    relative_path: &str,
) -> Option<PathBuf> {
    let normalized = normalize_attachment_relative_path(relative_path)?;
    let root = lexical_absolute(attachments_dir);
    let file = lexical_absolute(&root.join(normalized));
    (file.starts_with(&root) && file != root).then_some(file)
}

/// `toSafeThreadAttachmentSegment`.
pub fn to_safe_thread_attachment_segment(thread_id: &str) -> Option<String> {
    let lowered = turin_contracts::js_trim(thread_id).to_lowercase();
    let mut replaced = String::with_capacity(lowered.len());
    let mut in_run = false;
    for c in lowered.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            replaced.push(c);
            in_run = false;
        } else if !in_run {
            replaced.push('-');
            in_run = true;
        }
    }
    // collapse repeated dashes
    let mut collapsed = String::with_capacity(replaced.len());
    for c in replaced.chars() {
        if c == '-' && collapsed.ends_with('-') {
            continue;
        }
        collapsed.push(c);
    }
    let trimmed = collapsed.trim_matches(['-', '_']);
    let mut segment: String = trimmed
        .chars()
        .take(ATTACHMENT_ID_THREAD_SEGMENT_MAX_CHARS)
        .collect();
    segment = segment.trim_end_matches(['-', '_']).to_owned();
    if segment.is_empty() {
        None
    } else if segment == PENDING_ATTACHMENT_THREAD_SEGMENT {
        Some("_pending".to_owned())
    } else {
        Some(segment)
    }
}

/// `attachmentFileExtension`: ".part" is reserved for in-flight uploads; a
/// stored "archive.part" would look stale to the pending sweep and get deleted.
pub fn attachment_file_extension(file_name: &str) -> String {
    let extension = Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| format!(".{}", ext.to_lowercase()))
        .unwrap_or_default();
    let valid = extension.len() >= 2
        && extension.len() <= 11
        && extension[1..].chars().all(|c| c.is_ascii_alphanumeric());
    if extension == ".part" || !valid {
        ".bin".to_owned()
    } else {
        extension
    }
}

fn attachment_id_extension_suffix(extension: Option<&str>) -> String {
    let Some(extension) = extension.filter(|e| !e.is_empty()) else {
        return String::new();
    };
    let normalized = extension.trim_start_matches('.').to_lowercase();
    if EXTENSION_ID_SUFFIX.is_match(&normalized) {
        format!("-{normalized}")
    } else {
        "-bin".to_owned()
    }
}

pub fn create_pending_attachment_id(extension: Option<&str>) -> String {
    format!(
        "{PENDING_ATTACHMENT_THREAD_SEGMENT}-{}{}",
        uuid::Uuid::new_v4(),
        attachment_id_extension_suffix(extension)
    )
}

fn plain_attachment_id(attachment_id: &str) -> Option<String> {
    let normalized = normalize_attachment_relative_path(attachment_id)?;
    (!normalized.contains('/') && !normalized.contains('.')).then_some(normalized)
}

pub fn parse_attachment_uuid(attachment_id: &str) -> Option<String> {
    let normalized = plain_attachment_id(attachment_id)?;
    ATTACHMENT_ID_PATTERN
        .captures(&normalized)
        .and_then(|c| c.get(2))
        .map(|m| m.as_str().to_lowercase())
}

pub fn parse_attachment_file_extension(attachment_id: &str) -> Option<String> {
    let normalized = plain_attachment_id(attachment_id)?;
    ATTACHMENT_ID_PATTERN
        .captures(&normalized)
        .and_then(|c| c.get(3))
        .map(|m| m.as_str().to_lowercase())
}

pub fn parse_thread_segment_from_attachment_id(attachment_id: &str) -> Option<String> {
    let normalized = plain_attachment_id(attachment_id)?;
    ATTACHMENT_ID_PATTERN
        .captures(&normalized)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
}

pub fn create_attachment_id(thread_id: &str, extension: Option<&str>) -> Option<String> {
    let segment = to_safe_thread_attachment_segment(thread_id)?;
    Some(format!(
        "{segment}-{}{}",
        uuid::Uuid::new_v4(),
        attachment_id_extension_suffix(extension)
    ))
}

/// `createDeterministicAttachmentId`: the same thread and key always map to
/// the same id.
pub fn create_deterministic_attachment_id(thread_id: &str, stable_key: &str) -> Option<String> {
    let segment = to_safe_thread_attachment_segment(thread_id)?;
    let digest = Sha256::digest(
        serde_json::to_string(&[thread_id, stable_key])
            .unwrap_or_default()
            .as_bytes(),
    );
    let hash: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let hash = &hash[..32];
    Some(format!(
        "{segment}-{}-{}-{}-{}-{}",
        &hash[..8],
        &hash[8..12],
        &hash[12..16],
        &hash[16..20],
        &hash[20..]
    ))
}

/// Stable key for an inline chat attachment. A retry resends the same bytes
/// and keeps its id. Editing a queued message reuses the message id, so the
/// bytes also stop a new image from taking an earlier image's id and file.
pub fn chat_attachment_stable_key(message_id: &str, index: usize, bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("{message_id}:{index}:{hex}")
}

const IMAGE_EXTENSION_BY_MIME_TYPE: &[(&str, &str)] = &[
    ("image/avif", ".avif"),
    ("image/bmp", ".bmp"),
    ("image/gif", ".gif"),
    ("image/heic", ".heic"),
    ("image/heif", ".heif"),
    ("image/jpeg", ".jpg"),
    ("image/jpg", ".jpg"),
    ("image/png", ".png"),
    ("image/svg+xml", ".svg"),
    ("image/tiff", ".tiff"),
    ("image/webp", ".webp"),
    // Registry entries that land on the safe list.
    ("image/x-icon", ".ico"),
    ("image/vnd.microsoft.icon", ".ico"),
];

/// `inferImageExtension`.
pub fn infer_image_extension(mime_type: &str, file_name: Option<&str>) -> String {
    let key = mime_type.to_lowercase();
    if let Some((_, extension)) = IMAGE_EXTENSION_BY_MIME_TYPE.iter().find(|(m, _)| *m == key) {
        return (*extension).to_owned();
    }
    let name = turin_contracts::js_trim(file_name.unwrap_or(""));
    if let Some(dot) = name.rfind('.') {
        let ext = &name[dot + 1..];
        if (1..=8).contains(&ext.len()) && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
            let dotted = format!(".{}", ext.to_lowercase());
            if SAFE_IMAGE_FILE_EXTENSIONS.contains(&dotted.as_str()) {
                return dotted;
            }
        }
    }
    ".bin".to_owned()
}

/// Accessors for the shared fields of `ChatAttachment` members.
pub struct AttachmentFields<'a> {
    pub id: &'a str,
    pub name: &'a str,
    pub mime_type: &'a str,
    pub size_bytes: u64,
    pub is_image: bool,
}

pub fn attachment_fields(attachment: &ChatAttachment) -> AttachmentFields<'_> {
    match attachment {
        ChatAttachment::Image(a) => AttachmentFields {
            id: a.id.as_str(),
            name: a.name.as_str(),
            mime_type: a.mime_type.as_str(),
            size_bytes: a.size_bytes,
            is_image: true,
        },
        ChatAttachment::File(a) => AttachmentFields {
            id: a.id.as_str(),
            name: a.name.as_str(),
            mime_type: a.mime_type.as_str(),
            size_bytes: a.size_bytes,
            is_image: false,
        },
        ChatAttachment::Unknown(a) => AttachmentFields {
            id: a.id.as_str(),
            name: a.name.as_str(),
            mime_type: a.mime_type.as_str(),
            size_bytes: a.size_bytes,
            is_image: false,
        },
    }
}

/// `attachmentRelativePath`: None for attachment types this build does not
/// know; callers skip those.
pub fn attachment_relative_path(attachment: &ChatAttachment) -> Option<String> {
    match attachment {
        ChatAttachment::Image(a) => Some(format!(
            "{}{}",
            a.id,
            infer_image_extension(a.mime_type.as_str(), Some(a.name.as_str()))
        )),
        ChatAttachment::File(a) => Some(format!(
            "{}{}",
            a.id,
            attachment_file_extension(a.name.as_str())
        )),
        ChatAttachment::Unknown(_) => None,
    }
}

pub fn resolve_attachment_path(
    attachments_dir: &Path,
    attachment: &ChatAttachment,
) -> Option<PathBuf> {
    resolve_attachment_relative_path(attachments_dir, &attachment_relative_path(attachment)?)
}

/// `resolveAttachmentPathById`: the stored file for an id, if one exists.
pub fn resolve_attachment_path_by_id(
    attachments_dir: &Path,
    attachment_id: &str,
) -> Option<PathBuf> {
    let normalized = plain_attachment_id(attachment_id)?;
    if let Some(extension) = parse_attachment_file_extension(&normalized) {
        let path = resolve_attachment_relative_path(
            attachments_dir,
            &format!("{normalized}.{}", extension.to_lowercase()),
        )?;
        return path.exists().then_some(path);
    }
    attachment_filename_extensions().find_map(|extension| {
        resolve_attachment_relative_path(attachments_dir, &format!("{normalized}{extension}"))
            .filter(|path| path.exists())
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentClaimPlan {
    Ok {
        final_id: String,
        current_path: PathBuf,
        final_path: PathBuf,
    },
    Rejected {
        reason: &'static str,
    },
}

pub fn plan_attachment_claim(
    attachments_dir: &Path,
    thread_id: &str,
    attachment_id: &str,
) -> AttachmentClaimPlan {
    let reject = |reason| AttachmentClaimPlan::Rejected { reason };
    let (Some(_uuid), Some(requested_segment)) = (
        parse_attachment_uuid(attachment_id),
        parse_thread_segment_from_attachment_id(attachment_id),
    ) else {
        return reject("invalid attachment id");
    };
    if to_safe_thread_attachment_segment(thread_id).is_none() {
        return reject("invalid thread id");
    }
    if requested_segment != PENDING_ATTACHMENT_THREAD_SEGMENT {
        return reject("attachment must be a pending upload");
    }
    let Some(current_path) = resolve_attachment_path_by_id(attachments_dir, attachment_id) else {
        return reject("attachment not found (removed or expired)");
    };
    let file_extension = parse_attachment_file_extension(attachment_id);
    let Some(final_id) = create_attachment_id(thread_id, file_extension.as_deref()) else {
        return reject("failed to create attachment id");
    };
    let extension = current_path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let Some(final_path) =
        resolve_attachment_relative_path(attachments_dir, &format!("{final_id}{extension}"))
    else {
        return reject("failed to resolve attachment path");
    };
    AttachmentClaimPlan::Ok {
        final_id,
        current_path,
        final_path,
    }
}

fn attachment_id_from_relative_path(relative_path: &str) -> Option<String> {
    let normalized = normalize_attachment_relative_path(relative_path)?;
    if normalized.contains('/') {
        return None;
    }
    let dot = normalized.rfind('.')?;
    if dot == 0 {
        return None;
    }
    let id = &normalized[..dot];
    (!id.is_empty() && !id.contains('.')).then(|| id.to_owned())
}

/// Deletes pending uploads (24h) and partial uploads (1h) older than their
/// limits. Returns how many were deleted.
pub fn sweep_stale_pending_attachments(attachments_dir: &Path, now: SystemTime) -> usize {
    let Ok(entries) = std::fs::read_dir(attachments_dir) else {
        return 0;
    };
    let mut deleted = 0;
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        let is_partial = name.ends_with(".part");
        if !is_partial {
            let Some(id) = attachment_id_from_relative_path(&name) else {
                continue;
            };
            if parse_thread_segment_from_attachment_id(&id).as_deref()
                != Some(PENDING_ATTACHMENT_THREAD_SEGMENT)
            {
                continue;
            }
        }
        let Some(path) = resolve_attachment_relative_path(attachments_dir, &name) else {
            continue;
        };
        let max_age = if is_partial {
            PARTIAL_UPLOAD_MAX_AGE
        } else {
            PENDING_ATTACHMENT_MAX_AGE
        };
        let Ok(modified) = std::fs::metadata(&path).and_then(|m| m.modified()) else {
            continue;
        };
        if now.duration_since(modified).is_ok_and(|age| age > max_age)
            && std::fs::remove_file(&path).is_ok()
        {
            deleted += 1;
        }
    }
    deleted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_segments_are_slugged_and_pending_is_reserved() {
        assert_eq!(
            to_safe_thread_attachment_segment("  Thread:Foo/Bar!! ").as_deref(),
            Some("thread-foo-bar")
        );
        assert_eq!(
            to_safe_thread_attachment_segment("pending").as_deref(),
            Some("_pending")
        );
        assert_eq!(to_safe_thread_attachment_segment("::: "), None);
        let long = "a".repeat(200);
        assert_eq!(to_safe_thread_attachment_segment(&long).unwrap().len(), 80);
    }

    #[test]
    fn ids_round_trip_through_their_parsers() {
        let id = create_attachment_id("thread-claims-1", Some(".PNG")).unwrap();
        assert_eq!(
            parse_thread_segment_from_attachment_id(&id).as_deref(),
            Some("thread-claims-1")
        );
        assert_eq!(parse_attachment_file_extension(&id).as_deref(), Some("png"));
        assert!(parse_attachment_uuid(&id).is_some());
        let pending = create_pending_attachment_id(None);
        assert_eq!(
            parse_thread_segment_from_attachment_id(&pending).as_deref(),
            Some("pending")
        );
        assert_eq!(parse_attachment_file_extension(&pending), None);
        // Ids that are paths or have dots never parse.
        assert_eq!(parse_attachment_uuid("../pending-x"), None);
        assert_eq!(parse_thread_segment_from_attachment_id("a.b"), None);
        assert_eq!(parse_attachment_uuid("pending-not-a-uuid"), None);
        // An extension that is not alphanumeric falls back to bin.
        assert!(create_pending_attachment_id(Some(".t@r")).ends_with("-bin"));
    }

    #[test]
    fn deterministic_ids_depend_only_on_thread_and_key() {
        let a = create_deterministic_attachment_id("thread-1", "message:0:abc").unwrap();
        assert_eq!(
            a,
            create_deterministic_attachment_id("thread-1", "message:0:abc").unwrap()
        );
        assert_ne!(
            a,
            create_deterministic_attachment_id("thread-1", "message:1:abc").unwrap()
        );
        assert_eq!(
            parse_thread_segment_from_attachment_id(&a).as_deref(),
            Some("thread-1")
        );
        assert_eq!(
            chat_attachment_stable_key("m", 2, b"abc"),
            "m:2:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn relative_paths_cannot_escape_the_attachments_dir() {
        let dir = Path::new("/data/attachments");
        assert_eq!(
            resolve_attachment_relative_path(dir, "a/b.png"),
            Some(PathBuf::from("/data/attachments/a/b.png"))
        );
        assert_eq!(resolve_attachment_relative_path(dir, "../etc/passwd"), None);
        assert_eq!(resolve_attachment_relative_path(dir, "a/../../x"), None);
        assert_eq!(resolve_attachment_relative_path(dir, ""), None);
        assert_eq!(resolve_attachment_relative_path(dir, "bad\0name"), None);
        assert_eq!(
            normalize_attachment_relative_path("/abs\\path.png").as_deref(),
            Some("abs/path.png")
        );
    }

    #[test]
    fn extensions_follow_mime_then_name_then_bin() {
        assert_eq!(infer_image_extension("IMAGE/JPEG", None), ".jpg");
        assert_eq!(
            infer_image_extension("image/x-custom", Some("Photo.WEBP")),
            ".webp"
        );
        assert_eq!(
            infer_image_extension("image/x-custom", Some("photo.exe")),
            ".bin"
        );
        assert_eq!(attachment_file_extension("notes.TXT"), ".txt");
        assert_eq!(attachment_file_extension("archive.part"), ".bin");
        assert_eq!(attachment_file_extension("noext"), ".bin");
        assert_eq!(attachment_file_extension("x.toolongextension"), ".bin");
    }

    #[test]
    fn claim_planning_rejects_non_pending_and_missing_uploads() {
        let dir = tempfile::tempdir().unwrap();
        let pending = create_pending_attachment_id(None);
        assert_eq!(
            plan_attachment_claim(dir.path(), "thread-1", &pending),
            AttachmentClaimPlan::Rejected {
                reason: "attachment not found (removed or expired)"
            }
        );
        assert_eq!(
            plan_attachment_claim(dir.path(), "thread-1", "not-an-id"),
            AttachmentClaimPlan::Rejected {
                reason: "invalid attachment id"
            }
        );
        let owned = create_attachment_id("thread-1", None).unwrap();
        assert_eq!(
            plan_attachment_claim(dir.path(), "thread-1", &owned),
            AttachmentClaimPlan::Rejected {
                reason: "attachment must be a pending upload"
            }
        );
        assert_eq!(
            plan_attachment_claim(dir.path(), "!!!", &pending),
            AttachmentClaimPlan::Rejected {
                reason: "invalid thread id"
            }
        );
        std::fs::write(dir.path().join(format!("{pending}.png")), [1, 2]).unwrap();
        let AttachmentClaimPlan::Ok {
            final_id,
            current_path,
            final_path,
        } = plan_attachment_claim(dir.path(), "thread-1", &pending)
        else {
            panic!("expected a plan");
        };
        assert!(final_id.starts_with("thread-1-"));
        assert_eq!(current_path.extension().unwrap(), "png");
        assert_eq!(final_path.extension().unwrap(), "png");
    }

    #[test]
    fn the_sweep_removes_only_stale_pending_and_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let pending = format!("{}.png", create_pending_attachment_id(None));
        let owned = format!("{}.png", create_attachment_id("thread-1", None).unwrap());
        for name in [pending.as_str(), owned.as_str(), "upload.part", "notes.txt"] {
            std::fs::write(dir.path().join(name), [0]).unwrap();
        }
        // Nothing is stale yet.
        assert_eq!(
            sweep_stale_pending_attachments(dir.path(), SystemTime::now()),
            0
        );
        let two_days = SystemTime::now() + Duration::from_secs(2 * 24 * 60 * 60);
        assert_eq!(sweep_stale_pending_attachments(dir.path(), two_days), 2);
        assert!(!dir.path().join(&pending).exists());
        assert!(!dir.path().join("upload.part").exists());
        assert!(dir.path().join(&owned).exists());
        assert!(dir.path().join("notes.txt").exists());
        // A one-hour-old partial goes before a pending upload does.
        std::fs::write(dir.path().join("fresh.part"), [0]).unwrap();
        std::fs::write(dir.path().join(&pending), [0]).unwrap();
        let ninety_minutes = SystemTime::now() + Duration::from_secs(90 * 60);
        assert_eq!(
            sweep_stale_pending_attachments(dir.path(), ninety_minutes),
            1
        );
        assert!(dir.path().join(&pending).exists());
    }
}

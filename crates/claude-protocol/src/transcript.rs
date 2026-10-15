//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.mjs local session helpers.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use tokio::{fs, io::AsyncWriteExt};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Clone, Default)]
pub struct TranscriptOptions {
    pub dir: Option<PathBuf>,
    pub config_dir: Option<PathBuf>,
    pub limit: Option<usize>,
    pub offset: usize,
    pub include_system_messages: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub session_id: String,
    pub summary: String,
    pub last_modified: u64,
    pub file_size: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub custom_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<u64>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage {
    #[serde(rename = "type")]
    pub kind: String,
    pub uuid: String,
    pub session_id: String,
    pub message: Value,
    pub parent_tool_use_id: Option<String>,
    pub parent_agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_meta: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_compact_summary: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_queued_command: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin: Option<Value>,
}
fn config_dir(options: &TranscriptOptions) -> PathBuf {
    options
        .config_dir
        .clone()
        .or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude")
        })
}
pub fn project_key(dir: &Path) -> String {
    encode_project_path(&absolute_source(dir))
}
fn absolute_source(dir: &Path) -> String {
    let absolute = std::fs::canonicalize(dir).unwrap_or_else(|_| {
        if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(dir)
        }
    });
    let source = absolute.to_string_lossy().into_owned();
    // SDK `it`: darwin paths are NFC-normalized before encoding.
    #[cfg(target_os = "macos")]
    {
        return source.nfc().collect();
    }
    #[cfg(not(target_os = "macos"))]
    source
}

/// `fm`: non-ASCII-alphanumerics become `-` per UTF-16 code unit. Paths longer
/// than 200 units keep that prefix plus `abs(javaStringHash).toString(36)`.
fn dash_encode(source: &str) -> String {
    source
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(ch) if ch.is_ascii_alphanumeric() => ch,
            _ => '-',
        })
        .collect()
}
pub fn encode_project_path(source: &str) -> String {
    let key = dash_encode(source);
    if key.encode_utf16().count() <= 200 {
        return key;
    }
    let hash = js_string_hash(source).unsigned_abs();
    format!("{}-{}", utf16_prefix(&key, 200), to_base36(hash))
}

fn js_string_hash(source: &str) -> i32 {
    let mut hash = 0_i32;
    for unit in source.encode_utf16() {
        hash = hash
            .wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit));
    }
    hash
}

fn utf16_prefix(value: &str, units: usize) -> String {
    let encoded: Vec<u16> = value.encode_utf16().take(units).collect();
    String::from_utf16_lossy(&encoded)
}

fn to_base36(mut value: u32) -> String {
    if value == 0 {
        return "0".into();
    }
    let mut suffix = String::new();
    while value > 0 {
        let digit = value % 36;
        suffix.push(char::from_digit(digit, 36).unwrap());
        value /= 36;
    }
    suffix.chars().rev().collect()
}
fn projects_dir(options: &TranscriptOptions) -> PathBuf {
    config_dir(options).join("projects")
}
const CHUNK_BYTES: usize = 65536;
fn project_dir_override() -> Option<String> {
    if std::env::var_os("CLAUDE_CONFIG_DIR").is_none() {
        return None;
    }
    valid_project_dir_name(std::env::var("CLAUDE_CODE_PROJECT_DIR_NAME").ok()?)
}
fn valid_project_dir_name(name: String) -> Option<String> {
    if !(1..=64).contains(&name.len())
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-')
    {
        return None;
    }
    let lower = name.to_ascii_lowercase();
    let reserved = matches!(lower.as_str(), "con" | "prn" | "aux" | "nul")
        || (lower.len() == 4
            && (lower.starts_with("com") || lower.starts_with("lpt"))
            && lower.as_bytes()[3].is_ascii_digit());
    if reserved { None } else { Some(name) }
}
async fn nonempty_transcript(dir: &Path, id: &str) -> Option<PathBuf> {
    let path = dir.join(format!("{id}.jsonl"));
    let meta = fs::metadata(&path).await.ok()?;
    (meta.len() > 0).then_some(path)
}
async fn candidate_project_dirs(
    root: &Path,
    source: &str,
    override_name: Option<&str>,
) -> Vec<PathBuf> {
    let encoded = encode_project_path(source);
    let mut dirs = Vec::new();
    if let Some(name) = override_name {
        dirs.push(root.join(name));
        if encoded != dirs[0].file_name().unwrap().to_string_lossy() {
            dirs.push(root.join(&encoded));
        }
        return dirs;
    }
    dirs.push(root.join(&encoded));
    let dashed = dash_encode(source);
    if dashed.encode_utf16().count() <= 200 {
        return dirs;
    }
    let prefix = format!("{}-", utf16_prefix(&dashed, 200));
    let mut entries = match fs::read_dir(root).await {
        Ok(entries) => entries,
        Err(_) => return dirs,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.starts_with(&prefix) || name == encoded {
            continue;
        }
        let path = entry.path();
        if entry
            .file_type()
            .await
            .map(|kind| kind.is_dir())
            .unwrap_or(false)
            && cwd_matches(&path, source).await
        {
            dirs.push(path);
        }
    }
    dirs
}
async fn cwd_matches(project_dir: &Path, source: &str) -> bool {
    let expected = dash_encode(source);
    let mut entries = match fs::read_dir(project_dir).await {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        let Some((head, tail)) = head_tail(&path).await else {
            continue;
        };
        let found = last_relocated_cwd(&tail).or_else(|| first_field(&head, "cwd"));
        if found.is_some_and(|cwd| dash_encode(&normalize_text(&cwd)) == expected) {
            return true;
        }
    }
    false
}
fn normalize_text(value: &str) -> String {
    #[cfg(target_os = "macos")]
    {
        return value.nfc().collect();
    }
    #[cfg(not(target_os = "macos"))]
    value.to_owned()
}
async fn head_tail(path: &Path) -> Option<(String, String)> {
    let bytes = fs::read(path).await.ok()?;
    Some(windows(&bytes))
}
fn windows(bytes: &[u8]) -> (String, String) {
    let head_end = bytes.len().min(CHUNK_BYTES);
    let tail_start = bytes.len().saturating_sub(CHUNK_BYTES);
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let tail = String::from_utf8_lossy(&bytes[tail_start..]).into_owned();
    (head, tail)
}
/// SDK `fn`: the textually last `"key":"value"` in a raw chunk, without parsing lines.
fn last_string_field(text: &str, key: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut best: Option<(usize, &str)> = None;
    for pattern in [format!("\"{key}\":\""), format!("\"{key}\": \"")] {
        let mut from = 0;
        while let Some(found) = text.get(from..).and_then(|rest| rest.find(&pattern)) {
            let at = from + found;
            let start = at + pattern.len();
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'"' {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            if end >= bytes.len() {
                break;
            }
            if best.is_none_or(|(position, _)| at > position) {
                best = Some((at, &text[start..end]));
            }
            from = end + 1;
        }
    }
    best.map(|(_, raw)| {
        if raw.contains('\\') {
            serde_json::from_str(&format!("\"{raw}\"")).unwrap_or_else(|_| raw.to_owned())
        } else {
            raw.to_owned()
        }
    })
}
fn is_control_or_format(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}
/// SDK `YZ`: runs of control/format characters become one space, then a 200 code point cap.
fn sanitize_title(title: &str) -> String {
    let mut spaced = String::new();
    let mut in_run = false;
    for c in title.trim().chars() {
        if is_control_or_format(c) {
            if !in_run {
                spaced.push(' ');
            }
            in_run = true;
        } else {
            spaced.push(c);
            in_run = false;
        }
    }
    spaced
        .chars()
        .take(200)
        .collect::<String>()
        .trim()
        .to_owned()
}
async fn sidecar_title(transcript: &Path, id: &str) -> Option<String> {
    let path = transcript.parent()?.join(id).join("custom-title.json");
    let text = fs::read_to_string(path).await.ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let title = sanitize_title(value.get("customTitle")?.as_str()?);
    (!title.is_empty()).then_some(title)
}
fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|text| !text.is_empty())
}
/// SDK `Nu`/`RWe` title chain. `RWe` stops at an empty tail `customTitle`; `Nu` keeps looking.
fn window_title(
    head: &str,
    tail: &str,
    sidecar: Option<String>,
    empty_tail_custom_stops: bool,
) -> Option<String> {
    let custom = match last_string_field(tail, "customTitle") {
        Some(title) if !title.is_empty() || empty_tail_custom_stops => nonempty(Some(title)),
        Some(_) => nonempty(last_string_field(head, "customTitle")),
        None => nonempty(sidecar).or_else(|| nonempty(last_string_field(head, "customTitle"))),
    };
    custom
        .or_else(|| nonempty(last_string_field(tail, "aiTitle")))
        .or_else(|| nonempty(last_string_field(head, "aiTitle")))
}
fn first_field(chunk: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    for line in chunk.lines() {
        if !line.contains(&needle) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(text) = value.get(key).and_then(Value::as_str) {
            return Some(text.to_owned());
        }
    }
    None
}
fn last_relocated_cwd(chunk: &str) -> Option<String> {
    let mut found = None;
    for line in chunk.lines() {
        if !line.contains("\"relocatedCwd\"") {
            continue;
        }
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) == Some("relocated")
            && let Some(text) = value.get("relocatedCwd").and_then(Value::as_str)
        {
            found = Some(text.to_owned());
        }
    }
    found
}
async fn git_worktrees(dir: &Path) -> Vec<PathBuf> {
    let output = tokio::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=",
            "worktree",
            "list",
            "--porcelain",
        ])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    let Ok(Ok(output)) = tokio::time::timeout(std::time::Duration::from_secs(5), output).await
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(|path| PathBuf::from(normalize_text(path)))
        .collect()
}
async fn session_path(id: &str, options: &TranscriptOptions) -> Option<PathBuf> {
    session_path_with_override(id, options, project_dir_override().as_deref()).await
}
async fn session_path_with_override(
    id: &str,
    options: &TranscriptOptions,
    override_name: Option<&str>,
) -> Option<PathBuf> {
    if uuid::Uuid::parse_str(id).is_err() {
        return None;
    }
    let root = projects_dir(options);
    let Some(dir) = &options.dir else {
        let mut folders = fs::read_dir(&root).await.ok()?;
        while let Ok(Some(folder)) = folders.next_entry().await {
            if let Some(path) = nonempty_transcript(&folder.path(), id).await {
                return Some(path);
            }
        }
        return None;
    };
    let source = absolute_source(dir);
    for candidate in candidate_project_dirs(&root, &source, override_name).await {
        if let Some(path) = nonempty_transcript(&candidate, id).await {
            return Some(path);
        }
    }
    for worktree in git_worktrees(Path::new(&source)).await {
        let worktree_source = absolute_source(&worktree);
        if worktree_source == source {
            continue;
        }
        for candidate in candidate_project_dirs(&root, &worktree_source, override_name).await {
            if let Some(path) = nonempty_transcript(&candidate, id).await {
                return Some(path);
            }
        }
    }
    None
}
async fn read_entries(path: &Path) -> Result<Vec<Value>> {
    Ok(parse_entries(&fs::read_to_string(path).await?))
}
fn parse_entries(data: &str) -> Vec<Value> {
    data.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .collect()
}
fn entry_id(e: &Value) -> Option<&str> {
    e.get("uuid")?.as_str()
}
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Number(number)) => number.as_f64().unwrap_or(0.0) != 0.0,
        Some(_) => true,
    }
}
fn parent_uuid(entry: &Value) -> Option<&str> {
    entry
        .get("parentUuid")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
}
fn set_parent(by_id: &mut HashMap<String, Value>, id: &str, parent: Value) {
    let Some(entry) = by_id.get_mut(id) else {
        return;
    };
    if let Some(object) = entry.as_object_mut() {
        object.insert("parentUuid".into(), parent);
    }
}
fn parents_equal(entry: &Value, anchor: &Value) -> bool {
    match entry.get("parentUuid") {
        Some(parent) => parent == anchor,
        None => false,
    }
}
fn compact_metadata(entry: &Value) -> Option<&Value> {
    entry
        .get("compactMetadata")
        .or_else(|| entry.get("compact_metadata"))
}
fn apply_compacts(by_id: &mut HashMap<String, Value>, order: &[String]) {
    for id in order {
        let Some(entry) = by_id.get(id).cloned() else {
            continue;
        };
        if entry.get("type").and_then(Value::as_str) != Some("system")
            || entry.get("subtype").and_then(Value::as_str) != Some("compact_boundary")
        {
            continue;
        }
        let Some(metadata) = compact_metadata(&entry) else {
            continue;
        };
        if let Some(preserved) = metadata
            .get("preservedMessages")
            .or_else(|| metadata.get("preserved_messages"))
        {
            let uuids: Vec<String> = preserved
                .get("uuids")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if uuids.is_empty() || uuids.iter().any(|uuid| !by_id.contains_key(uuid)) {
                continue;
            }
            let anchor = preserved
                .get("anchorUuid")
                .or_else(|| preserved.get("anchor_uuid"))
                .cloned()
                .unwrap_or(Value::Null);
            let mut previous = anchor.clone();
            for uuid in &uuids {
                set_parent(by_id, uuid, previous);
                previous = Value::String(uuid.clone());
            }
            let first = uuids[0].clone();
            let last = uuids[uuids.len() - 1].clone();
            let rewired: Vec<String> = by_id
                .iter()
                .filter(|(uuid, entry)| *uuid != &first && parents_equal(entry, &anchor))
                .map(|(uuid, _)| uuid.clone())
                .collect();
            for uuid in rewired {
                set_parent(by_id, &uuid, Value::String(last.clone()));
            }
        } else if let Some(segment) = metadata
            .get("preservedSegment")
            .or_else(|| metadata.get("preserved_segment"))
        {
            let head = segment
                .get("headUuid")
                .or_else(|| segment.get("head_uuid"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let tail = segment
                .get("tailUuid")
                .or_else(|| segment.get("tail_uuid"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let anchor = segment
                .get("anchorUuid")
                .or_else(|| segment.get("anchor_uuid"))
                .cloned()
                .unwrap_or(Value::Null);
            if by_id.contains_key(&head) {
                set_parent(by_id, &head, anchor.clone());
            }
            let rewired: Vec<String> = by_id
                .iter()
                .filter(|(uuid, entry)| *uuid != &head && parents_equal(entry, &anchor))
                .map(|(uuid, _)| uuid.clone())
                .collect();
            for uuid in rewired {
                set_parent(by_id, &uuid, Value::String(tail.clone()));
            }
        }
    }
}
fn assistant_message_id(entry: &Value) -> Option<String> {
    if entry.get("type").and_then(Value::as_str) != Some("assistant") {
        return None;
    }
    entry
        .pointer("/message/id")
        .and_then(Value::as_str)
        .map(str::to_owned)
}
fn is_tool_result(entry: &Value) -> bool {
    parent_uuid(entry).is_some()
        && entry.get("type").and_then(Value::as_str) == Some("user")
        && entry
            .pointer("/message/content")
            .and_then(Value::as_array)
            .is_some_and(|blocks| {
                blocks
                    .iter()
                    .any(|block| block.get("type").and_then(Value::as_str) == Some("tool_result"))
            })
}
fn insert_siblings(
    by_id: &HashMap<String, Value>,
    order: &[String],
    chain: Vec<Value>,
) -> Vec<Value> {
    let assistants: Vec<Value> = chain
        .iter()
        .filter(|entry| entry.get("type").and_then(Value::as_str) == Some("assistant"))
        .cloned()
        .collect();
    if assistants.is_empty() {
        return chain;
    }
    let mut anchor_for = HashMap::new();
    for entry in &assistants {
        if let Some(id) = assistant_message_id(entry) {
            anchor_for.insert(id, entry_id(entry).unwrap_or("").to_owned());
        }
    }
    let mut on_chain: HashSet<String> = chain
        .iter()
        .filter_map(|entry| entry_id(entry).map(str::to_owned))
        .collect();
    let mut grouped: HashMap<String, Vec<Value>> = HashMap::new();
    let mut children: HashMap<String, Vec<Value>> = HashMap::new();
    for id in order {
        let Some(entry) = by_id.get(id) else { continue };
        if let Some(message_id) = assistant_message_id(entry) {
            grouped.entry(message_id).or_default().push(entry.clone());
        } else if is_tool_result(entry)
            && let Some(parent) = parent_uuid(entry)
        {
            children
                .entry(parent.to_owned())
                .or_default()
                .push(entry.clone());
        }
    }
    let mut extras: HashMap<String, Vec<Value>> = HashMap::new();
    let mut seen_messages = HashSet::new();
    for entry in &assistants {
        let Some(message_id) = assistant_message_id(entry) else {
            continue;
        };
        if !seen_messages.insert(message_id.clone()) {
            continue;
        }
        let group = grouped
            .get(&message_id)
            .cloned()
            .unwrap_or_else(|| vec![entry.clone()]);
        let mut missing: Vec<Value> = group
            .iter()
            .filter(|item| entry_id(item).is_none_or(|id| !on_chain.contains(id)))
            .cloned()
            .collect();
        let mut tool_results = Vec::new();
        for item in &group {
            let Some(id) = entry_id(item) else { continue };
            if let Some(found) = children.get(id) {
                tool_results.extend(
                    found
                        .iter()
                        .filter(|child| entry_id(child).is_none_or(|id| !on_chain.contains(id)))
                        .cloned(),
                );
            }
        }
        if missing.is_empty() && tool_results.is_empty() {
            continue;
        }
        let stamp = |item: &Value| {
            item.get("timestamp")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned()
        };
        missing.sort_by_key(|left| stamp(left));
        tool_results.sort_by_key(|left| stamp(left));
        let mut extra = missing;
        extra.extend(tool_results);
        for item in &extra {
            if let Some(id) = entry_id(item) {
                on_chain.insert(id.to_owned());
            }
        }
        if let Some(anchor) = anchor_for.get(&message_id) {
            extras.insert(anchor.clone(), extra);
        }
    }
    if extras.is_empty() {
        return chain;
    }
    let mut rewritten = Vec::new();
    for entry in chain {
        let id = entry_id(&entry).map(str::to_owned);
        rewritten.push(entry);
        if let Some(id) = id
            && let Some(extra) = extras.remove(&id)
        {
            rewritten.extend(extra);
        }
    }
    rewritten
}
/// SDK `fI`: compact-boundary parent rewrites, then the latest main-chain leaf, then `LGe`.
fn session_chain(entries: &[Value]) -> Vec<Value> {
    let mut by_id = HashMap::new();
    let mut order = Vec::new();
    let mut index = HashMap::new();
    for (position, entry) in entries.iter().enumerate() {
        let Some(id) = entry_id(entry) else { continue };
        if !by_id.contains_key(id) {
            order.push(id.to_owned());
        }
        by_id.insert(id.to_owned(), entry.clone());
        index.insert(id.to_owned(), position);
    }
    apply_compacts(&mut by_id, &order);
    let parents: HashSet<String> = by_id
        .values()
        .filter_map(|entry| parent_uuid(entry).map(str::to_owned))
        .collect();
    let leaves: Vec<Value> = order
        .iter()
        .filter_map(|id| {
            by_id
                .get(id)
                .filter(|entry| entry_id(entry).is_some_and(|uuid| !parents.contains(uuid)))
                .cloned()
        })
        .collect();
    let mut candidates = Vec::new();
    for leaf in leaves {
        let mut current = Some(leaf);
        let mut seen = HashSet::new();
        while let Some(entry) = current {
            let Some(id) = entry_id(&entry).map(str::to_owned) else {
                break;
            };
            if !seen.insert(id) {
                break;
            }
            if matches!(
                entry.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            ) {
                candidates.push(entry);
                break;
            }
            current = parent_uuid(&entry).and_then(|parent| by_id.get(parent).cloned());
        }
    }
    if candidates.is_empty() {
        return Vec::new();
    }
    let main: Vec<Value> = candidates
        .iter()
        .filter(|entry| {
            !truthy(entry.get("isSidechain"))
                && !truthy(entry.get("teamName"))
                && !truthy(entry.get("isMeta"))
        })
        .cloned()
        .collect();
    let pool = if main.is_empty() { &candidates } else { &main };
    let leaf = pool
        .iter()
        .max_by_key(|entry| {
            entry_id(entry)
                .and_then(|id| index.get(id))
                .copied()
                .unwrap_or(0)
        })
        .unwrap()
        .clone();
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut current = Some(leaf);
    while let Some(entry) = current {
        let Some(id) = entry_id(&entry).map(str::to_owned) else {
            break;
        };
        if !seen.insert(id) {
            break;
        }
        let parent = parent_uuid(&entry).and_then(|id| by_id.get(id).cloned());
        chain.push(entry);
        current = parent;
    }
    chain.reverse();
    insert_siblings(&by_id, &order, chain)
}
fn queued_command_source(entry: &Value) -> Option<&str> {
    if entry.get("type").and_then(Value::as_str) != Some("attachment") {
        return None;
    }
    let attachment = entry.get("attachment")?;
    if attachment.get("type").and_then(Value::as_str) != Some("queued_command") {
        return None;
    }
    attachment
        .get("source_uuid")
        .and_then(Value::as_str)
        .filter(|source| !source.is_empty())
}
/// SDK `bI`: an exact uuid wins anywhere in the list before any queued-command source match.
fn message_index(entries: &[Value], target: &str) -> Option<usize> {
    entries
        .iter()
        .position(|entry| entry_id(entry) == Some(target))
        .or_else(|| {
            entries
                .iter()
                .position(|entry| queued_command_source(entry) == Some(target))
        })
}
fn is_main_turn(entry: &Value) -> bool {
    matches!(
        entry.get("type").and_then(Value::as_str),
        Some("user" | "assistant")
    ) && !truthy(entry.get("isMeta"))
        && !truthy(entry.get("teamName"))
}
/// SDK `OWe`: drop a dangling tail that is not the main chain when the cut does not end on one.
fn drop_dangling(
    entries: Vec<Value>,
    chain: &[Value],
    index: &HashMap<String, usize>,
) -> Vec<Value> {
    let Some(last) = entries.last() else {
        return entries;
    };
    let Some(origin) = chain
        .first()
        .and_then(|entry| entry_id(entry))
        .and_then(|id| index.get(id).copied())
    else {
        return entries;
    };
    if is_main_turn(last) {
        return entries;
    }
    let by_uuid: HashMap<String, Value> = entries
        .iter()
        .filter_map(|entry| Some((entry_id(entry)?.to_owned(), entry.clone())))
        .collect();
    let mut kept: HashSet<String> = chain
        .iter()
        .filter_map(|entry| entry_id(entry).map(str::to_owned))
        .collect();
    let mut cursor = Some(last.clone());
    while let Some(entry) = cursor {
        let Some(id) = entry_id(&entry).map(str::to_owned) else {
            break;
        };
        if kept.contains(&id) {
            break;
        }
        kept.insert(id);
        cursor = parent_uuid(&entry).and_then(|parent| by_uuid.get(parent).cloned());
    }
    let start = entries.iter().rposition(|entry| {
        entry_id(entry).is_some_and(|id| kept.contains(id)) && is_main_turn(entry)
    });
    let tail = match start {
        Some(position) => &entries[position + 1..],
        None => entries.as_slice(),
    };
    let mut dropped = HashSet::new();
    for entry in tail {
        if !is_main_turn(entry) {
            continue;
        }
        let Some(id) = entry_id(entry) else { continue };
        if kept.contains(id) {
            continue;
        }
        let mut cursor = Some(entry.clone());
        while let Some(node) = cursor {
            let Some(id) = entry_id(&node).map(str::to_owned) else {
                break;
            };
            if kept.contains(&id) || dropped.contains(&id) {
                break;
            }
            if index.get(&id).copied().unwrap_or(origin) < origin {
                break;
            }
            dropped.insert(id);
            cursor = parent_uuid(&node).and_then(|parent| by_uuid.get(parent).cloned());
        }
    }
    if dropped.is_empty() {
        return entries;
    }
    for entry in &entries {
        if let Some(parent) = parent_uuid(entry)
            && dropped.contains(parent)
            && let Some(id) = entry_id(entry)
        {
            dropped.insert(id.to_owned());
        }
    }
    entries
        .into_iter()
        .filter(|entry| entry_id(entry).is_none_or(|id| !dropped.contains(id)))
        .collect()
}
fn cut_up_to(entries: Vec<Value>, target: &str) -> Result<Vec<Value>> {
    let chain = session_chain(&entries);
    let mut first_index = HashMap::new();
    for (position, entry) in entries.iter().enumerate() {
        if let Some(id) = entry_id(entry) {
            first_index.entry(id.to_owned()).or_insert(position);
        }
    }
    let mut max_index = None;
    for entry in &chain {
        if let Some(position) = entry_id(entry).and_then(|id| first_index.get(id)) {
            max_index = Some(max_index.map_or(*position, |current: usize| current.max(*position)));
        }
    }
    let mut extended = chain.clone();
    match max_index {
        Some(position) => extended.extend(entries.iter().skip(position + 1).cloned()),
        None => extended = entries.clone(),
    };
    let position = if let Some(found) = message_index(&extended, target) {
        entry_id(&extended[found])
            .and_then(|id| first_index.get(id).copied())
            .context("fork target not found")?
    } else {
        message_index(&entries, target).context("fork target not found")?
    };
    Ok(drop_dangling(
        entries.into_iter().take(position + 1).collect(),
        &chain,
        &first_index,
    ))
}
fn slice<T>(values: Vec<T>, offset: usize, limit: Option<usize>) -> Vec<T> {
    values
        .into_iter()
        .skip(offset)
        .take(limit.filter(|n| *n > 0).unwrap_or(usize::MAX))
        .collect()
}
fn to_message(
    e: &Value,
    parent_tool_use_id: Option<&str>,
    parent_agent_id: Option<&str>,
) -> Option<SessionMessage> {
    Some(SessionMessage {
        kind: e.get("type")?.as_str()?.into(),
        uuid: entry_id(e)?.into(),
        session_id: e.get("sessionId")?.as_str()?.into(),
        message: e.get("message").cloned().unwrap_or(Value::Null),
        parent_tool_use_id: parent_tool_use_id.map(str::to_owned),
        parent_agent_id: parent_agent_id.map(str::to_owned),
        timestamp: e
            .get("timestamp")
            .and_then(Value::as_str)
            .map(str::to_owned),
        is_meta: (truthy(e.get("isMeta"))
            || truthy(e.get("isCompactSummary"))
            || truthy(e.get("isVisibleInTranscriptOnly")))
        .then_some(true),
        is_compact_summary: e.get("isCompactSummary").and_then(Value::as_bool),
        is_queued_command: e.get("isQueuedCommand").and_then(Value::as_bool),
        origin: e.get("origin").cloned(),
    })
}
pub async fn get_session_messages(
    id: &str,
    options: &TranscriptOptions,
) -> Result<Vec<SessionMessage>> {
    let Some(path) = session_path(id, options).await else {
        return Ok(vec![]);
    };
    let entries = read_entries(&path).await?;
    let messages = session_chain(&entries)
        .into_iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            ) || (options.include_system_messages
                && e.get("type").and_then(Value::as_str) == Some("system"))
        })
        .filter(|e| {
            !truthy(e.get("isSidechain")) && !truthy(e.get("isMeta")) && !truthy(e.get("teamName"))
        })
        .filter_map(|e| to_message(&e, None, None))
        .collect();
    Ok(slice(messages, options.offset, options.limit))
}
pub async fn get_subagent_messages(
    id: &str,
    agent_id: &str,
    options: &TranscriptOptions,
) -> Result<Vec<SessionMessage>> {
    let Some(path) = session_path(id, options).await else {
        return Ok(vec![]);
    };
    let root = path.with_extension("").join("subagents");
    let mut stack = vec![root];
    let mut found = None;
    while let Some(dir) = stack.pop() {
        let Ok(mut entries) = fs::read_dir(dir).await else {
            continue;
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let file = entry.path();
            if file.is_dir() {
                stack.push(file);
            } else if file
                .file_name()
                .is_some_and(|name| name.to_string_lossy() == format!("agent-{agent_id}.jsonl"))
            {
                found = Some(file);
                break;
            }
        }
        if found.is_some() {
            break;
        }
    }
    let Some(file) = found else { return Ok(vec![]) };
    let entries = read_entries(&file).await?;
    let metadata = fs::read_to_string(file.with_extension("meta.json"))
        .await
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let tool_id = metadata
        .as_ref()
        .and_then(|v| v.get("toolUseId"))
        .and_then(Value::as_str);
    let parent_id = metadata
        .as_ref()
        .and_then(|v| v.get("parentAgentId"))
        .and_then(Value::as_str);
    let messages = session_chain(&entries)
        .into_iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(Value::as_str),
                Some("user" | "assistant")
            )
        })
        .filter_map(|e| to_message(&e, tool_id, parent_id))
        .collect();
    Ok(slice(messages, options.offset, options.limit))
}
pub async fn list_sessions(options: &TranscriptOptions) -> Result<Vec<SessionInfo>> {
    let mut result = vec![];
    let mut folders = match fs::read_dir(projects_dir(options)).await {
        Ok(v) => v,
        Err(_) => return Ok(result),
    };
    let filter = options.dir.as_ref().map(|dir| project_key(dir));
    while let Some(folder) = folders.next_entry().await? {
        if filter
            .as_ref()
            .is_some_and(|f| folder.file_name() != f.as_str())
        {
            continue;
        }
        let mut files = match fs::read_dir(folder.path()).await {
            Ok(v) => v,
            Err(_) => continue,
        };
        while let Some(file) = files.next_entry().await? {
            let path = file.path();
            if path.extension().and_then(|v| v.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|v| v.to_str()) else {
                continue;
            };
            if uuid::Uuid::parse_str(id).is_err() {
                continue;
            }
            let Ok(bytes) = fs::read(&path).await else {
                continue;
            };
            let entries = parse_entries(&String::from_utf8_lossy(&bytes));
            if entries
                .first()
                .and_then(|e| e.get("isSidechain"))
                .and_then(Value::as_bool)
                == Some(true)
            {
                continue;
            }
            let (head, tail) = windows(&bytes);
            let sidecar = match last_string_field(&tail, "customTitle") {
                Some(_) => None,
                None => sidecar_title(&path, id).await,
            };
            let named = window_title(&head, &tail, sidecar, false);
            let first_prompt = first_prompt(&head);
            let summary = named
                .clone()
                .or_else(|| nonempty(last_string_field(&tail, "lastPrompt")))
                .or_else(|| nonempty(last_string_field(&tail, "summary")))
                .or_else(|| first_prompt.clone())
                .or_else(|| media_hint(&head));
            let Some(summary) = summary else { continue };
            let metadata = file.metadata().await?;
            let modified = metadata
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            let created_at = entries
                .iter()
                .find_map(|e| e.get("timestamp").and_then(Value::as_str))
                .and_then(parse_millis);
            result.push(SessionInfo {
                session_id: id.into(),
                summary,
                last_modified: modified,
                file_size: metadata.len(),
                custom_title: named,
                first_prompt,
                git_branch: entries.iter().rev().find_map(|e| {
                    e.get("gitBranch")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                }),
                cwd: entries
                    .iter()
                    .find_map(|e| e.get("cwd").and_then(Value::as_str).map(str::to_owned)),
                tag: entries
                    .iter()
                    .rev()
                    .find_map(|e| e.get("tag").and_then(Value::as_str).map(str::to_owned)),
                created_at,
            });
        }
    }
    result.sort_by(|a, b| {
        b.last_modified
            .cmp(&a.last_modified)
            .then_with(|| b.session_id.cmp(&a.session_id))
    });
    Ok(slice(result, options.offset, options.limit))
}
fn remap_attachment(entry: &mut Value, ids: &HashMap<String, String>) {
    if entry.get("type").and_then(Value::as_str) != Some("attachment") {
        return;
    }
    let Some(attachment) = entry.get("attachment").cloned() else {
        return;
    };
    let Some(object) = attachment.as_object() else {
        return;
    };
    let replacement = match object.get("type").and_then(Value::as_str) {
        Some("deferred_tools_record") => {
            let names = object
                .get("nameOnlyAnnouncements")
                .and_then(Value::as_array);
            let Some(names) = names else { return };
            let mapped: Vec<Value> = names
                .iter()
                .filter_map(|name| name.as_str().and_then(|id| ids.get(id)).cloned())
                .map(Value::String)
                .collect();
            let mut next = object.clone();
            next.insert("nameOnlyAnnouncements".into(), Value::Array(mapped));
            Value::Object(next)
        }
        Some("queued_command") => {
            let Some(source) = object
                .get("source_uuid")
                .and_then(Value::as_str)
                .and_then(|id| ids.get(id))
                .cloned()
            else {
                return;
            };
            let mut next = object.clone();
            next.insert("source_uuid".into(), source.into());
            Value::Object(next)
        }
        _ => return,
    };
    if let Some(object) = entry.as_object_mut() {
        object.insert("attachment".into(), replacement);
    }
}
struct ForkSide {
    history_suppressed: bool,
    atis: Option<String>,
    replacements: Vec<Value>,
    relocated: Option<String>,
}
fn fork_side(raw: &[Value], session_id: &str) -> ForkSide {
    let mut side = ForkSide {
        history_suppressed: false,
        atis: None,
        replacements: Vec::new(),
        relocated: None,
    };
    for entry in raw {
        let same_session = entry.get("sessionId").and_then(Value::as_str) == Some(session_id);
        match entry.get("type").and_then(Value::as_str) {
            Some("history-suppression") => side.history_suppressed = true,
            Some("atis-latch") if same_session => {
                if let Some(atis) = entry
                    .get("atis")
                    .and_then(Value::as_str)
                    .filter(|atis| atis.bytes().all(|byte| (0x21..=0x7e).contains(&byte)))
                {
                    side.atis = Some(atis.to_owned());
                }
            }
            Some("content-replacement") if same_session => {
                if let Some(items) = entry.get("replacements").and_then(Value::as_array) {
                    side.replacements.extend(items.iter().cloned());
                }
            }
            Some("relocated") if same_session => {
                if let Some(cwd) = entry
                    .get("relocatedCwd")
                    .and_then(Value::as_str)
                    .filter(|cwd| !cwd.is_empty())
                {
                    side.relocated = Some(cwd.to_owned());
                }
            }
            _ => {}
        }
    }
    side
}
fn remap_parent(
    entry: &Value,
    by_old: &HashMap<String, Value>,
    ids: &HashMap<String, String>,
) -> Value {
    let mut current = entry
        .get("parentUuid")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut seen = HashSet::new();
    while let Some(parent_id) = current {
        if !seen.insert(parent_id.clone()) {
            break;
        }
        let Some(parent) = by_old.get(&parent_id) else {
            return Value::Null;
        };
        if parent.get("type").and_then(Value::as_str) != Some("progress") {
            return ids
                .get(&parent_id)
                .cloned()
                .map(Value::String)
                .unwrap_or(Value::Null);
        }
        current = parent
            .get("parentUuid")
            .and_then(Value::as_str)
            .map(str::to_owned);
    }
    Value::Null
}
/// SDK `Ca`: a user entry's first usable prompt line; `<command-name>` only feeds the fallback.
fn entry_prompt(entry: &Value, command_fallback: &mut String) -> Option<String> {
    if entry.get("type").and_then(Value::as_str) != Some("user")
        || entry.get("isMeta").and_then(Value::as_bool) == Some(true)
        || entry.get("isCompactSummary").and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    let texts: Vec<&str> = match entry.pointer("/message/content")? {
        Value::String(text) => vec![text],
        Value::Array(blocks) => {
            let mut texts = vec![];
            for block in blocks.iter().filter(|block| block.is_object()) {
                match block.get("type").and_then(Value::as_str) {
                    Some("tool_result") => return None,
                    Some("text") => texts.extend(block.get("text").and_then(Value::as_str)),
                    _ => {}
                }
            }
            texts
        }
        _ => vec![],
    };
    for text in texts {
        let text = text.replace('\n', " ");
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        if let Some(name) = between(text, "<command-name>", "</command-name>") {
            if command_fallback.is_empty() {
                *command_fallback = name.to_owned();
            }
            continue;
        }
        if let Some(input) = between(text, "<bash-input>", "</bash-input>") {
            return Some(format!("! {}", input.trim()));
        }
        if starts_with_tag_or_interrupt(text) {
            continue;
        }
        if text.encode_utf16().count() > 200 {
            let mut units = 0;
            let cut: String = text
                .chars()
                .take_while(|c| {
                    units += c.len_utf16();
                    units <= 200
                })
                .collect();
            return Some(format!("{}…", cut.trim()));
        }
        return Some(text.to_owned());
    }
    None
}
fn between<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let end = text[start..].find(close)?;
    Some(&text[start..start + end])
}
fn starts_with_tag_or_interrupt(text: &str) -> bool {
    if let Some(rest) = text.trim_start().strip_prefix('<') {
        let mut chars = rest.chars();
        if !chars.next().is_some_and(|c| c.is_ascii_lowercase()) {
            return false;
        }
        for c in chars {
            if !(c.is_ascii_alphanumeric() || c == '_' || c == '-') {
                return c.is_whitespace() || c == '>';
            }
        }
        return false;
    }
    text.strip_prefix("[Request interrupted by user")
        .is_some_and(|rest| rest.contains(']'))
}
fn is_candidate_user_line(line: &str) -> bool {
    (line.contains("\"type\":\"user\"") || line.contains("\"type\": \"user\""))
        && !line.contains("\"tool_result\"")
        && !line.contains("\"isMeta\":true")
        && !line.contains("\"isMeta\": true")
}
/// SDK `_S`: the first prompt in the head window, else the first slash-command name.
fn first_prompt(head: &str) -> Option<String> {
    let mut command_fallback = String::new();
    for line in head.split('\n').filter(|line| is_candidate_user_line(line)) {
        if line.contains("\"isCompactSummary\":true") || line.contains("\"isCompactSummary\": true")
        {
            continue;
        }
        let Ok(entry) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(prompt) = entry_prompt(&entry, &mut command_fallback) {
            return Some(prompt);
        }
    }
    nonempty(Some(command_fallback))
}
/// SDK `d4`: a media label when no text prompt exists.
fn media_hint(head: &str) -> Option<String> {
    head.split('\n')
        .filter(|line| is_candidate_user_line(line))
        .find_map(|line| {
            if line.contains("\"type\":\"image\"") || line.contains("\"type\": \"image\"") {
                Some("Image".to_owned())
            } else if line.contains("\"type\":\"document\"")
                || line.contains("\"type\": \"document\"")
            {
                Some("Document".to_owned())
            } else {
                None
            }
        })
}
fn parse_millis(value: &str) -> Option<u64> {
    chrono::DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|time| time.timestamp_millis() as u64)
}
fn fork_title(explicit: Option<&str>, head: &str, tail: &str, sidecar: Option<String>) -> String {
    if let Some(title) = explicit.map(str::trim).filter(|title| !title.is_empty()) {
        return title.to_owned();
    }
    let derived = window_title(head, tail, sidecar, true)
        .or_else(|| first_prompt(head))
        .unwrap_or_else(|| "Forked session".into());
    format!("{derived} (fork)")
}
pub async fn fork_session(
    id: &str,
    options: &TranscriptOptions,
    up_to_message_id: Option<&str>,
    title: Option<&str>,
) -> Result<String> {
    anyhow::ensure!(uuid::Uuid::parse_str(id).is_ok(), "invalid session ID");
    let path = session_path(id, options)
        .await
        .context("session not found")?;
    let bytes = fs::read(&path).await?;
    let raw = parse_entries(&String::from_utf8_lossy(&bytes));
    let (head, tail) = windows(&bytes);
    let sidecar = sidecar_title(&path, id).await;
    let title = fork_title(title, &head, &tail, sidecar);
    let side = fork_side(&raw, id);
    let mut entries: Vec<_> = raw
        .into_iter()
        .filter(|e| {
            matches!(
                e.get("type").and_then(Value::as_str),
                Some("user" | "assistant" | "system" | "attachment" | "progress")
            ) && e.get("uuid").and_then(Value::as_str).is_some()
                && e.get("isSidechain").and_then(Value::as_bool) != Some(true)
        })
        .collect();
    let no_messages = || anyhow::anyhow!("Session {id} has no messages to fork");
    if entries.is_empty() {
        return Err(no_messages());
    }
    if let Some(target) = up_to_message_id {
        entries = cut_up_to(entries, target)?;
    }
    if entries
        .iter()
        .all(|entry| entry.get("type").and_then(Value::as_str) == Some("progress"))
    {
        return Err(no_messages());
    }
    let new_id = uuid::Uuid::new_v4().to_string();
    let ids: HashMap<_, _> = entries
        .iter()
        .filter_map(|e| Some((entry_id(e)?.to_owned(), uuid::Uuid::new_v4().to_string())))
        .collect();
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let by_old: HashMap<String, Value> = entries
        .iter()
        .filter_map(|e| Some((entry_id(e)?.to_owned(), e.clone())))
        .collect();
    let kept: Vec<Value> = entries
        .iter()
        .filter(|e| e.get("type").and_then(Value::as_str) != Some("progress"))
        .cloned()
        .collect();
    let last = kept.len().saturating_sub(1);
    let mut forked = Vec::new();
    for (index, mut e) in kept.into_iter().enumerate() {
        let old_id = entry_id(&e).unwrap().to_owned();
        remap_attachment(&mut e, &ids);
        let parent = remap_parent(&e, &by_old, &ids);
        let logical = e.get("logicalParentUuid").cloned();
        if let Some(obj) = e.as_object_mut() {
            obj.insert("uuid".into(), ids[&old_id].clone().into());
            obj.insert("sessionId".into(), new_id.clone().into());
            obj.insert("parentUuid".into(), parent);
            if let Some(logical) = logical {
                match logical {
                    Value::Null => {
                        obj.insert("logicalParentUuid".into(), Value::Null);
                    }
                    Value::String(parent) => {
                        obj.insert(
                            "logicalParentUuid".into(),
                            ids.get(&parent)
                                .cloned()
                                .map(Value::String)
                                .unwrap_or(Value::Null),
                        );
                    }
                    _ => {
                        obj.insert("logicalParentUuid".into(), Value::Null);
                    }
                }
            }
            obj.insert("isSidechain".into(), false.into());
            obj.insert(
                "forkedFrom".into(),
                json!({"sessionId":id,"messageUuid":old_id}),
            );
            for key in [
                "teamName",
                "agentName",
                "sessionKind",
                "slug",
                "sourceToolAssistantUUID",
            ] {
                obj.remove(key);
            }
            if obj.get("type").and_then(Value::as_str) == Some("system")
                && obj.get("subtype").and_then(Value::as_str) == Some("model_refusal_fallback")
            {
                obj.insert("neutralizedByFork".into(), true.into());
            }
            if index == last {
                obj.insert("timestamp".into(), timestamp.clone().into());
            }
        }
        forked.push(e);
    }
    if side.history_suppressed {
        forked.insert(0, json!({"type":"history-suppression","sessionId":new_id,"cause":"fork_inherit","ts":timestamp}));
    }
    if !side.replacements.is_empty() {
        forked.push(json!({"type":"content-replacement","sessionId":new_id,"replacements":side.replacements,"uuid":uuid::Uuid::new_v4().to_string(),"timestamp":timestamp}));
    }
    if let Some(atis) = side.atis {
        forked.push(json!({"type":"atis-latch","sessionId":new_id,"atis":atis}));
    }
    if let Some(cwd) = side.relocated {
        forked.push(json!({"type":"relocated","sessionId":new_id,"relocatedCwd":cwd}));
    }
    forked.push(json!({"type":"custom-title","sessionId":new_id,"customTitle":title,"uuid":uuid::Uuid::new_v4().to_string(),"timestamp":timestamp}));
    let new_path = path.with_file_name(format!("{new_id}.jsonl"));
    let mut open = fs::OpenOptions::new();
    open.write(true).create_new(true);
    #[cfg(unix)]
    open.mode(0o600);
    let mut file = open.open(&new_path).await?;
    for entry in forked {
        file.write_all(serde_json::to_string(&entry)?.as_bytes())
            .await?;
        file.write_all(b"\n").await?;
    }
    file.flush().await?;
    Ok(new_id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reads_chain_subagent_and_forks_transcript() {
        let root =
            std::env::temp_dir().join(format!("turin-claude-transcripts-{}", uuid::Uuid::new_v4()));
        let dir = root.join("work");
        fs::create_dir_all(&dir).await.unwrap();
        let options = TranscriptOptions {
            dir: Some(dir.clone()),
            config_dir: Some(root.join("config")),
            ..Default::default()
        };
        let id = uuid::Uuid::new_v4().to_string();
        let folder = projects_dir(&options).join(project_key(&dir));
        fs::create_dir_all(&folder).await.unwrap();
        let file = folder.join(format!("{id}.jsonl"));
        let entries = [
            json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":id,"timestamp":"2026-10-01T00:00:00.000Z","message":{"role":"user","content":"first"}}),
            json!({"type":"progress","uuid":"p1","parentUuid":"u1","sessionId":id,"data":{}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"p1","sessionId":id,"timestamp":"2026-10-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]}}),
            json!({"type":"assistant","uuid":"side","parentUuid":"u1","sessionId":id,"isSidechain":true,"message":{"role":"assistant","content":[]}}),
            json!({"type":"custom-title","customTitle":"Example","sessionId":id}),
        ];
        fs::write(
            &file,
            entries
                .iter()
                .map(|e| format!("{}\n", serde_json::to_string(e).unwrap()))
                .collect::<String>(),
        )
        .await
        .unwrap();
        let messages = get_session_messages(&id, &options).await.unwrap();
        assert_eq!(
            messages.iter().map(|m| m.uuid.as_str()).collect::<Vec<_>>(),
            ["u1", "a1"]
        );
        let summaries = list_sessions(&options).await.unwrap();
        assert_eq!(summaries[0].summary, "Example");
        let sub = folder.join(&id).join("subagents");
        fs::create_dir_all(&sub).await.unwrap();
        fs::write(sub.join("agent-child.jsonl"),format!("{}\n",json!({"type":"user","uuid":"su","sessionId":id,"message":{"role":"user","content":"child"}}))).await.unwrap();
        fs::write(
            sub.join("agent-child.meta.json"),
            r#"{"toolUseId":"toolu_1","parentAgentId":"parent"}"#,
        )
        .await
        .unwrap();
        assert_eq!(
            get_subagent_messages(&id, "child", &options).await.unwrap()[0]
                .parent_tool_use_id
                .as_deref(),
            Some("toolu_1")
        );
        let fork = fork_session(&id, &options, Some("a1"), Some("Branch"))
            .await
            .unwrap();
        let fork_entries = read_entries(&folder.join(format!("{fork}.jsonl")))
            .await
            .unwrap();
        assert_eq!(fork_entries.len(), 3);
        assert_eq!(fork_entries[1]["parentUuid"], fork_entries[0]["uuid"]);
        assert_eq!(fork_entries[2]["customTitle"], "Branch");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(folder.join(format!("{fork}.jsonl")))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(
            fork_entries
                .iter()
                .all(|entry| entry.get("type").and_then(Value::as_str) != Some("progress"))
        );
        let compact_id = uuid::Uuid::new_v4().to_string();
        let compact = [
            json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":compact_id,"message":{"role":"user","content":"kept"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":compact_id,"message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"one"}]}}),
            json!({"type":"system","subtype":"compact_boundary","uuid":"c1","sessionId":compact_id,"compactMetadata":{"preservedMessages":{"anchorUuid":"u1","uuids":["a1"]}}}),
            json!({"type":"assistant","uuid":"a2","parentUuid":"u1","sessionId":compact_id,"timestamp":"2026-10-01T00:00:02.000Z","message":{"id":"msg_1","role":"assistant","content":[{"type":"text","text":"sibling"}]}}),
            json!({"type":"user","uuid":"u2","parentUuid":"u1","sessionId":compact_id,"message":{"role":"user","content":"after"}}),
        ];
        fs::write(
            folder.join(format!("{compact_id}.jsonl")),
            compact
                .iter()
                .map(|entry| format!("{}\n", serde_json::to_string(entry).unwrap()))
                .collect::<String>(),
        )
        .await
        .unwrap();
        let compact_messages = get_session_messages(&compact_id, &options).await.unwrap();
        assert_eq!(
            compact_messages
                .iter()
                .map(|message| message.uuid.as_str())
                .collect::<Vec<_>>(),
            ["u1", "a1", "a2", "u2"]
        );
        fs::remove_dir_all(root).await.unwrap();
    }
    async fn write_session(entries: &[Value]) -> (PathBuf, TranscriptOptions, String, PathBuf) {
        let root = std::env::temp_dir().join(format!("turin-claude-fork-{}", uuid::Uuid::new_v4()));
        let dir = root.join("work");
        fs::create_dir_all(&dir).await.unwrap();
        let options = TranscriptOptions {
            dir: Some(dir.clone()),
            config_dir: Some(root.join("config")),
            ..Default::default()
        };
        let id = uuid::Uuid::new_v4().to_string();
        let folder = projects_dir(&options).join(project_key(&dir));
        fs::create_dir_all(&folder).await.unwrap();
        let body: String = entries
            .iter()
            .map(|entry| {
                let mut entry = entry.clone();
                entry["sessionId"] = json!(id);
                format!("{}\n", serde_json::to_string(&entry).unwrap())
            })
            .collect();
        fs::write(folder.join(format!("{id}.jsonl")), body)
            .await
            .unwrap();
        (root, options, id, folder)
    }
    fn user(uuid: &str, parent: Option<&str>, text: &str) -> Value {
        json!({"type":"user","uuid":uuid,"parentUuid":parent,"message":{"role":"user","content":text}})
    }
    fn assistant(uuid: &str, parent: &str) -> Value {
        json!({"type":"assistant","uuid":uuid,"parentUuid":parent,"message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}})
    }
    #[tokio::test]
    async fn fork_cut_prefers_exact_uuid_over_queued_command_source() {
        let (root, options, id, folder) = write_session(&[
            user("u1", None, "first"),
            assistant("a2", "u1"),
            json!({"type":"attachment","uuid":"A3","parentUuid":"a2","attachment":{"type":"queued_command","prompt":"later","source_uuid":"u5"}}),
            assistant("a4", "A3"),
            user("u5", Some("a4"), "fifth"),
            assistant("a6", "u5"),
        ])
        .await;
        let fork = fork_session(&id, &options, Some("u5"), Some("Cut"))
            .await
            .unwrap();
        let forked = read_entries(&folder.join(format!("{fork}.jsonl")))
            .await
            .unwrap();
        let originals: Vec<&str> = forked
            .iter()
            .filter_map(|entry| {
                entry
                    .pointer("/forkedFrom/messageUuid")
                    .and_then(Value::as_str)
            })
            .collect();
        assert_eq!(originals, ["u1", "a2", "A3", "a4", "u5"]);
        let other =
            json!({"type":"attachment","uuid":"x","attachment":{"type":"other","source_uuid":"t"}});
        assert_eq!(message_index(&[other], "t"), None);
        fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn fork_rejects_a_cut_that_leaves_only_progress() {
        let (root, options, id, folder) = write_session(&[
            json!({"type":"progress","uuid":"p1","parentUuid":null,"data":{}}),
            user("u1", Some("p1"), "first"),
        ])
        .await;
        let error = fork_session(&id, &options, Some("p1"), None)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Session {id} has no messages to fork")
        );
        let mut files = fs::read_dir(&folder).await.unwrap();
        let mut count = 0;
        while files.next_entry().await.unwrap().is_some() {
            count += 1;
        }
        assert_eq!(count, 1, "no fork file should be written");
        fs::remove_dir_all(root).await.unwrap();

        let (root, options, id, _) =
            write_session(&[json!({"type":"progress","uuid":"p1","parentUuid":null,"data":{}})])
                .await;
        let error = fork_session(&id, &options, None, None).await.unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("Session {id} has no messages to fork")
        );
        fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn titles_use_sdk_windows_and_the_sidecar() {
        let (root, options, id, folder) =
            write_session(&[user("u1", None, "one"), assistant("a2", "u1")]).await;
        fs::create_dir_all(folder.join(&id)).await.unwrap();
        fs::write(
            folder.join(&id).join("custom-title.json"),
            r#"{"customTitle":"  Sidecar\u0007\u0007Name  "}"#,
        )
        .await
        .unwrap();
        let fork = fork_session(&id, &options, None, None).await.unwrap();
        let forked = read_entries(&folder.join(format!("{fork}.jsonl")))
            .await
            .unwrap();
        assert_eq!(forked.last().unwrap()["customTitle"], "Sidecar Name (fork)");
        let listed = list_sessions(&options).await.unwrap();
        let source = listed.iter().find(|info| info.session_id == id).unwrap();
        assert_eq!(source.custom_title.as_deref(), Some("Sidecar Name"));
        fs::remove_dir_all(root).await.unwrap();

        let padding = "x".repeat(CHUNK_BYTES);
        let mut entries = vec![
            json!({"type":"ai-title","aiTitle":"Head AI"}),
            user("u1", None, &padding),
            json!({"type":"custom-title","customTitle":"Middle"}),
        ];
        let mut parent = "u1".to_owned();
        for index in 0..2 {
            let uuid = format!("a{index}");
            entries.push(json!({"type":"assistant","uuid":uuid,"parentUuid":parent,"message":{"role":"assistant","content":[{"type":"text","text":padding}]}}));
            parent = uuid;
        }
        let (root, options, id, folder) = write_session(&entries).await;
        let fork = fork_session(&id, &options, None, None).await.unwrap();
        let forked = read_entries(&folder.join(format!("{fork}.jsonl")))
            .await
            .unwrap();
        assert_eq!(forked.last().unwrap()["customTitle"], "Head AI (fork)");
        let listed = list_sessions(&options).await.unwrap();
        let source = listed.iter().find(|info| info.session_id == id).unwrap();
        assert_eq!(source.custom_title.as_deref(), Some("Head AI"));
        fs::remove_dir_all(root).await.unwrap();
    }
    #[test]
    fn first_prompt_follows_sdk_prompt_rules() {
        let lines = |entries: &[Value]| {
            entries
                .iter()
                .map(|entry| serde_json::to_string(entry).unwrap())
                .collect::<Vec<_>>()
                .join("\n")
        };
        let head = lines(&[
            json!({"type":"user","uuid":"u0"}),
            user(
                "u1",
                None,
                "<ide_opened_file>The user opened a.rs</ide_opened_file>",
            ),
            user("u2", None, "[Request interrupted by user for tool use]"),
            user(
                "u3",
                None,
                "<command-name>/clear</command-name> <command-args></command-args>",
            ),
            json!({"type":"user","message":{"content":[{"type":"text","text":"<system-reminder>x</system-reminder>"},{"type":"text","text":"real\nprompt"}]}}),
        ]);
        assert_eq!(first_prompt(&head).as_deref(), Some("real prompt"));

        let head = lines(&[
            user(
                "u1",
                None,
                "<local-command-stdout>ok</local-command-stdout>",
            ),
            user("u2", None, "<command-name>/model</command-name>"),
            user("u3", None, "<command-name>/clear</command-name>"),
        ]);
        assert_eq!(first_prompt(&head).as_deref(), Some("/model"));

        let head = lines(&[user("u1", None, "<bash-input> ls -la </bash-input>")]);
        assert_eq!(first_prompt(&head).as_deref(), Some("! ls -la"));

        let long = format!("{} tail", "a".repeat(199));
        let head = lines(&[user("u1", None, &long)]);
        assert_eq!(first_prompt(&head), Some(format!("{}…", "a".repeat(199))));

        let head =
            lines(&[json!({"type":"user","message":{"content":[{"type":"image","source":{}}]}})]);
        assert_eq!(first_prompt(&head), None);
        assert_eq!(media_hint(&head).as_deref(), Some("Image"));
    }
    #[test]
    fn last_string_field_takes_the_textually_last_match() {
        let text = "{\"customTitle\":\"one\"}\n{\"customTitle\": \"two \\\"q\\\"\"}\n{\"customTitle\":\"three";
        assert_eq!(
            last_string_field(text, "customTitle").as_deref(),
            Some("two \"q\"")
        );
    }
    #[test]
    fn project_key_matches_cli_path_replacement() {
        assert_eq!(project_key(Path::new("/tmp/a b")), "-tmp-a-b");
        let long = format!("/{}", "a".repeat(250));
        assert_eq!(
            encode_project_path(&long),
            format!("{}-feo44x", "-".to_owned() + &"a".repeat(199))
        );
        assert_eq!(encode_project_path("café /tmp"), "caf---tmp");
    }
    #[tokio::test]
    async fn session_lookup_follows_worktrees_overrides_and_long_keys() {
        let root =
            std::env::temp_dir().join(format!("turin-claude-lookup-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let config = root.join("config");
        let main = root.join("main");
        let worktree = root.join("wt");
        fs::create_dir_all(&main).await.unwrap();
        let git = |args: &[&str], cwd: &Path| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(cwd)
                .env("GIT_AUTHOR_NAME", "Turin")
                .env("GIT_AUTHOR_EMAIL", "t@example.com")
                .env("GIT_COMMITTER_NAME", "Turin")
                .env("GIT_COMMITTER_EMAIL", "t@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init"], &main);
        git(&["commit", "--allow-empty", "-m", "init"], &main);
        git(&["worktree", "add", worktree.to_str().unwrap()], &main);
        let id = uuid::Uuid::new_v4().to_string();
        let projects = config.join("projects");
        let main_dir = projects.join(project_key(&main));
        fs::create_dir_all(&main_dir).await.unwrap();
        fs::write(main_dir.join(format!("{id}.jsonl")), format!("{}\n", json!({"type":"user","uuid":"u1","sessionId":id,"message":{"role":"user","content":"from-main"}}))).await.unwrap();
        let worktree_dir = projects.join(project_key(&worktree));
        fs::create_dir_all(&worktree_dir).await.unwrap();
        fs::write(worktree_dir.join(format!("{id}.jsonl")), "")
            .await
            .unwrap();
        let options = TranscriptOptions {
            dir: Some(worktree.clone()),
            config_dir: Some(config.clone()),
            ..Default::default()
        };
        let messages = get_session_messages(&id, &options).await.unwrap();
        assert_eq!(messages[0].message["content"], "from-main");

        let mut deep = root.join("deep");
        fs::create_dir_all(&deep).await.unwrap();
        while dash_encode(&absolute_source(&deep)).encode_utf16().count() <= 200 {
            deep.push("a".repeat(40));
            fs::create_dir_all(&deep).await.unwrap();
        }
        let source = absolute_source(&deep);
        let prefix = utf16_prefix(&dash_encode(&source), 200);
        let alt = projects.join(format!("{prefix}-altkey"));
        fs::create_dir_all(&alt).await.unwrap();
        let long_id = uuid::Uuid::new_v4().to_string();
        fs::write(alt.join(format!("{long_id}.jsonl")), format!("{}\n", json!({"type":"user","uuid":"u1","cwd":source,"sessionId":long_id,"message":{"role":"user","content":"long"}}))).await.unwrap();
        let long_options = TranscriptOptions {
            dir: Some(deep),
            config_dir: Some(config.clone()),
            ..Default::default()
        };
        assert_eq!(
            get_session_messages(&long_id, &long_options).await.unwrap()[0].message["content"],
            "long"
        );

        let named = projects.join("customkey");
        fs::create_dir_all(&named).await.unwrap();
        let named_id = uuid::Uuid::new_v4().to_string();
        fs::write(named.join(format!("{named_id}.jsonl")), format!("{}\n", json!({"type":"user","uuid":"u1","sessionId":named_id,"message":{"role":"user","content":"named"}}))).await.unwrap();
        let named_options = TranscriptOptions {
            dir: Some(main),
            config_dir: Some(config),
            ..Default::default()
        };
        assert!(
            session_path_with_override(&named_id, &named_options, None)
                .await
                .is_none()
        );
        let found = session_path_with_override(&named_id, &named_options, Some("customkey"))
            .await
            .unwrap();
        assert_eq!(found, named.join(format!("{named_id}.jsonl")));
        assert_eq!(
            valid_project_dir_name("custom_key-1".into()).as_deref(),
            Some("custom_key-1")
        );
        assert_eq!(valid_project_dir_name("com1".into()), None);
        assert_eq!(valid_project_dir_name("a/b".into()), None);
        fs::remove_dir_all(root).await.unwrap();
    }
}

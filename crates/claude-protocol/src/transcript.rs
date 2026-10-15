//! Port of @anthropic-ai/claude-agent-sdk@0.3.276 sdk.mjs local session helpers.
use std::{collections::{HashMap, HashSet}, path::{Path, PathBuf}, time::UNIX_EPOCH};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::{fs, io::AsyncWriteExt};

#[derive(Debug, Clone, Default)]
pub struct TranscriptOptions { pub dir: Option<PathBuf>, pub config_dir: Option<PathBuf>, pub limit: Option<usize>, pub offset: usize, pub include_system_messages: bool }
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all="camelCase")]
pub struct SessionInfo { pub session_id: String, pub summary: String, pub last_modified: u64, pub file_size: u64,
    #[serde(skip_serializing_if="Option::is_none")] pub custom_title: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub first_prompt: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub git_branch: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub cwd: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub tag: Option<String> }
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionMessage { #[serde(rename="type")] pub kind: String, pub uuid: String, pub session_id: String,
    pub message: Value, pub parent_tool_use_id: Option<String>, pub parent_agent_id: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub timestamp: Option<String>,
    #[serde(skip_serializing_if="Option::is_none")] pub is_meta: Option<bool>,
    #[serde(skip_serializing_if="Option::is_none")] pub is_compact_summary: Option<bool>,
    #[serde(skip_serializing_if="Option::is_none")] pub is_queued_command: Option<bool>,
    #[serde(skip_serializing_if="Option::is_none")] pub origin: Option<Value> }
fn config_dir(options: &TranscriptOptions) -> PathBuf {
    options.config_dir.clone().or_else(|| std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"))
}
pub fn project_key(dir: &Path) -> String {
    let absolute = std::fs::canonicalize(dir).unwrap_or_else(|_| if dir.is_absolute() { dir.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(dir) });
    let source = absolute.to_string_lossy();
    let key: String = source.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    if key.len() <= 200 { return key; }
    // Claude CLI truncates at 200 and appends abs(JavaScript string hash).toString(36).
    let mut hash = 0_i32;
    for unit in source.encode_utf16() { hash = hash.wrapping_mul(31).wrapping_add(i32::from(unit)); }
    let mut value = hash.unsigned_abs(); let mut suffix = String::new();
    while value > 0 { let digit = value % 36; suffix.push(char::from_digit(digit, 36).unwrap()); value /= 36; }
    if suffix.is_empty() { suffix.push('0'); }
    format!("{}-{}", &key[..200], suffix.chars().rev().collect::<String>())
}
fn projects_dir(options: &TranscriptOptions) -> PathBuf { config_dir(options).join("projects") }
async fn session_path(id: &str, options: &TranscriptOptions) -> Option<PathBuf> {
    if uuid::Uuid::parse_str(id).is_err() { return None; }
    let root = projects_dir(options);
    if let Some(dir) = &options.dir { let p = root.join(project_key(dir)).join(format!("{id}.jsonl")); if fs::metadata(&p).await.is_ok() { return Some(p); } return None; }
    let mut folders = fs::read_dir(root).await.ok()?;
    while let Ok(Some(folder)) = folders.next_entry().await { let p = folder.path().join(format!("{id}.jsonl")); if fs::metadata(&p).await.is_ok() { return Some(p); } }
    None
}
async fn read_entries(path: &Path) -> Result<Vec<Value>> {
    let data = fs::read_to_string(path).await?;
    Ok(data.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok()).collect())
}
fn entry_id(e: &Value) -> Option<&str> { e.get("uuid")?.as_str() }
fn chain(entries: &[Value]) -> Vec<Value> {
    let indexed: HashMap<_,_> = entries.iter().filter_map(|e| Some((entry_id(e)?.to_owned(), e))).collect();
    let Some(last) = entries.iter().rev().find(|e| matches!(e.get("type").and_then(Value::as_str), Some("user"|"assistant")) && !e.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) && !e.get("isMeta").and_then(Value::as_bool).unwrap_or(false)) else { return vec![]; };
    let mut result = vec![]; let mut seen = HashSet::new(); let mut current = Some(last);
    while let Some(e) = current { let Some(id) = entry_id(e) else { break }; if !seen.insert(id.to_owned()) { break; } result.push(e.clone()); current = e.get("parentUuid").and_then(Value::as_str).and_then(|parent| indexed.get(parent).copied()); }
    result.reverse(); result
}
fn slice<T>(values: Vec<T>, offset: usize, limit: Option<usize>) -> Vec<T> { values.into_iter().skip(offset).take(limit.filter(|n| *n > 0).unwrap_or(usize::MAX)).collect() }
fn to_message(e: &Value, parent_tool_use_id: Option<&str>, parent_agent_id: Option<&str>) -> Option<SessionMessage> {
    Some(SessionMessage { kind: e.get("type")?.as_str()?.into(), uuid: entry_id(e)?.into(),
        session_id: e.get("sessionId")?.as_str()?.into(), message: e.get("message").cloned().unwrap_or(Value::Null),
        parent_tool_use_id: parent_tool_use_id.map(str::to_owned), parent_agent_id: parent_agent_id.map(str::to_owned),
        timestamp: e.get("timestamp").and_then(Value::as_str).map(str::to_owned),
        is_meta: (e.get("isMeta").and_then(Value::as_bool)==Some(true) || e.get("isCompactSummary").and_then(Value::as_bool)==Some(true)).then_some(true),
        is_compact_summary: e.get("isCompactSummary").and_then(Value::as_bool),
        is_queued_command: e.get("isQueuedCommand").and_then(Value::as_bool), origin: e.get("origin").cloned() })
}
pub async fn get_session_messages(id: &str, options: &TranscriptOptions) -> Result<Vec<SessionMessage>> {
    let Some(path) = session_path(id, options).await else { return Ok(vec![]) };
    let entries = read_entries(&path).await?;
    let messages = chain(&entries).into_iter().filter(|e| {
        matches!(e.get("type").and_then(Value::as_str), Some("user"|"assistant")) || (options.include_system_messages && e.get("type").and_then(Value::as_str)==Some("system"))
    }).filter(|e| !e.get("isSidechain").and_then(Value::as_bool).unwrap_or(false) && !e.get("isMeta").and_then(Value::as_bool).unwrap_or(false) && e.get("teamName").is_none())
        .filter_map(|e| to_message(&e,None,None)).collect();
    Ok(slice(messages, options.offset, options.limit))
}
pub async fn get_subagent_messages(id: &str, agent_id: &str, options: &TranscriptOptions) -> Result<Vec<SessionMessage>> {
    let Some(path) = session_path(id, options).await else { return Ok(vec![]) };
    let root = path.with_extension("").join("subagents");
    let mut stack = vec![root]; let mut found = None;
    while let Some(dir) = stack.pop() { let Ok(mut entries) = fs::read_dir(dir).await else { continue }; while let Ok(Some(entry)) = entries.next_entry().await {
        let file = entry.path(); if file.is_dir() { stack.push(file); } else if file.file_name().is_some_and(|name| name.to_string_lossy() == format!("agent-{agent_id}.jsonl")) { found = Some(file); break; }
    } if found.is_some() { break; } }
    let Some(file) = found else { return Ok(vec![]) };
    let entries = read_entries(&file).await?;
    let metadata = fs::read_to_string(file.with_extension("meta.json")).await.ok().and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let tool_id = metadata.as_ref().and_then(|v| v.get("toolUseId")).and_then(Value::as_str);
    let parent_id = metadata.as_ref().and_then(|v| v.get("parentAgentId")).and_then(Value::as_str);
    let messages = chain(&entries).into_iter().filter(|e| matches!(e.get("type").and_then(Value::as_str), Some("user"|"assistant")))
        .filter_map(|e| to_message(&e,tool_id,parent_id)).collect();
    Ok(slice(messages, options.offset, options.limit))
}
pub async fn list_sessions(options: &TranscriptOptions) -> Result<Vec<SessionInfo>> {
    let mut result = vec![]; let mut folders = match fs::read_dir(projects_dir(options)).await { Ok(v) => v, Err(_) => return Ok(result) };
    let filter = options.dir.as_ref().map(|dir| project_key(dir));
    while let Some(folder) = folders.next_entry().await? { if filter.as_ref().is_some_and(|f| folder.file_name() != f.as_str()) { continue; }
        let mut files = match fs::read_dir(folder.path()).await { Ok(v) => v, Err(_) => continue };
        while let Some(file) = files.next_entry().await? { let path = file.path(); if path.extension().and_then(|v| v.to_str()) != Some("jsonl") { continue; }
            let Some(id) = path.file_stem().and_then(|v| v.to_str()) else { continue }; if uuid::Uuid::parse_str(id).is_err() { continue; }
            let Ok(entries) = read_entries(&path).await else { continue }; if entries.first().and_then(|e| e.get("isSidechain")).and_then(Value::as_bool)==Some(true) { continue; }
            let custom_title = entries.iter().rev().find_map(|e| e.get("customTitle").and_then(Value::as_str)).map(str::to_owned);
            let first_prompt = entries.iter().find(|e| e.get("type").and_then(Value::as_str)==Some("user") && e.get("isMeta").and_then(Value::as_bool)!=Some(true)).and_then(|e| e.pointer("/message/content")).and_then(Value::as_str).map(str::to_owned);
            let ai_title = entries.iter().rev().find_map(|e| e.get("aiTitle").and_then(Value::as_str));
            let summary = custom_title.clone().or_else(|| ai_title.map(str::to_owned)).or_else(|| entries.iter().rev().find_map(|e| e.get("summary").and_then(Value::as_str).map(str::to_owned))).or_else(|| first_prompt.clone());
            let Some(summary) = summary else { continue };
            let metadata = file.metadata().await?;
            let modified = metadata.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
            result.push(SessionInfo { session_id:id.into(), summary, last_modified:modified, file_size:metadata.len(), custom_title, first_prompt,
                git_branch:entries.iter().rev().find_map(|e| e.get("gitBranch").and_then(Value::as_str).map(str::to_owned)),
                cwd:entries.iter().find_map(|e| e.get("cwd").and_then(Value::as_str).map(str::to_owned)), tag:entries.iter().rev().find_map(|e| e.get("tag").and_then(Value::as_str).map(str::to_owned)) });
        }
    }
    result.sort_by(|a,b| b.last_modified.cmp(&a.last_modified).then_with(|| a.session_id.cmp(&b.session_id)));
    Ok(slice(result,options.offset,options.limit))
}
pub async fn fork_session(id: &str, options: &TranscriptOptions, up_to_message_id: Option<&str>, title: Option<&str>) -> Result<String> {
    anyhow::ensure!(uuid::Uuid::parse_str(id).is_ok(), "invalid session ID");
    let path = session_path(id, options).await.context("session not found")?;
    let mut entries: Vec<_> = read_entries(&path).await?.into_iter().filter(|e| matches!(e.get("type").and_then(Value::as_str),Some("user"|"assistant"|"system"|"attachment"|"progress")) && e.get("uuid").and_then(Value::as_str).is_some() && e.get("isSidechain").and_then(Value::as_bool)!=Some(true)).collect();
    anyhow::ensure!(!entries.is_empty(), "session has no messages to fork");
    if let Some(target) = up_to_message_id { let index = entries.iter().position(|e| entry_id(e)==Some(target) || e.pointer("/attachment/source_uuid").and_then(Value::as_str)==Some(target)).context("fork target not found")?; entries.truncate(index+1); }
    let new_id = uuid::Uuid::new_v4().to_string();
    let ids: HashMap<_,_> = entries.iter().filter_map(|e| Some((entry_id(e)?.to_owned(), uuid::Uuid::new_v4().to_string()))).collect();
    let timestamp = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis,true);
    let mut forked = Vec::new();
    for mut e in entries { if e.get("type").and_then(Value::as_str)==Some("progress") { continue; }
        let old_id = entry_id(&e).unwrap().to_owned();
        if let Some(obj) = e.as_object_mut() {
            obj.insert("uuid".into(), ids[&old_id].clone().into()); obj.insert("sessionId".into(),new_id.clone().into());
            for key in ["parentUuid","logicalParentUuid"] { if let Some(parent) = obj.get(key).and_then(Value::as_str).map(str::to_owned) { obj.insert(key.into(), ids.get(&parent).cloned().map(Value::String).unwrap_or(Value::Null)); } }
            obj.insert("isSidechain".into(),false.into()); obj.insert("forkedFrom".into(),json!({"sessionId":id,"messageUuid":old_id}));
            for key in ["teamName","agentName","sessionKind","slug","sourceToolAssistantUUID"] { obj.remove(key); }
            if obj.get("type").and_then(Value::as_str)==Some("system") && obj.get("subtype").and_then(Value::as_str)==Some("model_refusal_fallback") { obj.insert("neutralizedByFork".into(),true.into()); }
        }
        forked.push(e);
    }
    if let Some(last) = forked.last_mut().and_then(Value::as_object_mut) { last.insert("timestamp".into(),timestamp.clone().into()); }
    let original_title = title.filter(|s| !s.trim().is_empty()).map(str::to_owned).or_else(|| Some("Forked session (fork)".into())).unwrap();
    forked.push(json!({"type":"custom-title","sessionId":new_id,"customTitle":original_title,"uuid":uuid::Uuid::new_v4().to_string(),"timestamp":timestamp}));
    let new_path = path.with_file_name(format!("{new_id}.jsonl"));
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&new_path).await?;
    for entry in forked { file.write_all(serde_json::to_string(&entry)?.as_bytes()).await?; file.write_all(b"\n").await?; }
    file.flush().await?;
    Ok(new_id)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn reads_chain_subagent_and_forks_transcript() {
        let root = std::env::temp_dir().join(format!("turin-claude-transcripts-{}",uuid::Uuid::new_v4()));
        let dir = root.join("work"); fs::create_dir_all(&dir).await.unwrap();
        let options = TranscriptOptions { dir: Some(dir.clone()), config_dir: Some(root.join("config")), ..Default::default() };
        let id = uuid::Uuid::new_v4().to_string();
        let folder = projects_dir(&options).join(project_key(&dir)); fs::create_dir_all(&folder).await.unwrap();
        let file = folder.join(format!("{id}.jsonl"));
        let entries = [
            json!({"type":"user","uuid":"u1","parentUuid":null,"sessionId":id,"timestamp":"2026-10-01T00:00:00.000Z","message":{"role":"user","content":"first"}}),
            json!({"type":"assistant","uuid":"a1","parentUuid":"u1","sessionId":id,"timestamp":"2026-10-01T00:00:01.000Z","message":{"role":"assistant","content":[{"type":"text","text":"reply"}]}}),
            json!({"type":"assistant","uuid":"side","parentUuid":"u1","sessionId":id,"isSidechain":true,"message":{"role":"assistant","content":[]}}),
            json!({"type":"custom-title","customTitle":"Example","sessionId":id}),
        ];
        fs::write(&file, entries.iter().map(|e| format!("{}\n",serde_json::to_string(e).unwrap())).collect::<String>()).await.unwrap();
        let messages = get_session_messages(&id,&options).await.unwrap();
        assert_eq!(messages.iter().map(|m| m.uuid.as_str()).collect::<Vec<_>>(),["u1","a1"]);
        let summaries = list_sessions(&options).await.unwrap(); assert_eq!(summaries[0].summary,"Example");
        let sub = folder.join(&id).join("subagents"); fs::create_dir_all(&sub).await.unwrap();
        fs::write(sub.join("agent-child.jsonl"),format!("{}\n",json!({"type":"user","uuid":"su","sessionId":id,"message":{"role":"user","content":"child"}}))).await.unwrap();
        fs::write(sub.join("agent-child.meta.json"),r#"{"toolUseId":"toolu_1","parentAgentId":"parent"}"#).await.unwrap();
        assert_eq!(get_subagent_messages(&id,"child",&options).await.unwrap()[0].parent_tool_use_id.as_deref(),Some("toolu_1"));
        let fork = fork_session(&id,&options,Some("a1"),Some("Branch")).await.unwrap();
        let fork_entries = read_entries(&folder.join(format!("{fork}.jsonl"))).await.unwrap();
        assert_eq!(fork_entries.len(),3);
        assert_eq!(fork_entries[1]["parentUuid"],fork_entries[0]["uuid"]);
        assert_eq!(fork_entries[2]["customTitle"],"Branch");
        fs::remove_dir_all(root).await.unwrap();
    }
    #[test]
    fn project_key_matches_cli_path_replacement() {
        assert_eq!(project_key(Path::new("/tmp/a b")),"-tmp-a-b");
    }
}

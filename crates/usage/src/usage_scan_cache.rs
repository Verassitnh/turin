//! Port of upstream/apps/server/src/usage/usageScanCache.ts.
//!
//! Durable per-file scan cache.
//!
//! Transcripts are append-only and a file that has not changed can never yield
//! different usage, so parsed records are keyed by `(size, mtime)` and reused.
//!
//! Caching *per file* rather than per day is deliberate. It is timezone
//! independent, so changing the reporting zone does not invalidate anything, and
//! it keeps cross-file de-duplication exact: cached entries are de-duplicated
//! within their own file only, and the aggregator still applies the global
//! dedupe pass over the small surviving key set.
//!
//! Deviation: upstream memoises each entry's JSON fragment by identity between
//! persists. Here every persist re-encodes the whole cache. Same document shape,
//! more CPU per persist; see PORTING.md.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};

use indexmap::IndexMap;
use serde_json::{Map, Value, json};
use turin_contracts::usage::UsageProviderKind;
use turin_provider_core::usage::{TranscriptUsageFormat, UsageRecord, UsageSpeed};

use crate::usage_transcript_reader::{GUARD_LENGTH, TranscriptParsePosition};

// v2: Codex fork-copy suppression changed what a file parses to, so v1
// entries would keep serving double-counted records forever.
// v3: entries carry the parse position and reducer state so a grown file
// re-parses only its appended bytes instead of starting over.
// v4: records carry Claude fast mode, which v3 rows never captured.
// v5: Codex records carry their service tier. v4 rows store speed the same
// way, so v4 entries still load; see `decode_scan_cache` for v4 stateful entries.
const USAGE_SCAN_CACHE_VERSION: u64 = 5;
const SPEED_COMPATIBLE_SINCE_VERSION: u64 = 4;

/// Each cache version writes its own file in the state directory. An older
/// server sharing that directory cannot read a newer cache and would replace
/// it, dropping saved usage for deleted transcripts. Separate files keep both.
/// A v5 server reads the legacy (v4) file once, when its own file is missing.
pub const SCAN_CACHE_FILE_NAME: &str = "usage-scan-cache-v5.json";
pub const LEGACY_SCAN_CACHE_FILE_NAME: &str = "usage-scan-cache.json";

/// Serialised as the index into this list.
const SPEEDS: [UsageSpeed; 3] = [
    UsageSpeed::Standard,
    UsageSpeed::Fast,
    UsageSpeed::Ultrafast,
];

static NEXT_ENTRY_ID: AtomicU64 = AtomicU64::new(1);

/// One cached transcript. Entries are replaced, never edited, so an entry's id
/// names its content: the writer memoises each entry's JSON under that id.
#[derive(Debug, Clone)]
pub struct CachedFile {
    /// `-1` for a legacy entry whose size was never recorded; it never matches a file.
    pub size: i64,
    pub mtime_ms: f64,
    pub provider: UsageProviderKind,
    /// Records from newline-terminated lines, up to `position.resume_offset`.
    pub records: Vec<UsageRecord>,
    /// Records from a trailing segment the writer had not newline-terminated at
    /// parse time. Kept apart from `records` because an incremental parse
    /// re-reads that segment and would otherwise double count it.
    pub tail_records: Vec<UsageRecord>,
    pub position: TranscriptParsePosition,
    id: u64,
}

impl PartialEq for CachedFile {
    fn eq(&self, other: &Self) -> bool {
        self.size == other.size
            && self.mtime_ms == other.mtime_ms
            && self.provider == other.provider
            && self.records == other.records
            && self.tail_records == other.tail_records
            && self.position == other.position
    }
}

impl CachedFile {
    pub fn new(
        size: i64,
        mtime_ms: f64,
        provider: UsageProviderKind,
        records: Vec<UsageRecord>,
        tail_records: Vec<UsageRecord>,
        position: TranscriptParsePosition,
    ) -> Self {
        Self {
            size,
            mtime_ms,
            provider,
            records,
            tail_records,
            position,
            id: NEXT_ENTRY_ID.fetch_add(1, Ordering::Relaxed),
        }
    }
}

/// Insertion order is kept: the aggregator's first-seen dedupe follows walk order.
pub type ScanCache = IndexMap<String, CachedFile>;

/// Interning tables: each model and session string is stored once and referenced
/// by index from the record rows.
#[derive(Default)]
struct InternTables {
    models: Vec<String>,
    sessions: Vec<String>,
    model_index: HashMap<String, usize>,
    session_index: HashMap<String, usize>,
}

impl InternTables {
    fn model(&mut self, value: &str) -> usize {
        intern(&mut self.models, &mut self.model_index, value)
    }

    fn session(&mut self, value: &str) -> usize {
        intern(&mut self.sessions, &mut self.session_index, value)
    }
}

fn intern(table: &mut Vec<String>, index: &mut HashMap<String, usize>, value: &str) -> usize {
    if let Some(existing) = index.get(value) {
        return *existing;
    }
    let next = table.len();
    table.push(value.to_owned());
    index.insert(value.to_owned(), next);
    next
}

fn speed_index(speed: UsageSpeed) -> u64 {
    SPEEDS
        .iter()
        .position(|candidate| *candidate == speed)
        .unwrap_or(0) as u64
}

fn serialize_record(record: &UsageRecord, tables: &mut InternTables) -> Value {
    json!([
        record.timestamp_ms,
        tables.model(&record.model),
        tables.session(&record.session_id),
        record.totals.uncached_input_tokens,
        record.totals.cached_input_tokens,
        record.totals.cache_creation_tokens,
        record.totals.output_tokens,
        record.totals.reasoning_tokens,
        record.dedupe_key,
        record.reported_cost_usd,
        speed_index(record.speed),
        match &record.rate_model {
            None => Value::Null,
            Some(rate_model) => json!(tables.model(rate_model)),
        },
    ])
}

fn serialize_file(entry: &CachedFile, tables: &mut InternTables) -> Value {
    json!({
        "s": entry.size,
        "m": entry.mtime_ms,
        "p": entry.provider,
        "r": entry.records.iter().map(|record| serialize_record(record, tables)).collect::<Vec<_>>(),
        "t": entry.tail_records.iter().map(|record| serialize_record(record, tables)).collect::<Vec<_>>(),
        "o": entry.position.resume_offset,
        "gl": entry.position.guard_length,
        "gh": entry.position.guard_hash,
        "cs": entry.position.state,
    })
}

/// Serialises the cache as one document, with fresh intern tables.
pub fn encode_scan_cache(cache: &ScanCache) -> Value {
    serde_json::from_str(&ScanCacheWriter::default().write(cache, &Map::new()))
        .expect("the writer emits valid JSON")
}

/// Writes the cache to JSON text. It keeps its intern tables and each entry's
/// JSON between writes, so a persist re-encodes only the entries that changed
/// (tables only grow, so a memoised entry's indexes stay valid).
#[derive(Default)]
pub struct ScanCacheWriter {
    tables: InternTables,
    /// Entry JSON by path, with the entry id it was encoded from.
    fragments: HashMap<String, (u64, String)>,
}

impl ScanCacheWriter {
    /// `extra` keys (the scanned-source table, for one) lead the document, as in the TS writer.
    pub fn write(&mut self, cache: &ScanCache, extra: &Map<String, Value>) -> String {
        let mut files = Vec::with_capacity(cache.len());
        for (path, entry) in cache {
            let fragment = match self.fragments.get(path) {
                Some((id, fragment)) if *id == entry.id => fragment.clone(),
                _ => {
                    let fragment = serialize_file(entry, &mut self.tables).to_string();
                    self.fragments
                        .insert(path.clone(), (entry.id, fragment.clone()));
                    fragment
                }
            };
            files.push(format!("{}:{fragment}", Value::String(path.clone())));
        }
        // Entries removed from the cache leave their fragments behind otherwise.
        self.fragments.retain(|path, _| cache.contains_key(path));

        // Encoded after the files, which may have added to the intern tables.
        let mut head = extra.clone();
        head.insert("version".to_owned(), json!(USAGE_SCAN_CACHE_VERSION));
        head.insert("models".to_owned(), json!(self.tables.models));
        head.insert("sessions".to_owned(), json!(self.tables.sessions));
        let head = Value::Object(head).to_string();
        format!(
            "{},\"files\":{{{}}}}}",
            &head[..head.len() - 1],
            files.join(",")
        )
    }
}

/// Whether a JSON number is a safe integer (JS `Number.isSafeInteger`).
fn safe_integer(value: &Value) -> Option<u64> {
    let number = value.as_f64()?;
    if number.fract() == 0.0 && (0.0..=9_007_199_254_740_991.0).contains(&number) {
        Some(number as u64)
    } else {
        None
    }
}

/// Rebuilds the cache from a parsed document.
///
/// Anything malformed yields an empty cache rather than an error: a corrupt
/// cache should cost one cold scan, never a broken page. Entries of a provider
/// without a transcript format (`format_for` returns `None`) are dropped.
pub fn decode_scan_cache<'a, F>(document: &Value, format_for: F) -> ScanCache
where
    F: Fn(UsageProviderKind) -> Option<&'a TranscriptUsageFormat>,
{
    let mut cache = ScanCache::new();
    let Some(root) = document.as_object() else {
        return cache;
    };
    let Some(version) = root.get("version").and_then(Value::as_u64) else {
        return cache;
    };
    if !(SPEED_COMPATIBLE_SINCE_VERSION..=USAGE_SCAN_CACHE_VERSION).contains(&version) {
        return cache;
    }
    let (Some(models), Some(sessions), Some(files)) = (
        root.get("models").and_then(Value::as_array),
        root.get("sessions").and_then(Value::as_array),
        root.get("files").and_then(Value::as_object),
    ) else {
        return cache;
    };

    // The intern tables must be all strings: a numeric entry would land in a
    // record's model and crash aggregation. A corrupt table rejects the whole cache.
    let Some(models) = strings(models) else {
        return cache;
    };
    let Some(sessions) = strings(sessions) else {
        return cache;
    };

    for (path, raw) in files {
        let Some(entry) = raw.as_object() else {
            continue;
        };
        let (Some(size), Some(mtime_ms)) = (
            entry.get("s").and_then(Value::as_f64),
            entry.get("m").and_then(Value::as_f64),
        ) else {
            continue;
        };
        let Some(provider) = entry
            .get("p")
            .and_then(|value| serde_json::from_value::<UsageProviderKind>(value.clone()).ok())
        else {
            continue;
        };
        let Some(format) = format_for(provider) else {
            continue;
        };
        let (Some(records_raw), Some(tail_raw)) = (
            entry.get("r").and_then(Value::as_array),
            entry.get("t").and_then(Value::as_array),
        ) else {
            continue;
        };
        // Position fields feed byte offsets and a buffer size in the reader, so
        // anything outside their real ranges must reject the entry: a bogus guard
        // length would otherwise fail every parse of the file, silently dropping
        // its usage instead of costing the documented cold re-parse.
        let (Some(resume_offset), Some(guard_length), Some(guard_hash)) = (
            entry.get("o").and_then(safe_integer),
            entry.get("gl").and_then(safe_integer),
            entry.get("gh").and_then(safe_integer),
        ) else {
            continue;
        };
        if guard_length > GUARD_LENGTH
            || guard_length > resume_offset
            || guard_hash > u64::from(u32::MAX)
        {
            continue;
        }
        let saved_state = entry.get("cs").cloned().unwrap_or(Value::Null);
        // v4 records of stateful formats (Codex) predate service tiers, so they
        // all priced as standard. Keep them, because the rollout may be gone, but
        // make a live rollout re-parse whole: no file has size -1, and a zero
        // position cannot resume.
        let legacy = format.state.is_some() && version < USAGE_SCAN_CACHE_VERSION;
        // A corrupt state disqualifies the entry: resuming with it would attach
        // appended usage to the wrong model or replay fork-copied history.
        if !legacy && !is_valid_state(format, &saved_state) {
            continue;
        }

        let (Some(records), Some(tail_records)) = (
            decode_records(records_raw, provider, &models, &sessions),
            decode_records(tail_raw, provider, &models, &sessions),
        ) else {
            continue;
        };

        let position = if legacy {
            TranscriptParsePosition {
                resume_offset: 0,
                guard_length: 0,
                guard_hash: 0,
                state: Value::Null,
            }
        } else {
            TranscriptParsePosition {
                resume_offset,
                guard_length,
                guard_hash: guard_hash as u32,
                state: saved_state,
            }
        };
        cache.insert(
            path.clone(),
            CachedFile::new(
                if legacy { -1 } else { size as i64 },
                mtime_ms,
                provider,
                records,
                tail_records,
                position,
            ),
        );
    }

    cache
}

fn strings(values: &[Value]) -> Option<Vec<String>> {
    values
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

/// `null`, or a state the format's decoder accepts. A format without state
/// accepts only `null`.
fn is_valid_state(format: &TranscriptUsageFormat, value: &Value) -> bool {
    if value.is_null() {
        return true;
    }
    match &format.state {
        Some(codec) => (codec.decode)(value).is_ok(),
        None => false,
    }
}

/// Any corrupt row disqualifies the whole entry. Keeping the survivors under the
/// original (size, mtime) would read as a valid warm hit and the file would never
/// be re-parsed, silently losing the dropped rows' usage.
fn decode_records(
    rows: &[Value],
    provider: UsageProviderKind,
    models: &[String],
    sessions: &[String],
) -> Option<Vec<UsageRecord>> {
    rows.iter()
        .map(|row| decode_record(row, provider, models, sessions))
        .collect()
}

fn decode_record(
    row: &Value,
    provider: UsageProviderKind,
    models: &[String],
    sessions: &[String],
) -> Option<UsageRecord> {
    let row = row.as_array()?;
    if row.len() < 11 {
        return None;
    }
    let timestamp_ms = row[0].as_f64().filter(|value| value.is_finite())?;
    let model = models.get(safe_integer(&row[1])? as usize)?.clone();
    let session_id = safe_integer(&row[2])
        .and_then(|index| sessions.get(index as usize))
        .cloned()
        .unwrap_or_default();
    let count = |value: &Value| safe_integer(value);
    let totals = turin_contracts::usage::UsageTokenTotals {
        uncached_input_tokens: count(&row[3])?,
        cached_input_tokens: count(&row[4])?,
        cache_creation_tokens: count(&row[5])?,
        output_tokens: count(&row[6])?,
        reasoning_tokens: count(&row[7])?,
    };
    let dedupe_key = row[8].as_str().map(str::to_owned);
    let reported_cost_usd = row[9].as_f64();
    let speed = row[10]
        .as_u64()
        .and_then(|index| SPEEDS.get(index as usize).copied())?;
    let rate_model = match row.get(11) {
        None | Some(Value::Null) => None,
        Some(index) => Some(models.get(safe_integer(index)? as usize)?.clone()),
    };
    Some(UsageRecord {
        provider,
        timestamp_ms,
        model,
        rate_model,
        session_id,
        totals,
        reported_cost_usd,
        speed,
        dedupe_key,
    })
}

/// Keeps saved usage after transcript cleanup, until the reporting retention expires.
pub fn prune_scan_cache(cache: &mut ScanCache, retention_cutoff_ms: f64) -> usize {
    let before = cache.len();
    cache.retain(|_, entry| entry.mtime_ms >= retention_cutoff_ms);
    before - cache.len()
}

/// Within-file de-duplication, applied before an entry is cached.
///
/// Callers stitching an incremental parse together pass one `seen` set across
/// the line and tail record batches so the whole file stays deduplicated as a
/// unit; the set is mutated in place.
pub fn dedupe_within_file(records: &[UsageRecord], seen: &mut HashSet<String>) -> Vec<UsageRecord> {
    let mut kept = Vec::new();
    for record in records {
        if record
            .dedupe_key
            .as_ref()
            .is_some_and(|key| !seen.insert(key.clone()))
        {
            continue;
        }
        kept.push(record.clone());
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_formats::test_format_map;
    use serde_json::json;
    use turin_contracts::usage::UsageTokenTotals;

    fn record(overrides: impl FnOnce(&mut UsageRecord)) -> UsageRecord {
        let mut record = UsageRecord {
            provider: UsageProviderKind::Claude,
            timestamp_ms: 1_786_000_000_000.0,
            model: "claude-fable-5".to_owned(),
            rate_model: None,
            session_id: "session-a".to_owned(),
            totals: UsageTokenTotals {
                uncached_input_tokens: 2,
                cached_input_tokens: 1000,
                cache_creation_tokens: 10,
                output_tokens: 50,
                reasoning_tokens: 0,
            },
            reported_cost_usd: None,
            speed: UsageSpeed::Standard,
            dedupe_key: Some("msg_1:".to_owned()),
        };
        overrides(&mut record);
        record
    }

    fn plain_record() -> UsageRecord {
        record(|_| {})
    }

    fn position(overrides: impl FnOnce(&mut TranscriptParsePosition)) -> TranscriptParsePosition {
        let mut position = TranscriptParsePosition {
            resume_offset: 120,
            guard_length: 64,
            guard_hash: 0xdead_beef,
            state: Value::Null,
        };
        overrides(&mut position);
        position
    }

    fn cached(path_mtime: f64, records: Vec<UsageRecord>) -> CachedFile {
        CachedFile::new(
            records.len() as i64 * 10,
            path_mtime,
            UsageProviderKind::Claude,
            records,
            Vec::new(),
            position(|_| {}),
        )
    }

    fn cache_with(entries: Vec<(&str, f64, Vec<UsageRecord>)>) -> ScanCache {
        entries
            .into_iter()
            .map(|(path, mtime, records)| (path.to_owned(), cached(mtime, records)))
            .collect()
    }

    fn decode(document: &Value) -> ScanCache {
        let formats = test_format_map();
        decode_scan_cache(document, |provider| formats.get(&provider))
    }

    fn roundtrip(cache: &ScanCache) -> ScanCache {
        let encoded = serde_json::to_string(&encode_scan_cache(cache)).unwrap();
        decode(&serde_json::from_str(&encoded).unwrap())
    }

    fn file_mut<'a>(document: &'a mut Value, path: &str) -> &'a mut Value {
        &mut document["files"][path]
    }

    #[test]
    fn restores_records_unchanged() {
        let mut original = cache_with(vec![
            (
                "/a.jsonl",
                100.0,
                vec![
                    plain_record(),
                    record(|record| {
                        record.dedupe_key = Some("msg_2:".to_owned());
                        record.model = "claude-opus-5-5".to_owned();
                        record.speed = UsageSpeed::Fast;
                    }),
                ],
            ),
            (
                "/b.jsonl",
                200.0,
                vec![record(|record| {
                    record.session_id = "session-b".to_owned();
                    record.reported_cost_usd = Some(1.5);
                })],
            ),
        ]);
        let mut codex = cached(
            400.0,
            vec![record(|record| {
                record.provider = UsageProviderKind::Codex;
                record.model = "gpt-6-astra".to_owned();
                record.dedupe_key = None;
                record.speed = UsageSpeed::Ultrafast;
            })],
        );
        codex.size = 80;
        codex.provider = UsageProviderKind::Codex;
        codex.position = position(|position| {
            position.state = json!({
                "session": "session-c",
                "model": "gpt-6-astra",
                "speed": "ultrafast",
                "last": null,
            });
        });
        original.insert("/codex.jsonl".to_owned(), codex);

        let restored = roundtrip(&original);

        assert_eq!(restored.len(), 3);
        for path in ["/a.jsonl", "/b.jsonl", "/codex.jsonl"] {
            assert_eq!(restored.get(path), original.get(path), "{path}");
        }
    }

    #[test]
    fn drops_an_entry_whose_persisted_parse_state_is_corrupt() {
        // Resuming with a bad reducer state would attach appended usage to the
        // wrong model or replay fork-copied history; that entry must cold parse.
        let mut encoded =
            encode_scan_cache(&cache_with(vec![("/a.jsonl", 100.0, vec![plain_record()])]));
        file_mut(&mut encoded, "/a.jsonl")["cs"] = json!({ "model": 42 });

        assert!(!decode(&encoded).contains_key("/a.jsonl"));
    }

    #[test]
    fn drops_an_entry_whose_guard_length_is_outside_the_supported_range() {
        // The guard length sizes a buffer in the reader; a bogus value would make
        // every parse of that file fail and silently drop its usage.
        let mut encoded =
            encode_scan_cache(&cache_with(vec![("/a.jsonl", 100.0, vec![plain_record()])]));
        file_mut(&mut encoded, "/a.jsonl")["gl"] = json!(1e20);

        assert!(!decode(&encoded).contains_key("/a.jsonl"));
    }

    #[test]
    fn drops_an_entry_whose_speed_is_not_a_known_index() {
        let mut encoded = encode_scan_cache(&cache_with(vec![(
            "/a.jsonl",
            100.0,
            vec![record(|record| record.speed = UsageSpeed::Fast)],
        )]));
        file_mut(&mut encoded, "/a.jsonl")["r"][0][10] = json!(true);

        assert!(!decode(&encoded).contains_key("/a.jsonl"));
    }

    #[test]
    fn rejects_a_document_from_before_records_carried_a_speed() {
        let mut encoded =
            encode_scan_cache(&cache_with(vec![("/a.jsonl", 100.0, vec![plain_record()])]));
        encoded["version"] = json!(3);

        assert!(decode(&encoded).is_empty());
    }

    #[test]
    fn writes_the_whole_cache_with_its_extra_keys_and_restores_it() {
        let cache = cache_with(vec![
            ("/a.jsonl", 100.0, vec![plain_record()]),
            (
                "/b.jsonl",
                200.0,
                vec![record(|record| record.session_id = "session-b".to_owned())],
            ),
        ]);
        let mut extra = Map::new();
        extra.insert(
            "sources".to_owned(),
            json!({ "claude\u{0}/projects": { "dir": "/projects", "volumeId": "1:2" } }),
        );

        let text = ScanCacheWriter::default().write(&cache, &extra);
        let document: Value = serde_json::from_str(&text).unwrap();

        assert_eq!(decode(&document), cache);
        assert_eq!(document["sources"], extra["sources"]);
    }

    #[test]
    fn interns_repeated_model_and_session_strings() {
        let encoded = encode_scan_cache(&cache_with(vec![(
            "/a.jsonl",
            100.0,
            vec![
                plain_record(),
                record(|record| record.dedupe_key = Some("msg_2:".to_owned())),
                plain_record(),
            ],
        )]));

        assert_eq!(encoded["models"], json!(["claude-fable-5"]));
        assert_eq!(encoded["sessions"], json!(["session-a"]));
    }

    #[test]
    fn treats_a_corrupt_or_foreign_document_as_an_empty_cache() {
        // A bad cache should cost one cold scan, never a broken page.
        assert!(decode(&Value::Null).is_empty());
        assert!(decode(&json!("nonsense")).is_empty());
        assert!(
            decode(&json!({ "version": 999, "models": [], "sessions": [], "files": {} }))
                .is_empty()
        );
    }

    #[test]
    fn skips_malformed_file_entries_but_keeps_good_ones() {
        let mut encoded = encode_scan_cache(&cache_with(vec![(
            "/good.jsonl",
            100.0,
            vec![plain_record()],
        )]));
        encoded["files"]["/bad.jsonl"] = json!({ "s": "nope", "m": 1, "p": "claude", "r": [] });

        let restored = decode(&encoded);
        assert_eq!(restored.keys().collect::<Vec<_>>(), vec!["/good.jsonl"]);
    }

    #[test]
    fn rejects_the_whole_cache_when_an_intern_table_holds_a_non_string() {
        // A numeric model would reach aggregation and break pricing there.
        let mut encoded =
            encode_scan_cache(&cache_with(vec![("/a.jsonl", 100.0, vec![plain_record()])]));
        encoded["models"] = json!([1]);

        assert!(decode(&encoded).is_empty());
    }

    #[test]
    fn drops_the_whole_entry_when_any_row_is_corrupt() {
        // Keeping the surviving rows under the original (size, mtime) would read
        // as a valid warm hit and the file would never be re-parsed.
        let mut encoded = encode_scan_cache(&cache_with(vec![(
            "/a.jsonl",
            100.0,
            vec![
                plain_record(),
                record(|record| record.dedupe_key = Some("msg_2:".to_owned())),
            ],
        )]));
        file_mut(&mut encoded, "/a.jsonl")["r"][1][3] = json!("not-a-number");

        assert!(!decode(&encoded).contains_key("/a.jsonl"));
    }

    #[test]
    fn prune_drops_entries_older_than_retention_and_keeps_entries_whose_file_is_gone() {
        let mut cache = cache_with(vec![("/old.jsonl", 500.0, vec![plain_record()])]);
        assert_eq!(prune_scan_cache(&mut cache, 1000.0), 1);
        assert!(cache.is_empty());

        // A file that disappeared but is recent stays: its usage is still saved.
        let mut cache = cache_with(vec![("/gone.jsonl", 5000.0, vec![plain_record()])]);
        prune_scan_cache(&mut cache, 1000.0);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn dedupe_within_file_keeps_the_first_record_per_dedupe_key() {
        let first = record(|record| record.totals.output_tokens = 1);
        let second = record(|record| record.totals.output_tokens = 999);
        let third = record(|record| record.dedupe_key = Some("msg_2:".to_owned()));
        let kept = dedupe_within_file(&[first, second, third], &mut HashSet::new());

        assert_eq!(kept.len(), 2);
        assert_eq!(kept[0].totals.output_tokens, 1);
    }

    #[test]
    fn dedupe_within_file_keeps_every_record_without_a_dedupe_key() {
        let keyless = record(|record| record.dedupe_key = None);
        assert_eq!(
            dedupe_within_file(&[keyless.clone(), keyless], &mut HashSet::new()).len(),
            2
        );
    }

    #[test]
    fn writer_reuses_unchanged_entries_and_forgets_removed_ones() {
        let mut writer = ScanCacheWriter::default();
        let mut cache = cache_with(vec![
            ("/a.jsonl", 100.0, vec![plain_record()]),
            (
                "/b.jsonl",
                200.0,
                vec![record(|record| record.session_id = "session-b".to_owned())],
            ),
        ]);
        let first = writer.write(&cache, &Map::new());
        let a_fragment = writer.fragments["/a.jsonl"].1.clone();

        // Replacing /b gives it a new id; /a keeps its id and its memoised JSON.
        cache.insert(
            "/b.jsonl".to_owned(),
            cached(
                300.0,
                vec![record(|record| record.session_id = "session-c".to_owned())],
            ),
        );
        let second = writer.write(&cache, &Map::new());
        assert_eq!(writer.fragments["/a.jsonl"].1, a_fragment);
        assert_eq!(decode(&serde_json::from_str(&second).unwrap()), cache);

        // A removed entry leaves no fragment behind.
        cache.shift_remove("/a.jsonl");
        let third = writer.write(&cache, &Map::new());
        assert_eq!(writer.fragments.len(), 1);
        assert_eq!(decode(&serde_json::from_str(&third).unwrap()), cache);
        assert_ne!(first, third);
    }
}

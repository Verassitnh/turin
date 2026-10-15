# QC wf_0fd743c1-ca2 — T01 contracts (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 9 · refuted: 1

## Lead report

1. [low] Optional and NullOr keys accept null or absent values upstream rejects. Affected: orchestration_v2.rs:168-179 (native_id, fingerprint) and about 600 other attributes there; git.rs:232-238 (is_remote, remote_name, worktree_path) and git.rs:560; checkpoint_diff.rs:55, 66, 87 (ignore_whitespace accepts null); provider_runtime.rs:777-779 and orchestration_v2.rs:2143-2145 (initial_answer accepts null). Problem: serde Option<T> accepts null on optional keys and absent on required NullOr keys. Upstream Effect rejects both. PORTING.md rows 61-62 prescribe this shape with no null guard, and only settings.rs:51 (optional_key) handles it. Valid T3 data and the DB are unaffected. Fix: either move the optional_key codec into base_schemas and add a required-nullable codec (Option deserialize with no default) for kept modules, with null/absent rejection tests, or log the leniency in PORTING.md Deviations and correct rows 61-62. Minimum acceptable fix is the Deviations entry.

2. [low] orchestration_v2.rs:3309-3311. `#[serde(default)] pub payload: serde_json::Value` accepts an absent key. Upstream orchestrationV2.ts:1720 (Schema.Unknown in a struct) rejects a missing key with "Missing key". The doc comment "an absent key decodes (to null)" misstates upstream. Fix: drop `#[serde(default)]` to match upstream, or add a Deviations row and correct the comment.

3. [low] Duplicated UTF-16 helper. Local `fn utf16_len` at assistant_citations.rs:17-19, thread_search.rs:7-9, pull_request.rs:16-18 and provider_setup.rs:17-19 duplicates base_schemas.rs:42-44 `js_len`. PORTING.md:46-47 forbids hand-rolled equivalents. Fix: delete the four helpers and call crate::base_schemas::js_len. Also replace the inline `.encode_utf16().count()` at settings.rs:740, 788, provider.rs:107, orchestration_v2.rs:2301, 2341, 5988, 6032, provider_instance.rs:31, 137 and provider_setup.rs:219.

4. [low] usage_limit_source_id.rs:14. `pub struct UsageLimitSourceId(pub Arc<str>)` exposes the inner field, which breaks the opaque newtype shape in PORTING.md:70. Nothing outside the file reads `.0`. Fix: make the field private, and add a Deviations row explaining why this plain-string brand does not use id! (id! trims and rejects blank input, while upstream usageLimitSourceId.ts:8 accepts ""). The other cited types (source_control.rs:20, usage.rs:45, the provider_slug!/context_id! macros) are slugs or dates, not ids, and need no change.

5. [low] pull_request.rs:1952-1953 and :1966 say "see PORTING.md Deviations", but the Deviations table has no pull_request.rs row. The dropped Forgejo remoteUrl branch in pull_request_host_of (around pull_request.rs:1979-1990) is also unlogged. Fix: add a Deviations row covering the provider_requirement cut hosts and the Forgejo branch, or repoint the comments to the cut list at PORTING.md:148. source_control.rs needs no entry.

6. [low] checkpoint_diff.rs:127-163. Tests cover only the two inverted-range negative cases. Missing versus checkpointDiff.test.ts:15-75: the positive OrchestrationGetTurnDiffInput decode, ignoreWhitespace on turn-diff input, and OrchestrationGetFullThreadDiffInput decode (checkpoint_diff.rs:84-88 is untested). Fix: port the three missing cases.

7. [low] provider_runtime.rs:2282 and :2293, and orchestration_v2_a.rs:1428-1436. initialAnswer is tested only with "". Missing versus userInputQuestion.test.ts:10-22: absent to None, whitespace-preserving text, and the 500-line string, for both question types. Fix: port those cases. The code itself is correct.

VERDICT: PASS (no critical/high; all seven items are low severity).

## Confirmed findings (raw)

### [low] Optional/NullOr keys outside settings accept null (optional) or absent (NullOr); upstream rejects both
- `crates/contracts/src/orchestration_v2.rs:` · dimension: wire-fidelity · upheld 1/2
- Evidence: Upstream orchestrationV2.ts:103 `fingerprint: Schema.optional(TrimmedNonEmptyString)` and :101 `nativeId: Schema.NullOr(TrimmedNonEmptyString)`. Effect Schema: optional(X) = UndefinedOr(X) on an optional key, so null fails for a string; NullOr on a required key fails when absent (upstream/apps/server/node_modules/effect/src/Schema.ts:2459). Rust orchestration_v2.rs:168-179: `pub native_id: Option<TrimmedNonEmptyString>` has no default and no skip, so serde treats an absent key as None; `fingerprint` is `#[serde(default, skip_serializing_if)]` with no null guard, so null becomes None. Probe against turin_contracts: `{...,"fingerprint":null}` -> Ok(None); `{"driver","strength"}` without nativeId -> Ok(None). Deviations (PORTING.md:210) documents the null guard only for settings.rs (`optional_key`). Valid T3 data is unaffected, but invalid input is accepted, and the deviation is undocumented for every other module.
- Fix: Add an optional_key/required-nullable guard to base_schemas (the settings.rs helper, moved there) and apply it to every non-nullable optional key and every required NullOr key in the kept modules, or record the leniency in the PORTING.md Deviations table with the rationale.

### [low] Schema.Unknown payload absent-key decodes to null and re-encodes as "payload": null
- `crates/contracts/src/orchestration_v2.rs:` · dimension: wire-fidelity · upheld 1/2
- Evidence: Rust orchestration_v2.rs:3309-3311: `/// Schema.Unknown: an absent key decodes (to null).` `#[serde(default)] pub payload: serde_json::Value`. Upstream orchestrationV2.ts:1720 `payload: Schema.Unknown`. Effect decodes an absent key of an Unknown-typed property as undefined, and JSON.stringify omits undefined keys. Probe: a raw provider event with no payload key, once decoded and re-encoded by turin_contracts, contains `"payload":null`. The upstream encoding omits the key. The byte difference only appears on re-encode of input that upstream itself never writes.
- Fix: Keep the decode leniency if desired, but make encode skip the key when the value was absent (e.g. Option<Value> with skip_serializing_if, or a presence flag), or add a Deviations row describing the re-encode difference.

### [low] Schema.optional/optionalKey fields accept explicit null outside settings.rs
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/git.rs:233` · dimension: tests-and-rules · upheld 2/2
- Evidence: git.rs:233-235 `pub is_remote: Option<bool>` / `pub remote_name: Option<TrimmedNonEmptyString>` use only `#[serde(default, skip_serializing_if)]`, so serde treats null as None. Upstream git.ts:86-87 `isRemote: Schema.optional(Schema.Boolean)`, `remoteName: Schema.optional(...)`, and Effect's `optional` is `UndefinedOr(self)` (effect/src/Schema.ts:2459-2462). Verified with a throwaway crate: `{"name":"main","isRemote":null,...}` -> Rust Ok(None). Verified with upstream effect 4.0.4 `S.decodeUnknownSync(Struct({isRemote: S.optional(S.Boolean)}))({isRemote:null})` -> FAILS. The PORTING table's optional/optionalKey row maps to plain Option, which is wrong for this. settings.rs:51 has a file-local `optional_key` codec that handles this, and its Deviations row says it 'belongs in base_schemas if other modules want it', but it was never moved and no other kept module uses it (~hundreds of optionalKey fields in orchestration_v2.rs, provider_runtime.rs, pull_request.rs, project.rs, git.rs).
- Fix: Move the optional_key codec into base_schemas.rs, apply it to every Schema.optional/optionalKey (non-nullable) field in kept modules, and correct the PORTING table row for optional/optionalKey. Add a null-rejection test per module, as settings.rs:6909 does.

### [low] Required NullOr keys accept a missing key (VcsRef.worktree_path)
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/git.rs:238` · dimension: tests-and-rules · upheld 2/2
- Evidence: git.rs:238 `pub worktree_path: Option<TrimmedNonEmptyString>` has no serde attribute, so serde derive defaults an absent key to None. Upstream git.ts:90 `worktreePath: TrimmedNonEmptyStringSchema.pipe(Schema.NullOr)` is a required key. Verified: Rust `from_value::<VcsRef>({"name":"main","current":true,"isDefault":false})` -> Ok(None); Effect `S.Struct({name:S.String, worktreePath:S.String.pipe(S.NullOr)})` decoding `{name:'main'}` -> FAILS. The PORTING 'Schema.NullOr(X) (required key)' row says only 'Option<X>, no skip', which does not reject an absent key.
- Fix: Add a required-nullable serde codec to base_schemas (deserialize with `Option::<T>::deserialize` and no `default`, so absent fails and null is accepted). Use it on every required NullOr field, correct the PORTING NullOr row, and add an absent-key test.

### [low] Hand-rolled utf16_len duplicates base_schemas::js_len
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/assistant_citations.rs:17` · dimension: tests-and-rules · upheld 2/2
- Evidence: assistant_citations.rs:17-19, thread_search.rs:7-9, pull_request.rs:16-18 and provider_setup.rs:17-19 each define `fn utf16_len(value: &str) -> usize { value.encode_utf16().count() }`. base_schemas.rs:42-44 already provides `pub fn js_len`, used by chat_attachment.rs. PORTING says helpers live in base_schemas and 'never hand-roll equivalents'.
- Fix: Delete the four local utf16_len fns and call crate::base_schemas::js_len.

### [low] Branded ids hand-written outside the id! list, with no Deviations row
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/usage_limit_source_id.rs:14` · dimension: tests-and-rules · upheld 2/2
- Evidence: usage_limit_source_id.rs:14 `pub struct UsageLimitSourceId(pub Arc<str>)` (public inner field, so not opaque). Upstream usageLimitSourceId.ts:8 is `Schema.String.pipe(Schema.brand(...))`. Same pattern: source_control.rs:20 `SourceControlProviderKind(String)` (upstream sourceControl.ts:12-15, branded); usage.rs:45 `UsageDay(String)` (upstream usage.ts:54, branded); provider_instance.rs:35 `provider_slug!` macro and composer_context.rs:60 `context_id!` macro generate branded newtypes. PORTING says branded ids go in the id! list and 'never hand-write id newtypes'. None of these has a Deviations row.
- Fix: Make the UsageLimitSourceId field private. Either add a shared branded-slug macro to base_schemas or add a Deviations row explaining why slugs/dates can't use id!. Cover the four slug/date types in that row.

### [low] Cut-host variants dropped in pull_request.rs with a Deviations reference that does not exist
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/pull_request.rs:1952` · dimension: tests-and-rules · upheld 2/2
- Evidence: pull_request.rs:1952-1953 says 'Only GitHub is kept; upstream's forgejo, gitlab, azure-devops and bitbucket entries are cut with those hosts (see PORTING.md Deviations)'. PORTING.md's Deviations table has no pull_request.rs row. Upstream pullRequest.ts:1323-1376 has forgejo/gitlab/azure-devops/bitbucket provider-requirement entries; pull_request.rs:1966 also drops the Forgejo `locator.remoteUrl` read in pull_request_host_of. Only GitHub remains in provider_requirement (pull_request.rs:1957-1964).
- Fix: Add Deviations rows for pull_request.rs (provider_requirement table, the Forgejo remoteUrl read in pull_request_host_of) and for the source-control kinds dropped in source_control.rs, or fix the comment's reference.

### [low] checkpoint diff input tests cover 2 of 5 upstream cases
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/checkpoint_diff.rs:128` · dimension: tests-and-rules · upheld 2/2
- Evidence: checkpoint_diff.rs:128-162 has only turn_diff_range_is_validated and get_turn_diff_input_rejects_inverted_range. Upstream checkpointDiff.test.ts:17-37 also asserts 'parses turn diff input with whitespace ignoring enabled' and 'parses full thread diff input with whitespace ignoring enabled' (ignoreWhitespace round-trips). grep finds no Rust test that decodes OrchestrationGetFullThreadDiffInput (checkpoint_diff.rs:84) at all.
- Fix: Port the two missing ignoreWhitespace decode cases, including OrchestrationGetFullThreadDiffInput, into checkpoint_diff.rs tests.

### [low] initialAnswer test only checks the empty string
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T01-contracts/crates/contracts/src/provider_runtime.rs:2282` · dimension: tests-and-rules · upheld 2/2
- Evidence: provider_runtime.rs:2282 and 2293 check only `initialAnswer: ""`. Upstream userInputQuestion.test.ts:10-22 asserts that an absent initialAnswer decodes to undefined, that whitespace-only and trailing-newline text is preserved exactly ('  fix(server): keep whitespace\n\n'), and that a 500-line string round-trips, for both UserInputQuestion and OrchestrationV2UserInputQuestion. The orchestration side only encodes '' (tests/orchestration_v2_a.rs:1432-1436).
- Fix: Port the upstream cases: absent to None, exact whitespace-preserving text, and the long string, for both question types.

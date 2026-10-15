# QC wf_7e833cf1-e0c — T08 provider core (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 15 · refuted: 2

## Lead report

MEDIUM

1. crates/provider-core/src/driver.rs:117, :131 (close, Drop; channel built at :79 and :148). `cancel.send(true)` is a no-op when there are no receivers, so after close `is_cancelled()` stays false and a later `cancelled()` never resolves. Fix: use `send_replace(true)` in close and Drop, have `new()` and `Default` share one constructor, and add a test that calls close() with no live `cancelled()` future and then asserts both.

2. crates/provider-core/src/failure.rs:170-216 (`redact_url` / `strip_url_secrets`). String-splitting diverges from upstream `redactUrl` (failure.ts ~88-101). A malformed authority such as `https://user:pass/x@host/path` passes through with the credentials intact, where upstream returns `[REDACTED_URL]`. Well-formed URLs also normalize differently (`?token=x` gives no trailing slash, `:443` and host case are kept). Fix: parse with `url::Url`, clear username, password, query and fragment, return `[REDACTED_URL]` on parse failure, and add tests for both cases.

LOW

3. crates/provider-core/src/package_managed_maintenance.rs:355 (`guard.disarm()`, defined ~368-371). The guard is disarmed even on timeout, so `GroupKill` never signals the group. Only the brew pid gets SIGKILL, and grandchildren survive. This contradicts the doc comment at :321-323 and PORTING.md:243. Fix: disarm only inside the `Some(output)` branch, and add a test where a grandchild outlives the timeout.

4. crates/provider-core/src/credential_store.rs:225-233 (`KeychainSecretStore::set`). The hex secret is passed as the `-X` argv to `security add-generic-password`, so it is visible to same-user `ps` while the process runs. The backend has no callers today. Fix: use `security-framework` `set_generic_password`, or feed the command over stdin via `security -i`, before any caller adopts the backend. Keep the account name unchanged.

5. crates/provider-core/src/credential_store.rs:291-294 (`security_status`). The `security` child has no `kill_on_drop(true)`, which violates PORTING.md:95. Fix: add `.kill_on_drop(true)`. Optionally wrap the call in a timeout that maps to `SecretStoreError`.

6. crates/provider-core/src/credential_store.rs:283 (`security()` error text), plus :288-310. `args.last()` is `-w` for `get()`, so the error reads "Failed to read secret -w." and the account name is lost. stderr is also dropped for find failures. Fix: pass the secret name into `security()`, and include stderr in `detail`. `into_cause()` also needs to surface `detail` for it to be visible.

7. crates/provider-core/src/failure.rs:332-335. `match input.code { Some(code) => code.map(..), None => cause }` makes an explicit null skip the cause's code. Upstream failure.ts:157 uses `??`, which falls through. Fix: `input.code.flatten()` before the match, and add a test with `Some(None)` plus a cause carrying `code`. Latent until an adapter calls it that way.

8. crates/provider-core/src/model_manifest.rs:341-348 (`updated_at_ms`). `parse_from_rfc3339` returns 0 for date-only or offset-less strings, but upstream ModelManifest.ts:131-133 uses `Date.parse`, which accepts them. A newer remote manifest with such a stamp is rejected as older than the bundle, and a disk cache with such a stamp is dropped. The same narrow parser is at crates/provider-core/src/usage.rs:71-73 (`parse_timestamp_ms`), which has no caller yet. Fix: one shared helper that accepts Date.parse forms, or log the narrowing as a Deviation.

9. crates/provider-core/src/failure.rs:241 and :245. The key alternation needs a word boundary before `token` or `secret`, so `refresh_token=`, `client_secret=`, `id_token=` and their JSON forms pass unredacted. The gap is inherited from upstream failure.ts:129 and is not in the Deviations table. Fix: widen the key pattern (e.g. `[a-z0-9]*[_-]?(token|secret|key|password)`) or list the compound keys explicitly. Add a test and log a Deviation if the gap is kept.

10. Debug derives that print secrets (latent, grouped). crates/provider-core/src/mcp_session.rs:13 derives Debug on `McpProviderSessionConfig`, which holds the bearer `authorization_header` (:20) and `agent_device_environment` (:28). crates/provider-core/src/host_process.rs:42-47 derives Debug over the full process env, which can include provider API keys. package_managed_maintenance.rs:50-51 (`ProviderMaintenanceResolutionContext`) has the same pattern. Fix: manual Debug impls that redact values and keep keys and non-secret fields.

11. Upstream tests not ported (rule 5). crates/provider-core/src/provider_instance_registry_hydration.rs tests module (from ~:146) lacks a counterpart to upstream ProviderInstanceRegistryHydration.test.ts:100 ("folds overrides from multiple projects"). package_managed_maintenance.rs lacks ports of upstream maintenanceResolver.test.ts:882 (cache until fresh), :160 (update checks disabled) and :145 (latest preferred). Fix: port the pure cases, and log any intentional skips.

Merged as duplicates: the brew timeout guard (two entries), and the keychain argv exposure (two entries).
Dropped: FileSecretStore blocking `std::fs` inside async methods (credential_store.rs:132-174). PORTING.md has no tokio::fs rule, and the impact is negligible.

PASS (no critical/high; fix items 1 and 2 before T09 builds on the scope and URL redaction)

## Confirmed findings (raw)

### [low] Homebrew probe guard is disarmed unconditionally, so a timed-out brew never has its process group killed
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/package_managed_maintenance.rs:355` · dimension: logic-fidelity · upheld 2/2
- Evidence: Line 351: `let output = tokio::time::timeout(HOMEBREW_INFO_TIMEOUT, collect).await.ok().flatten();` then line 355: `guard.disarm();` runs on every path. `fn disarm(mut self) { self.0 = None; }` (~line 369) makes the `Drop` for `GroupKill` (~line 375) a no-op. On timeout the `collect` future is dropped, and `child` only gets `kill_on_drop` (SIGKILL to the brew pid). brew's own children survive. The doc comment at ~line 318 ('the timeout kills the whole tree, not just brew') and PORTING.md Deviations row for `maintenanceResolver.ts runHomebrew` ('a guard that kills the group on timeout') both promise the group kill, and the code does not deliver it.
- Fix: Disarm only on success: `if output.is_some() { guard.disarm(); }` (or move the disarm into the Some branch). Add a test that a probe whose child spawns a sleeping grandchild is fully reaped after the timeout.

### [low] Keychain backend passes the secret as a command-line argument
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/credential_store.rs:231` · dimension: logic-fidelity · upheld 2/2
- Evidence: `KeychainSecretStore::set` (lines ~222-233) builds `security_status(&["add-generic-password", "-U", "-s", &self.service, "-a", name, "-X", &hex])`, so the hex-encoded credential bytes are in the argv of the `security` process. Other local users can read that with `ps -ww` for as long as the process runs. The module doc calls this an opt-in backend that holds the same bytes, so the leak applies to every credential written through it.
- Fix: Do not put the value in argv. Feed the command through stdin (`security -i` with an `add-generic-password ... -X <hex>` line written to stdin, which keeps the value out of the process table), or call the Security framework directly. Keep the file store as the default.

### [low] Explicit `code: null` does not fall through to the cause's code
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/failure.rs:332` · dimension: logic-fidelity · upheld 2/2
- Evidence: Rust: `let raw_code = match input.code { Some(code) => code.map(str::to_owned), None => input.cause.and_then(cause_code) };`, so `code: Some(None)` (an explicit null) returns None. Upstream failure.ts: `const rawCode = input.code ?? stringField(input.cause, "code") ?? null;`. In JS `??` treats null as absent, so an explicit null code takes the cause's `code`. The two diverge whenever a caller passes `code: null` alongside a cause that carries a code. No caller in the crate does this yet, so it bites the T09-T12 adapters.
- Fix: Treat `Some(None)` as absent: `match input.code.flatten() { Some(code) => Some(code.to_owned()), None => input.cause.and_then(cause_code) }`. Add a test with `code: Some(None)` and a cause carrying `code`.

### [low] RFC3339-only timestamp parsing diverges from JS Date.parse for manifest updatedAt and usage timestamps
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/model_manifest.rs:345` · dimension: logic-fidelity · upheld 2/2
- Evidence: `updated_at_ms` (line 341-348) uses `chrono::DateTime::parse_from_rfc3339(date).ok()` and maps failure to 0. Upstream ModelManifest.ts `manifestUpdatedAtMs` uses `Date.parse(manifest.updatedAt)`. Node check: `Date.parse("2026-10-10")` = 1791590400000, `Date.parse("2026-10-10T23:31:10")` = 1791700270000 (local time). Rust returns 0 for both. Effect: a newer remote manifest with a date-only or offset-less updatedAt is treated as older than the bundle and rejected, and a disk cache with that stamp is dropped. The same RFC3339-only parse is in usage.rs:73 (`parse_timestamp_ms`, upstream `Date.parse`). Not listed in the Deviations table. The bundled and current remote manifests use RFC3339 `Z`, so impact today is limited.
- Fix: Either accept the same forms as Date.parse (date-only and offset-less ISO strings, interpreting offset-less as local time like V8), or log the narrowed format as a Deviation with the reason. Use one shared helper for both call sites.

### [medium] Redacted URLs are not normalized the way upstream `new URL().toString()` does it
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/failure.rs:170` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream failure.ts `redactUrl` returns `new URL(candidate)` with username, password, search and hash cleared, then `url.toString()`. Node check: `new URL("https://example.test?token=x")` with search cleared gives `https://example.test/`, and `new URL("https://[bad")` throws (-> `[REDACTED_URL]`). Rust `strip_url_secrets` (lines ~190-215) only string-splits, so it yields `https://example.test` (no trailing slash), keeps `:443`, does not lowercase the host, and does not reject malformed authorities. Output text differs from upstream for these inputs.
- Fix: Parse with `url::Url` (already available through reqwest) and clear username, password, query and fragment before `to_string()`; return `[REDACTED_URL]` on parse failure, as upstream does.

### [low] Keychain error message names the last argv element (`-w`) instead of the secret
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/credential_store.rs:283` · dimension: logic-fidelity · upheld 2/2
- Evidence: `security()` builds the error as `resource: format!("secret {}", args.last().unwrap_or(&""))`. For `find-generic-password ... -w` the last argument is `-w`, so the error reads `Failed to read secret -w.` and the account name is lost. Separately, `security_status` takes stdout as the body for `find-generic-password`, so a failed lookup reports `security exited with ...` and drops the stderr text.
- Fix: Pass the secret name into `security()` and format the resource from it, and include stderr in the detail for find failures.

### [low] Upstream test cases not ported for the hydration fold and the maintenance resolution cache
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T08-provider-core/crates/provider-core/src/provider_instance_registry_hydration.rs:146` · dimension: logic-fidelity · upheld 2/2
- Evidence: Upstream ProviderInstanceRegistryHydration.test.ts:100 'folds overrides from multiple projects' has no Rust counterpart (the Rust tests module has 6 cases, none with two projects). Upstream maintenanceResolver.test.ts:882 'caches resolution until a fresh read is requested' and :145/:160 (latest-version preference and update-checks-disabled in enrichment) have no Rust port; `CachedMaintenanceResolution::get` (package_managed_maintenance.rs ~737) and `enrich_provider_snapshot_with_version_advisory` (~828) have no tests.
- Fix: Port the three pure cases: the two-project fold test into the hydration tests module, and the cache-until-fresh and update-checks-disabled cases into package_managed_maintenance tests (the cache test can use a counting definition with a fixed binary path).

### [medium] ProviderInstanceScope cancellation never fires after close (watch receiver dropped at construction)
- `crates/provider-core/src/driver.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Line 79 `let (cancel, _) = watch::channel(false);` (same in Default impl, line 148) drops the only receiver immediately. close() (line 117) and Drop (line 131) call `self.cancel.send(true)`, which tokio 1.53.3 ignores when there are no receivers (src/sync/watch.rs:1066-1068: `if 0 == self.receiver_count() { return Err(..) }` before storing). Result: `is_cancelled()` (lines 87-89) stays false after close, and `cancelled()` (lines 91-97) subscribes a fresh receiver that sees false and waits forever unless some cancelled() future was already parked at close time. Throwaway repro mirroring this code printed `is_cancelled after close = false` and `cancelled() resolved after close = false`. The trait is the cancellation contract adapters T09-T12 build on, so in-flight provider work that polls it after a remove/replace will keep running (process/task leak).
- Fix: Use `self.cancel.send_replace(true)` in close() and Drop (send_replace stores the value regardless of receiver count), and make the Default impl and new() share one constructor. Add a test that calls close() with no live cancelled() future and then asserts is_cancelled() and cancelled() resolve.

### [low] Brew probe timeout kills only the brew process, not its process group (guard disarmed unconditionally)
- `crates/provider-core/src/package_managed_maintenance.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Line 355 `guard.disarm();` runs after `tokio::time::timeout(...)` whether or not the probe finished, so GroupKill's Drop (lines 374-379) never signals the group on timeout. Only the direct child is SIGKILLed via kill_on_drop (line 332) when `child` drops at function end. The doc comment (lines 322-323) and PORTING.md Deviations row 243 claim the timeout kills the whole tree. Throwaway repro with the same guard logic (`/bin/sh -c 'sleep 47 & wait'`, 500 ms timeout, process_group(0)) left the grandchild `sleep` alive after the timeout path.
- Fix: Call `guard.disarm()` only on the success path (inside the `Some(output)` branch), so a timeout or failure leaves the guard armed and the group is SIGKILLed on drop. Add a test with a grandchild that outlives the timeout.

### [low] Keychain backend passes the secret hex in argv
- `crates/provider-core/src/credential_store.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Lines 224-233: `security_status(&["add-generic-password", "-U", "-s", &self.service, "-a", name, "-X", &hex])`. The credential bytes, hex-encoded, become a process argument, visible to other local users through `ps -ww` for the life of the call. The backend is opt-in, but the module doc (lines 1-12) presents it as a credential store.
- Fix: Stop putting the secret in argv. Use the `security-framework` crate (`set_generic_password` with the same service/account), or feed the command over stdin with `security -i` if that route is kept. Keep the account name `provider-auth-<sha256>` unchanged.

### [low] Keychain `security` child has no kill_on_drop and no timeout
- `crates/provider-core/src/credential_store.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Lines 291-294: `tokio::process::Command::new("security").args(args).output().await` with no `.kill_on_drop(true)` and no `tokio::time::timeout`. The comment at lines 178-179 says `security` may prompt for keychain access, so the call can block indefinitely. A dropped future leaves the child running. PORTING.md line 95 requires kill_on_drop(true) for child processes.
- Fix: Set `.kill_on_drop(true)` on the Command and wrap `output()` in `tokio::time::timeout` with a bounded duration, mapping the timeout to SecretStoreError.

### [low] Redaction misses refresh_token, client_secret, id_token and similar keys
- `crates/provider-core/src/failure.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Lines 241 and 245 match keys with `\b(?:access[_-]?token|api[_-]?key|authorization|credential|password|secret|token)\b`. Inside `refresh_token` or `client_secret` there is no word boundary before `token` or `secret`, so the pair regex does not match. Checked with the upstream regex (upstream/packages/provider-core/src/server/failure.ts:129, same semantics in JS): `request failed refresh_token=RT-abc123 client_secret=CS-xyz` passes through unchanged. The gap is inherited from upstream and not recorded in the Deviations table, although PORTING.md rule 4 asks to fix such bugs.
- Fix: Widen the key alternation to allow an optional prefix, for example `[a-z0-9]*[_-]?(?:token|secret|key|password)` or list refresh_token, client_secret, id_token, session_token, and auth_token explicitly. Add a Deviations row, and a test that covers `refresh_token=` and `client_secret=`.

### [low] McpProviderSessionConfig derives Debug and holds a bearer token and a device auth token
- `crates/provider-core/src/mcp_session.rs:` · dimension: safety-and-lifecycle · upheld 2/2
- Evidence: Line 13 `#[derive(Debug, Clone)]` on `McpProviderSessionConfig`. Line 20 `pub authorization_header: String` holds the `Bearer <token>` credential. Line 28 `agent_device_environment: Option<ProcessEnv>` can hold `AGENT_DEVICE_DAEMON_AUTH_TOKEN` (see the test fixture at lines 83 and 99). Any `{:?}` or `tracing` field of this struct prints the token. Latent today (no logging of it in this crate), but adapters T09-T12 will hold this value.
- Fix: Replace the derived Debug with a manual impl that prints `authorization_header` and the device environment values as `[REDACTED]` (keep keys and non-secret fields such as thread_id and endpoint).

### [low] HostProcess derives Debug over the full process environment
- `crates/provider-core/src/host_process.rs:` · dimension: safety-and-lifecycle · upheld 1/2
- Evidence: Lines 42-47: `#[derive(Debug, Clone)] pub struct HostProcess { ... pub environment: ProcessEnv }`, with `HostProcess::current()` (lines 51-58) filling it from `std::env::vars()`. Any `{:?}` of this struct prints every environment variable, including provider API keys. Latent; no current logging found in the crate.
- Fix: Use a manual Debug impl that prints only the platform and home directory, or wrap the environment in a type whose Debug prints the key names only.

### [low] FileSecretStore async methods call blocking std::fs
- `crates/provider-core/src/credential_store.rs:` · dimension: safety-and-lifecycle · upheld 1/2
- Evidence: Lines 132-142 (`get`: `std::fs::read`), lines 144-161 (`set`: `std::fs::write`, `set_mode`, `std::fs::rename`), and lines 164-174 (`remove`: `std::fs::remove_file`) are inside `async fn` bodies of the SecretStore trait. They block a tokio worker thread on the async path. PORTING.md's rule for async IO is tokio::fs, as used in rotating_file_sink.rs and model_manifest.rs.
- Fix: Switch to tokio::fs (read, write, rename, remove_file, and set_permissions through spawn_blocking or the tokio fs equivalents) inside these async methods.

# QC wf_118a9a5d-526 — T11a OpenCode transport

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 24 · refuted: 1

## Lead report

FIX LIST (ranked)

1. HIGH, v2_server.rs:132-156 (`OpenCode2Server::with_connection`)
   - Problem: the Spawned arm clones the connection inside `owner.with_server(...)`, which releases the borrow when it returns. `use_connection(connection).await` then runs with borrowers at 0. The 30s idle timer can kill the server mid-turn or mid-SSE. A probe with idle=300ms showed the process alive at the start and dead after 1.5s of use. Upstream OpenCode2Server.ts runs `use(latest)` inside `owner.withServer`.
   - Fix: run `use_connection(connection).await` inside the `with_server` closure so the borrow spans the use. Keep the `E: From<OpenCodeRuntimeError>` bound. Add a test that a long `use` keeps the server alive past the idle TTL.

2. MEDIUM, ledger.rs:440 (`parse_darwin_line`)
   - Problem: `rest.splitn(4, char::is_whitespace).nth(3)` breaks on macOS `ps` output, which right-aligns pid/pgid to width 5 (`    1     1 Ss   Tue Oct 13 ...`). Any pgid below 10000 returns None. `observe()` then returns None, `track` records nothing, and orphan reaping silently does nothing. Five `ledger::tests::live` tests fail on low-pid hosts. The existing unit test uses single-spaced input.
   - Fix: tokenise like upstream's `^\s*(\d+)\s+(\d+)\s+(\S+)\s+(\w{3} \w{3} +\d{1,2} \d{2}:\d{2}:\d{2} \d{4})\s+(.*)$`. Use `split_whitespace` for pid, pgid and stat, then `trim_start` before `darwin_lstart`. Add a unit test with padded real ps lines, such as `    1     1 Ss ...` and `  648   648 Ss ...`.

3. MEDIUM, server_owner.rs:166 (`Inner::start_input`), plus OwnerConfig at server_owner.rs:46 and OpenCode2Config at v2_server.rs:36-46
   - Problem: `ledger: None` is hard-coded, and neither config has a ledger field. Servers spawned through the owner (v1 and all v2 spawned mode) are never recorded or reaped after an app SIGKILL. Upstream always tracks through the injected service. This is not logged in Deviations.
   - Fix: add `ledger: Option<OpenCodeServerLedger>` to `OwnerConfig` and `OpenCode2Config`, store it in `Inner`, and pass a clone in `start_input`. Alternatively make it required, as upstream does.

4. MEDIUM, runtime.rs:338-370 (`start_open_code_server_process`)
   - Problem: if the start future is dropped while awaiting readiness, the `child` is owned by a detached wait task and the JoinHandles in the local Vec are dropped. `kill_on_drop` never fires, and no `OpenCodeServerProcess` exists yet, so its Drop cannot run. A probe confirmed the server stayed alive after a 500ms timeout. This violates PORTING.md:93-94. (Cancellation during `verify` is already handled, because `process` exists by then.)
   - Fix: build a guard whose Drop SIGKILLs the group and aborts the tasks immediately after spawn and track. Disarm it on success. Add a cancel test.

5. MEDIUM, http_client.rs:183-193 and :71 (`send` and `send_json`)
   - Problem: the timeout wraps only `builder.send()`, which resolves on headers. `response.bytes()` has no deadline. A server that sends headers and then stalls leaves `V1Client::health()` and V2 `server_info()` pending forever (probe: still pending after 12s with a 5s timeout). `verify` after start has no outer timeout, and in External mode the `cached` mutex is held across the call. Upstream's `Effect.timeoutOption` covers the whole call, and `probe_one` in version_probe.rs already does this correctly.
   - Fix: put send and body read in one `tokio::time::timeout` for calls that pass a timeout. Add a test with a stalled body.

6. MEDIUM, http_client.rs:265 (`shared_http`)
   - Problem: the reqwest client honours HTTP_PROXY/ALL_PROXY. Node fetch, which upstream uses, ignores them. With a proxy env var set and no loopback NO_PROXY, health fails and every spawned server is torn down. This was reproduced. It is not logged in Deviations.
   - Fix: call `.no_proxy()` on the builder. Make sure any client built elsewhere for T11 (version_probe callers, tests) does the same.

7. MEDIUM/LOW, version_probe.rs:363 (`last_success`)
   - Problem: `last_success` does `state.lock().await`, while `get` and `refresh` hold the same lock across the whole probe. A probe showed it blocking about 2.9s. The doc says it never waits, and upstream's `Cache.getSuccess` is non-blocking. There is no caller yet, so this is latent until the T11 driver uses it on the hot path.
   - Fix: keep the cached success in a sync slot (`parking_lot::Mutex<Option<_>>` or `watch`). Keep the async mutex only for single-flighting.

8. LOW, runtime.rs:191-199 and 227-233 (`shutdown` and Drop)
   - Problem: `stopped.swap(true)` happens before the grace sleep. If `shutdown()` is cancelled mid-grace, Drop sees `stopped` and skips SIGKILL, forget_lease and finish_tasks. A concurrent second `shutdown()` returns immediately. `server_owner.rs` `force_close` and the next `acquire` abort the `on_exit` task mid-grace, with the same effect.
   - Fix: use a separate `killed` flag, or a drop guard inside `shutdown` that always does the SIGKILL and lease cleanup. Make concurrent callers wait for the first to finish.

9. LOW, runtime.rs:542 and :568 (stdout/stderr pumps)
   - Problem: `read_line(..).unwrap_or(0)` turns an InvalidData error on non-UTF-8 bytes into EOF. The pump exits and drops the pipe read end, so the child eventually gets SIGPIPE or EPIPE (a probe confirmed this). Invalid bytes before the listening line cause a startup timeout. Upstream's `Stream.decodeText` is lossy and keeps draining. `read_line` also buffers an unbounded line.
   - Fix: use `read_until(b'\n')` or fixed-size `read`, decode with `String::from_utf8_lossy`, and stop only on EOF or a real IO error.

10. LOW, http_client.rs:118-131 (`open_event_stream`), comment at :214-216, test at v1_client.rs:594-598
    - Problem: the SSE GET sends `?directory=` and omits the `x-opencode-directory` header. The upstream SDK's SSE path skips the interceptor that moves the header into the query, so it sends the header only. On OpenCode 1.19.0 the query value is double-decoded. A cwd with a literal `%XX` therefore gets no events (verified live) and the turn hangs. The comment claiming the SDK copies the header to the query for SSE is wrong.
    - Fix: send `x-opencode-directory: encodeURIComponent(dir)` on the SSE GET, as upstream does. Correct the comment and the test, and log it in Deviations. For non-SSE GETs, the same `%XX` problem exists upstream (SDK parity). Either double-encode when the directory contains `%XX` (verify against 1.14.19) or log it as a known limitation.

11. LOW, v1_client.rs:302-315 (`V1Client::events`)
    - Problem: `event_open.swap(true)` is reset only in the `Err` arm. If the `events()` future is cancelled during `open_event_stream` (a timeout or select), the flag stays true and every later `events()` on that client and its clones fails with "already open". This was reproduced. The exclusivity is also not in Deviations, and upstream has no such limit.
    - Fix: use a drop guard, or drop the flag. Log it if kept.

12. LOW, server_owner.rs:98-111 (`acquire`)
    - Problem: on a dead server, the state lock is dropped around `server.shutdown().await` (1s grace). A concurrent acquirer can start S1, and the first acquirer then starts S2 without re-checking. That overwrites `state.server` and aborts S1's watcher, leaving S1 untracked. Upstream does this inside one mutex permit.
    - Fix: hold the lock across the shutdown, or re-check `state.server` after re-locking.

13. LOW, version_probe.rs:65 (`OpenCodeBinary`)
    - Problem: the trait doc says "The runtime implements this", but the only impl is the test `PanicBinary` (version_probe.rs:396). Upstream `runOpenCodeCommand` is not ported: it spawns detached with `extendEnv`, caps output, kills the group on finalize or timeout, maps Windows ENOENT, and wraps errors as `Failed to execute '<bin> <args>'`. The `opencode --version` probe cannot run, and there is no Deviations row.
    - Fix: port it as an `OpenCodeBinary` impl (own process group, group kill on drop or timeout, byte cap, same error wrapping). Add a test that a timed-out `--version` leaves no process. If deferred to T11, add a Deviations row and fix the doc comment.

14. LOW, runtime.rs:238-262 (`verify_open_code_server_version`), v1_client.rs:71-86, and the same pattern for v2 `server_info`
    - Problem: any health decode failure (missing `version`, `healthy:false`) yields "OpenCode returned JSON this build cannot read: ...". Upstream yields "OpenCode server returned an invalid health response. T3 Code requires OpenCode v1.14.19 or newer." (operation global.health). The Rust tests only cover the too-old case.
    - Fix: map any decode failure to the upstream detail, and add tests for the missing-version and `healthy:false` bodies.

15. LOW, ledger.rs:31 and ledger.rs:117-124
    - Problem: `#[serde(deny_unknown_fields)]` on `Entry`. Upstream ignores excess properties, so an entry with an extra key is reaped upstream. In Rust, `decode_entry` returns None and `reap_entry` deletes the file without calling `stop_orphan`, leaving the group running. Also, `OpenCodeServerLedger::start` drops the `tokio::spawn` JoinHandle for the reap task (ledger.rs:120), which violates PORTING.md:93. The `reap_orphans` joins at line 222 are fine.
    - Fix: remove `deny_unknown_fields`. Keep the reap task handle in an abort-on-drop wrapper owned by the ledger.

16. LOW, permission_rules.rs (v1 rules and reply mapping)
    - Problem: only the v2 sessionRules are ported. Nothing in the crate covers upstream's `openCodePermissionRules(runtimePolicy)` (adapter.ts:617-700, used in v1 `POST /session` bodies), `toOpenCodePermissionReply`, or `toOpenCodeQuestionAnswers` (adapter.ts:3498 and 3521). `buildOpenCodePermissionRules` in OpenCodeRuntime.ts is dead upstream and is not needed. The deferral is not recorded in PORTING.md Deviations (rule 4).
    - Fix: add a Deviations row saying v1 permission rules and reply mapping belong to the T11 adapter task. Do not port the dead `buildOpenCodePermissionRules`.

17. LOW, http_client.rs:133 and :179, and runtime.rs:316
    - Problem: only one of two verifiers upheld this one. The canned text "Couldn't reach the OpenCode server." and "No such file or directory (os error 2)" lack the tokens that upstream status.ts classifies on (`enoent`, `fetch failed`, `econnrefused`). No status port exists yet, so nothing is broken today. The detail text must not include the URL, because reqwest appends it.
    - Fix: before the T11 status port, either expose a structured error kind (connect, timeout, NotFound) for the port to consume, or include the tokens without the URL (for example `spawn <bin> ENOENT` for NotFound). Add a note in Deviations.

18. LOW, semver.rs:19 and :40
    - Problem: `split_once('-')` keeps the full prerelease (`beta-1`), while upstream `split('-', 2)` truncates to `beta`. `compare_semver_versions` of `rc-1` against `rc-2` returns -1 here and 0 upstream. No caller reaches this today, and the divergence is unlogged.
    - Fix: use `splitn(3, '-')` and take the first two pieces (main and `nth(1)`). Add a test with a multi-hyphen version.

VERDICT: FAIL (1 high, item 1: `OpenCode2Server::with_connection` runs the caller outside the owner borrow, so the idle TTL can kill an in-use server; plus 5 medium findings)

## Confirmed findings (raw)

### [medium] macOS ps parser fails on padded pid/pgid columns, so the server ledger never records or reaps on macOS (5 ledger tests fail here)
- `crates/provider-opencode/src/ledger.rs:` · dimension: fidelity · upheld 2/2
- Evidence: ledger.rs:440 `rest.splitn(4, char::is_whitespace).nth(3)` splits on single whitespace chars. `ps -o pid=,pgid=,stat=,lstart=,args=` right-aligns pid/pgid to width 5, e.g. `    1     1 Ss   Tue Oct 13 20:41:43 2026     /sbin/launchd`. Upstream uses `\s+` (DARWIN_PS_LINE). I copied parse_darwin_line verbatim into a scratch program. Inputs `  4321  4321 Ss ...` and `    1     1 ...` return None. Only a 5-digit pid with an equal-width pgid parses. `cargo test -p turin-provider-opencode` on this machine: `61 passed; 5 failed`. The failures are ledger::tests::live::{stops_a_group_left_by_a_server_that_died, records_a_group_whose_wrapper_already_exited, leaves_a_recycled_pid_alone..., leaves_a_serve_process_on_another_port_alone, leaves_the_servers_of_a_running_owner_alone}, with `the dead server did not record the group`. When parsing fails, `observe(owner)` returns None, so `owner` is None and `track` records nothing. Orphan reaping is then a silent no-op.
- Fix: Tokenise like upstream's regex: skip whitespace runs between pid, pgid and stat, then locate the lstart with `darwin_lstart`. Add a unit test with padded columns such as `    1     1 Ss   Tue Oct 13 ...`.

### [medium] OpenCodeServerOwner never passes the ledger, so the production spawn path (owner, v1 and v2) is not recorded or reaped
- `crates/provider-opencode/src/server_owner.rs:` · dimension: fidelity · upheld 2/2
- Evidence: server_owner.rs:166 `fn start_input` hard-codes `ledger: None`. `OwnerConfig` (server_owner.rs:46) has no ledger field. `OpenCode2Server::start` (v2_server.rs) builds an `OwnerConfig` with no ledger either. Upstream OpenCodeRuntime.ts takes `serverLedger = yield* OpenCodeServerLedger` as a service, and `startOpenCodeServerProcess` always calls `serverLedger.track({pid, port, args})` (OpenCodeRuntime.ts, after registering `terminateChild`). Upstream OpenCodeServerOwner calls `runtime.startOpenCodeServerProcess`, so every owner-spawned server is tracked. In Turin only direct callers of `start_open_code_server_process` with `ledger: Some(..)` get tracking, and no production caller does.
- Fix: Add `ledger: Option<OpenCodeServerLedger>` to `OwnerConfig`, thread it through `start_input`, and have `OpenCode2Server::start` and the v1 owner pass it. Alternatively make the ledger a required constructor dependency, as it is upstream.

### [low] No production OpenCodeBinary / runOpenCodeCommand, so the local-binary version probe cannot run
- `crates/provider-opencode/src/version_probe.rs:` · dimension: fidelity · upheld 2/2
- Evidence: version_probe.rs:65 defines `trait OpenCodeBinary`, but the only impl in the crate is the test-only `PanicBinary` (grep for `impl OpenCodeBinary` finds only version_probe.rs:396). Upstream `probeOpenCodeBinary` calls `runtime.runOpenCodeCommand` (OpenCodeRuntime.ts). That function does the following. It spawns detached with `extendEnv` handling. It collects stdout/stderr with a maxBytes cap. It kills the process group on completion or timeout. It maps a Windows command-not-found to `spawn <bin> ENOENT`. It wraps failures as `Failed to execute '<bin> <args>': ...`. None of this is ported, and the Deviations table has no row for it. `opencode --version` is the only way to pick v1 or v2 for a spawned binary.
- Fix: Port `runOpenCodeCommand` as an `OpenCodeBinary` impl: tokio Command, own process group, group kill on drop or timeout, byte cap, and the same error wrapping. Or record the cut in PORTING Deviations.

### [low] v1 permission rules (buildOpenCodePermissionRules) and reply mapping not ported
- `crates/provider-opencode/src/permission_rules.rs:` · dimension: fidelity · upheld 2/2
- Evidence: permission_rules.rs only has the v2 `{action,resource,effect}` sessionRules. Upstream OpenCodeRuntime.ts also exports `buildOpenCodePermissionRules(runtimeMode)`. It returns `[{permission:'*',pattern:'*',action:'allow'},{permission:'external_directory',...allow}]` for full-access. Otherwise it returns ask-by-default plus read/glob/grep/lsp/skill/todowrite allow, `*.env` and `*.env.*` read ask, `*.env.example` allow, bash/webfetch/websearch/codesearch/external_directory/doom_loop ask, edit allow only for auto-accept-edits, and question allow. It also exports `toOpenCodePermissionReply` (acceptForSession/acceptAlways -> always) and `toOpenCodeQuestionAnswers`. These are sent in v1 `POST /session` bodies. `grep -rn 'doom_loop\|build_open_code\|to_open_code' src` returns nothing. The pure-logic tests in OpenCodeRuntime.permissions.test.ts are not ported either (PORTING rule 5). The docs/protocols spec calls out the v1 `auto` -> edit ask rule as load-bearing.
- Fix: Port buildOpenCodePermissionRules, toOpenCodePermissionReply and toOpenCodeQuestionAnswers with the permissions.test.ts cases. If deferred to the adapter task, say so in Deviations.

### [low] v1 health verification returns a different error for an invalid health body than upstream
- `crates/provider-opencode/src/runtime.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Upstream verifyOpenCodeServerVersion decodes `{healthy:true,version}` and on failure fails with `OpenCode server returned an invalid health response. T3 Code requires OpenCode v1.14.19 or newer.` (operation global.health). In Rust, `V1Client::health` (v1_client.rs) uses `decode_json`, which yields `OpenCode returned JSON this build cannot read: ...` (http_client.rs:280). `verify_open_code_server_version` (runtime.rs ~244) only remaps the timeout string. Separately, the 5s timeout in `HttpClient::send` covers only the response headers, whereas upstream `Effect.timeoutOption` covers the whole call including the body. The same applies to v2 `server_info` and its INFO_TIMEOUT.
- Fix: In verify_open_code_server_version, map any decode failure to the upstream 'invalid health response' detail with operation global.health. Wrap header and body read in the timeout for health and info.

### [low] Transport error text no longer matches status.ts substring classifiers (enoent, fetch failed, econnrefused)
- `crates/provider-opencode/src/http_client.rs:` · dimension: fidelity · upheld 1/2
- Evidence: Upstream status.ts `formatOpenCodeProbeError` classifies on lower-cased detail substrings: `enoent`/`notfound` (CLI not installed), and for external servers `econnrefused|enotfound|fetch failed|networkerror|timed out|socket hang up` (reachability) and `401|403|unauthorized|forbidden`. Upstream SDK/spawn failures carry Node text such as `fetch failed` and `spawn opencode ENOENT`. Rust produces `Couldn't reach the OpenCode server.` (http_client.rs:133 and :179) and `Failed to spawn OpenCode server process: No such file or directory (os error 2)` (runtime.rs:316). Neither contains a matched substring. With these strings, the missing-binary message becomes the generic 'Failed to execute OpenCode CLI health check: ...', and an unreachable external server loses the 'Couldn't reach the configured OpenCode server at <url>' message. The 401 path still works because the detail has `status=401`.
- Fix: Keep status-compatible tokens in the detail text, e.g. include 'fetch failed'/ECONNREFUSED for connect errors and 'spawn <bin> ENOENT' for io::ErrorKind::NotFound. Alternatively, expose a structured error kind that the status port consumes.

### [low] Server owner drops its state lock while shutting down a dead server, so concurrent acquires can start two servers
- `crates/provider-opencode/src/server_owner.rs:` · dimension: fidelity · upheld 2/2
- Evidence: Upstream acquireServer runs `closeServer` and the start inside one `mutex.withPermit`. Rust `acquire` (server_owner.rs ~99-112) sets `state.server=None; state.borrowers=0; drop(state); server.shutdown().await;` (a SIGTERM, 1s grace, SIGKILL) and then re-locks and starts a server unconditionally. A second acquirer that locks in the gap sees `server == None` and starts S1. The first acquirer then re-locks without re-checking and starts S2, overwriting `state.server` and `borrowers=1`. It also aborts S1's exit watcher, so S1 is untracked and never idle-closed while its borrower is still using it.
- Fix: Hold the lock across the shutdown, or re-check `state.server` after re-locking and reuse an existing running server.

### [low] Startup pipe readers stop draining on invalid UTF-8 (read_line error treated as EOF)
- `crates/provider-opencode/src/runtime.rs:` · dimension: fidelity · upheld 2/2
- Evidence: runtime.rs:542 and :568 use `reader.read_line(&mut line).await.unwrap_or(0)` and `break` on 0. `read_line` returns Err(InvalidData) on non-UTF-8 bytes, which is treated as EOF, so the reader exits and the pipe is no longer drained. The module comment and the Deviations row ('Stopping the readers fills OpenCode's stdio buffers and stalls the server') say draining must continue. Upstream uses `Stream.decodeText()` (lossy), which never stops.
- Fix: Read raw bytes (`read_until(b'\n')` or `read_buf`) and convert with `String::from_utf8_lossy`. Keep looping on any non-EOF result.

### [low] reqwest honours proxy env/system proxy for loopback OpenCode servers; Node fetch does not
- `crates/provider-opencode/src/http_client.rs:` · dimension: fidelity · upheld 2/2
- Evidence: http_client.rs:265 `shared_http()` builds `reqwest::Client::builder().connect_timeout(10s).redirect(limited(10))` with default proxy detection. Upstream uses Node/Effect fetch, which ignores HTTP(S)_PROXY and the macOS system proxy. With HTTP_PROXY set and no NO_PROXY entry for 127.0.0.1, every request to the spawned `opencode serve` (and the SSE) goes through the proxy and fails.
- Fix: Call `.no_proxy()` on the builder (or at least bypass loopback) to match upstream.

### [low] Semver prerelease parsing differs from JS split('-', 2) for versions with multiple hyphens
- `crates/provider-opencode/src/semver.rs:` · dimension: fidelity · upheld 2/2
- Evidence: semver.rs:19 uses `split_once('-')`, so `1.2.3-beta-1` gives prerelease `beta-1`. Upstream `normalizeSemverVersion` and `parseSemver` use `version.split('-', 2)`, which truncates to `['1.2.3','beta']` (verified in node). `compare_semver_versions` can therefore order prerelease builds differently (e.g. `1.14.19-rc-1` vs `1.14.19-rc-2` compare equal upstream).
- Fix: Take at most the first two '-'-separated pieces: `splitn(3,'-')` and use the first two.

### [low] Ledger entry decode rejects entries with unknown fields; upstream ignores excess properties
- `crates/provider-opencode/src/ledger.rs:` · dimension: fidelity · upheld 2/2
- Evidence: ledger.rs:31 `#[serde(deny_unknown_fields)]` on Entry. The Effect `Schema.Struct` decode used by upstream's `decodeEntry` ignores extra keys by default. An entry with an extra field decodes to None in Rust, so `reap_entry` deletes the file without stopping the orphaned group. Upstream would still reap it. Related: `OpenCodeServerLedger::start` (ledger.rs:117-120) and `reap_orphans` (ledger.rs:222) drop bare `tokio::spawn` JoinHandles, which PORTING.md forbids ('Never drop a bare JoinHandle'). Upstream uses `forkScoped`, which is cancelled with the layer scope.
- Fix: Remove deny_unknown_fields. Keep the reap task's JoinHandle (or AbortOnDropHandle) in the ledger so dropping the ledger cancels it.

### [high] OpenCode2Server runs the caller's closure outside the owner borrow, so the idle TTL kills a server that is in use
- `crates/provider-opencode/src/v2_server.rs:` · dimension: correctness · upheld 2/2
- Evidence: v2_server.rs:132-156: the Spawned arm does `owner.with_server(|server| ...Ok(connection)).await?` and only afterwards calls `use_connection(connection).await` (line 156). The borrow guard is released when the inner closure returns, so borrowers drops to 0 and the 30s idle timer starts while `use_connection` is still running. Upstream OpenCode2Server.ts runs `use(latest)` inside `owner.withServer`. Throwaway probe (fake binary plus fake /api/info, idle=300ms): `with_connection(|c| async { sleep(1500ms); alive(pid) })` printed (alive at start, alive after 1.5s) = (true, false). With the default 30s idle, any v2 turn or SSE stream that outlives 30s with no other borrower loses its server mid-turn.
- Fix: Run `use_connection` inside the `owner.with_server` closure, as upstream does. Keep the `E: From<OpenCodeRuntimeError>` bound and have the closure return `use_connection(connection).await`.

### [medium] macOS ledger ps parser fails on real ps output when pid or pgid has fewer than 5 digits, so nothing is recorded or reaped
- `crates/provider-opencode/src/ledger.rs:` · dimension: correctness · upheld 2/2
- Evidence: ledger.rs:440 `rest.splitn(4, char::is_whitespace).nth(3)` assumes single spaces. Real `/bin/ps -ww -o pid=,pgid=,stat=,lstart=,args= -p N` pads columns: `' 5648  5647 SN   Thu Oct 15 12:22:10 2026     sleep 30'`. The double space between pid and pgid gives an empty token, so nth(3) is `SN   Thu...` and `darwin_lstart` returns None. I copied `parse_darwin_line` into a scratch file: the 4-digit-pid line returns None, and `' 648   648 Ss   Thu Oct  5 ...'` returns None. 5-digit pid/pgid lines parse. `cargo test -p turin-provider-opencode` on this macOS host: 5 of 66 fail, all in ledger::tests::live (`stops_a_group_left_by_a_server_that_died`, `records_a_group_whose_wrapper_already_exited`, `leaves_a_recycled_pid_alone...`, `leaves_a_serve_process_on_another_port_alone`, `leaves_the_servers_of_a_running_owner_alone`), each failing with 'the dead server did not record the group'. The unit test uses single-spaced input, so it passes. Effect: `observe()` returns None for the owner (no recordable identity) and for the spawned leader (pgid == pid), `track` returns None, and orphan reaping never happens whenever pid < 10000.
- Fix: Parse by tokens. Take pid, pgid and stat with `split_whitespace`, then trim_start the remainder before `darwin_lstart`. Equivalent: port the upstream regex `^\s*(\d+)\s+(\d+)\s+(\S+)\s+(\w{3} \w{3} +\d{1,2} \d{2}:\d{2}:\d{2} \d{4})\s+(.*)$`. Add a test with padded real ps output.

### [medium] start_open_code_server_process leaks the running server and ledger entry if the start future is cancelled
- `crates/provider-opencode/src/runtime.rs:` · dimension: correctness · upheld 2/2
- Evidence: runtime.rs:338-358 moves `child` into a spawned wait task and keeps the other JoinHandles in a local Vec. If the caller drops the future while it awaits readiness (runtime.rs ~362-370) or `verify`, the Vec of JoinHandles is dropped, which detaches the tasks. The Child stays owned by the detached wait task, so kill_on_drop never fires. No OpenCodeServerProcess exists yet, so its Drop cannot run, and the ledger lease is never forgotten. Upstream binds the kill to the caller's Scope, so closing the scope kills the group. Probe: `tokio::time::timeout(500ms, start_open_code_server_process(silent server, timeout=30s))` then drop, 500ms later `kill -0` on the server pid printed `alive = true`. PORTING.md requires that on timeout 'the underlying work must actually stop'. This is reachable from OpenCodeServerOwner::acquire and connect_to_open_code_server under any caller-side timeout.
- Fix: Build the OpenCodeServerProcess (or a guard struct whose Drop SIGKILLs the group, aborts the tasks and forgets the lease) immediately after spawn/track. Do the readiness and verify steps through that guard and `mem::forget`/disarm it only on success.

### [low] A cancelled shutdown() marks the process stopped, so Drop skips the kill and the SIGKILL never comes
- `crates/provider-opencode/src/runtime.rs:` · dimension: correctness · upheld 2/2
- Evidence: runtime.rs:191-199 `shutdown` does `stopped.swap(true)`, sends SIGTERM, then `sleep(grace).await` before SIGKILL, forget_lease and finish_tasks. The Drop impl at runtime.rs:227-233 returns early when `stopped` is already true. Probe: a server that traps SIGTERM, `timeout(300ms, p.shutdown())` with grace=3s, then `drop(p)`; 500ms later the group was still alive (`alive = true`). The same path hits OpenCodeServerOwner::close/close_if_idle/on_exit if their futures are dropped mid-grace, and a second concurrent shutdown() returns immediately before the first has finished.
- Fix: Make the SIGKILL / forget_lease / finish_tasks step run from Drop even when shutdown started (for example a separate `killed` flag set after the SIGKILL, or a drop guard inside shutdown). Let concurrent shutdown callers wait for the first to finish.

### [low] Startup stdout/stderr pumps stop draining on invalid UTF-8, which closes the pipe and kills the server
- `crates/provider-opencode/src/runtime.rs:` · dimension: correctness · upheld 2/2
- Evidence: runtime.rs:542 and 568 use `reader.read_line(&mut line).await.unwrap_or(0)`. On non-UTF-8 bytes read_line returns Err(InvalidData), unwrap_or turns it into 0, and the loop breaks. The reader is dropped, so the read end of the pipe closes. Upstream uses `Stream.decodeText`, which is lossy and keeps draining. Probe: a fake server prints the listening line, then `printf '\377\376 bad\n'`, then keeps echoing to stdout. A few seconds later `alive=false running=false`: the server was killed by SIGPIPE. The comment at runtime.rs:6-9 and PORTING.md say stopping the readers stalls OpenCode. `read_line` also buffers an unbounded line when no newline ever arrives.
- Fix: Read raw bytes with `read(&mut [u8; 8192])` in a loop. Decode lossily with `String::from_utf8_lossy` for the capture buffer and discard after startup, ending only on EOF or a real IO error.

### [medium] Spawned servers are never recorded in the ledger when started through OpenCodeServerOwner or OpenCode2Server
- `crates/provider-opencode/src/server_owner.rs:` · dimension: correctness · upheld 2/2
- Evidence: server_owner.rs:166 `Inner::start_input` hardcodes `ledger: None`. `OwnerConfig` (server_owner.rs:47-55) and `OpenCode2Config` (v2_server.rs:38-46) have no ledger field. Upstream injects OpenCodeServerLedger into the runtime, so every `startOpenCodeServerProcess` is tracked. Only direct callers of `start_open_code_server_process` or `connect_to_open_code_server` can use the ledger. The shared-server path (all v2 sessions) can never be reaped after a SIGKILL of the app.
- Fix: Add `ledger: Option<OpenCodeServerLedger>` to OwnerConfig and OpenCode2Config, store it in Inner, and pass a clone in `start_input`.

### [medium] JSON calls with a timeout stop timing out once response headers arrive, so a stalled body hangs health and verify forever
- `crates/provider-opencode/src/http_client.rs:` · dimension: correctness · upheld 2/2
- Evidence: http_client.rs:184-193 wraps only `builder.send()` in `tokio::time::timeout`; `send_json` then awaits `response.bytes()` at line 71 with no deadline. Probe: a server answers `GET /global/health` with `HTTP/1.1 200 ... Content-Length: 100` plus a few body bytes, then stalls. `V1Client::health()` (5s OPENCODE_HEALTH_TIMEOUT) was still pending after 12s (outer timeout fired). The same holds for V2 `server_info` (5s INFO_TIMEOUT). Upstream wraps the whole SDK call in `Effect.timeoutOption`, and the version_probe.rs probe_one here already times the body read correctly. A half-open proxy therefore hangs `verify`, and the server start cannot complete or fail.
- Fix: Apply the deadline to the whole request: send, headers and `bytes()` inside one `tokio::time::timeout`, so the "Timed out while waiting for the OpenCode server." detail also covers the body read.

### [medium] OpenCodeRuntimeProbe::last_success blocks behind an in-flight probe or refresh
- `crates/provider-opencode/src/version_probe.rs:` · dimension: correctness · upheld 2/2
- Evidence: version_probe.rs:363 `last_success` does `self.state.lock().await`, while `get`/`refresh` hold the same tokio Mutex across the whole probe (4s CLI, 5s per URL x2). The docs say `last_success` 'never probes' and exists for hot calls ('too hot to wait on one'). Probe: a probe closure that sleeps 3s with `refresh()` spawned, then `last_success()` took 2.899s to return. Effect `Cache.getSuccess` returns immediately.
- Fix: Keep the cached success in a separate `parking_lot::Mutex<Option<ProbedOpenCode>>` or `watch` that `last_success` reads without waiting. Keep the async mutex only for single-flighting.

### [medium] reqwest honours HTTP_PROXY/ALL_PROXY for the loopback OpenCode server, so spawned servers become unreachable
- `crates/provider-opencode/src/http_client.rs:` · dimension: correctness · upheld 2/2
- Evidence: http_client.rs:265 `shared_http()` builds the client without `.no_proxy()`, so reqwest reads proxy settings from the environment. Probe: a fake server on 127.0.0.1 with `HTTP_PROXY=http://127.0.0.1:9` gave `V1Client::health()` = Err("Couldn't reach the OpenCode server."); `ALL_PROXY` does the same. Without the variables it returns Ok("1.19.0"). Upstream's fetch/Effect client does not use proxy environment variables. With a proxy set and no NO_PROXY exemption, `verify` fails on every start and the freshly spawned server is torn down.
- Fix: Use `.no_proxy()` in `shared_http()` for parity with upstream. If proxy support is wanted for external URLs, bypass loopback explicitly.

### [low] v1 GET/HEAD ?directory= sends the raw cwd, but OpenCode 1.19.0 percent-decodes the query value a second time
- `crates/provider-opencode/src/http_client.rs:` · dimension: correctness · upheld 2/2
- Evidence: http_client.rs:208 pushes the raw directory into `query_pairs_mut`. Live check against ~/.opencode/bin/opencode 1.19.0 with a directory containing a literal `%25`: POST /session with the header (encodeURIComponent) creates the session in the right directory, but `GET /event?directory=%2F...pct%2525eq` delivered no `session.created`. Sending the value pre-encoded (`directory=%252Ftmp%252F...`) delivered it. Directories with spaces, unicode/emoji, `+ & # = ?`, quotes, tabs and a 785-char path all work. The 1.3.13 TS SDK also sends the raw value, so this is parity, but PORTING rule 4 says to fix bugs. A path with a literal `%XX` makes the turn hang because no events arrive.
- Fix: When the directory contains a `%` followed by two hex digits, send `encodeURIComponent(directory)` as the query value (a double encode). Note this in the Deviations table.

### [low] Ledger Entry rejects unknown fields where the upstream schema ignores them, so a newer entry is deleted without stopping its group
- `crates/provider-opencode/src/ledger.rs:` · dimension: correctness · upheld 2/2
- Evidence: ledger.rs:31 `#[serde(deny_unknown_fields)]` on Entry. Effect `Schema.Struct` decoding ignores excess properties. If an entry has any extra key (written by a newer build or a copied state dir), `decode_entry` returns None, and `reap_entry` falls through to `remove_file` without calling `stop_orphan`. The orphan then runs forever with no record.
- Fix: Remove `deny_unknown_fields`.

### [low] V1Client::events refuses a second subscription per client, which breaks a reconnect that subscribes before dropping the old stream
- `crates/provider-opencode/src/v1_client.rs:` · dimension: correctness · upheld 1/2
- Evidence: v1_client.rs:302 `event_open.swap(true)` returns 'already open' until the previous `Subscribed` stream is dropped. After `EventStreamError::Silent` or EOF, an adapter loop written as `events = client.events().await?` evaluates the new subscribe while the old binding is still alive, and gets that error. Upstream's SDK client has no such limit. The claim 'matching the SDK client the v1 adapter keeps for a directory' does not make this a constraint.
- Fix: Drop the per-client exclusivity flag (the caller already owns the stream), or release it as soon as the stream yields Err or None.

### [low] No implementation of OpenCodeBinary / runOpenCodeCommand, so the `opencode --version` probe path cannot run or be cancel-tested
- `crates/provider-opencode/src/version_probe.rs:` · dimension: correctness · upheld 2/2
- Evidence: version_probe.rs:65 declares `trait OpenCodeBinary` with the comment 'The runtime implements this', but no `impl OpenCodeBinary` exists outside tests (grep). The upstream `runOpenCodeCommand` (OpenCodeRuntime.ts, spawn in its own group, kill the group on finalize, maxOutputBytes cap) is not ported, and `probe_open_code_binary` wraps `run_version` in `tokio::time::timeout`. A timed-out probe would only stop the process if the missing impl kills the group on drop.
- Fix: Port `run_open_code_command` (process group, bounded stdout/stderr, group kill on drop/timeout) and implement OpenCodeBinary on it, with a test that a timed-out `--version` leaves no process.

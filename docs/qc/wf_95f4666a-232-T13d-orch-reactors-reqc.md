# QC wf_95f4666a-232 — T13d reactors re-QC

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 2 · refuted: 0

## Lead report

1. LOW, crates/git/src/git_vcs_driver.rs:851-852 (capture_with_index, under the checkpoint workspace lock). Problem: `remove_force` (line 1315-1316, `std::fs::remove_file`) runs as a blocking syscall on a tokio worker. Fix: replace both calls with `tokio::fs::remove_file(...).await` and ignore the error as `remove_force` does, or wrap them in `spawn_blocking`. Leave the Drop cleanup at lines 1326-1327 synchronous.

2. LOW, crates/git/src/git_vcs_driver.rs:1116 (stage_excluding_empty_nested, reached from capture_with_index on the `git add` recovery path). Problem: `nested.join(".git").exists()` is a blocking stat on a tokio worker. Fix: `tokio::fs::try_exists(nested.join(".git")).await.unwrap_or(false)`. Note: this line and 851-852 come from dec15c96 (T06), not T13d. If T13d should not touch T06 code, route this item to the T06 owner.

Verdict: PASS (no critical/high). Both items are low and overlap on the same capture path, so one fix pass covers them.

## Confirmed findings (raw)

### [low] Blocking std::fs unlink left on the async checkpoint capture path (fix for async fs incomplete)
- `/Users/jackson/.t3/scratch/2026-10-15-consider-creating-a-t3-code-21b503f2/turin-wt/T13d-orch-reactors/crates/git/src/git_vcs_driver.rs:` · dimension: fix-verification · upheld 2/2
- Evidence: Line 851-852 in async fn capture_with_index (line 781), under the checkpoint workspace lock: `remove_force(temp_index);` and `remove_force(PathBuf::from(format!("{}.lock", temp_index.display())));`. `fn remove_force` (line 1315-1316) is `let _ = std::fs::remove_file(path);`, a blocking syscall on a runtime worker. Commit 731e3ae converted the sibling std::fs calls in try_reuse_index and restore_checkpoint to tokio::fs, but this call site on the same capture round trip was not converted. PORTING.md:102-103 (Async-first, user rule) says filesystem work runs on tokio. Drop-time cleanup in TempIndex (line 1326) is synchronous by necessity and is not part of this finding. Verified: all other fixes pass (see summary).
- Fix: Replace the two remove_force calls in capture_with_index with `tokio::fs::remove_file(...).await` (ignoring the error as remove_force does), or run them via tokio::task::spawn_blocking. Keep TempIndex's Drop cleanup as is.

### [low] Blocking std::fs calls remain on the async checkpoint capture path after the 731e3ae fix
- `crates/git/src/git_vcs_driver.rs:` · dimension: regressions · upheld 2/2
- Evidence: Line 1116 in async fn stage_excluding_empty_nested (reached from capture_with_index at line 901 during checkpoint capture): `if nested.join(".git").exists()` is a blocking stat on a tokio worker. Lines 851-852 in async fn capture_with_index: `remove_force(temp_index);` and `remove_force(PathBuf::from(format!("{}.lock", ...)))`, where remove_force (line 1315) calls std::fs::remove_file. Commit 731e3ae converted only try_reuse_index and restore_checkpoint to tokio::fs, so these three sync calls on the same capture path were left as they were. All three were introduced in dec15c96 (T06), not by a T13d commit. PORTING.md 'Async-first (user rule)' requires filesystem work to run on tokio. The Drop-based TempIndex cleanup at line 1322-1327 is sync by necessity and is not a finding.
- Fix: Replace line 1116 with `tokio::fs::try_exists(nested.join(".git")).await.unwrap_or(false)` (or equivalent) and make the two remove calls at 851-852 use `tokio::fs::remove_file(..).await` with the error ignored, matching remove_force's `let _ =` semantics. Keep the Drop path as is. Route to the T06 owner if T13d should not touch dec15c96 code.

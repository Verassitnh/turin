# QC wf_68b8231e-e69 — T15 sidebar (Haiku QC)

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 6 · refuted: 0

## Lead report

1. [medium] crates/app/src/components/sidebar/sidebar_thread_row.rs:552, :565, :642. The shared `woke_button` hardcodes `theme.warning` for text and icon, and the slim call (`rems(0.75)`) uses it too. Upstream's slim Woke pill (Sidebar.tsx ~1870-1884) uses `text-warning-foreground` with `size-3` and `hover:underline`. On light theme this is #fe9a00 on a light surface, which is low contrast. Fix: pass a color into `woke_button`. Use `theme.warning_foreground` for the slim call and keep `theme.warning` for the card call at :730. Add `hover:underline` to both pills.

2. [medium] crates/app/src/components/sidebar/thread_status_indicators.rs:1-3 (header), with the missing logic in the same file. The header says the file ports `prStatusIndicator`, but none of the upstream functions exist in crates/app/src. Missing: resolveThreadPr, nextThreadChangeRequestSnapshot, resolveDisplayedThreadPr (+Provider), linkedPullRequestSnapshotStatus, prStatusIndicator, resolveThreadPullRequestBadgePresentation, threadChangeRequestSnapshotsEqual, and their ThreadStatusIndicators.test.ts cases (about 32 `it(` calls plus `it.each` tables). PORTING.md:228 defers PR snapshots to T19 but does not name these functions or the skipped tests. Fix: correct the header now. Either port the pure functions and tests, or add a Deviations row that names each function and test group as deferred to T19.

3. [low] crates/app/src/components/sidebar/runtime/thread_settled.rs:358-362. `from_local_datetime(...).single()?` returns None for a fall-back ambiguous local time, so custom snooze rejects 01:xx on the DST fall-back day. Upstream threadSettled.ts:345-347 accepts it and picks the earlier instant (05:30Z in America/New_York on 2026-11-01). Fix: replace `.single()` with the ambiguous-case choice that yields the pre-transition (EDT) instant. One verifier's probe found `latest()` gives 05:30Z here, while `earliest()` gives 06:30Z, which is wrong. Keep the round-trip check so DST-gap times stay rejected. Add a test that pins the exact instant.

4. [low] crates/app/src/components/sidebar/sidebar_snooze.rs:57-61. The match arms are `0`, `1`, `2..=6`, `_`, so a negative day delta (wake before today) falls into the date arm and prints "Apr 7, 9:00". Upstream Sidebar.snooze.ts:50-54 uses `dayDelta < 7` and prints "Tue 9:00". Fix: make the weekday arm cover all negatives up to 6 after the 0 and 1 arms. Add a past-wake test. The function has no production caller yet, so there is no user-visible impact today.

5. [low] crates/app/src/components/sidebar/sidebar_logic.rs (around :1528-1560). `sortScopedProjectsForSidebar` (Sidebar.logic.ts:1428) has no Rust counterpart, and its tests (Sidebar.logic.test.ts:1778, :1811) are not ported. The file header says the whole upstream API is kept, and no Deviations row logs this. Fix: port the function and its two tests, or add a Deviations row naming it as cut with its landing surfaces. One verifier also found that `sort_sidebar_v2_project_groups` (:1560) is called only from tests, while upstream useSidebarProjectGroups.ts:46 sorts the live v2 groups. Check whether the live path needs to call it, for example from mock_source.rs:150 and view_model.rs.

6. [low] crates/app/src/components/sidebar/thread_hover_card.rs:254-261. The container sets `TEXT_XS.line_height` (1rem = 16px) and the title div inherits it. Upstream ThreadHoverCard.tsx:27 uses `text-xs leading-tight`, which is 15px. Fix: set `line_height(px(15.))` (or `rems(0.9375)`) on the title div only.

Verdict: PASS (no critical or high findings). Items 1 and 2 should be fixed before merge. Items 3-6 can be fixed or logged in Deviations.

## Confirmed findings (raw)

### [medium] PR badge snapshot logic and its 36 upstream tests are not ported, and the header claims prStatusIndicator is ported
- `crates/app/src/components/sidebar/thread_status_indicators.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Header (lines 1-4) says the file ports ThreadStatusIndicators.tsx incl. `prStatusIndicator`. Upstream exports resolveThreadPr (ThreadStatusIndicators.tsx:484), nextThreadChangeRequestSnapshot (:589), resolveDisplayedThreadPr (:657), resolveDisplayedThreadPrProvider (:712), linkedPullRequestSnapshotStatus (:136), resolveThreadPullRequestBadgePresentation (:173), prStatusIndicator (:442), threadChangeRequestSnapshotsEqual (:543). Upstream ThreadStatusIndicators.test.ts (806 lines, ~36 it() cases: branch-mismatch hiding, retaining closed/merged snapshots across worktree branch switches, live checkout vs stored branch) imports all of these. grep over crates/app/src finds none of resolve_thread_pr / next_thread_change_request_snapshot / resolve_displayed_thread_pr / pr_status_indicator / linked_pull_request_snapshot_status. The port has only format_diff_count, format_worktree_path_for_display and pull_request_state_presentation (2 tests). The only related row in PORTING.md Deviations is the models.rs narrowing (current link = only open link else newest), which does not cover the snapshot/branch-retention rules or the skipped tests. PORTING rule 5 requires porting tests for pure logic.
- Fix: Port resolveThreadPr / nextThreadChangeRequestSnapshot / resolveDisplayedThreadPr(+Provider) / linkedPullRequestSnapshotStatus / prStatusIndicator / resolveThreadPullRequestBadgePresentation and the ThreadStatusIndicators.test.ts cases, or add a Deviations row explaining the deferral to T19 and correct the header comment.

### [low] Custom snooze rejects ambiguous fall-back local times that upstream accepts
- `crates/app/src/components/sidebar/runtime/thread_settled.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Line 361: `.from_local_datetime(&date_value.and_time(time_value)).single()?;`. chrono's `single()` returns None for Ambiguous (fold) results (chrono-0.4.45 offset/mod.rs:105). Upstream threadSettled.ts:345-347 does `new Date(`${date}T${time}:00`)` and then checks localSnoozeDate/localSnoozeTime round-trip. JS picks the earliest instant for ambiguous local times. Verified with `TZ=America/New_York node -e 'new Date("2026-11-01T01:30:00").toISOString()'` gives 2026-11-01T05:30:00.000Z, and the round-trip matches, so upstream accepts 01:30 on the fall-back day and the port returns None (the dialog reports invalid).
- Fix: Use `.earliest()` (matching JS disambiguation) instead of `.single()`, keeping the round-trip check so DST-gap times are still rejected; add a test for an ambiguous fall-back time.

### [low] snooze_wake_description treats a negative day delta as a calendar date, upstream uses the weekday form
- `crates/app/src/components/sidebar/sidebar_snooze.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Port lines 58-61: `match day_delta { 0 => time, 1 => tomorrow, 2..=6 => weekday, _ => date }`. Upstream Sidebar.snooze.ts:54 is `if (dayDelta < 7) return `${weekday} ${time}`;` (covers negative deltas, i.e. a wake time before today's start). For a wake time yesterday or earlier the port prints e.g. "Apr 7, 9:00" where upstream prints "Tue 9:00". No test covers a past wake time.
- Fix: Match upstream: use `i64::MIN..=6` (or `..7`) for the weekday arm after handling 0 and 1, and add a past-wake test.

### [low] Upstream project-sort API and its tests are not ported
- `crates/app/src/components/sidebar/sidebar_logic.rs:` · dimension: logic-fidelity · upheld 2/2
- Evidence: Port has sort_logical_projects_for_sidebar (line 1528) and sort_sidebar_v2_project_groups (line 1560) but not sortProjectsForSidebar or sortScopedProjectsForSidebar (Sidebar.logic.ts:1408-1440, callers in LegacySidebar.tsx, promptLinks/NewThreadLinkView.tsx, routes/_chat.index.tsx). Upstream Sidebar.logic.test.ts cases 'sorts projects by the most recent user message across their threads' (:1575), 'ignores archived threads when sorting projects' (:1686), 'keeps identical project ids in different environments separate' (:1778) and 'does not use archived threads as project activity' (:1811) have no port equivalent. The archived-thread and per-environment rules are implemented in sort_logical_projects_for_sidebar, but the tests are missing and the omission is not in the Deviations table.
- Fix: Port sortScopedProjectsForSidebar (or log it as cut with the landing surfaces) and port the four named tests against the logical sort.

### [medium] Slim-row Woke pill uses warning instead of warning-foreground
- `crates/app/src/components/sidebar/sidebar_thread_row.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: Shared woke_button closure sets `.text_color(theme.warning)` (line 552) and icon `.color(theme.warning)` (line 565). It is called for slim rows with `woke_button(rems(0.75), ...)` (line 642). Upstream upstream/apps/web/src/components/Sidebar.tsx ~L1880 (slim variant) uses `className="... text-xs font-medium text-warning-foreground ..."` with `<AlarmClockIcon className="size-3" />`. Only the card status (topStatus.className "text-warning", L1310-1325) uses text-warning. The card call (line 730, rems(1.)) is correct; the slim call is not.
- Fix: Give woke_button a color parameter. Use theme.warning_foreground for the slim call (rems(0.75)) and theme.warning for the card call (rems(1.)). Matching upstream, the slim pill should also get the hover:underline affordance.

### [low] Hover-card title line-height is 16px, upstream leading-tight is 15px
- `crates/app/src/components/sidebar/thread_hover_card.rs:` · dimension: look-and-feel · upheld 2/2
- Evidence: The container at line 254-255 sets `.text_size(TEXT_XS.size).line_height(TEXT_XS.line_height)` (TEXT_XS line height = 1rem). The title div (lines 258-261) inherits it. Upstream upstream/apps/web/src/components/ThreadHoverCard.tsx:27 uses `min-w-0 truncate text-xs leading-tight font-medium text-foreground`, where leading-tight = 1.25 x 0.75rem = 15px.
- Fix: Set `.line_height(rems(0.9375) or px(15.))` on the title div (leading-tight). Leave the rows at the text-xs default (1rem).

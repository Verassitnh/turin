# WIRE-pr-backend

Factory: `pull_request::live_source::start(bus: CommandBus, client: ClientStateHandle,
cx: &mut App) -> Entity<PullRequestStore>`.

Shell should use `start_with_navigation(bus, client,
Arc<dyn Fn(&ThreadId, &mut App)>, cx)` to select/open linked threads. Default factory
reports missing navigation visibly if clicked. Both factories start the real list.
Existing list/detail views use `store.read(cx).source()` / `PullRequestSource` unchanged.

Requires final runtime contract: `Commands::runtime() -> Arc<WriteRuntime>`,
`rt.pull_requests: Arc<PullRequestService>`,
`rt.review_guides: Arc<PullRequestReviewGuideService>` (WIRE-runtime owns composition).
Adds app dependency `turin-source-control.workspace = true` for the existing linked-thread query.

Implemented via existing service methods: all-project/all-state list with real cursors,
load more, manual refresh/cache invalidation; fast core detail then activity, checks,
native stack, viewed files, reviewer/label candidates, stored guide and linked threads;
all PR actions (including selected merge/update strategies), title/body/comment edits,
comments, reviews with line drafts, paged thread replies, resolve/unresolve, reactions,
diff pages and full file context, viewed-file writes, reviewer and label add/remove,
guide generation/refresh, thread link/unlink and legacy meta change through real
orchestrator commands. Persisted thread links also supply saved stack fallback.

All IO uses CommandBus Tokio tasks. Weak store updates return to GPUI. Writes happen
only on user commands and publish success after the service returns. No fixtures.
Errors remain in list/detail/guide fields. A 30s list refresh retries startup and updates
project/thread choices, respecting service caches. Opened details refresh on the existing
45s/60s checks policy (15s scheduling tick), pausing during active writes/activity reads.
Guide run coordinator already polls `refresh_guide`. Cached activity stays visible while
refreshing. Repeated pending host-action clicks do not execute duplicate merge/revert calls.
Listings resolve existing batched `list_stats` before publishing rows, avoiding unknown
search line counts displayed as zero; unavailable stats produce an explicit read error.

Limits: current view-model seam filters/searches loaded rows locally (99/repository
initially); load-more uses opaque host cursors, or raises limit to 500 when unavailable.
No automatic retry of writes. Reviewers/labels apply the picker desired set as real
add/remove operations; failure after a partial multi-operation write is an error.
Candidate reads happen on detail open/refresh. Host capabilities/permissions and actual
gh credentials determine available operations. Guide configuration errors surface
unchanged; generation requires runtime's real text-generation adapter.

Verification: rustfmt and git diff --check only. No cargo/tests/clippy or live actions;
parent owns integrated build. No push; only this worktree edited.

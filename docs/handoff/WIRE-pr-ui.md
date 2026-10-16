# WIRE-pr-ui handoff

Worktree: `turin-wt/WIRE-pr-ui`, branch `task/WIRE-pr-ui`.

## Live pull-requests route (this task)

Module: `crates/app/src/pull_request/live_navigation.rs` (registered in `pull_request/mod.rs`).

Depends on Sol backend: `crates/app/src/pull_request/live_source.rs` (`task/WIRE-pr-backend`).

### Backend contract (WIRE-pr-backend)

```rust
pub fn start(
    bus: CommandBus,
    client: ClientStateHandle,
    cx: &mut App,
) -> Entity<PullRequestStore>;

pub fn start_with_navigation(
    bus: CommandBus,
    client: ClientStateHandle,
    open_thread: OpenThreadCallback, // same type as live_navigation::OpenThreadCallback
    cx: &mut App,
) -> Entity<PullRequestStore>;
```

Runtime must expose on `Commands::runtime().source_control`:

- `pull_requests: Arc<PullRequestService>`
- `review_guides: Arc<PullRequestReviewGuideService>`

### Shell entry points

```rust
use turin_contracts::ThreadId;
use std::sync::Arc;

pub type OpenThreadCallback = Arc<dyn Fn(&ThreadId, &mut App)>;

pub fn mount_route(
    bus: CommandBus,
    client: ClientStateHandle,
    open_thread: Option<OpenThreadCallback>,
    search: PullRequestsSearch,
    preferences: &dyn PreferenceStorage,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PullRequestsRouteHost>;

impl PullRequestsRouteHost {
    pub fn page_view(&self) -> AnyView;
    pub fn panel_view(&self) -> AnyView;
    pub fn apply_search(&mut self, search: PullRequestsSearch, preferences: &dyn PreferenceStorage, cx: &mut Context<Self>);
}

pub enum PullRequestsRouteEvent {
    OpenPanel, // mount panel_view in the right column
}
```

Suggested wiring (mirrors `pull_request/demo.rs`, without mock data):

1. `mount_route` once per pull-requests route activation (keyed window state if the shell remounts).
2. `layout.set_main_content(Some(host.read(cx).page_view()), cx)`.
3. Subscribe to `PullRequestsRouteEvent::OpenPanel` → `layout.set_right_panel_content(Some(host.read(cx).panel_view()), cx)`.
4. Pass `open_thread: Some(Arc::new(|id, cx| { /* ClientStateHandle::open_thread + navigate to chat */ }))` so linked-thread UI reaches the chat route.
5. Decode in-app search with `validate_pull_requests_search`; call `apply_search` when search changes without remounting.
6. List filter persistence: implement `PreferenceStorage` over UI state / disk (same key as upstream: `t3.pullRequests.preferences`).

### Out of scope here

- `live_source.rs`, `pull_request/mod.rs` `live_source` mod line (Sol).
- `AppSidebarLayout` / shell route table (WIRE-shell).
- `cargo check` in this worktree (parent integration build).

### Demos

`TURIN_PULL_REQUESTS_DEMO` still uses `pull_request/demo.rs` + `MockPullRequestSource`. Production route must use `live_navigation` only.

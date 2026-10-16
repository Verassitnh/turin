//! The caches `PullRequestService` builds from Effect's `Cache`, `Effect.runFork` and its
//! hand-rolled "last good" / "stale diff" holders.
//!
//! - [`TtlCache`] is `Cache.makeWith(lookup, { capacity, timeToLive })`: concurrent gets of one key
//!   share one in-flight lookup; a success is held for the time `ttl(value, key)` says (zero is
//!   not held at all); a failure is never held; an entry is expired once `now >= expiresAt`; the
//!   oldest entry goes first when over capacity and a hit moves the entry to the back.
//! - [`Background`] is `Effect.runForkWith(context)`: work that outlives the caller but keeps the
//!   caller's ambient references, owned by the service so dropping it stops the work.
//! - [`LastGood`] / [`StaleDiff`] are `makeLastGoodRead` and `staleDiff`.

use std::future::Future;
use std::sync::Arc;

use futures::FutureExt;
use futures::future::{BoxFuture, Shared, WeakShared};
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::task::JoinSet;
use turin_contracts::pull_request::PullRequestDiffResult;

use super::errors::{PullRequestError, provider_failure_reason};
use super::routing::{self, RoutingCredential};
use crate::source_control_core::ambient;
use crate::source_control_core::clock::Clock;
use crate::source_control_core::pull_request_provider::PullRequestProviderErrorReason;

/// The fiber-local references a piece of work was started under: the rate-limit ones plus
/// the service's own routing credential. Tokio task-locals are not inherited by anything that
/// is polled elsewhere (`tokio::spawn`, a `Shared` future polled by another waiter), so work that
/// can move is wrapped in the context of whoever started it.
#[derive(Clone)]
pub(crate) struct Scoped {
    ambient: ambient::Captured,
    routing: Option<RoutingCredential>,
}

impl Scoped {
    pub(crate) fn capture() -> Self {
        Self {
            ambient: ambient::capture(),
            routing: routing::routing_credential(),
        }
    }

    pub(crate) async fn run<F: Future>(self, fut: F) -> F::Output {
        self.ambient
            .run(routing::with_routing_credential_value(self.routing, fut))
            .await
    }
}

// ---------------------------------------------------------------------------------------------
// Background work
// ---------------------------------------------------------------------------------------------

/// `runFork`: tasks owned by the service, aborted when it is dropped.
#[derive(Default)]
pub(crate) struct Background {
    tasks: Mutex<JoinSet<()>>,
}

impl Background {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Runs `fut` as its own task under the caller's references.
    pub(crate) fn spawn(&self, fut: impl Future<Output = ()> + Send + 'static) {
        let scoped = Scoped::capture();
        let mut tasks = self.tasks.lock();
        while tasks.try_join_next().is_some() {}
        tasks.spawn(scoped.run(fut));
    }

    /// `revalidate`: the read runs on its own and its outcome is ignored.
    pub(crate) fn revalidate<T: Send + 'static, E: Send + 'static>(
        &self,
        read: impl Future<Output = Result<T, E>> + Send + 'static,
    ) {
        self.spawn(async move {
            let _ = read.await;
        });
    }
}

// ---------------------------------------------------------------------------------------------
// TtlCache
// ---------------------------------------------------------------------------------------------

type Flight<V> = Shared<BoxFuture<'static, Result<V, PullRequestError>>>;
type WeakFlight<V> = WeakShared<BoxFuture<'static, Result<V, PullRequestError>>>;

enum Slot<V: Clone> {
    /// A lookup in flight. Dead once every waiter has gone (the lookup is cancelled with them).
    Pending(WeakFlight<V>),
    Done(V),
}

struct Entry<V: Clone> {
    id: u64,
    expires_at: Option<i64>,
    slot: Slot<V>,
}

struct State<V: Clone> {
    entries: IndexMap<String, Entry<V>>,
    next_id: u64,
}

struct Shared_<V: Clone> {
    capacity: usize,
    clock: Arc<dyn Clock>,
    /// Milliseconds a success is held for; zero means not at all.
    #[allow(clippy::type_complexity)]
    ttl: Box<dyn Fn(&V, &str) -> i64 + Send + Sync>,
    state: Mutex<State<V>>,
}

pub(crate) struct TtlCache<V: Clone + Send + Sync + 'static> {
    shared: Arc<Shared_<V>>,
}

impl<V: Clone + Send + Sync + 'static> TtlCache<V> {
    pub(crate) fn new(
        capacity: usize,
        clock: Arc<dyn Clock>,
        ttl: impl Fn(&V, &str) -> i64 + Send + Sync + 'static,
    ) -> Self {
        Self {
            shared: Arc::new(Shared_ {
                capacity,
                clock,
                ttl: Box::new(ttl),
                state: Mutex::new(State {
                    entries: IndexMap::new(),
                    next_id: 0,
                }),
            }),
        }
    }

    fn has_expired<T: Clone>(entry: &Entry<T>, now: i64) -> bool {
        entry.expires_at.is_some_and(|at| now >= at)
    }

    /// The value for `key`, from the cache or from `lookup`, which runs at most once however many
    /// callers ask while it is in flight.
    pub(crate) async fn get(
        &self,
        key: &str,
        lookup: impl FnOnce() -> BoxFuture<'static, Result<V, PullRequestError>>,
    ) -> Result<V, PullRequestError> {
        let flight = {
            let now = self.shared.clock.now_millis();
            let mut state = self.shared.state.lock();
            let mut live: Option<Flight<V>> = None;
            let mut cached: Option<V> = None;
            if let Some(entry) = state.entries.shift_remove(key)
                && !Self::has_expired(&entry, now)
            {
                match &entry.slot {
                    Slot::Done(value) => cached = Some(value.clone()),
                    Slot::Pending(weak) => live = weak.upgrade(),
                }
                if cached.is_some() || live.is_some() {
                    // Moved to the end of the map to keep it fresh.
                    state.entries.insert(key.to_owned(), entry);
                }
            }
            if let Some(value) = cached {
                return Ok(value);
            }
            match live {
                Some(flight) => flight,
                None => {
                    let id = state.next_id;
                    state.next_id += 1;
                    let scoped = Scoped::capture();
                    let shared = self.shared.clone();
                    let owned_key = key.to_owned();
                    let work = lookup();
                    let flight: Flight<V> = async move {
                        let result = scoped.run(work).await;
                        shared.complete(&owned_key, id, &result);
                        result
                    }
                    .boxed()
                    .shared();
                    if let Some(weak) = flight.downgrade() {
                        state.entries.insert(
                            key.to_owned(),
                            Entry {
                                id,
                                expires_at: None,
                                slot: Slot::Pending(weak),
                            },
                        );
                    }
                    self.shared.check_capacity(&mut state);
                    flight
                }
            }
        };
        flight.await
    }

    /// `Cache.getSuccess`: a held, unexpired, finished value; never starts or awaits a lookup.
    pub(crate) fn get_success(&self, key: &str) -> Option<V> {
        let now = self.shared.clock.now_millis();
        let mut state = self.shared.state.lock();
        let expired = state
            .entries
            .get(key)
            .is_some_and(|entry| Self::has_expired(entry, now));
        if expired {
            state.entries.shift_remove(key);
            return None;
        }
        let entry = state.entries.shift_remove(key)?;
        let value = match &entry.slot {
            Slot::Done(value) => Some(value.clone()),
            Slot::Pending(_) => None,
        };
        state.entries.insert(key.to_owned(), entry);
        value
    }

    /// `Cache.invalidateAll`
    pub(crate) fn invalidate_all(&self) {
        self.shared.state.lock().entries.clear();
    }

    /// `Cache.keys`: how many unexpired entries are held. Expired ones are dropped as they are
    /// walked, as upstream does.
    pub(crate) fn live_count(&self) -> usize {
        let now = self.shared.clock.now_millis();
        let mut state = self.shared.state.lock();
        state
            .entries
            .retain(|_, entry| !Self::has_expired(entry, now));
        state.entries.len()
    }

    /// Whether the held value for `key` is the one given (`current.value === value`), by
    /// the comparison the caller supplies.
    pub(crate) fn invalidate_if(&self, key: &str, is_value: impl Fn(&V) -> bool) {
        let mut state = self.shared.state.lock();
        let matches = matches!(
            state.entries.get(key),
            Some(Entry { slot: Slot::Done(value), .. }) if is_value(value)
        );
        if matches {
            state.entries.shift_remove(key);
        }
    }
}

impl<V: Clone + Send + Sync + 'static> Shared_<V> {
    /// The lookup finished: hold a success for its time to live, forget a failure.
    fn complete(&self, key: &str, id: u64, result: &Result<V, PullRequestError>) {
        let mut state = self.state.lock();
        let owned = state.entries.get(key).is_some_and(|entry| entry.id == id);
        if !owned {
            return;
        }
        let ttl = match result {
            Ok(value) => (self.ttl)(value, key),
            Err(_) => 0,
        };
        if ttl <= 0 {
            state.entries.shift_remove(key);
            return;
        }
        let now = self.clock.now_millis();
        if let (Some(entry), Ok(value)) = (state.entries.get_mut(key), result) {
            entry.expires_at = Some(now + ttl);
            entry.slot = Slot::Done(value.clone());
        }
    }

    fn check_capacity(&self, state: &mut State<V>) {
        // A lookup abandoned by all its waiters leaves a dead slot behind; those go first.
        state.entries.retain(|_, entry| match &entry.slot {
            Slot::Pending(weak) => weak.upgrade().is_some(),
            Slot::Done(_) => true,
        });
        while state.entries.len() > self.capacity {
            state.entries.shift_remove_index(0);
        }
    }
}

// ---------------------------------------------------------------------------------------------
// LastGood
// ---------------------------------------------------------------------------------------------

/// How a held value is served: `Reuse` spends nothing; `Revalidate` answers the same way and
/// refreshes behind it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ServeMode {
    Reuse,
    Revalidate,
}

struct Held<V> {
    at: i64,
    value: V,
}

/// `makeLastGoodRead`: the last value a read produced, kept so a failed refresh can answer from it
/// for `STALE_DETAIL_WINDOW`.
pub(crate) struct LastGood<V: Clone + Send + Sync + 'static> {
    capacity: usize,
    window_ms: i64,
    clock: Arc<dyn Clock>,
    background: Arc<Background>,
    held: Mutex<IndexMap<String, Held<V>>>,
}

impl<V: Clone + Send + Sync + 'static> LastGood<V> {
    pub(crate) fn new(
        capacity: usize,
        window_ms: i64,
        clock: Arc<dyn Clock>,
        background: Arc<Background>,
    ) -> Arc<Self> {
        Arc::new(Self {
            capacity,
            window_ms,
            clock,
            background,
            held: Mutex::new(IndexMap::new()),
        })
    }

    pub(crate) fn peek(&self, key: &str) -> Option<V> {
        self.held.lock().get(key).map(|held| held.value.clone())
    }

    pub(crate) fn record(&self, key: &str, value: V) {
        let at = self.clock.now_millis();
        let mut held = self.held.lock();
        held.shift_remove(key);
        if held.len() >= self.capacity {
            held.shift_remove_index(0);
        }
        held.insert(key.to_owned(), Held { at, value });
    }

    /// Runs the read, remembers a success, and answers a transient provider failure from a value
    /// that is recent enough.
    pub(crate) async fn read(
        &self,
        key: &str,
        effect: impl Future<Output = Result<V, PullRequestError>>,
    ) -> Result<V, PullRequestError> {
        match effect.await {
            Ok(value) => {
                self.record(key, value.clone());
                Ok(value)
            }
            Err(error) => {
                let PullRequestError::Operation(operation) = &error else {
                    return Err(error);
                };
                let Some(reason) = provider_failure_reason(operation) else {
                    return Err(error);
                };
                if reason != PullRequestProviderErrorReason::Failed
                    && reason != PullRequestProviderErrorReason::RateLimited
                {
                    return Err(error);
                }
                let now = self.clock.now_millis();
                let snapshot = {
                    let held = self.held.lock();
                    held.get(key)
                        .filter(|snapshot| now - snapshot.at <= self.window_ms)
                        .map(|snapshot| snapshot.value.clone())
                };
                match snapshot {
                    None => Err(error),
                    Some(value) => {
                        let turin_contracts::pull_request::PullRequestOperationError::Operation {
                            operation: name,
                            ..
                        } = operation;
                        tracing::warn!(
                            operation = %name,
                            reason = reason.as_str(),
                            "using recent pull request data after a failed refresh"
                        );
                        Ok(value)
                    }
                }
            }
        }
    }

    /// A change request already read does not wait on the host again. `Reuse` answers from what we
    /// hold and spends nothing; `Revalidate` answers the same way and refreshes behind it.
    pub(crate) async fn serve_held(
        self: &Arc<Self>,
        key: &str,
        effect: BoxFuture<'static, Result<V, PullRequestError>>,
        mode: ServeMode,
    ) -> Result<V, PullRequestError> {
        let snapshot = self.peek(key);
        let Some(snapshot) = snapshot else {
            return self.read(key, effect).await;
        };
        if mode == ServeMode::Reuse {
            return Ok(snapshot);
        }
        let this = self.clone();
        let owned_key = key.to_owned();
        self.background
            .revalidate(async move { this.read(&owned_key, effect).await });
        Ok(snapshot)
    }
}

// ---------------------------------------------------------------------------------------------
// StaleDiff
// ---------------------------------------------------------------------------------------------

/// A diff arrives as a cached value held for `DIFF_STALE_WINDOW` and served while the next one is
/// fetched. Explicit refreshes and mutations strand held values through the reference epoch.
pub(crate) struct StaleDiff {
    capacity: usize,
    stale_ms: i64,
    clock: Arc<dyn Clock>,
    background: Arc<Background>,
    held: Mutex<IndexMap<String, Held<PullRequestDiffResult>>>,
}

impl StaleDiff {
    pub(crate) fn new(
        capacity: usize,
        stale_ms: i64,
        clock: Arc<dyn Clock>,
        background: Arc<Background>,
    ) -> Self {
        Self {
            capacity,
            stale_ms,
            clock,
            background,
            held: Mutex::new(IndexMap::new()),
        }
    }

    /// Returns `true` when a value was left held (so the caller arms the sweep).
    fn record(&self, key: &str, value: &PullRequestDiffResult) -> bool {
        let at = self.clock.now_millis();
        let mut held = self.held.lock();
        held.shift_remove(key);
        if !super::can_cache_diff(value) {
            return false;
        }
        if held.len() >= self.capacity {
            held.shift_remove_index(0);
        }
        held.insert(
            key.to_owned(),
            Held {
                at,
                value: value.clone(),
            },
        );
        true
    }

    /// Serves the held value while the read refreshes behind it, or the read itself when nothing
    /// usable is held. `on_held` runs whenever a read leaves a value held.
    pub(crate) async fn serve(
        self: &Arc<Self>,
        key: &str,
        read: BoxFuture<'static, Result<PullRequestDiffResult, PullRequestError>>,
        on_held: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<PullRequestDiffResult, PullRequestError> {
        let this = self.clone();
        let owned_key = key.to_owned();
        let recorded = async move {
            let value = read.await?;
            if this.record(&owned_key, &value) {
                on_held();
            }
            Ok(value)
        };
        let now = self.clock.now_millis();
        let snapshot = {
            let held = self.held.lock();
            held.get(key)
                .map(|snapshot| (snapshot.at, snapshot.value.clone()))
        };
        match snapshot {
            Some((at, value)) if now - at <= self.stale_ms => {
                // Run as its own task rather than a child: the caller is answered and gone before
                // the refresh lands. The read still coalesces on the cache key, so ten stale reads
                // in one window cost one host request, and a failed refresh costs only the retry.
                self.background.revalidate(recorded);
                Ok(value)
            }
            _ => recorded.await,
        }
    }

    /// Drops what `serve` would already refuse, and says how many values are still held.
    pub(crate) fn drop_expired(&self) -> usize {
        let now = self.clock.now_millis();
        let mut held = self.held.lock();
        held.retain(|_, snapshot| now - snapshot.at <= self.stale_ms);
        held.len()
    }
}

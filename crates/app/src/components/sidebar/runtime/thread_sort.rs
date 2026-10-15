//! Port of upstream/packages/client-runtime/src/state/threadSort.ts
//!
//! Upstream's functions are structurally typed (`Pick<…>`); here they read
//! through [`SortableThread`], which [`EnvironmentThreadShell`] implements
//! and tests implement on small fixtures.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use super::collate::locale_compare;
use super::models::{EnvironmentThreadShell, ThreadRunSummary};
use crate::timestamp_format::parse_timestamp_ms;

/// `SidebarThreadSortOrder` / the non-manual `SidebarProjectSortOrder`s.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadSortOrder {
    CreatedAt,
    UpdatedAt,
}

/// A message as `ThreadSortInput["messages"]` sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortMessage {
    pub created_at: String,
    pub role: String,
}

/// The fields upstream's sort helpers pick from a thread.
pub trait SortableThread {
    fn id(&self) -> &str;
    fn created_at(&self) -> &str;
    fn environment_id(&self) -> Option<&str> {
        None
    }
    fn updated_at(&self) -> &str {
        self.created_at()
    }
    fn latest_user_message_at(&self) -> Option<&str> {
        None
    }
    fn messages(&self) -> &[SortMessage] {
        &[]
    }
    fn unsettled_at(&self) -> Option<&str> {
        None
    }
    fn pin_order_key(&self) -> Option<&str> {
        None
    }
    fn active_order_key(&self) -> Option<&str> {
        None
    }
    fn settled_at(&self) -> Option<&str> {
        None
    }
    fn latest_run(&self) -> Option<&ThreadRunSummary> {
        None
    }
}

impl SortableThread for EnvironmentThreadShell {
    fn id(&self) -> &str {
        self.id.as_str()
    }
    fn created_at(&self) -> &str {
        &self.created_at
    }
    fn environment_id(&self) -> Option<&str> {
        Some(self.environment_id.as_str())
    }
    fn updated_at(&self) -> &str {
        &self.updated_at
    }
    fn latest_user_message_at(&self) -> Option<&str> {
        self.latest_user_message_at.as_deref()
    }
    fn unsettled_at(&self) -> Option<&str> {
        self.unsettled_at.as_deref()
    }
    fn pin_order_key(&self) -> Option<&str> {
        self.pin_order_key.as_deref()
    }
    fn active_order_key(&self) -> Option<&str> {
        self.active_order_key.as_deref()
    }
    fn settled_at(&self) -> Option<&str> {
        self.settled_at.as_deref()
    }
    fn latest_run(&self) -> Option<&ThreadRunSummary> {
        self.latest_run.as_ref()
    }
}

/// `toSortableTimestamp`.
pub fn to_sortable_timestamp(iso: Option<&str>) -> Option<i64> {
    iso.filter(|iso| !iso.is_empty())
        .and_then(parse_timestamp_ms)
}

/// `resolveSettledThreadTimestamp`: the timestamp a settled row sorts and
/// labels by on every client: settledAt when stamped, otherwise the latest
/// message or turn stamp, then updatedAt.
pub fn resolve_settled_thread_timestamp<T: SortableThread + ?Sized>(thread: &T) -> Option<String> {
    if let Some(settled_at) = thread.settled_at()
        && to_sortable_timestamp(Some(settled_at)).is_some()
    {
        return Some(settled_at.to_string());
    }
    let run = thread.latest_run();
    let mut latest: Option<&str> = None;
    let mut latest_ms = i64::MIN;
    for candidate in [
        thread.latest_user_message_at(),
        run.and_then(|run| run.requested_at.as_deref()),
        run.and_then(|run| run.started_at.as_deref()),
        run.and_then(|run| run.completed_at.as_deref()),
    ] {
        if let Some(parsed) = to_sortable_timestamp(candidate)
            && parsed > latest_ms
        {
            latest = candidate;
            latest_ms = parsed;
        }
    }
    if let Some(latest) = latest {
        return Some(latest.to_string());
    }
    to_sortable_timestamp(Some(thread.updated_at())).map(|_| thread.updated_at().to_string())
}

/// `sortSettledThreads`: newest end of work first, id tiebreak.
pub fn sort_settled_threads<T: SortableThread>(threads: Vec<T>) -> Vec<T> {
    let mut keyed: Vec<(i64, T)> = threads
        .into_iter()
        .map(|thread| {
            let ms = resolve_settled_thread_timestamp(&thread)
                .and_then(|timestamp| parse_timestamp_ms(&timestamp))
                .unwrap_or(0);
            (ms, thread)
        })
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| locale_compare(left.id(), right.id()))
    });
    keyed.into_iter().map(|(_, thread)| thread).collect()
}

fn get_first_sortable_timestamp(values: &[Option<&str>]) -> Option<i64> {
    values
        .iter()
        .find_map(|value| to_sortable_timestamp(*value))
}

fn get_latest_user_message_timestamp<T: SortableThread + ?Sized>(thread: &T) -> f64 {
    if let Some(latest) = thread.latest_user_message_at()
        && let Some(timestamp) = to_sortable_timestamp(Some(latest))
    {
        return timestamp as f64;
    }
    let latest = thread
        .messages()
        .iter()
        .filter(|message| message.role == "user")
        .filter_map(|message| to_sortable_timestamp(Some(&message.created_at)))
        .max();
    if let Some(latest) = latest {
        return latest as f64;
    }
    get_first_sortable_timestamp(&[Some(thread.updated_at()), Some(thread.created_at())])
        .map_or(f64::NEG_INFINITY, |ms| ms as f64)
}

/// `getThreadSortTimestamp` (`-Infinity` when nothing parses).
pub fn get_thread_sort_timestamp<T: SortableThread + ?Sized>(
    thread: &T,
    sort_order: ThreadSortOrder,
) -> f64 {
    match sort_order {
        ThreadSortOrder::CreatedAt => {
            get_first_sortable_timestamp(&[Some(thread.created_at()), Some(thread.updated_at())])
                .map_or(f64::NEG_INFINITY, |ms| ms as f64)
        }
        ThreadSortOrder::UpdatedAt => get_latest_user_message_timestamp(thread),
    }
}

/// `activeThreadAnchorTimestampMs`: creation time, re-anchored to
/// unsettledAt when the thread last re-entered the active list.
pub fn active_thread_anchor_timestamp_ms<T: SortableThread + ?Sized>(thread: &T) -> i64 {
    to_sortable_timestamp(Some(thread.created_at()))
        .unwrap_or(0)
        .max(to_sortable_timestamp(thread.unsettled_at()).unwrap_or(0))
}

fn compare_f64_desc(left: f64, right: f64) -> Ordering {
    right.partial_cmp(&left).unwrap_or(Ordering::Equal)
}

/// `sortThreads`: newest first, descending id ties (plain string order).
pub fn sort_threads<T: SortableThread + Clone>(
    threads: &[T],
    sort_order: ThreadSortOrder,
) -> Vec<T> {
    if threads.len() < 2 {
        return threads.to_vec();
    }
    let mut keyed: Vec<(f64, &T)> = threads
        .iter()
        .map(|thread| (get_thread_sort_timestamp(thread, sort_order), thread))
        .collect();
    keyed.sort_by(|(left_ts, left), (right_ts, right)| {
        compare_f64_desc(*left_ts, *right_ts).then_with(|| right.id().cmp(left.id()))
    });
    keyed
        .into_iter()
        .map(|(_, thread)| thread.clone())
        .collect()
}

/// `getLatestThreadForProject` over `(project id, archived, thread)` triples.
pub fn get_latest_thread_for_project<'a, T: SortableThread>(
    threads: impl IntoIterator<Item = (&'a str, bool, &'a T)>,
    project_id: &str,
    sort_order: ThreadSortOrder,
) -> Option<&'a T>
where
    T: 'a,
{
    let mut latest: Option<&T> = None;
    let mut latest_timestamp = f64::NEG_INFINITY;
    for (thread_project, archived, thread) in threads {
        if thread_project != project_id || archived {
            continue;
        }
        let timestamp = get_thread_sort_timestamp(thread, sort_order);
        let wins = match latest {
            None => true,
            Some(current) => {
                timestamp > latest_timestamp
                    || (timestamp == latest_timestamp && thread.id() > current.id())
            }
        };
        if wins {
            latest = Some(thread);
            latest_timestamp = timestamp;
        }
    }
    latest
}

// ── Pinned reorder: fractional index keys ──────────────────────────────
// Pinned threads carry an optional pinOrderKey (a base-26 string). The
// pinned block sorts keyed threads by plain string comparison, so a drag
// writes ONE key to ONE thread on that thread's own server — neighbors,
// possibly living on other servers, are never touched, and every client
// connected to the same servers converges on the same order.
const PIN_ORDER_DIGITS: &[u8] = b"abcdefghijklmnopqrstuvwxyz";
const BASE: usize = PIN_ORDER_DIGITS.len();

fn digit_index(byte: u8) -> Option<usize> {
    PIN_ORDER_DIGITS.iter().position(|digit| *digit == byte)
}

fn is_valid_pin_order_key(key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    if !key.bytes().all(|byte| digit_index(byte).is_some()) {
        return false;
    }
    // A trailing minimum digit would leave no room to sort a key immediately
    // before this one; generators never produce it, so treat it as corrupt.
    key.as_bytes().last() != Some(&PIN_ORDER_DIGITS[0])
}

/// JS `Math.round` for non-negative values.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// Midpoint of two digit strings read as fractions in (0, 1). `""` is the
/// open bound on either side. Requires a < b.
fn pin_order_midpoint(a: &str, b: &str) -> String {
    debug_assert!(
        b.is_empty() || a < b,
        "pinOrderMidpoint: bounds out of order"
    );
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if !b.is_empty() {
        // Recurse past the longest common prefix ("a" pads the shorter side).
        let mut n = 0;
        while n < b_bytes.len() && *a_bytes.get(n).unwrap_or(&PIN_ORDER_DIGITS[0]) == b_bytes[n] {
            n += 1;
        }
        if n > 0 {
            let rest_a = if n <= a.len() { &a[n..] } else { "" };
            return format!("{}{}", &b[..n], pin_order_midpoint(rest_a, &b[n..]));
        }
    }
    let digit_a = a_bytes
        .first()
        .and_then(|byte| digit_index(*byte))
        .unwrap_or(0);
    let digit_b = b_bytes
        .first()
        .and_then(|byte| digit_index(*byte))
        .unwrap_or(BASE);
    if digit_b as isize - digit_a as isize > 1 {
        let middle = js_round((digit_a + digit_b) as f64 / 2.) as usize;
        return (PIN_ORDER_DIGITS[middle] as char).to_string();
    }
    // Consecutive leading digits: either b has spare digits to shorten into,
    // or we extend a (never producing a trailing minimum digit).
    if b.len() > 1 {
        return b[..1].to_string();
    }
    let rest = if a.is_empty() { "" } else { &a[1..] };
    format!(
        "{}{}",
        PIN_ORDER_DIGITS[digit_a] as char,
        pin_order_midpoint(rest, "")
    )
}

/// `pinOrderKeyBetween`: a key strictly between two neighbors; `None`
/// bounds mean "top of the block" / "bottom of the keyed run". Returns
/// `None` instead of panicking when existing keys are corrupt or out of order.
pub fn pin_order_key_between(before: Option<&str>, after: Option<&str>) -> Option<String> {
    let a = before.unwrap_or("");
    let b = after.unwrap_or("");
    if !a.is_empty() && !is_valid_pin_order_key(a) {
        return None;
    }
    if !b.is_empty() && !is_valid_pin_order_key(b) {
        return None;
    }
    if !b.is_empty() && a >= b {
        return None;
    }
    Some(pin_order_midpoint(a, b))
}

/// `generateSpreadPinOrderKeys`: evenly spaced keys for materializing an order.
pub fn generate_spread_pin_order_keys(count: usize) -> Vec<String> {
    let mut width = 2u32;
    let mut space = (BASE as f64).powi(width as i32);
    while space <= ((count + 1) * 2) as f64 {
        width += 1;
        space *= BASE as f64;
    }
    let step = space / (count + 1) as f64;
    (0..count)
        .map(|i| {
            let mut value = js_round(step * (i + 1) as f64) as u64;
            // Skip values whose low digit is the minimum (a trailing "a" key).
            if value % BASE as u64 == 0 {
                value += 1;
            }
            let mut key = Vec::with_capacity(width as usize);
            for _ in 0..width {
                key.push(PIN_ORDER_DIGITS[(value % BASE as u64) as usize]);
                value /= BASE as u64;
            }
            key.reverse();
            String::from_utf8(key).expect("ascii digits")
        })
        .collect()
}

/// One `{ id, orderKey }` write.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrderKeyAssignment {
    pub id: String,
    pub order_key: String,
}

/// `planPinnedReorder`: assignments realizing a new order. One write when
/// the moved thread's neighbors are keyed (or absent); otherwise a one-time
/// materialization of the whole section.
pub fn plan_pinned_reorder(
    ordered_ids: &[String],
    keys_by_id: &HashMap<String, Option<String>>,
    moved_id: &str,
) -> Vec<OrderKeyAssignment> {
    let visible: HashSet<&str> = ordered_ids.iter().map(String::as_str).collect();
    let mut reserved: Vec<&str> = keys_by_id
        .iter()
        .filter(|(id, key)| !visible.contains(id.as_str()) && key.is_some())
        .filter_map(|(_, key)| key.as_deref())
        .collect();
    reserved.sort_unstable();
    reserved.dedup();
    let reserved_set: HashSet<&str> = reserved.iter().copied().collect();
    let Some(moved_index) = ordered_ids.iter().position(|id| id == moved_id) else {
        return Vec::new();
    };
    let before_id = moved_index.checked_sub(1).map(|index| &ordered_ids[index]);
    let after_id = ordered_ids.get(moved_index + 1);
    let key_of = |id: Option<&String>| id.and_then(|id| keys_by_id.get(id).cloned().flatten());
    let before_key = key_of(before_id);
    let after_key = key_of(after_id);
    let before_usable = before_id.is_none() || before_key.is_some();
    let after_usable = after_id.is_none() || after_key.is_some();
    if before_usable && after_usable {
        let mut key = pin_order_key_between(before_key.as_deref(), after_key.as_deref());
        while let Some(candidate) = key.as_deref() {
            if !reserved_set.contains(candidate) {
                break;
            }
            key = pin_order_key_between(Some(candidate), after_key.as_deref());
        }
        if let Some(key) = key {
            return vec![OrderKeyAssignment {
                id: moved_id.to_string(),
                order_key: key,
            }];
        }
    }
    // Keyless neighbor (or corrupt keys): rewrite the section in the new order.
    let keys: Vec<String> = generate_spread_pin_order_keys(ordered_ids.len() + reserved.len())
        .into_iter()
        .filter(|key| !reserved_set.contains(key.as_str()))
        .take(ordered_ids.len())
        .collect();
    ordered_ids
        .iter()
        .zip(keys)
        .filter(|(id, key)| keys_by_id.get(*id).and_then(Option::as_ref) != Some(key))
        .map(|(id, key)| OrderKeyAssignment {
            id: id.clone(),
            order_key: key,
        })
        .collect()
}

/// `planPinnedMove`: Move up / Move down as a single-step reorder; `None`
/// when the move falls off either end.
pub fn plan_pinned_move(
    ordered_ids: &[String],
    keys_by_id: &HashMap<String, Option<String>>,
    moved_id: &str,
    up: bool,
) -> Option<Vec<OrderKeyAssignment>> {
    let from = ordered_ids.iter().position(|id| id == moved_id)?;
    let to = if up { from.checked_sub(1)? } else { from + 1 };
    if to >= ordered_ids.len() {
        return None;
    }
    let mut next = ordered_ids.to_vec();
    let moved = next.remove(from);
    next.insert(to, moved);
    Some(plan_pinned_reorder(&next, keys_by_id, moved_id))
}

fn identity_tiebreak<T: SortableThread>(left: &T, right: &T) -> Ordering {
    locale_compare(left.id(), right.id()).then_with(|| {
        locale_compare(
            left.environment_id().unwrap_or(""),
            right.environment_id().unwrap_or(""),
        )
    })
}

/// `sortPinnedThreadsByOrderKey`: keyed first (string order, id then
/// environment tiebreak), keyless below, newest-created first.
pub fn sort_pinned_threads_by_order_key<T: SortableThread>(threads: Vec<T>) -> Vec<T> {
    if threads.len() < 2 {
        return threads;
    }
    let (mut keyed, keyless): (Vec<T>, Vec<T>) = threads
        .into_iter()
        .partition(|thread| thread.pin_order_key().is_some());
    keyed.sort_by(|left, right| {
        left.pin_order_key()
            .cmp(&right.pin_order_key())
            .then_with(|| identity_tiebreak(left, right))
    });
    let mut keyless: Vec<(i64, T)> = keyless
        .into_iter()
        .map(|thread| {
            (
                to_sortable_timestamp(Some(thread.created_at())).unwrap_or(0),
                thread,
            )
        })
        .collect();
    keyless.sort_by(|(left_ms, left), (right_ms, right)| {
        right_ms
            .cmp(left_ms)
            .then_with(|| identity_tiebreak(left, right))
    });
    keyed.extend(keyless.into_iter().map(|(_, thread)| thread));
    keyed
}

/// `sortActiveThreadsByOrderKey`: new and reopened threads lead; arranged
/// threads follow their saved keys.
pub fn sort_active_threads_by_order_key<T: SortableThread>(threads: Vec<T>) -> Vec<T> {
    if threads.len() < 2 {
        return threads;
    }
    let mut keyed: Vec<(i64, T)> = threads
        .into_iter()
        .map(|thread| {
            let anchor = if thread.active_order_key().is_none() {
                active_thread_anchor_timestamp_ms(&thread)
            } else {
                0
            };
            (anchor, thread)
        })
        .collect();
    keyed.sort_by(|(left_ms, left), (right_ms, right)| {
        let order = match (left.active_order_key(), right.active_order_key()) {
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(left_key), Some(right_key)) => left_key.cmp(right_key),
            (None, None) => right_ms.cmp(left_ms),
        };
        order.then_with(|| identity_tiebreak(left, right))
    });
    keyed.into_iter().map(|(_, thread)| thread).collect()
}

#[cfg(test)]
mod tests {
    use super::super::models::fixtures::run_id;
    use super::super::models::{RunStatus, ThreadRunSummary};
    use super::*;

    #[derive(Clone, Debug, PartialEq)]
    struct T {
        id: String,
        environment_id: Option<String>,
        created_at: String,
        updated_at: String,
        latest_user_message_at: Option<String>,
        messages: Vec<SortMessage>,
        unsettled_at: Option<String>,
        pin_order_key: Option<String>,
        active_order_key: Option<String>,
        settled_at: Option<String>,
        latest_run: Option<ThreadRunSummary>,
        row: usize,
    }

    impl SortableThread for T {
        fn id(&self) -> &str {
            &self.id
        }
        fn created_at(&self) -> &str {
            &self.created_at
        }
        fn environment_id(&self) -> Option<&str> {
            self.environment_id.as_deref()
        }
        fn updated_at(&self) -> &str {
            &self.updated_at
        }
        fn latest_user_message_at(&self) -> Option<&str> {
            self.latest_user_message_at.as_deref()
        }
        fn messages(&self) -> &[SortMessage] {
            &self.messages
        }
        fn unsettled_at(&self) -> Option<&str> {
            self.unsettled_at.as_deref()
        }
        fn pin_order_key(&self) -> Option<&str> {
            self.pin_order_key.as_deref()
        }
        fn active_order_key(&self) -> Option<&str> {
            self.active_order_key.as_deref()
        }
        fn settled_at(&self) -> Option<&str> {
            self.settled_at.as_deref()
        }
        fn latest_run(&self) -> Option<&ThreadRunSummary> {
            self.latest_run.as_ref()
        }
    }

    fn make_thread(id: &str) -> T {
        T {
            id: id.into(),
            environment_id: None,
            created_at: "2026-03-09T10:00:00.000Z".into(),
            updated_at: "2026-03-09T10:00:00.000Z".into(),
            latest_user_message_at: None,
            messages: vec![],
            unsettled_at: None,
            pin_order_key: None,
            active_order_key: None,
            settled_at: None,
            latest_run: None,
            row: 0,
        }
    }

    fn settled(id: &str) -> T {
        T {
            updated_at: "2026-03-09T09:00:00.000Z".into(),
            ..make_thread(id)
        }
    }

    fn ids<X: SortableThread>(threads: &[X]) -> Vec<String> {
        threads.iter().map(|t| t.id().to_string()).collect()
    }

    fn keys(entries: &[(&str, Option<&str>)]) -> HashMap<String, Option<String>> {
        entries
            .iter()
            .map(|(id, key)| (id.to_string(), key.map(str::to_string)))
            .collect()
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    // describe("activeThreadAnchorTimestampMs")
    #[test]
    fn uses_the_later_unsettle_time_when_an_old_thread_re_enters_the_active_list() {
        let thread = T {
            created_at: "2026-01-01T00:00:00.000Z".into(),
            unsettled_at: Some("2026-08-01T00:00:00.000Z".into()),
            ..make_thread("t")
        };
        assert_eq!(
            active_thread_anchor_timestamp_ms(&thread),
            parse_timestamp_ms("2026-08-01T00:00:00.000Z").unwrap()
        );
    }

    // describe("resolveSettledThreadTimestamp")
    #[test]
    fn prefers_the_persisted_settlement_stamp_over_later_activity() {
        let thread = T {
            settled_at: Some("2026-03-09T10:00:00.000Z".into()),
            latest_user_message_at: Some("2026-03-09T11:00:00.000Z".into()),
            updated_at: "2026-03-09T12:00:00.000Z".into(),
            ..make_thread("t")
        };
        assert_eq!(
            resolve_settled_thread_timestamp(&thread).as_deref(),
            Some("2026-03-09T10:00:00.000Z")
        );
    }

    #[test]
    fn falls_back_to_the_latest_activity_when_the_stamp_is_missing_or_malformed() {
        let thread = T {
            settled_at: Some("invalid".into()),
            latest_user_message_at: Some("2026-03-09T11:00:00.000Z".into()),
            updated_at: "2026-03-09T12:00:00.000Z".into(),
            ..make_thread("t")
        };
        assert_eq!(
            resolve_settled_thread_timestamp(&thread).as_deref(),
            Some("2026-03-09T11:00:00.000Z")
        );
        let thread = T {
            updated_at: "2026-03-09T12:00:00.000Z".into(),
            ..make_thread("t")
        };
        assert_eq!(
            resolve_settled_thread_timestamp(&thread).as_deref(),
            Some("2026-03-09T12:00:00.000Z")
        );
    }

    // describe("sortSettledThreads")
    #[test]
    fn orders_by_settle_time_most_recently_settled_first() {
        let sorted = sort_settled_threads(vec![
            T {
                settled_at: Some("2026-03-09T10:00:00.000Z".into()),
                latest_user_message_at: Some("2026-03-09T09:59:00.000Z".into()),
                ..settled("settled-first")
            },
            T {
                settled_at: Some("2026-03-09T12:00:00.000Z".into()),
                latest_user_message_at: Some("2026-03-09T08:00:00.000Z".into()),
                ..settled("settled-last")
            },
        ]);
        assert_eq!(ids(&sorted), strings(&["settled-last", "settled-first"]));
    }

    #[test]
    fn falls_back_to_last_activity_for_auto_settled_threads_without_a_settled_at_stamp() {
        let sorted = sort_settled_threads(vec![
            T {
                latest_user_message_at: Some("2026-03-09T08:00:00.000Z".into()),
                ..settled("auto-old")
            },
            T {
                settled_at: Some("2026-03-09T10:00:00.000Z".into()),
                ..settled("explicit")
            },
            T {
                latest_user_message_at: Some("2026-03-09T11:00:00.000Z".into()),
                ..settled("auto-recent")
            },
        ]);
        assert_eq!(
            ids(&sorted),
            strings(&["auto-recent", "explicit", "auto-old"])
        );
    }

    #[test]
    fn counts_a_turn_completion_as_activity_for_auto_settled_threads() {
        let sorted = sort_settled_threads(vec![
            T {
                latest_user_message_at: Some("2026-03-09T10:04:00.000Z".into()),
                ..settled("message-only")
            },
            T {
                latest_user_message_at: Some("2026-03-09T10:00:00.000Z".into()),
                latest_run: Some(ThreadRunSummary {
                    run_id: run_id("run-1"),
                    status: RunStatus::Completed,
                    requested_at: Some("2026-03-09T10:00:00.000Z".into()),
                    started_at: Some("2026-03-09T10:00:00.000Z".into()),
                    completed_at: Some("2026-03-09T10:30:00.000Z".into()),
                }),
                ..settled("completed-later")
            },
        ]);
        assert_eq!(ids(&sorted), strings(&["completed-later", "message-only"]));
    }

    #[test]
    fn breaks_timestamp_ties_by_id_so_the_order_is_stable() {
        let sorted = sort_settled_threads(vec![
            T {
                settled_at: Some("2026-03-09T10:00:00.000Z".into()),
                ..settled("b")
            },
            T {
                settled_at: Some("2026-03-09T10:00:00.000Z".into()),
                ..settled("a")
            },
        ]);
        assert_eq!(ids(&sorted), strings(&["a", "b"]));
    }

    #[test]
    fn matches_the_per_comparison_order_on_a_shuffled_list_with_ties() {
        let stamp = |row: usize, thread: T| match row % 5 {
            0 => T {
                settled_at: Some("2026-03-09T10:00:00.000Z".into()),
                ..thread
            },
            1 => T {
                settled_at: Some("invalid".into()),
                latest_user_message_at: Some("2026-03-09T10:00:00.000Z".into()),
                ..thread
            },
            2 => T {
                latest_user_message_at: Some("2026-03-09T11:00:00.000Z".into()),
                ..thread
            },
            3 => T {
                updated_at: "2026-03-09T09:00:00.000Z".into(),
                ..thread
            },
            _ => T {
                updated_at: "invalid".into(),
                ..thread
            },
        };
        let threads: Vec<T> = (0..30)
            .map(|index| {
                let row = (index * 7) % 30;
                T {
                    row,
                    ..stamp(row, settled(&format!("thread-{}", row % 3)))
                }
            })
            .collect();
        let timestamp_ms = |thread: &T| {
            resolve_settled_thread_timestamp(thread)
                .and_then(|t| parse_timestamp_ms(&t))
                .unwrap_or(0)
        };
        let mut expected = threads.clone();
        expected.sort_by(|left, right| {
            timestamp_ms(right)
                .cmp(&timestamp_ms(left))
                .then_with(|| locale_compare(&left.id, &right.id))
        });
        let actual: Vec<usize> = sort_settled_threads(threads)
            .iter()
            .map(|t| t.row)
            .collect();
        assert_eq!(actual, expected.iter().map(|t| t.row).collect::<Vec<_>>());
    }

    // describe("sortThreads")
    #[test]
    fn preserves_input_order_and_descending_id_ties() {
        for sort_order in [ThreadSortOrder::CreatedAt, ThreadSortOrder::UpdatedAt] {
            let invalid = |id: &str| T {
                created_at: "invalid".into(),
                updated_at: "invalid".into(),
                ..make_thread(id)
            };
            let threads = vec![
                make_thread("a"),
                make_thread("z"),
                invalid("invalid-a"),
                invalid("invalid-z"),
            ];
            let sorted = sort_threads(&threads, sort_order);
            assert_eq!(
                sorted,
                vec![
                    threads[1].clone(),
                    threads[0].clone(),
                    threads[3].clone(),
                    threads[2].clone()
                ]
            );
        }
    }

    #[test]
    fn falls_back_to_updated_at_and_created_at_when_latest_user_message_at_is_invalid() {
        let sorted = sort_threads(
            &[
                T {
                    latest_user_message_at: Some("not-a-date".into()),
                    created_at: "2026-03-09T10:00:00.000Z".into(),
                    updated_at: "2026-03-09T10:05:00.000Z".into(),
                    ..make_thread("thread-1")
                },
                T {
                    latest_user_message_at: Some("still-not-a-date".into()),
                    created_at: "invalid-created-at".into(),
                    updated_at: "invalid-updated-at".into(),
                    ..make_thread("thread-2")
                },
                T {
                    latest_user_message_at: Some("invalid-latest-user-message-at".into()),
                    created_at: "2026-03-09T10:06:00.000Z".into(),
                    updated_at: "invalid-updated-at".into(),
                    ..make_thread("thread-3")
                },
            ],
            ThreadSortOrder::UpdatedAt,
        );
        assert_eq!(ids(&sorted), strings(&["thread-3", "thread-1", "thread-2"]));
    }

    #[test]
    fn falls_back_to_the_latest_valid_user_message_when_latest_user_message_at_is_invalid() {
        let message = |role: &str, at: &str| SortMessage {
            role: role.into(),
            created_at: at.into(),
        };
        let sorted = sort_threads(
            &[
                T {
                    latest_user_message_at: Some("invalid-latest-user-message-at".into()),
                    updated_at: "2026-03-09T10:00:00.000Z".into(),
                    messages: vec![
                        message("user", "2026-03-09T10:05:00.000Z"),
                        message("assistant", "2026-03-09T10:30:00.000Z"),
                        message("user", "2026-03-09T10:20:00.000Z"),
                    ],
                    ..make_thread("thread-1")
                },
                T {
                    created_at: "2026-03-09T10:15:00.000Z".into(),
                    updated_at: "2026-03-09T10:15:00.000Z".into(),
                    ..make_thread("thread-2")
                },
            ],
            ThreadSortOrder::UpdatedAt,
        );
        assert_eq!(ids(&sorted), strings(&["thread-1", "thread-2"]));
    }

    // describe("getLatestThreadForProject")
    #[test]
    fn matches_the_first_sorted_eligible_thread() {
        for sort_order in [ThreadSortOrder::CreatedAt, ThreadSortOrder::UpdatedAt] {
            let a = make_thread("a");
            let z = make_thread("z");
            let zz = make_thread("zz");
            let zzz = make_thread("zzz");
            let threads = vec![
                ("project", false, &a),
                ("project", false, &z),
                ("project", true, &zz),
                ("other", false, &zzz),
            ];
            assert_eq!(
                get_latest_thread_for_project(threads.clone(), "project", sort_order),
                Some(&z)
            );
            assert_eq!(
                get_latest_thread_for_project(
                    Vec::<(&str, bool, &T)>::new(),
                    "project",
                    sort_order
                ),
                None
            );
            assert_eq!(
                get_latest_thread_for_project(threads, "missing", sort_order),
                None
            );
            let invalid_a = T {
                created_at: "invalid".into(),
                updated_at: "invalid".into(),
                ..a.clone()
            };
            let invalid_z = T {
                created_at: "invalid".into(),
                updated_at: "invalid".into(),
                ..z.clone()
            };
            let latest = get_latest_thread_for_project(
                [
                    ("project", false, &invalid_a),
                    ("project", false, &invalid_z),
                ],
                "project",
                sort_order,
            );
            assert!(std::ptr::eq(latest.unwrap(), &invalid_z));
            let copy = z.clone();
            let latest = get_latest_thread_for_project(
                [("project", false, &z), ("project", false, &copy)],
                "project",
                sort_order,
            );
            assert!(std::ptr::eq(latest.unwrap(), &z));
        }
    }

    // describe("planPinnedReorder with hidden rows")
    #[test]
    fn keeps_hidden_slots_available_when_inserting_between_visible_neighbors() {
        let midpoint = pin_order_key_between(Some("f"), Some("t")).unwrap();
        let keys_by_id = keys(&[
            ("a", Some("f")),
            ("b", Some("t")),
            ("moved", Some("z")),
            ("snoozed", Some(&midpoint)),
        ]);
        let assignments = plan_pinned_reorder(&strings(&["a", "moved", "b"]), &keys_by_id, "moved");
        assert_eq!(assignments.len(), 1);
        let key = &assignments[0].order_key;
        assert!(key.as_str() > "f" && key.as_str() < "t");
        assert_ne!(key, &midpoint);
        assert_eq!(assignments[0].id, "moved");
    }

    #[test]
    fn materializes_keyless_rows_without_overwriting_hidden_slots() {
        let reserved = generate_spread_pin_order_keys(6);
        let mut keys_by_id = keys(&[("a", None), ("b", None), ("c", None)]);
        for (i, key) in reserved.iter().enumerate() {
            keys_by_id.insert(format!("hidden-{i}"), Some(key.clone()));
        }
        let assignments = plan_pinned_reorder(&strings(&["c", "a", "b"]), &keys_by_id, "c");
        assert_eq!(
            assignments
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>(),
            vec!["c", "a", "b"]
        );
        let ks: Vec<&String> = assignments.iter().map(|a| &a.order_key).collect();
        let mut sorted = ks.clone();
        sorted.sort();
        assert_eq!(ks, sorted);
        assert_eq!(ks.iter().collect::<HashSet<_>>().len(), 3);
        assert!(ks.iter().all(|k| !reserved.contains(k)));
    }

    // describe("planPinnedMove")
    #[test]
    fn moves_a_thread_up_with_a_single_key_write() {
        let assignments = plan_pinned_move(
            &strings(&["a", "b", "c"]),
            &keys(&[("a", Some("f")), ("b", Some("m")), ("c", Some("t"))]),
            "c",
            true,
        )
        .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].id, "c");
        assert!(assignments[0].order_key.as_str() > "f" && assignments[0].order_key.as_str() < "m");
    }

    #[test]
    fn returns_null_when_the_move_falls_off_the_end_of_the_list() {
        let ordered = strings(&["a", "b"]);
        let keys_by_id = keys(&[("a", Some("f")), ("b", Some("m"))]);
        assert_eq!(plan_pinned_move(&ordered, &keys_by_id, "a", true), None);
        assert_eq!(plan_pinned_move(&ordered, &keys_by_id, "b", false), None);
    }

    #[test]
    fn materializes_keys_for_the_whole_section_when_a_neighbor_is_keyless() {
        let assignments = plan_pinned_move(
            &strings(&["a", "b", "c"]),
            &keys(&[("a", None), ("b", Some("m")), ("c", None)]),
            "b",
            true,
        )
        .unwrap();
        let ks: Vec<&String> = assignments.iter().map(|a| &a.order_key).collect();
        let mut sorted = ks.clone();
        sorted.sort();
        assert_eq!(sorted, ks);
    }

    // describe("sortPinnedThreadsByOrderKey")
    #[test]
    fn breaks_equal_keys_by_id_then_environment_so_merged_lists_are_stable_everywhere() {
        let sorted = sort_pinned_threads_by_order_key(vec![
            T {
                pin_order_key: Some("m".into()),
                environment_id: Some("env-b".into()),
                ..make_thread("thread-1")
            },
            T {
                created_at: "2026-03-09T11:00:00.000Z".into(),
                pin_order_key: Some("m".into()),
                environment_id: Some("env-a".into()),
                ..make_thread("thread-1")
            },
        ]);
        assert_eq!(
            sorted
                .iter()
                .map(|t| t.environment_id.clone().unwrap())
                .collect::<Vec<_>>(),
            strings(&["env-a", "env-b"])
        );
    }

    // describe("generateSpreadPinOrderKeys")
    #[test]
    fn leaves_unique_insertable_keys() {
        for count in [0usize, 1, 650, 675, 676, 1_001, 2_000] {
            let keys = generate_spread_pin_order_keys(count);
            assert_eq!(keys.len(), count);
            assert_eq!(keys.iter().collect::<HashSet<_>>().len(), count);
            let mut sorted = keys.clone();
            sorted.sort();
            assert_eq!(sorted, keys);
            for index in 0..keys.len() {
                let before = index.checked_sub(1).map(|i| keys[i].as_str());
                let after = keys[index].as_str();
                assert!(after.bytes().all(|b| b.is_ascii_lowercase()));
                assert_ne!(after.as_bytes().last(), Some(&b'a'));
                let between = pin_order_key_between(before, Some(after)).unwrap();
                assert!(between.as_str() < after);
                if let Some(before) = before {
                    assert!(between.as_str() > before);
                }
            }
        }
    }

    // describe("sortActiveThreadsByOrderKey")
    #[test]
    fn keeps_new_and_reopened_threads_ahead_of_the_saved_order() {
        let sorted = sort_active_threads_by_order_key(vec![
            T {
                created_at: "2026-03-09T09:00:00.000Z".into(),
                active_order_key: Some("f".into()),
                ..make_thread("arranged-first")
            },
            T {
                created_at: "2026-03-09T11:00:00.000Z".into(),
                ..make_thread("new")
            },
            T {
                created_at: "2026-03-09T12:00:00.000Z".into(),
                unsettled_at: Some("2026-03-09T13:00:00.000Z".into()),
                active_order_key: Some("t".into()),
                ..make_thread("arranged-last")
            },
            T {
                created_at: "2026-03-01T09:00:00.000Z".into(),
                unsettled_at: Some("2026-03-09T12:00:00.000Z".into()),
                ..make_thread("reopened")
            },
        ]);
        assert_eq!(
            ids(&sorted),
            strings(&["reopened", "new", "arranged-first", "arranged-last"])
        );
    }

    #[test]
    fn breaks_equal_order_keys_and_timestamps_by_thread_then_environment() {
        for key in [None, Some("m")] {
            let threads: Vec<T> = [
                ("thread-b", "env-a"),
                ("thread-a", "env-b"),
                ("thread-a", "env-a"),
            ]
            .iter()
            .map(|(id, env)| T {
                environment_id: Some(env.to_string()),
                active_order_key: key.map(str::to_string),
                ..make_thread(id)
            })
            .collect();
            let sorted = sort_active_threads_by_order_key(threads);
            assert_eq!(
                sorted
                    .iter()
                    .map(|t| format!("{}:{}", t.id, t.environment_id.clone().unwrap()))
                    .collect::<Vec<_>>(),
                strings(&["thread-a:env-a", "thread-a:env-b", "thread-b:env-a"])
            );
        }
    }

    #[test]
    fn applies_every_move_across_a_mixed_keyless_and_keyed_section() {
        let threads: Vec<T> = (0..6)
            .map(|index| T {
                created_at: format!("2026-03-09T0{}:00:00.000Z", 6 - index),
                active_order_key: if index < 3 {
                    None
                } else {
                    Some(["f", "m", "t"][index - 3].to_string())
                },
                ..make_thread(&index.to_string())
            })
            .collect();
        let all_ids: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
        let keys_by_id: HashMap<String, Option<String>> = threads
            .iter()
            .map(|t| (t.id.clone(), t.active_order_key.clone()))
            .collect();
        for moved in &all_ids {
            for target in 0..all_ids.len() {
                let mut desired: Vec<String> =
                    all_ids.iter().filter(|id| *id != moved).cloned().collect();
                desired.insert(target, moved.clone());
                let assignments = plan_pinned_reorder(&desired, &keys_by_id, moved);
                let next: HashMap<&str, &str> = assignments
                    .iter()
                    .map(|a| (a.id.as_str(), a.order_key.as_str()))
                    .collect();
                let updated: Vec<T> = threads
                    .iter()
                    .map(|t| T {
                        active_order_key: next
                            .get(t.id.as_str())
                            .map(|k| k.to_string())
                            .or(t.active_order_key.clone()),
                        ..t.clone()
                    })
                    .collect();
                assert_eq!(ids(&sort_active_threads_by_order_key(updated)), desired);
            }
        }
    }

    #[test]
    fn moves_a_keyless_thread_into_the_arranged_run_with_one_write() {
        let assignments = plan_pinned_move(
            &strings(&["new", "reopened", "first", "last"]),
            &keys(&[
                ("new", None),
                ("reopened", None),
                ("first", Some("f")),
                ("last", Some("t")),
            ]),
            "reopened",
            false,
        )
        .unwrap();
        assert_eq!(assignments.len(), 1);
        assert_eq!(assignments[0].id, "reopened");
        assert!(assignments[0].order_key.as_str() > "f");
        assert!(assignments[0].order_key.as_str() < "t");
    }

    #[test]
    fn materializes_a_large_active_list_without_changing_the_requested_order() {
        let threads: Vec<T> = (0..1_200).map(|i| make_thread(&i.to_string())).collect();
        let mut ordered: Vec<String> = threads.iter().map(|t| t.id.clone()).collect();
        ordered.reverse();
        let keys_by_id: HashMap<String, Option<String>> =
            threads.iter().map(|t| (t.id.clone(), None)).collect();
        let assignments = plan_pinned_reorder(&ordered, &keys_by_id, &ordered[0]);
        let next: HashMap<&str, &str> = assignments
            .iter()
            .map(|a| (a.id.as_str(), a.order_key.as_str()))
            .collect();
        let updated: Vec<T> = threads
            .iter()
            .map(|t| T {
                active_order_key: next.get(t.id.as_str()).map(|k| k.to_string()),
                ..t.clone()
            })
            .collect();
        assert_eq!(ids(&sort_active_threads_by_order_key(updated)), ordered);
    }
}

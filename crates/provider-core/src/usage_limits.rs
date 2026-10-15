//! Port of upstream/packages/provider-core/src/server/usageLimits.ts.
//!
//! Upstream's `applyUsageLimitsUpdate` / `resolveUsageLimitsAfterProbe` return
//! the *same object* when nothing moved and callers test identity. Rust
//! callers compare the returned value with the published one (`==`), which is
//! equivalent because a changed result always differs structurally.

use turin_contracts::provider_usage_limits::{
    ProviderUsageLimitsUpdate, ServerProviderUsageLimits, ServerProviderUsageUnavailable,
    ServerProviderUsageUnavailableReason, ServerProviderUsageWindow, ServerProviderUsageWindowKind,
};
use turin_contracts::{IsoDateTime, TrimmedNonEmptyString};

fn window_kind_order(kind: ServerProviderUsageWindowKind) -> u8 {
    match kind {
        ServerProviderUsageWindowKind::Session => 0,
        ServerProviderUsageWindowKind::Weekly => 1,
        ServerProviderUsageWindowKind::Monthly => 2,
        ServerProviderUsageWindowKind::Other => 3,
    }
}

pub fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() {
        value.clamp(0.0, 100.0)
    } else {
        0.0
    }
}

fn sort_windows(mut windows: Vec<ServerProviderUsageWindow>) -> Vec<ServerProviderUsageWindow> {
    windows.sort_by(|left, right| {
        window_kind_order(left.kind)
            .cmp(&window_kind_order(right.kind))
            .then_with(|| left.id.as_str().cmp(right.id.as_str()))
    });
    windows
}

fn empty_limits(
    checked_at: IsoDateTime,
    windows: Vec<ServerProviderUsageWindow>,
) -> ServerProviderUsageLimits {
    ServerProviderUsageLimits {
        checked_at,
        windows,
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: None,
    }
}

pub fn make_usage_limits(
    checked_at: IsoDateTime,
    windows: impl IntoIterator<Item = ServerProviderUsageWindow>,
) -> ServerProviderUsageLimits {
    empty_limits(checked_at, sort_windows(windows.into_iter().collect()))
}

pub fn make_unavailable_usage_limits(
    checked_at: IsoDateTime,
    reason: ServerProviderUsageUnavailableReason,
    message: Option<&str>,
) -> ServerProviderUsageLimits {
    ServerProviderUsageLimits {
        unavailable: Some(ServerProviderUsageUnavailable {
            reason,
            message: message.and_then(|message| TrimmedNonEmptyString::new(message).ok()),
        }),
        ..empty_limits(checked_at, Vec::new())
    }
}

/// Fold a sparse runtime update into the limits a provider currently
/// publishes. Windows upsert by `id`; a window the update omits keeps its
/// previous values, and a window that arrives without `resetsAt` or
/// `windowDurationMins` keeps whatever the last probe resolved for it. An
/// update with no windows leaves `previous` untouched.
///
/// An `unsupported` snapshot stays unsupported: an account that cannot have
/// subscription windows will not start reporting them mid-turn.
pub fn apply_usage_limits_update(
    previous: Option<&ServerProviderUsageLimits>,
    update: &ProviderUsageLimitsUpdate,
    checked_at: &str,
) -> Option<ServerProviderUsageLimits> {
    if update.windows.is_empty()
        || previous
            .and_then(|previous| previous.unavailable.as_ref())
            .is_some_and(|unavailable| {
                unavailable.reason == ServerProviderUsageUnavailableReason::Unsupported
            })
    {
        return previous.cloned();
    }
    // Insertion-ordered, like a JS `Map`.
    let mut merged: Vec<ServerProviderUsageWindow> = previous
        .map(|previous| previous.windows.clone())
        .unwrap_or_default();
    // Codex sends this notification beside every token-usage tick, almost
    // always with unchanged numbers. Decide "nothing changed" per window on
    // the way through so the no-op case never allocates a new snapshot.
    let mut changed = false;
    for window in &update.windows {
        let existing_index = merged.iter().position(|existing| existing.id == window.id);
        let existing = existing_index.map(|index| &merged[index]);
        let mut next = window.clone();
        next.used_percent = clamp_percent(window.used_percent);
        if next.resets_at.is_none() {
            next.resets_at = existing.and_then(|existing| existing.resets_at.clone());
        }
        if next.window_duration_mins.is_none() {
            next.window_duration_mins = existing.and_then(|existing| existing.window_duration_mins);
        }
        if existing != Some(&next) {
            match existing_index {
                Some(index) => merged[index] = next,
                None => merged.push(next),
            }
            changed = true;
        }
    }
    if !changed && previous.is_some_and(|previous| previous.unavailable.is_none()) {
        return previous.cloned();
    }
    let mut limits = make_usage_limits(checked_at.to_owned(), merged);
    if let Some(reset_credits) = previous.and_then(|previous| previous.reset_credits.clone()) {
        limits.reset_credits = Some(reset_credits);
    }
    Some(limits)
}

/// Choose what to publish after a status probe finishes. A probe that failed
/// this time must not wipe bars a previous probe or a turn already
/// established, so the last good snapshot stays; `unsupported` is
/// authoritative and replaces them.
///
/// A successful probe replaces the published windows outright, including any
/// runtime update that landed while it was running. That is a deliberate
/// trade-off: the Codex and Claude reads take a few seconds at most, the
/// probe is the fresher full read in every case except that window, and the
/// per-window epoch bookkeeping needed to reconcile the two was more code
/// than the sub-second regression it prevented. The next runtime event
/// corrects it.
pub fn resolve_usage_limits_after_probe(
    published: Option<&ServerProviderUsageLimits>,
    probed: Option<&ServerProviderUsageLimits>,
) -> Option<ServerProviderUsageLimits> {
    if let (Some(probed), Some(published)) = (probed, published)
        && probed.unavailable.as_ref().is_some_and(|unavailable| {
            unavailable.reason == ServerProviderUsageUnavailableReason::ProbeFailed
        })
        && published.unavailable.is_none()
    {
        return Some(published.clone());
    }
    probed.cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CHECKED_AT: &str = "2026-09-03T12:00:00.000Z";

    fn window(value: serde_json::Value) -> ServerProviderUsageWindow {
        serde_json::from_value(value).unwrap()
    }

    fn session() -> ServerProviderUsageWindow {
        window(json!({
            "id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 40,
            "windowDurationMins": 300, "resetsAt": "2026-09-03T14:00:00.000Z"
        }))
    }

    fn weekly() -> ServerProviderUsageWindow {
        window(json!({
            "id": "seven_day", "kind": "weekly", "label": "Weekly", "usedPercent": 20,
            "windowDurationMins": 10080
        }))
    }

    fn published() -> ServerProviderUsageLimits {
        empty_limits(CHECKED_AT.into(), vec![session(), weekly()])
    }

    fn unavailable(reason: ServerProviderUsageUnavailableReason) -> ServerProviderUsageLimits {
        ServerProviderUsageLimits {
            unavailable: Some(ServerProviderUsageUnavailable {
                reason,
                message: None,
            }),
            ..empty_limits(CHECKED_AT.into(), vec![])
        }
    }

    #[test]
    fn returns_the_published_value_when_no_window_moved() {
        let next = apply_usage_limits_update(
            Some(&published()),
            &ProviderUsageLimitsUpdate {
                windows: vec![
                    weekly(),
                    window(
                        json!({"id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 40}),
                    ),
                ],
            },
            "2026-09-03T12:00:05.000Z",
        );
        assert_eq!(next, Some(published()));
    }

    #[test]
    fn upserts_by_id_and_keeps_the_reset_a_percent_only_update_omits() {
        let next = apply_usage_limits_update(
            Some(&published()),
            &ProviderUsageLimitsUpdate {
                windows: vec![window(
                    json!({"id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 55}),
                )],
            },
            "2026-09-03T12:00:05.000Z",
        );
        let mut moved = session();
        moved.used_percent = 55.0;
        assert_eq!(
            next,
            Some(empty_limits(
                "2026-09-03T12:00:05.000Z".into(),
                vec![moved, weekly()]
            ))
        );
    }

    #[test]
    fn leaves_an_unsupported_account_and_an_empty_update_alone() {
        let unsupported = unavailable(ServerProviderUsageUnavailableReason::Unsupported);
        assert_eq!(
            apply_usage_limits_update(
                Some(&unsupported),
                &ProviderUsageLimitsUpdate {
                    windows: vec![session()]
                },
                CHECKED_AT
            ),
            Some(unsupported)
        );
        assert_eq!(
            apply_usage_limits_update(
                Some(&published()),
                &ProviderUsageLimitsUpdate { windows: vec![] },
                CHECKED_AT
            ),
            Some(published())
        );
    }

    #[test]
    fn preserves_reset_credits_when_a_streamed_window_update_changes_usage() {
        let reset_credits = serde_json::from_value(
            json!({"availableCount": 2, "nextExpiresAt": "2026-10-01T00:00:00.000Z"}),
        )
        .unwrap();
        let mut previous = published();
        previous.reset_credits = Some(reset_credits);
        let mut moved = session();
        moved.used_percent = 55.0;
        let next = apply_usage_limits_update(
            Some(&previous),
            &ProviderUsageLimitsUpdate {
                windows: vec![moved.clone()],
            },
            "2026-09-03T12:00:05.000Z",
        )
        .unwrap();
        assert_eq!(next.windows, vec![moved, weekly()]);
        assert_eq!(next.reset_credits, previous.reset_credits);
        assert_eq!(next.checked_at, "2026-09-03T12:00:05.000Z");
    }

    #[test]
    fn keeps_the_last_good_windows_through_a_failed_probe_but_not_an_unsupported_one() {
        let failed = unavailable(ServerProviderUsageUnavailableReason::ProbeFailed);
        let unsupported = unavailable(ServerProviderUsageUnavailableReason::Unsupported);
        let published = published();
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&published), Some(&failed)),
            Some(published.clone())
        );
        assert_eq!(
            resolve_usage_limits_after_probe(Some(&published), Some(&unsupported)),
            Some(unsupported)
        );
        assert_eq!(
            resolve_usage_limits_after_probe(None, Some(&failed)),
            Some(failed)
        );
    }

    #[test]
    fn clamps_and_orders_windows() {
        assert_eq!(clamp_percent(f64::NAN), 0.0);
        assert_eq!(clamp_percent(140.0), 100.0);
        let limits = make_usage_limits(CHECKED_AT.into(), vec![weekly(), session()]);
        assert_eq!(limits.windows[0].id.as_str(), "five_hour");
    }
}

//! Port of upstream/packages/client-runtime/src/state/threadSettled.ts
//!
//! Upstream accepts both the V2 shell (`runtime`, `latestRun`) and the
//! legacy V1 shapes (`session`, `latestTurn`). The V1 importer is cut
//! (PORTING), so only the V2 fields exist here.
// A faithful port keeps upstream's whole API (rule 3); callers that are
// not ported yet (T14 reducers, the chat header menu, settings) use the rest.
#![allow(dead_code)]

use chrono::{
    DateTime, Datelike, Duration, Local, MappedLocalTime, NaiveDate, NaiveTime, TimeZone, Timelike,
};

use crate::state::models::{EnvironmentThreadShell, RunStatus};
use crate::timestamp_format::{
    TimestampFormat, format_time_of_day, parse_timestamp_ms, to_iso_string,
};

/// A queued turn start lives for at most this long: session adoption takes
/// seconds, so a user message still unadopted after the grace window is a
/// failed start (or stale data), not pending work.
pub const QUEUED_TURN_START_GRACE_MS: i64 = 2 * 60 * 1_000;
const HOUR_MS: i64 = 60 * 60 * 1_000;
const DAY_MS: i64 = 24 * HOUR_MS;

/// `hasQueuedTurnStart`: a user message no turn has picked up yet.
pub fn has_queued_turn_start(shell: &EnvironmentThreadShell, now: &str) -> bool {
    if matches!(
        shell.runtime.as_ref().map(|runtime| runtime.status),
        Some(RunStatus::Preparing | RunStatus::Queued | RunStatus::Starting)
    ) {
        return true;
    }
    let Some(latest_user_message_at) = shell.latest_user_message_at.as_deref() else {
        return false;
    };
    let Some(message_at) = parse_timestamp_ms(latest_user_message_at) else {
        return false;
    };
    let Some(now_ms) = parse_timestamp_ms(now) else {
        return false;
    };
    // Bounded on both sides: message timestamps originate on whichever device
    // sent the message, so a clock ahead of this one yields a negative age.
    if (now_ms - message_at).abs() > QUEUED_TURN_START_GRACE_MS {
        return false;
    }
    let Some(turn) = shell.latest_run.as_ref() else {
        return true;
    };
    [&turn.requested_at, &turn.started_at, &turn.completed_at]
        .into_iter()
        .all(|candidate| match candidate {
            None => true,
            // `Date.parse(x) < messageAt` is false for NaN.
            Some(value) => parse_timestamp_ms(value).is_some_and(|ms| ms < message_at),
        })
}

fn ms_after(value: Option<&str>, reference: Option<&str>) -> bool {
    match (
        value.and_then(parse_timestamp_ms),
        reference.and_then(parse_timestamp_ms),
    ) {
        (Some(value), Some(reference)) => value > reference,
        _ => false,
    }
}

fn completed_after_snooze(shell: &EnvironmentThreadShell) -> Option<&str> {
    let snoozed_at = shell.snoozed_at.as_deref()?;
    let run = shell.latest_run.as_ref()?;
    if run.status != RunStatus::Completed {
        return None;
    }
    let completed_at = run.completed_at.as_deref()?;
    ms_after(Some(completed_at), Some(snoozed_at)).then_some(completed_at)
}

/// `threadRaisedHandWhileSnoozed`: something outranks the user's snooze.
pub fn thread_raised_hand_while_snoozed(shell: &EnvironmentThreadShell) -> bool {
    if shell.has_pending_approvals || shell.has_pending_user_input {
        return true;
    }
    if shell.pending_subagent_request.is_some() {
        return true;
    }
    // Only a FRESH failure raises the hand: a thread snoozed while already
    // failed stays snoozed.
    if let Some(runtime) = &shell.runtime
        && runtime.status == RunStatus::Failed
        && (shell.snoozed_at.is_none()
            || ms_after(Some(&runtime.updated_at), shell.snoozed_at.as_deref()))
    {
        return true;
    }
    completed_after_snooze(shell).is_some()
}

/// `canSnooze`: never on blocked-on-you work or a queued turn start.
pub fn can_snooze(shell: &EnvironmentThreadShell, now: &str) -> bool {
    if shell.has_pending_approvals || shell.has_pending_user_input {
        return false;
    }
    if shell.pending_subagent_request.is_some() {
        return false;
    }
    !has_queued_turn_start(shell, now)
}

/// `effectiveSnoozed`: hidden while the wake time is in the future and the
/// thread has not raised its hand. Malformed data never hides a thread.
pub fn effective_snoozed(shell: &EnvironmentThreadShell, now: &str) -> bool {
    let Some(snoozed_until) = shell.snoozed_until.as_deref() else {
        return false;
    };
    let Some(wake_at_ms) = parse_timestamp_ms(snoozed_until) else {
        return false;
    };
    // `wakeAtMs <= Date.parse(now)` is false for a NaN now, which keeps the snooze.
    if parse_timestamp_ms(now).is_some_and(|now_ms| wake_at_ms <= now_ms) {
        return false;
    }
    !thread_raised_hand_while_snoozed(shell)
}

/// `threadWokeAt`: when a previously-snoozed thread woke, or `None`.
pub fn thread_woke_at(shell: &EnvironmentThreadShell, now: &str) -> Option<String> {
    let snoozed_until = shell.snoozed_until.as_deref()?;
    let wake_at_ms = parse_timestamp_ms(snoozed_until)?;
    // An early hand-raise wake stays authoritative even after the scheduled
    // wake time passes.
    if thread_raised_hand_while_snoozed(shell) {
        if let Some(completed_at) = completed_after_snooze(shell) {
            return Some(completed_at.to_string());
        }
        return shell
            .runtime
            .as_ref()
            .map(|runtime| runtime.updated_at.clone())
            .or_else(|| shell.snoozed_at.clone());
    }
    parse_timestamp_ms(now)
        .is_some_and(|now_ms| wake_at_ms <= now_ms)
        .then(|| snoozed_until.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnoozePresetId {
    Hour,
    ThreeHours,
    Evening,
    Tomorrow,
    NextWeek,
}

impl SnoozePresetId {
    /// The upstream id (`snooze:<id>` menu values).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hour => "hour",
            Self::ThreeHours => "three-hours",
            Self::Evening => "evening",
            Self::Tomorrow => "tomorrow",
            Self::NextWeek => "next-week",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnoozePreset {
    pub id: SnoozePresetId,
    pub label: &'static str,
    /// Menu-row time column ("9:00 AM", "Mon 9:00 AM").
    pub when_label: String,
    /// ISO wake time.
    pub snoozed_until: String,
}

const EVENING_HOUR: u32 = 18;
const MORNING_HOUR: u32 = 9;

/// The instant a local time means when it is ambiguous (a DST fall-back fold):
/// the earlier one, which is what `new Date("YYYY-MM-DDTHH:MM")` and
/// `setHours` pick. Chrono's `earliest()`/`latest()` are not usable here:
/// for `Local` it orders the fold by the tz database's local-time-type list,
/// so `latest()` is the pre-transition instant on America/New_York.
fn earlier_local_instant<Tz: TimeZone>(
    mapped: MappedLocalTime<DateTime<Tz>>,
) -> Option<DateTime<Tz>> {
    match mapped {
        MappedLocalTime::Single(instant) => Some(instant),
        MappedLocalTime::Ambiguous(a, b) => Some(if a <= b { a } else { b }),
        MappedLocalTime::None => None,
    }
}

fn snooze_at_hour<Tz: TimeZone>(base: &DateTime<Tz>, hour: u32) -> DateTime<Tz> {
    let date = base.date_naive();
    let naive = date.and_time(NaiveTime::from_hms_opt(hour, 0, 0).expect("valid hour"));
    // `setHours` on a skipped local hour moves forward; the base instant is
    // the fallback here.
    earlier_local_instant(base.timezone().from_local_datetime(&naive))
        .unwrap_or_else(|| base.clone())
}

/// Calendar-day advance (DST-safe), as `setDate(getDate() + days)`.
fn add_snooze_days<Tz: TimeZone>(base: &DateTime<Tz>, days: i64) -> DateTime<Tz> {
    let naive = base.naive_local() + Duration::days(days);
    earlier_local_instant(base.timezone().from_local_datetime(&naive))
        .unwrap_or_else(|| base.clone() + Duration::days(days))
}

/// `resolveSnoozePresets`: the shared "snooze until" choices. "This evening"
/// only while it is more than an hour away; "Next week" collapses into
/// "Tomorrow" on Sundays.
pub fn resolve_snooze_presets<Tz: TimeZone>(
    now: &DateTime<Tz>,
    timestamp_format: TimestampFormat,
) -> Vec<SnoozePreset>
where
    Tz::Offset: std::fmt::Display,
{
    let label = |date: &DateTime<Tz>| format_time_of_day(date, timestamp_format);
    let iso = |date: &DateTime<Tz>| to_iso_string(date.timestamp_millis());
    let in_an_hour = now.clone() + Duration::milliseconds(HOUR_MS);
    let in_three_hours = now.clone() + Duration::milliseconds(3 * HOUR_MS);
    let mut presets = vec![
        SnoozePreset {
            id: SnoozePresetId::Hour,
            label: "In 1 hour",
            when_label: label(&in_an_hour),
            snoozed_until: iso(&in_an_hour),
        },
        SnoozePreset {
            id: SnoozePresetId::ThreeHours,
            label: "In 3 hours",
            when_label: label(&in_three_hours),
            snoozed_until: iso(&in_three_hours),
        },
    ];
    let evening = snooze_at_hour(now, EVENING_HOUR);
    if evening.timestamp_millis() - now.timestamp_millis() > HOUR_MS {
        presets.push(SnoozePreset {
            id: SnoozePresetId::Evening,
            label: "This evening",
            when_label: label(&evening),
            snoozed_until: iso(&evening),
        });
    }
    let tomorrow = snooze_at_hour(&add_snooze_days(now, 1), MORNING_HOUR);
    presets.push(SnoozePreset {
        id: SnoozePresetId::Tomorrow,
        label: "Tomorrow",
        when_label: label(&tomorrow),
        snoozed_until: iso(&tomorrow),
    });
    let weekday = i64::from(now.weekday().num_days_from_sunday());
    let days_until_monday = match (1 - weekday + 7) % 7 {
        0 => 7,
        days => days,
    };
    let next_week = snooze_at_hour(&add_snooze_days(now, days_until_monday), MORNING_HOUR);
    if next_week.timestamp_millis() != tomorrow.timestamp_millis() {
        presets.push(SnoozePreset {
            id: SnoozePresetId::NextWeek,
            label: "Next week",
            // Sidebar.snooze.ts re-renders this with the user's timestamp
            // format; the weekday prefix is shared.
            when_label: format!("{} {}", next_week.format("%a"), label(&next_week)),
            snoozed_until: iso(&next_week),
        });
    }
    presets
}

/// `snoozeWakeLabel`: compact "wakes in" label ("2h", "18h", "3d"); minutes
/// round up so a snooze never reads "0m" while still hidden.
pub fn snooze_wake_label(snoozed_until: &str, now: &str) -> String {
    let (Some(wake_ms), Some(now_ms)) =
        (parse_timestamp_ms(snoozed_until), parse_timestamp_ms(now))
    else {
        return "now".into();
    };
    let remaining = wake_ms - now_ms;
    if remaining <= 0 {
        return "now".into();
    }
    let ceil_div = |value: i64, unit: i64| (value + unit - 1) / unit;
    if remaining < HOUR_MS {
        return format!("{}m", ceil_div(remaining, 60_000).max(1));
    }
    if remaining < DAY_MS {
        return format!("{}h", ceil_div(remaining, HOUR_MS));
    }
    format!("{}d", ceil_div(remaining, DAY_MS))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CustomSnoozeUnit {
    Minutes,
    Hours,
    Days,
}

/// `CustomSnoozeInput`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomSnoozeInput {
    Date {
        date: String,
        time: String,
    },
    Duration {
        amount: String,
        unit: CustomSnoozeUnit,
    },
}

fn matches_digits(value: &str, pattern: &[usize], separator: char) -> bool {
    let parts: Vec<&str> = value.split(separator).collect();
    parts.len() == pattern.len()
        && parts
            .iter()
            .zip(pattern)
            .all(|(part, len)| part.len() == *len && part.bytes().all(|b| b.is_ascii_digit()))
}

/// JS `Number(string)` for the duration field: decimal literals only;
/// `""` and non-finite values are rejected by the caller's checks.
fn js_number(value: &str) -> f64 {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return 0.;
    }
    if trimmed.eq_ignore_ascii_case("nan") {
        return f64::NAN;
    }
    trimmed.parse::<f64>().unwrap_or(f64::NAN)
}

/// `resolveCustomSnooze`: local calendar input or elapsed time; rejects
/// past and invalid dates (including rolled-over and DST-skipped times).
pub fn resolve_custom_snooze<Tz: TimeZone>(
    input: &CustomSnoozeInput,
    now: &DateTime<Tz>,
) -> Option<String> {
    let wake_ms = match input {
        CustomSnoozeInput::Duration { amount, unit } => {
            let amount = js_number(amount);
            if !amount.is_finite() || amount <= 0. {
                return None;
            }
            let unit_ms = match unit {
                CustomSnoozeUnit::Minutes => 60_000.,
                CustomSnoozeUnit::Hours => HOUR_MS as f64,
                CustomSnoozeUnit::Days => DAY_MS as f64,
            };
            let wake = now.timestamp_millis() as f64 + amount * unit_ms;
            // Out of `Date`'s range (±8.64e15 ms) is an Invalid Date upstream.
            if !wake.is_finite() || wake.abs() > 8.64e15 {
                return None;
            }
            wake as i64
        }
        CustomSnoozeInput::Date { date, time } => {
            if !matches_digits(date, &[4, 2, 2], '-') || !matches_digits(time, &[2, 2], ':') {
                return None;
            }
            let date_value = NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
            let time_value = NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            // A fall-back fold is accepted at its earlier instant, as upstream
            // does. A DST gap has no instant and is rejected here.
            let local = earlier_local_instant(
                now.timezone()
                    .from_local_datetime(&date_value.and_time(time_value)),
            )?;
            if local_snooze_date(&local) != *date || local_snooze_time(&local) != *time {
                return None;
            }
            local.timestamp_millis()
        }
    };
    (wake_ms > now.timestamp_millis()).then(|| to_iso_string(wake_ms))
}

/// `localSnoozeDate`: `YYYY-MM-DD` in local time.
pub fn local_snooze_date<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    format!("{:04}-{:02}-{:02}", date.year(), date.month(), date.day())
}

/// `localSnoozeTime`: `HH:MM` in local time.
pub fn local_snooze_time<Tz: TimeZone>(date: &DateTime<Tz>) -> String {
    format!("{:02}:{:02}", date.hour(), date.minute())
}

/// Presets for "now" in the user's zone.
pub fn resolve_snooze_presets_now(timestamp_format: TimestampFormat) -> Vec<SnoozePreset> {
    resolve_snooze_presets(&Local::now(), timestamp_format)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::models::fixtures::run_id;
    use crate::state::models::fixtures::{runtime, shell};
    use crate::state::models::{PendingSubagentRequest, ThreadRunSummary};
    use chrono::{FixedOffset, NaiveDateTime};

    const NOW: &str = "2026-04-10T12:00:00.000Z";
    const SNOOZED_AT: &str = "2026-04-10T09:00:00.000Z";
    const FUTURE_WAKE: &str = "2026-04-11T09:00:00.000Z";
    const PAST_WAKE: &str = "2026-04-10T10:00:00.000Z";

    fn local_date(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(year, month, day, hour, minute, 0)
            .earliest()
            .unwrap()
    }

    #[derive(Default)]
    struct Shell {
        snoozed_until: Option<&'static str>,
        snoozed_at: Option<&'static str>,
        session: Option<RunStatus>,
        pending: Option<&'static str>,
        turn_completed_at: Option<&'static str>,
    }

    fn make_shell(input: Shell) -> EnvironmentThreadShell {
        let mut thread = shell("thread-1");
        thread.snoozed_until = input.snoozed_until.map(Into::into);
        thread.snoozed_at = input
            .snoozed_at
            .or(input.snoozed_until.map(|_| SNOOZED_AT))
            .map(Into::into);
        thread.has_pending_approvals = input.pending == Some("approval");
        thread.has_pending_user_input = input.pending == Some("user-input");
        thread.runtime = input.session.map(|status| {
            let mut rt = runtime(status);
            rt.updated_at = "2026-04-10T11:00:00.000Z".into();
            rt
        });
        thread.latest_run = input
            .turn_completed_at
            .map(|completed_at| ThreadRunSummary {
                run_id: run_id("turn-1"),
                status: RunStatus::Completed,
                requested_at: Some(SNOOZED_AT.into()),
                started_at: None,
                completed_at: Some(completed_at.into()),
            });
        thread
    }

    // describe("effectiveSnoozed")
    #[test]
    fn hides_a_thread_whose_wake_time_is_in_the_future() {
        assert!(effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn stops_classifying_as_snoozed_once_the_wake_time_passes() {
        assert!(!effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(PAST_WAKE),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn never_snoozes_a_thread_with_no_snooze_state() {
        assert!(!effective_snoozed(&make_shell(Shell::default()), NOW));
    }

    #[test]
    fn never_hides_on_malformed_wake_data() {
        assert!(!effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some("not-a-date"),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn wakes_early_when_the_agent_is_blocked_on_the_user() {
        for pending in ["approval", "user-input"] {
            assert!(!effective_snoozed(
                &make_shell(Shell {
                    snoozed_until: Some(FUTURE_WAKE),
                    pending: Some(pending),
                    ..Default::default()
                }),
                NOW
            ));
        }
    }

    #[test]
    fn wakes_early_on_a_failure_that_happened_after_the_snooze() {
        assert!(!effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                session: Some(RunStatus::Failed),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn stays_snoozed_when_the_failure_predates_the_snooze() {
        assert!(effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                session: Some(RunStatus::Failed),
                snoozed_at: Some("2026-04-10T11:30:00.000Z"),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn stays_snoozed_while_the_session_keeps_working() {
        assert!(effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                session: Some(RunStatus::Running),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn wakes_early_when_a_run_completes_after_the_snooze_was_set() {
        assert!(!effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                turn_completed_at: Some("2026-04-10T10:30:00.000Z"),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn ignores_runs_that_completed_before_the_snooze() {
        assert!(effective_snoozed(
            &make_shell(Shell {
                snoozed_until: Some(FUTURE_WAKE),
                turn_completed_at: Some("2026-04-10T08:00:00.000Z"),
                ..Default::default()
            }),
            NOW
        ));
    }

    // describe("threadRaisedHandWhileSnoozed")
    #[test]
    fn is_false_for_a_quiet_snoozed_thread() {
        assert!(!thread_raised_hand_while_snoozed(&make_shell(Shell {
            snoozed_until: Some(FUTURE_WAKE),
            ..Default::default()
        })));
    }

    #[test]
    fn is_true_for_approvals_input_and_failures() {
        for input in [
            Shell {
                snoozed_until: Some(FUTURE_WAKE),
                pending: Some("approval"),
                ..Default::default()
            },
            Shell {
                snoozed_until: Some(FUTURE_WAKE),
                pending: Some("user-input"),
                ..Default::default()
            },
            Shell {
                snoozed_until: Some(FUTURE_WAKE),
                session: Some(RunStatus::Failed),
                ..Default::default()
            },
        ] {
            assert!(thread_raised_hand_while_snoozed(&make_shell(input)));
        }
    }

    // describe("canSnooze")
    #[test]
    fn allows_snoozing_quiet_and_working_threads_alike() {
        assert!(can_snooze(&make_shell(Shell::default()), NOW));
        assert!(can_snooze(
            &make_shell(Shell {
                session: Some(RunStatus::Running),
                ..Default::default()
            }),
            NOW
        ));
    }

    #[test]
    fn refuses_blocked_on_you_work() {
        assert!(!can_snooze(
            &make_shell(Shell {
                pending: Some("approval"),
                ..Default::default()
            }),
            NOW
        ));
        assert!(!can_snooze(
            &make_shell(Shell {
                pending: Some("user-input"),
                ..Default::default()
            }),
            NOW
        ));
        let mut subagent = make_shell(Shell::default());
        subagent.pending_subagent_request = Some(PendingSubagentRequest::Approval);
        assert!(!can_snooze(&subagent, NOW));
    }

    #[test]
    fn refuses_a_queued_turn_start() {
        let mut fresh = make_shell(Shell::default());
        fresh.latest_user_message_at = Some("2026-04-10T11:59:30.000Z".into());
        assert!(!can_snooze(&fresh, NOW));
        let mut stale = make_shell(Shell::default());
        stale.latest_user_message_at = Some("2026-04-10T11:00:00.000Z".into());
        assert!(can_snooze(&stale, NOW));
    }

    // describe("hasQueuedTurnStart")
    #[test]
    fn expires_queued_state_after_two_minutes() {
        let mut thread = shell("t");
        thread.latest_user_message_at = Some("2026-04-10T11:57:59.000Z".into());
        assert!(!has_queued_turn_start(&thread, NOW));
    }

    // The upstream case also clears on a V1 `session.status === "error"`;
    // V1 shapes are cut, so only the adoption half applies.
    #[test]
    fn clears_queued_state_when_a_turn_adopts_the_message() {
        let message_at = "2026-04-10T11:59:00.000Z";
        let mut adopted = shell("t");
        adopted.latest_user_message_at = Some(message_at.into());
        adopted.latest_run = Some(ThreadRunSummary {
            run_id: run_id("turn-adopted"),
            status: RunStatus::Running,
            requested_at: Some(message_at.into()),
            started_at: None,
            completed_at: None,
        });
        assert!(!has_queued_turn_start(&adopted, NOW));
    }

    #[test]
    fn bounds_future_client_clock_skew() {
        let mut far = shell("t");
        far.latest_user_message_at = Some("2026-04-10T12:03:00.000Z".into());
        let mut slight = shell("t");
        slight.latest_user_message_at = Some("2026-04-10T12:01:00.000Z".into());
        assert!(!has_queued_turn_start(&far, NOW));
        assert!(has_queued_turn_start(&slight, NOW));
    }

    // describe("threadWokeAt")
    #[test]
    fn is_null_for_never_snoozed_and_still_snoozed_threads() {
        assert_eq!(thread_woke_at(&make_shell(Shell::default()), NOW), None);
        assert_eq!(
            thread_woke_at(
                &make_shell(Shell {
                    snoozed_until: Some(FUTURE_WAKE),
                    ..Default::default()
                }),
                NOW
            ),
            None
        );
    }

    #[test]
    fn reports_the_wake_time_for_a_timer_wake() {
        assert_eq!(
            thread_woke_at(
                &make_shell(Shell {
                    snoozed_until: Some(PAST_WAKE),
                    ..Default::default()
                }),
                NOW
            )
            .as_deref(),
            Some(PAST_WAKE)
        );
    }

    #[test]
    fn reports_the_completion_time_for_an_early_run_completed_wake() {
        assert_eq!(
            thread_woke_at(
                &make_shell(Shell {
                    snoozed_until: Some(FUTURE_WAKE),
                    turn_completed_at: Some("2026-04-10T10:30:00.000Z"),
                    ..Default::default()
                }),
                NOW
            )
            .as_deref(),
            Some("2026-04-10T10:30:00.000Z")
        );
    }

    #[test]
    fn falls_back_to_session_activity_for_blocked_failed_early_wakes() {
        assert_eq!(
            thread_woke_at(
                &make_shell(Shell {
                    snoozed_until: Some(FUTURE_WAKE),
                    session: Some(RunStatus::Failed),
                    ..Default::default()
                }),
                NOW
            )
            .as_deref(),
            Some("2026-04-10T11:00:00.000Z")
        );
    }

    #[test]
    fn keeps_the_early_wake_authoritative_after_the_scheduled_time_passes() {
        assert_eq!(
            thread_woke_at(
                &make_shell(Shell {
                    snoozed_until: Some(PAST_WAKE),
                    turn_completed_at: Some("2026-04-10T09:30:00.000Z"),
                    ..Default::default()
                }),
                NOW
            )
            .as_deref(),
            Some("2026-04-10T09:30:00.000Z")
        );
    }

    // describe("snoozeWakeLabel")
    #[test]
    fn formats_remaining_time_coarsely_rounding_up() {
        let now = "2026-06-02T00:00:00.000Z";
        assert_eq!(snooze_wake_label("2026-06-02T00:30:00.000Z", now), "30m");
        assert_eq!(snooze_wake_label("2026-06-02T01:30:00.000Z", now), "2h");
        assert_eq!(snooze_wake_label("2026-06-03T02:00:00.000Z", now), "2d");
    }

    #[test]
    fn never_reads_zero_or_negative_while_still_snoozed() {
        let now = "2026-06-02T00:00:00.000Z";
        assert_eq!(snooze_wake_label("2026-06-02T00:00:30.000Z", now), "1m");
        assert_eq!(snooze_wake_label("2026-06-01T23:59:59.000Z", now), "now");
        assert_eq!(snooze_wake_label("not-a-date", now), "now");
        assert_eq!(snooze_wake_label("2026-06-02T09:00:00.000Z", "bad"), "now");
    }

    fn preset_ids(presets: &[SnoozePreset]) -> Vec<&'static str> {
        presets.iter().map(|preset| preset.id.as_str()).collect()
    }

    fn wake(presets: &[SnoozePreset], id: SnoozePresetId) -> DateTime<Local> {
        let ms = parse_timestamp_ms(&presets.iter().find(|p| p.id == id).unwrap().snoozed_until)
            .unwrap();
        Local.timestamp_millis_opt(ms).unwrap()
    }

    // describe("resolveSnoozePresets")
    #[test]
    fn offers_the_shared_desktop_and_mobile_choices() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 4, 8, 10, 0), TimestampFormat::Locale);
        assert_eq!(
            preset_ids(&presets),
            vec!["hour", "three-hours", "evening", "tomorrow", "next-week"]
        );
        let three = presets
            .iter()
            .find(|p| p.id == SnoozePresetId::ThreeHours)
            .unwrap();
        assert_eq!(
            three.snoozed_until,
            to_iso_string(local_date(2026, 4, 8, 13, 0).timestamp_millis())
        );
        assert_eq!(three.label, "In 3 hours");
        assert_eq!(
            presets
                .iter()
                .find(|p| p.id == SnoozePresetId::Evening)
                .unwrap()
                .label,
            "This evening"
        );
        assert_eq!(wake(&presets, SnoozePresetId::Tomorrow).hour(), 9);
    }

    #[test]
    fn drops_the_evening_choice_once_evening_is_near_or_past() {
        assert_eq!(
            preset_ids(&resolve_snooze_presets(
                &local_date(2026, 4, 8, 17, 30),
                TimestampFormat::Locale
            )),
            vec!["hour", "three-hours", "tomorrow", "next-week"]
        );
    }

    #[test]
    fn puts_next_week_on_the_following_monday() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 4, 6, 10, 0), TimestampFormat::Locale);
        let next_week = wake(&presets, SnoozePresetId::NextWeek);
        assert_eq!(next_week.weekday().num_days_from_sunday(), 1);
        assert_eq!(next_week.day(), 13);
    }

    #[test]
    fn drops_next_week_on_sundays_when_it_lands_on_the_same_monday_as_tomorrow() {
        let presets =
            resolve_snooze_presets(&local_date(2026, 8, 30, 7, 1), TimestampFormat::Locale);
        assert_eq!(
            preset_ids(&presets),
            vec!["hour", "three-hours", "evening", "tomorrow"]
        );
        assert_eq!(
            wake(&presets, SnoozePresetId::Tomorrow)
                .weekday()
                .num_days_from_sunday(),
            1
        );
    }

    // describe("custom snooze") (customSnooze.test.ts). The two TZ-stubbed
    // DST cases need a named zone; chrono-tz is not a dependency.
    fn custom_now() -> DateTime<Local> {
        local_date(2026, 9, 14, 14, 30)
    }

    #[test]
    fn converts_local_date_and_time_to_an_absolute_wake_time() {
        assert_eq!(
            resolve_custom_snooze(
                &CustomSnoozeInput::Date {
                    date: "2026-09-15".into(),
                    time: "09:15".into()
                },
                &custom_now()
            ),
            Some(to_iso_string(
                local_date(2026, 9, 15, 9, 15).timestamp_millis()
            ))
        );
    }

    #[test]
    fn rejects_invalid_or_non_future_calendar_input() {
        for (date, time) in [
            ("2026-09-14", "14:30"),
            ("2026-09-13", "09:00"),
            ("2027-02-29", "09:00"),
            ("2026-13-01", "09:00"),
            ("2026-09-15", "24:00"),
            ("2026-09-15", "09:60"),
            ("", "09:00"),
            ("2026-09-15", ""),
        ] {
            assert_eq!(
                resolve_custom_snooze(
                    &CustomSnoozeInput::Date {
                        date: date.into(),
                        time: time.into()
                    },
                    &custom_now()
                ),
                None,
                "{date} {time}"
            );
        }
    }

    #[test]
    fn resolves_durations_from_the_confirmation_time() {
        for (unit, amount, elapsed) in [
            (CustomSnoozeUnit::Minutes, "45", 45 * 60_000),
            (CustomSnoozeUnit::Hours, "1.5", 90 * 60_000),
            (CustomSnoozeUnit::Days, "2", 48 * 3_600_000),
        ] {
            let confirmed_at = custom_now() + Duration::minutes(5);
            assert_eq!(
                resolve_custom_snooze(
                    &CustomSnoozeInput::Duration {
                        amount: amount.into(),
                        unit
                    },
                    &confirmed_at
                ),
                Some(to_iso_string(confirmed_at.timestamp_millis() + elapsed))
            );
        }
    }

    #[test]
    fn rejects_invalid_duration() {
        for amount in ["", "0", "-1", "NaN", "Infinity", "1e300"] {
            assert_eq!(
                resolve_custom_snooze(
                    &CustomSnoozeInput::Duration {
                        amount: amount.into(),
                        unit: CustomSnoozeUnit::Hours
                    },
                    &custom_now()
                ),
                None,
                "{amount}"
            );
        }
    }

    /// America/New_York across the 2026 DST changes, fixed offsets only:
    /// spring forward Sun 2026-03-08 02:00 EST → 03:00 EDT (07:00Z), fall
    /// back Sun 2026-11-01 02:00 EDT → 01:00 EST (06:00Z). Local times in
    /// 01:00–02:00 on fall-back day are ambiguous; 02:00–03:00 on spring-
    /// forward day do not exist.
    #[derive(Clone, Copy, Debug)]
    struct NewYork2026;

    impl NewYork2026 {
        fn edt() -> FixedOffset {
            FixedOffset::west_opt(4 * 3600).unwrap()
        }
        fn est() -> FixedOffset {
            FixedOffset::west_opt(5 * 3600).unwrap()
        }
        fn spring_utc() -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2026, 3, 8)
                .unwrap()
                .and_hms_opt(7, 0, 0)
                .unwrap()
        }
        fn fall_utc() -> NaiveDateTime {
            NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(6, 0, 0)
                .unwrap()
        }
    }

    impl TimeZone for NewYork2026 {
        type Offset = FixedOffset;

        fn from_offset(_: &FixedOffset) -> Self {
            NewYork2026
        }

        fn offset_from_local_date(&self, local: &NaiveDate) -> MappedLocalTime<FixedOffset> {
            self.offset_from_local_datetime(&local.and_time(NaiveTime::MIN))
        }

        fn offset_from_local_datetime(
            &self,
            local: &NaiveDateTime,
        ) -> MappedLocalTime<FixedOffset> {
            let date = |m, d| NaiveDate::from_ymd_opt(2026, m, d).unwrap();
            let fold_start = date(11, 1).and_hms_opt(1, 0, 0).unwrap();
            let fold_end = date(11, 1).and_hms_opt(2, 0, 0).unwrap();
            let gap_start = date(3, 8).and_hms_opt(2, 0, 0).unwrap();
            let gap_end = date(3, 8).and_hms_opt(3, 0, 0).unwrap();
            if *local < gap_start {
                MappedLocalTime::Single(Self::est())
            } else if *local < gap_end {
                MappedLocalTime::None
            } else if *local < fold_start {
                MappedLocalTime::Single(Self::edt())
            } else if *local < fold_end {
                // Listed later-instant first, as chrono's `Local` lists the
                // fold, so nothing that relies on `earliest()` passes by accident.
                MappedLocalTime::Ambiguous(Self::est(), Self::edt())
            } else {
                MappedLocalTime::Single(Self::est())
            }
        }

        fn offset_from_utc_date(&self, utc: &NaiveDate) -> FixedOffset {
            self.offset_from_utc_datetime(&utc.and_time(NaiveTime::MIN))
        }

        fn offset_from_utc_datetime(&self, utc: &NaiveDateTime) -> FixedOffset {
            if *utc < Self::spring_utc() || *utc >= Self::fall_utc() {
                Self::est()
            } else {
                Self::edt()
            }
        }
    }

    #[test]
    fn accepts_an_ambiguous_fall_back_time_at_its_earlier_instant() {
        // 2026-10-31 12:00 EDT, a day before the fold.
        let now = NewYork2026.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 10, 31)
                .unwrap()
                .and_hms_opt(16, 0, 0)
                .unwrap(),
        );
        // 01:30 happens twice: EDT (05:30Z) then EST (06:30Z). The earlier is
        // the one `new Date("2026-11-01T01:30")` picks.
        assert_eq!(
            resolve_custom_snooze(
                &CustomSnoozeInput::Date {
                    date: "2026-11-01".into(),
                    time: "01:30".into()
                },
                &now
            ),
            Some("2026-11-01T05:30:00.000Z".into())
        );
    }

    #[test]
    fn rejects_a_local_time_skipped_by_spring_forward() {
        let now = NewYork2026.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 3, 7)
                .unwrap()
                .and_hms_opt(17, 0, 0)
                .unwrap(),
        );
        assert_eq!(
            resolve_custom_snooze(
                &CustomSnoozeInput::Date {
                    date: "2026-03-08".into(),
                    time: "02:30".into()
                },
                &now
            ),
            None
        );
    }

    #[test]
    fn earlier_local_instant_takes_the_smaller_instant_whatever_the_order() {
        let early = NewYork2026.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(5, 30, 0)
                .unwrap(),
        );
        let late = NewYork2026.from_utc_datetime(
            &NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(6, 30, 0)
                .unwrap(),
        );
        assert_eq!(
            earlier_local_instant(MappedLocalTime::Ambiguous(late.clone(), early.clone())),
            Some(early.clone())
        );
        assert_eq!(
            earlier_local_instant(MappedLocalTime::Ambiguous(early.clone(), late)),
            Some(early)
        );
        assert_eq!(
            earlier_local_instant::<NewYork2026>(MappedLocalTime::None),
            None
        );
    }

    #[test]
    fn formats_local_input_values_without_converting_to_utc() {
        let date = local_date(2026, 1, 2, 3, 4);
        assert_eq!(local_snooze_date(&date), "2026-01-02");
        assert_eq!(local_snooze_time(&date), "03:04");
    }
}

//! Port of upstream/apps/web/src/components/chat/UsageLimitRecoveryBanner.tsx
//!
//! The composer notice a usage-limit stop leaves behind: when the provider resets, and the two
//! standing choices (resume at reset, snooze until reset). The banner stack renders the
//! [`BannerItem`]; its source owns what `RecoveryActions` keeps in React state (`pending`,
//! `error`, the reset timer's `nowMs`) and answers the two action ids with
//! [`plan_recovery_toggle`].

use chrono::DateTime;
use turin_contracts::orchestration_v2::{
    OrchestrationV2LimitRecovery, OrchestrationV2LimitRecoveryUpdate,
};

use crate::composer::view_model::{BannerItem, BannerPriority, BannerVariant};
use crate::pull_request::summary_tab::locale_string;
use crate::ui::IconName;

/// Action ids the banner's buttons report.
pub const RESUME_ACTION_ID: &str = "resume";
pub const SNOOZE_ACTION_ID: &str = "snooze";

pub const RESET_PASSED_MESSAGE: &str = "The reset time has passed. Retry the thread manually.";

#[derive(Clone, Debug, PartialEq)]
pub struct UsageLimitRecoveryProps {
    pub run_id: String,
    pub reset_at: Option<String>,
    pub stopped_at: String,
    pub snoozed_until: Option<String>,
    pub recovery: Option<OrchestrationV2LimitRecovery>,
}

/// What `RecoveryActions` keeps in `useState`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RecoveryActionsState {
    pub pending: bool,
    pub error: Option<String>,
    /// `nowMs`: the clock as of the last reset-timer tick.
    pub now_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    Resume,
    Snooze,
}

impl RecoveryAction {
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            RESUME_ACTION_ID => Some(Self::Resume),
            SNOOZE_ACTION_ID => Some(Self::Snooze),
            _ => None,
        }
    }
}

/// `Date.parse`: epoch milliseconds, or `None` where JS says `NaN`.
fn parse_ms(value: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|date| date.timestamp_millis())
}

/// `resetAt !== null && Date.parse(resetAt) > Date.parse(stoppedAt)`.
pub fn can_schedule_recovery(props: &UsageLimitRecoveryProps) -> bool {
    match (
        props.reset_at.as_deref().and_then(parse_ms),
        parse_ms(&props.stopped_at),
    ) {
        (Some(reset), Some(stopped)) => reset > stopped,
        _ => false,
    }
}

/// Whether auto-resume is already armed for this run and reset time.
pub fn recovery_scheduled(props: &UsageLimitRecoveryProps) -> bool {
    props.recovery.as_ref().is_some_and(|recovery| {
        recovery.run_id.as_str() == props.run_id
            && Some(recovery.reset_at.as_str()) == props.reset_at.as_deref()
            && recovery.auto_resume
    })
}

/// Whether the thread is snoozed until exactly this reset.
pub fn recovery_snoozed(props: &UsageLimitRecoveryProps) -> bool {
    let (Some(recovery), Some(reset_at), Some(snoozed_until)) = (
        props.recovery.as_ref(),
        props.reset_at.as_deref(),
        props.snoozed_until.as_deref(),
    ) else {
        return false;
    };
    recovery.snooze == Some(true)
        && recovery.run_id.as_str() == props.run_id
        && recovery.reset_at.as_str() == reset_at
        && parse_ms(snoozed_until).is_some()
        && parse_ms(snoozed_until) == parse_ms(reset_at)
}

/// What a click on one of the two buttons does.
#[derive(Clone, Debug, PartialEq)]
pub enum RecoveryToggle {
    /// Nothing to change (no reset time to hold a choice against).
    Ignore,
    /// Refused without calling the server; the banner shows the message.
    Error(String),
    Update(OrchestrationV2LimitRecoveryUpdateInput),
}

/// The fields of `OrchestrationV2LimitRecoveryUpdate` the banner sets; the caller builds the
/// contract value (its constructor validates).
#[derive(Clone, Debug, PartialEq)]
pub struct OrchestrationV2LimitRecoveryUpdateInput {
    pub run_id: String,
    pub reset_at: String,
    pub auto_resume: Option<bool>,
    pub snooze: Option<bool>,
}

impl OrchestrationV2LimitRecoveryUpdateInput {
    pub fn into_update(self) -> Option<OrchestrationV2LimitRecoveryUpdate> {
        serde_json::from_value(serde_json::json!({
            "runId": self.run_id,
            "resetAt": self.reset_at,
            "autoResume": self.auto_resume,
            "snooze": self.snooze,
        }))
        .ok()
    }
}

/// `toggle(action)`.
pub fn plan_recovery_toggle(
    props: &UsageLimitRecoveryProps,
    action: RecoveryAction,
    now_ms: i64,
) -> RecoveryToggle {
    let Some(reset_at) = props.reset_at.as_deref() else {
        return RecoveryToggle::Ignore;
    };
    let snoozed = recovery_snoozed(props);
    if action == RecoveryAction::Snooze
        && !snoozed
        && parse_ms(reset_at).is_some_and(|reset| reset <= now_ms)
    {
        return RecoveryToggle::Error(RESET_PASSED_MESSAGE.to_owned());
    }
    let (auto_resume, snooze) = match action {
        RecoveryAction::Resume => (Some(!recovery_scheduled(props)), None),
        RecoveryAction::Snooze => (None, Some(!snoozed)),
    };
    RecoveryToggle::Update(OrchestrationV2LimitRecoveryUpdateInput {
        run_id: props.run_id.clone(),
        reset_at: reset_at.to_owned(),
        auto_resume,
        snooze,
    })
}

/// `usageLimitRecoveryBannerItem`.
pub fn usage_limit_recovery_banner_item(
    props: &UsageLimitRecoveryProps,
    state: &RecoveryActionsState,
) -> BannerItem {
    let can_schedule = can_schedule_recovery(props);
    let mut actions = Vec::new();
    let mut disabled_actions = Vec::new();
    if can_schedule {
        let scheduled = recovery_scheduled(props);
        actions.push((
            if state.pending {
                "Saving..."
            } else if scheduled {
                "Cancel auto-resume"
            } else {
                "Resume at reset"
            }
            .to_owned(),
            RESUME_ACTION_ID.to_owned(),
        ));
        if state.pending {
            disabled_actions.push(RESUME_ACTION_ID.to_owned());
        }
        if !recovery_snoozed(props) {
            actions.push((
                if state.pending {
                    "Saving..."
                } else {
                    "Snooze until reset"
                }
                .to_owned(),
                SNOOZE_ACTION_ID.to_owned(),
            ));
            let reset_passed = props
                .reset_at
                .as_deref()
                .and_then(parse_ms)
                .is_some_and(|reset| reset <= state.now_ms);
            if state.pending || reset_passed {
                disabled_actions.push(SNOOZE_ACTION_ID.to_owned());
            }
        }
    }
    BannerItem {
        id: format!("usage-limit-recovery:{}", props.run_id),
        variant: BannerVariant::Warning,
        priority: Some(BannerPriority::Urgent),
        icon: IconName::Gauge,
        title: "Usage limit reached".to_owned(),
        description: Some(match props.reset_at.as_deref() {
            Some(reset_at) => format!("Resets {}", locale_string(reset_at)),
            None => "Reset time unavailable; retry manually".to_owned(),
        }),
        dismissible: false,
        dismiss_label: None,
        actions,
        actions_ghost: true,
        disabled_actions,
        error: can_schedule.then(|| state.error.clone()).flatten(),
    }
}

/// A recovery notice for demos: the reset is `hours_ahead` hours away.
pub fn demo_usage_limit_recovery_props(hours_ahead: i64) -> UsageLimitRecoveryProps {
    let now = chrono::Utc::now();
    let iso =
        |at: chrono::DateTime<chrono::Utc>| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    UsageLimitRecoveryProps {
        run_id: "run-demo".to_owned(),
        reset_at: Some(iso(now + chrono::Duration::hours(hours_ahead))),
        stopped_at: iso(now - chrono::Duration::minutes(2)),
        snoozed_until: None,
        recovery: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RESET: &str = "2026-10-15T20:00:00.000Z";
    const STOPPED: &str = "2026-10-15T15:00:00.000Z";

    fn props() -> UsageLimitRecoveryProps {
        UsageLimitRecoveryProps {
            run_id: "run-1".into(),
            reset_at: Some(RESET.into()),
            stopped_at: STOPPED.into(),
            snoozed_until: None,
            recovery: None,
        }
    }

    fn recovery(auto_resume: bool, snooze: Option<bool>) -> OrchestrationV2LimitRecovery {
        serde_json::from_value(json!({
            "runId": "run-1",
            "resetAt": RESET,
            "autoResume": auto_resume,
            "snooze": snooze,
        }))
        .unwrap()
    }

    fn now(iso: &str) -> i64 {
        parse_ms(iso).unwrap()
    }

    #[test]
    fn offers_the_choices_only_when_the_reset_follows_the_stop() {
        let item = usage_limit_recovery_banner_item(&props(), &RecoveryActionsState::default());
        assert_eq!(item.id, "usage-limit-recovery:run-1");
        assert_eq!(item.title, "Usage limit reached");
        assert_eq!(
            item.actions,
            vec![
                ("Resume at reset".to_owned(), "resume".to_owned()),
                ("Snooze until reset".to_owned(), "snooze".to_owned()),
            ]
        );
        let mut no_reset = props();
        no_reset.reset_at = None;
        let item = usage_limit_recovery_banner_item(&no_reset, &RecoveryActionsState::default());
        assert_eq!(
            item.description.as_deref(),
            Some("Reset time unavailable; retry manually")
        );
        assert!(item.actions.is_empty());
        let mut stale = props();
        stale.reset_at = Some(STOPPED.into());
        assert!(
            usage_limit_recovery_banner_item(&stale, &RecoveryActionsState::default())
                .actions
                .is_empty()
        );
    }

    #[test]
    fn a_scheduled_resume_reads_as_cancel_and_a_snooze_hides_its_button() {
        let mut scheduled = props();
        scheduled.recovery = Some(recovery(true, None));
        let item = usage_limit_recovery_banner_item(&scheduled, &RecoveryActionsState::default());
        assert_eq!(item.actions[0].0, "Cancel auto-resume");

        let mut snoozed = props();
        snoozed.recovery = Some(recovery(false, Some(true)));
        snoozed.snoozed_until = Some("2026-10-15T20:00:00Z".into());
        let item = usage_limit_recovery_banner_item(&snoozed, &RecoveryActionsState::default());
        assert_eq!(item.actions.len(), 1);
        // Another reset time is another run of the choice.
        snoozed.snoozed_until = Some("2026-10-15T21:00:00Z".into());
        assert!(!recovery_snoozed(&snoozed));
    }

    #[test]
    fn a_choice_for_another_run_or_reset_does_not_count() {
        let mut other_run = props();
        other_run.run_id = "run-2".into();
        other_run.recovery = Some(recovery(true, None));
        assert!(!recovery_scheduled(&other_run));
        let mut other_reset = props();
        other_reset.recovery = Some(recovery(true, None));
        other_reset.reset_at = Some("2026-10-15T21:00:00.000Z".into());
        assert!(!recovery_scheduled(&other_reset));
    }

    #[test]
    fn saving_disables_both_buttons_and_relabels_them() {
        let state = RecoveryActionsState {
            pending: true,
            error: None,
            now_ms: now("2026-10-15T16:00:00Z"),
        };
        let item = usage_limit_recovery_banner_item(&props(), &state);
        assert!(item.actions.iter().all(|(label, _)| label == "Saving..."));
        assert_eq!(item.disabled_actions, vec!["resume", "snooze"]);
    }

    #[test]
    fn snooze_disables_itself_once_the_reset_passes() {
        let state = RecoveryActionsState {
            pending: false,
            error: None,
            now_ms: now("2026-10-15T20:00:01Z"),
        };
        let item = usage_limit_recovery_banner_item(&props(), &state);
        assert_eq!(item.disabled_actions, vec!["snooze"]);
    }

    #[test]
    fn toggling_resume_flips_the_schedule_and_keeps_the_other_choice_unset() {
        let toggle = plan_recovery_toggle(
            &props(),
            RecoveryAction::Resume,
            now("2026-10-15T16:00:00Z"),
        );
        assert_eq!(
            toggle,
            RecoveryToggle::Update(OrchestrationV2LimitRecoveryUpdateInput {
                run_id: "run-1".into(),
                reset_at: RESET.into(),
                auto_resume: Some(true),
                snooze: None,
            })
        );
        let mut scheduled = props();
        scheduled.recovery = Some(recovery(true, None));
        let RecoveryToggle::Update(update) = plan_recovery_toggle(
            &scheduled,
            RecoveryAction::Resume,
            now("2026-10-15T16:00:00Z"),
        ) else {
            panic!("expected an update");
        };
        assert_eq!(update.auto_resume, Some(false));
        assert!(update.into_update().is_some());
    }

    #[test]
    fn snoozing_after_the_reset_is_refused_without_a_server_call() {
        assert_eq!(
            plan_recovery_toggle(
                &props(),
                RecoveryAction::Snooze,
                now("2026-10-15T20:00:00Z")
            ),
            RecoveryToggle::Error(RESET_PASSED_MESSAGE.to_owned())
        );
        // Cancelling an existing snooze stays possible after the reset.
        let mut snoozed = props();
        snoozed.recovery = Some(recovery(false, Some(true)));
        snoozed.snoozed_until = Some(RESET.into());
        let RecoveryToggle::Update(update) = plan_recovery_toggle(
            &snoozed,
            RecoveryAction::Snooze,
            now("2026-10-15T21:00:00Z"),
        ) else {
            panic!("expected an update");
        };
        assert_eq!(update.snooze, Some(false));
    }

    #[test]
    fn nothing_toggles_without_a_reset_time() {
        let mut no_reset = props();
        no_reset.reset_at = None;
        assert_eq!(
            plan_recovery_toggle(&no_reset, RecoveryAction::Resume, 0),
            RecoveryToggle::Ignore
        );
    }

    #[test]
    fn the_error_shows_only_while_the_choices_do() {
        let state = RecoveryActionsState {
            pending: false,
            error: Some("boom".into()),
            now_ms: 0,
        };
        assert_eq!(
            usage_limit_recovery_banner_item(&props(), &state)
                .error
                .as_deref(),
            Some("boom")
        );
        let mut no_reset = props();
        no_reset.reset_at = None;
        assert_eq!(
            usage_limit_recovery_banner_item(&no_reset, &state).error,
            None
        );
    }
}

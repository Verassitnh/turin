//! Port of upstream/packages/client-runtime/src/userMessage.ts

use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::ScheduledTaskId;
use turin_contracts::orchestration_v2::{OrchestrationV2Actor, OrchestrationV2CreationSource};

use super::shared_support::JS_NOT_LINE_TERMINATOR_CLASS;

static LEGACY_AUTOMATION_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[Triggered by schedule task: [^\r\n]+\]\r?\n\r?\n").expect("valid regex")
});
static LEGACY_AUTOMATION_MESSAGE_ID: LazyLock<Regex> = LazyLock::new(|| {
    // JS `.` is any non-line-terminator character.
    Regex::new(&format!(
        r"^scheduled-task-message:([{JS_NOT_LINE_TERMINATOR_CLASS}]+):[0-9]+:(?:scheduled|manual)$"
    ))
    .expect("valid regex")
});

/// The fields of a message `resolve_user_message_presentation` reads.
#[derive(Debug, Clone, Copy, Default)]
pub struct UserMessageInput<'a> {
    pub id: Option<&'a str>,
    pub role: &'a str,
    pub text: &'a str,
    pub created_by: Option<OrchestrationV2Actor>,
    pub creation_source: Option<OrchestrationV2CreationSource>,
    pub scheduled_task_id: Option<&'a ScheduledTaskId>,
}

/// Who sent a user-role message the user did not type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserMessageAttribution {
    Automation,
    Agent,
    T3Code,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserMessagePresentation {
    pub text: String,
    pub attribution: Option<UserMessageAttribution>,
    pub scheduled_task_id: Option<ScheduledTaskId>,
}

/// Text and sender label for a user-role message. Older scheduled messages
/// stored their attribution in the prompt itself.
pub fn resolve_user_message_presentation(
    message: &UserMessageInput<'_>,
) -> UserMessagePresentation {
    if message.role != "user" {
        return UserMessagePresentation {
            text: message.text.to_owned(),
            attribution: None,
            scheduled_task_id: None,
        };
    }
    if let Some(scheduled_task_id) = message.scheduled_task_id {
        return UserMessagePresentation {
            text: message.text.to_owned(),
            attribution: Some(UserMessageAttribution::Automation),
            scheduled_task_id: Some(scheduled_task_id.clone()),
        };
    }
    let legacy_prefix = LEGACY_AUTOMATION_PREFIX.find(message.text);
    let legacy_task_id = legacy_prefix.and_then(|_| {
        LEGACY_AUTOMATION_MESSAGE_ID
            .captures(message.id.unwrap_or(""))
            .and_then(|captures| captures.get(1))
            .map(|task_id| task_id.as_str())
    });
    if let Some(legacy_prefix) = legacy_prefix
        && (legacy_task_id.is_some() || message.created_by == Some(OrchestrationV2Actor::Agent))
    {
        return UserMessagePresentation {
            text: message.text[legacy_prefix.end()..].to_owned(),
            attribution: Some(UserMessageAttribution::Automation),
            scheduled_task_id: legacy_task_id
                .and_then(|task_id| ScheduledTaskId::new(task_id).ok()),
        };
    }
    UserMessagePresentation {
        text: message.text.to_owned(),
        // Restart continuations were sent as the agent before they became notices.
        attribution: if message.created_by != Some(OrchestrationV2Actor::Agent) {
            None
        } else if message.creation_source == Some(OrchestrationV2CreationSource::Server) {
            Some(UserMessageAttribution::T3Code)
        } else {
            Some(UserMessageAttribution::Agent)
        },
        scheduled_task_id: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY_TEXT: &str = "[Triggered by schedule task: Daily audit]\n\nCheck for crashes.\n";

    fn presentation(
        text: &str,
        attribution: Option<UserMessageAttribution>,
        scheduled_task_id: Option<&str>,
    ) -> UserMessagePresentation {
        UserMessagePresentation {
            text: text.to_owned(),
            attribution,
            scheduled_task_id: scheduled_task_id.map(|id| ScheduledTaskId::new(id).unwrap()),
        }
    }

    #[test]
    fn removes_attribution_from_legacy_scheduled_prompts() {
        assert_eq!(
            resolve_user_message_presentation(&UserMessageInput {
                role: "user",
                created_by: Some(OrchestrationV2Actor::Agent),
                text: LEGACY_TEXT,
                ..Default::default()
            }),
            presentation(
                "Check for crashes.\n",
                Some(UserMessageAttribution::Automation),
                None
            )
        );
    }

    #[test]
    fn uses_task_metadata_without_changing_the_prompt() {
        let task_id = ScheduledTaskId::new("task-1").unwrap();
        assert_eq!(
            resolve_user_message_presentation(&UserMessageInput {
                role: "user",
                created_by: Some(OrchestrationV2Actor::Agent),
                scheduled_task_id: Some(&task_id),
                text: LEGACY_TEXT,
                ..Default::default()
            }),
            presentation(
                LEGACY_TEXT,
                Some(UserMessageAttribution::Automation),
                Some("task-1")
            )
        );
    }

    #[test]
    fn preserves_user_written_and_assistant_quoted_schedule_headers() {
        for (role, created_by) in [
            ("user", Some(OrchestrationV2Actor::User)),
            ("user", None),
            ("assistant", Some(OrchestrationV2Actor::Agent)),
        ] {
            assert_eq!(
                resolve_user_message_presentation(&UserMessageInput {
                    role,
                    created_by,
                    text: LEGACY_TEXT,
                    ..Default::default()
                }),
                presentation(LEGACY_TEXT, None, None),
                "{role} {created_by:?}"
            );
        }
    }

    #[test]
    fn leaves_other_agent_prompts_and_embedded_headers_intact() {
        let quoted = format!("Quoted prompt:\n{LEGACY_TEXT}");
        for text in [
            "Review this area",
            quoted.as_str(),
            "[Triggered by schedule task: Daily audit]",
        ] {
            assert_eq!(
                resolve_user_message_presentation(&UserMessageInput {
                    role: "user",
                    created_by: Some(OrchestrationV2Actor::Agent),
                    text,
                    ..Default::default()
                }),
                presentation(text, Some(UserMessageAttribution::Agent), None),
                "{text}"
            );
        }
    }

    #[test]
    fn attributes_older_server_sent_restart_continuations_to_t3_code_not_another_agent() {
        let text = "Note: the T3 server restarted.";
        assert_eq!(
            resolve_user_message_presentation(&UserMessageInput {
                role: "user",
                created_by: Some(OrchestrationV2Actor::Agent),
                creation_source: Some(OrchestrationV2CreationSource::Server),
                text,
                ..Default::default()
            })
            .attribution,
            Some(UserMessageAttribution::T3Code)
        );
        for creation_source in [
            OrchestrationV2CreationSource::Mcp,
            OrchestrationV2CreationSource::Provider,
        ] {
            assert_eq!(
                resolve_user_message_presentation(&UserMessageInput {
                    role: "user",
                    created_by: Some(OrchestrationV2Actor::Agent),
                    creation_source: Some(creation_source),
                    text,
                    ..Default::default()
                })
                .attribution,
                Some(UserMessageAttribution::Agent),
                "{creation_source:?}"
            );
        }
    }

    #[test]
    fn recovers_the_triggering_automation_from_older_scheduler_message_ids() {
        for trigger in ["scheduled", "manual"] {
            let id = format!("scheduled-task-message:task:daily-audit:1788661140000:{trigger}");
            assert_eq!(
                resolve_user_message_presentation(&UserMessageInput {
                    id: Some(&id),
                    role: "user",
                    created_by: Some(OrchestrationV2Actor::User),
                    text: LEGACY_TEXT,
                    ..Default::default()
                }),
                presentation(
                    "Check for crashes.\n",
                    Some(UserMessageAttribution::Automation),
                    Some("task:daily-audit")
                ),
                "{trigger}"
            );
        }
    }
}

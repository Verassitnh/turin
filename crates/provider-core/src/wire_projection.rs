//! Port of `projectContextHandoffForWire` in
//! upstream/apps/server/src/orchestration-v2/WireProjection.ts.
//!
//! The rest of that file projects turn items. This export only strips a
//! context handoff for the wire: history and delivery stay off the socket,
//! and summary text is blanked so a preview cannot leak a transcript.

use turin_contracts::orchestration_v2::OrchestrationV2ContextHandoff;

/// `projectContextHandoffForWire`.
pub fn project_context_handoff_for_wire(
    handoff: &OrchestrationV2ContextHandoff,
) -> OrchestrationV2ContextHandoff {
    let mut projected = handoff.clone();
    projected.history = None;
    projected.delivery = None;
    projected.summary_text.clear();
    projected
}

#[cfg(test)]
mod tests {
    //! The first `it` of upstream `ContextHandoffBudget.test.ts`.

    use super::project_context_handoff_for_wire;
    use turin_contracts::orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffDelivery,
        OrchestrationV2ContextHandoffDeliveryStatus, OrchestrationV2ContextHandoffHistory,
        OrchestrationV2ContextHandoffStatus, OrchestrationV2ContextHandoffStrategy,
        OrchestrationV2CoveredRunOrdinals, OrchestrationV2HistoricalMessage,
        OrchestrationV2HistoricalMessageRole,
    };
    use turin_contracts::{
        ContextHandoffId, PositiveInt, ProviderThreadId, RunId, ThreadId, TurnItemId, UtcDateTime,
    };

    fn message(
        id: &str,
        role: OrchestrationV2HistoricalMessageRole,
        text: &str,
    ) -> OrchestrationV2HistoricalMessage {
        OrchestrationV2HistoricalMessage {
            role,
            text: text.to_owned(),
            run_status: None,
            thread_id: ThreadId::new("thread:handoff").unwrap(),
            run_id: Some(RunId::new("run:source").unwrap()),
            item_id: TurnItemId::new(id).unwrap(),
            provider_thread_id: Some(ProviderThreadId::new("provider-thread:source").unwrap()),
            status: "interrupted".to_owned(),
            kind: match role {
                OrchestrationV2HistoricalMessageRole::User => "user_message",
                OrchestrationV2HistoricalMessageRole::Assistant => "assistant_message",
            }
            .to_owned(),
        }
    }

    fn sample_messages() -> Vec<OrchestrationV2HistoricalMessage> {
        vec![
            message(
                "item:one",
                OrchestrationV2HistoricalMessageRole::User,
                "Preserve every line.\n\n  And this indentation.\n",
            ),
            message(
                "item:two",
                OrchestrationV2HistoricalMessageRole::Assistant,
                &format!("Partial work: 日本語 🧪 مرحبا\n{}", "x".repeat(600)),
            ),
        ]
    }

    fn handoff(messages: Vec<OrchestrationV2HistoricalMessage>) -> OrchestrationV2ContextHandoff {
        let now: UtcDateTime = "2026-09-17T00:00:00.000Z".parse().unwrap();
        OrchestrationV2ContextHandoff {
            id: ContextHandoffId::new("handoff:one").unwrap(),
            transfer_id: None,
            thread_id: ThreadId::new("thread:handoff").unwrap(),
            target_run_id: RunId::new("run:target").unwrap(),
            from_provider_thread_ids: Vec::new(),
            to_provider_thread_id: ProviderThreadId::new("provider-thread:target").unwrap(),
            covered_run_ordinals: OrchestrationV2CoveredRunOrdinals {
                from: PositiveInt::new(1).unwrap(),
                to: PositiveInt::new(2).unwrap(),
            },
            strategy: OrchestrationV2ContextHandoffStrategy::FullThreadSummary,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: String::new(),
            history: Some(OrchestrationV2ContextHandoffHistory {
                messages,
                coverage: "Historical context; retrieve thread:handoff with t3_thread_read."
                    .to_owned(),
                omitted_items: 0,
                omitted_item_ids: None,
            }),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn keeps_old_preview_handoffs_readable_and_delivery_history_off_the_wire() {
        let messages = sample_messages();
        let mut source = handoff(messages.clone());
        source.summary_text = "private transcript and command output".to_owned();
        source.delivery = Some(OrchestrationV2ContextHandoffDelivery {
            native_thread_id: "native:target".to_owned(),
            status: OrchestrationV2ContextHandoffDeliveryStatus::Injected,
            item_ids: vec![messages[0].item_id.clone()],
            omitted_item_ids: None,
        });
        let projected = project_context_handoff_for_wire(&source);
        assert_eq!(projected.summary_text, "");
        assert!(projected.history.is_none());
        assert!(projected.delivery.is_none());
        let wire = serde_json::to_value(&projected).unwrap();
        assert!(wire.get("history").is_none());
        assert!(wire.get("delivery").is_none());
        let decoded: OrchestrationV2ContextHandoff = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded.id, source.id);
        assert_eq!(
            source.history.as_ref().map(|history| &history.messages),
            Some(&messages)
        );
    }
}

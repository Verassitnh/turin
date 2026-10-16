//! Port of upstream/apps/server/src/orchestration-v2/ContextHandoffService.ts.
//!
//! Builds the context handoffs that carry a conversation from one provider
//! thread to another: a legacy import for the first native turn, a manual
//! placeholder, a fork-delta summary for merge-back, and a provider handoff
//! when a thread moves between providers. Pure: ids come from the allocator
//! and the history selection from `handoff_budget`.

use turin_contracts::{
    ProviderThreadId, RunId, ThreadId, UtcDateTime,
    orchestration_v2::{
        OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffHistory,
        OrchestrationV2ContextHandoffStatus, OrchestrationV2ContextHandoffStrategy,
        OrchestrationV2CoveredRunOrdinals, OrchestrationV2HistoricalMessage, OrchestrationV2Run,
        OrchestrationV2TurnItem,
    },
    provider_instance::ProviderInstanceId,
};
use turin_provider_core::{
    handoff_budget::{
        DEFAULT_HANDOFF_TOKEN_CAP, SelectHistoryInput, handoff_coverage, historical_message,
        render_history, select_history,
    },
    id_allocator::IdAllocator,
};

/// `compactText`: whitespace collapsed, cut to `max_length` UTF-16 units.
fn compact_text(text: &str, max_length: usize) -> String {
    let compacted = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let units: Vec<u16> = compacted.encode_utf16().collect();
    if units.len() <= max_length {
        return compacted;
    }
    format!("{}...", String::from_utf16_lossy(&units[..max_length - 3]))
}

/// One line of a fork delta, for the items a person would recognise.
fn summarize_delta_item(item: &OrchestrationV2TurnItem) -> Option<String> {
    let wire = serde_json::to_value(item).ok()?;
    let text = |key: &str| wire.get(key).and_then(|value| value.as_str()).unwrap_or("");
    match wire.get("type")?.as_str()? {
        "user_message" => Some(format!("- User: {}", compact_text(text("text"), 240))),
        "assistant_message" => Some(format!("- Assistant: {}", compact_text(text("text"), 240))),
        "command_execution" => Some(format!("- Command: {}", compact_text(text("input"), 240))),
        "file_change" => Some(format!("- File change: {}", text("fileName"))),
        "checkpoint" => Some(format!(
            "- Checkpoint: {} files",
            wire.get("files")
                .and_then(|files| files.as_array())
                .map_or(0, Vec::len)
        )),
        "handoff" => {
            let summary = wire
                .get("summary")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| text("strategy"));
            Some(format!("- Handoff: {}", compact_text(summary, 240)))
        }
        _ => None,
    }
}

fn make_fork_delta_summary(
    source_thread_id: &ThreadId,
    target_thread_id: &ThreadId,
    covered: &OrchestrationV2CoveredRunOrdinals,
    delta_items: &[OrchestrationV2TurnItem],
) -> String {
    let mut lines = vec![
        "Merge-back context from forked conversation.".to_owned(),
        format!("Source thread: {source_thread_id}"),
        format!("Target thread: {target_thread_id}"),
        format!(
            "Covered fork runs: {}-{}",
            covered.from.get(),
            covered.to.get()
        ),
        String::new(),
        "Fork delta:".to_owned(),
    ];
    let item_lines: Vec<String> = delta_items
        .iter()
        .filter_map(summarize_delta_item)
        .collect();
    if item_lines.is_empty() {
        lines.push("- No user-visible delta items.".to_owned());
    } else {
        lines.extend(item_lines);
    }
    lines.join("\n")
}

/// JavaScript's `\s`, on one UTF-16 code unit.
fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x09 | 0x0a | 0x0b | 0x0c | 0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000
            ..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff
    )
}

/// The imported history as one summary text, newest sections kept. Lengths and
/// cut points are UTF-16 code units, as upstream's string indexing.
fn make_legacy_import_summary(items: &[OrchestrationV2TurnItem]) -> String {
    let sections: Vec<(&str, &str)> = items
        .iter()
        .filter_map(|item| match item {
            OrchestrationV2TurnItem::UserMessage { text, .. } => Some(("User", text.as_str())),
            OrchestrationV2TurnItem::AssistantMessage { text, .. } => {
                Some(("Assistant", text.as_str()))
            }
            _ => None,
        })
        .collect();
    let header = "Imported conversation history from the previous T3 Code orchestrator. Use it as context; do not repeat it unless the user asks.";
    const MAX_CHARS: i64 = 32_000;
    let units = |text: &str| text.encode_utf16().count() as i64;
    let mut selected: Vec<String> = Vec::new();
    let mut remaining = MAX_CHARS - units(header) - 2;
    for (label, body) in sections.iter().rev() {
        if remaining <= 0 {
            break;
        }
        let full_section = format!("{label}:\n{body}");
        if units(&full_section) <= remaining {
            remaining -= units(&full_section) + 2;
            selected.insert(0, full_section);
            continue;
        }
        let prefix = format!("{label}:\n... ");
        let body_budget = remaining - units(&prefix);
        if body_budget <= 0 {
            continue;
        }
        let body_units: Vec<u16> = body.encode_utf16().collect();
        let initial_start = body_units.len() as i64 - body_budget;
        let mut body_start = initial_start.max(0) as usize;
        if body_start > 0
            && !is_js_whitespace(body_units[body_start - 1])
            && body_units
                .get(body_start)
                .is_some_and(|unit| !is_js_whitespace(*unit))
            && let Some(next) = body_units[body_start..]
                .iter()
                .position(|unit| is_js_whitespace(*unit))
        {
            body_start += next;
        }
        // Do not start inside a surrogate pair.
        if body_start > 0
            && body_units
                .get(body_start)
                .is_some_and(|unit| (0xdc00..=0xdfff).contains(unit))
            && (0xd800..=0xdbff).contains(&body_units[body_start - 1])
        {
            body_start += 1;
        }
        let suffix_units = &body_units[body_start.min(body_units.len())..];
        let trimmed_start = suffix_units
            .iter()
            .position(|unit| !is_js_whitespace(*unit))
            .unwrap_or(suffix_units.len());
        let body_suffix = String::from_utf16_lossy(&suffix_units[trimmed_start..]);
        if body_suffix.is_empty() {
            continue;
        }
        let suffix = format!("{prefix}{body_suffix}");
        remaining -= units(&suffix) + 2;
        selected.insert(0, suffix);
    }
    std::iter::once(header.to_owned())
        .chain(selected)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `providerMessageWithContextHandoff`: the user's text behind its handoff.
pub fn provider_message_with_context_handoff(
    handoff: &OrchestrationV2ContextHandoff,
    user_text: &str,
) -> String {
    provider_message_with_context_handoffs(std::slice::from_ref(handoff), user_text)
}

fn provider_message_with_context_handoffs(
    handoffs: &[OrchestrationV2ContextHandoff],
    user_text: &str,
) -> String {
    let mut lines: Vec<String> = Vec::new();
    for handoff in handoffs {
        let label = match handoff.strategy {
            OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary => {
                "merge_back / fork_delta_summary".to_owned()
            }
            other => serde_json::to_value(other)
                .ok()
                .and_then(|value| value.as_str().map(str::to_owned))
                .unwrap_or_default(),
        };
        lines.push(format!("Context handoff ({label}):"));
        lines.push(handoff.summary_text.clone());
        lines.push(String::new());
    }
    lines.push("User message:".to_owned());
    lines.push(user_text.to_owned());
    lines.join("\n")
}

/// The service. `token_cap` is the history budget, the handoff token cap.
#[derive(Debug, Clone, Copy)]
pub struct ContextHandoffService {
    token_cap: i64,
}

impl Default for ContextHandoffService {
    fn default() -> Self {
        Self {
            token_cap: DEFAULT_HANDOFF_TOKEN_CAP,
        }
    }
}

/// `prepareLegacyImport` input.
pub struct LegacyImportInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

/// `prepareForkDelta` input.
pub struct ForkDeltaInput<'a> {
    pub source_thread_id: &'a ThreadId,
    pub target_thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub transfer_id: Option<turin_contracts::ContextTransferId>,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub from_provider_instance_id: &'a ProviderInstanceId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub covered_run_ordinals: OrchestrationV2CoveredRunOrdinals,
    pub delta_items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

/// `prepareProviderHandoff` input.
pub struct ProviderHandoffInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub transfer_id: Option<turin_contracts::ContextTransferId>,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub from_provider_instance_id: &'a ProviderInstanceId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub covered_run_ordinals: OrchestrationV2CoveredRunOrdinals,
    pub runs: &'a [OrchestrationV2Run],
    /// `delta_since_target_last_seen` or `full_thread_summary`.
    pub strategy: OrchestrationV2ContextHandoffStrategy,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

impl ContextHandoffService {
    pub fn new(token_cap: i64) -> Self {
        Self { token_cap }
    }

    fn history(
        &self,
        coverage: String,
        messages: Vec<OrchestrationV2HistoricalMessage>,
    ) -> (
        OrchestrationV2ContextHandoffHistory,
        String,
        Vec<OrchestrationV2HistoricalMessage>,
    ) {
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        let history = OrchestrationV2ContextHandoffHistory {
            messages: selected.messages.clone(),
            coverage,
            omitted_items: selected.omitted_items.max(0) as u64,
            omitted_item_ids: Some(selected.omitted_item_ids),
        };
        (history, selected.context, selected.messages)
    }

    /// `prepareLegacyImport`: imported history for the first native v2 turn.
    pub fn prepare_legacy_import(
        &self,
        input: &LegacyImportInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let handoff_id = IdAllocator.context_handoff(
            input.thread_id,
            &provider_instance("legacy"),
            input.to_provider_instance_id,
        );
        let covered = covered(1, 1);
        let coverage = handoff_coverage(input.thread_id.as_str(), &covered, input.items);
        let (history, _, _) = self.history(
            coverage,
            input.items.iter().filter_map(historical_message).collect(),
        );
        OrchestrationV2ContextHandoff {
            id: handoff_id,
            transfer_id: Some(None),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: Vec::new(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: covered,
            strategy: OrchestrationV2ContextHandoffStrategy::ManualContext,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: make_legacy_import_summary(input.items),
            history: Some(history),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    /// `prepare`: a manual placeholder handoff.
    pub fn prepare(
        &self,
        thread_id: &ThreadId,
        target_run_id: &RunId,
        from_provider_thread_ids: &[ProviderThreadId],
        to_provider_thread_id: &ProviderThreadId,
        now: UtcDateTime,
    ) -> OrchestrationV2ContextHandoff {
        OrchestrationV2ContextHandoff {
            id: IdAllocator.context_handoff(
                thread_id,
                &provider_instance("manual"),
                &provider_instance("manual"),
            ),
            transfer_id: Some(None),
            thread_id: thread_id.clone(),
            target_run_id: target_run_id.clone(),
            from_provider_thread_ids: from_provider_thread_ids.to_vec(),
            to_provider_thread_id: to_provider_thread_id.clone(),
            covered_run_ordinals: covered(1, 1),
            strategy: OrchestrationV2ContextHandoffStrategy::ManualContext,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: "Manual context handoff.".to_owned(),
            history: None,
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    /// `prepareForkDelta`: what a fork did, for merge-back into its parent.
    pub fn prepare_fork_delta(&self, input: &ForkDeltaInput<'_>) -> OrchestrationV2ContextHandoff {
        let handoff_id = IdAllocator.context_handoff(
            input.target_thread_id,
            input.from_provider_instance_id,
            input.to_provider_instance_id,
        );
        let coverage = handoff_coverage(
            input.source_thread_id.as_str(),
            &input.covered_run_ordinals,
            input.delta_items,
        );
        let (history, _, _) = self.history(
            coverage,
            input
                .delta_items
                .iter()
                .filter_map(historical_message)
                .collect(),
        );
        OrchestrationV2ContextHandoff {
            id: handoff_id,
            transfer_id: Some(input.transfer_id.clone()),
            thread_id: input.target_thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: input.from_provider_thread_ids.to_vec(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: input.covered_run_ordinals.clone(),
            strategy: OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: make_fork_delta_summary(
                input.source_thread_id,
                input.target_thread_id,
                &input.covered_run_ordinals,
                input.delta_items,
            ),
            history: Some(history),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    /// `prepareProviderHandoff`: the conversation a new provider has not seen.
    pub fn prepare_provider_handoff(
        &self,
        input: &ProviderHandoffInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let handoff_id = IdAllocator.context_handoff(
            input.thread_id,
            input.from_provider_instance_id,
            input.to_provider_instance_id,
        );
        let coverage = handoff_coverage(
            input.thread_id.as_str(),
            &input.covered_run_ordinals,
            input.items,
        );
        let messages: Vec<OrchestrationV2HistoricalMessage> = input
            .items
            .iter()
            .filter_map(|item| {
                let mut message = historical_message(item)?;
                let run_id = turin_provider_core::turn_item::turn_item_common(item).run_id;
                if let Some(run) = run_id.and_then(|id| input.runs.iter().find(|run| &run.id == id))
                {
                    message.run_status = serde_json::to_value(run.status)
                        .ok()
                        .and_then(|value| value.as_str().map(str::to_owned));
                }
                Some(message)
            })
            .collect();
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        let summary_text = render_history(&selected.messages, &selected.context);
        OrchestrationV2ContextHandoff {
            id: handoff_id,
            transfer_id: Some(input.transfer_id.clone()),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: input.from_provider_thread_ids.to_vec(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: input.covered_run_ordinals.clone(),
            strategy: input.strategy,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text,
            history: Some(OrchestrationV2ContextHandoffHistory {
                messages: selected.messages,
                coverage,
                omitted_items: selected.omitted_items.max(0) as u64,
                omitted_item_ids: Some(selected.omitted_item_ids),
            }),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }
}

fn provider_instance(name: &str) -> ProviderInstanceId {
    ProviderInstanceId::new(name).expect("provider instance id")
}

fn covered(from: u64, to: u64) -> OrchestrationV2CoveredRunOrdinals {
    OrchestrationV2CoveredRunOrdinals {
        from: turin_contracts::PositiveInt::new(from).expect("positive"),
        to: turin_contracts::PositiveInt::new(to).expect("positive"),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const T: &str = "2026-01-01T00:00:00.000Z";

    fn item(role: &str, id: &str, text: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        let mut value = json!({
            "id": format!("turn-item:{id}"),
            "threadId": "thread:legacy-context",
            "runId": null,
            "nodeId": null,
            "providerThreadId": null,
            "providerTurnId": null,
            "nativeItemRef": null,
            "parentItemId": null,
            "ordinal": ordinal,
            "status": "completed",
            "title": null,
            "startedAt": T,
            "completedAt": T,
            "updatedAt": T,
            "messageId": format!("message:{id}"),
            "text": text,
        });
        let fields = value.as_object_mut().unwrap();
        if role == "user" {
            fields.insert("createdBy".into(), json!("user"));
            fields.insert("creationSource".into(), json!("server"));
            fields.insert("type".into(), json!("user_message"));
            fields.insert("inputIntent".into(), json!("turn_start"));
            fields.insert("attachments".into(), json!([]));
        } else {
            fields.insert("type".into(), json!("assistant_message"));
            fields.insert("streaming".into(), json!(false));
        }
        serde_json::from_value(value).expect("item decodes")
    }

    fn import(items: &[OrchestrationV2TurnItem]) -> OrchestrationV2ContextHandoff {
        ContextHandoffService::default().prepare_legacy_import(&LegacyImportInput {
            thread_id: &ThreadId::new("thread:legacy-context").unwrap(),
            target_run_id: &RunId::new("run:first-v2").unwrap(),
            to_provider_thread_id: &ProviderThreadId::new("provider-thread:first-v2").unwrap(),
            to_provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
            items,
            created_at: serde_json::from_value(json!("2026-01-02T00:00:00.000Z")).unwrap(),
        })
    }

    #[test]
    fn prepares_imported_history_for_the_first_native_v2_turn() {
        let handoff = import(&[
            item("user", "one", "What did we decide?", 1),
            item(
                "assistant",
                "two",
                "We decided to keep the migration lightweight.",
                2,
            ),
        ]);
        assert_eq!(
            handoff.strategy,
            OrchestrationV2ContextHandoffStrategy::ManualContext
        );
        assert!(handoff.from_provider_thread_ids.is_empty());
        assert!(handoff.summary_text.contains("What did we decide?"));
        assert!(
            handoff
                .summary_text
                .contains("keep the migration lightweight")
        );
        let message = provider_message_with_context_handoff(&handoff, "Continue from there.");
        assert!(message.contains(&handoff.summary_text));
        assert!(message.contains("User message:\nContinue from there."));
    }

    #[test]
    fn preserves_role_attribution_when_truncating_imported_history() {
        let handoff = import(&[item(
            "user",
            "long",
            &format!("{} retained final words", "x".repeat(35_000)),
            1,
        )]);
        assert!(handoff.summary_text.encode_utf16().count() <= 32_000);
        assert!(
            handoff
                .summary_text
                .contains("User:\n... retained final words")
        );
        assert!(!handoff.summary_text.contains("\nxxxx retained final words"));
    }

    #[test]
    fn retains_the_newest_oversized_import_even_when_it_has_no_whitespace() {
        let handoff = import(&[
            item("assistant", "older", "older message", 1),
            item(
                "user",
                "long-single-token",
                &format!("{}LATEST_SINGLE_TOKEN", "🧪".repeat(20_000)),
                2,
            ),
        ]);
        let omitted = handoff
            .history
            .as_ref()
            .and_then(|history| history.omitted_item_ids.clone())
            .unwrap_or_default();
        assert!(
            omitted
                .iter()
                .any(|id| id.as_str() == "turn-item:long-single-token")
        );
        assert!(handoff.summary_text.encode_utf16().count() <= 32_000);
        assert!(handoff.summary_text.contains("User:\n... "));
        assert!(handoff.summary_text.contains("LATEST_SINGLE_TOKEN"));
        assert!(!handoff.summary_text.contains('\u{fffd}'));
    }

    #[test]
    fn a_fork_delta_names_the_threads_and_lists_what_the_fork_did() {
        let service = ContextHandoffService::default();
        let items = [
            item("user", "q", "Try   the\nother approach", 1),
            item("assistant", "a", "Done.", 2),
        ];
        let handoff = service.prepare_fork_delta(&ForkDeltaInput {
            source_thread_id: &ThreadId::new("thread:fork").unwrap(),
            target_thread_id: &ThreadId::new("thread:parent").unwrap(),
            target_run_id: &RunId::new("run:parent-2").unwrap(),
            transfer_id: None,
            from_provider_thread_ids: &[],
            to_provider_thread_id: &ProviderThreadId::new("provider-thread:parent").unwrap(),
            from_provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
            to_provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
            covered_run_ordinals: covered(2, 3),
            delta_items: &items,
            created_at: serde_json::from_value(json!(T)).unwrap(),
        });
        assert_eq!(
            handoff.strategy,
            OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary
        );
        assert_eq!(
            handoff.summary_text,
            "Merge-back context from forked conversation.\nSource thread: thread:fork\nTarget thread: thread:parent\nCovered fork runs: 2-3\n\nFork delta:\n- User: Try the other approach\n- Assistant: Done."
        );
        let message = provider_message_with_context_handoff(&handoff, "Merge it.");
        assert!(message.starts_with("Context handoff (merge_back / fork_delta_summary):\n"));
    }

    #[test]
    fn a_provider_handoff_marks_each_message_with_its_runs_status() {
        let service = ContextHandoffService::default();
        let mut with_run = item("user", "u", "hello", 1);
        if let OrchestrationV2TurnItem::UserMessage { run_id, .. } = &mut with_run {
            *run_id = Some(RunId::new("run:old").unwrap());
        }
        let run: OrchestrationV2Run = serde_json::from_value(crate::projection::test_support::run(
            "thread:legacy-context",
            "run:old",
            1,
            "failed",
        ))
        .unwrap();
        let handoff = service.prepare_provider_handoff(&ProviderHandoffInput {
            thread_id: &ThreadId::new("thread:legacy-context").unwrap(),
            target_run_id: &RunId::new("run:next").unwrap(),
            transfer_id: None,
            from_provider_thread_ids: &[],
            to_provider_thread_id: &ProviderThreadId::new("provider-thread:next").unwrap(),
            from_provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
            to_provider_instance_id: &ProviderInstanceId::new("claudeAgent").unwrap(),
            covered_run_ordinals: covered(1, 1),
            runs: &[run],
            strategy: OrchestrationV2ContextHandoffStrategy::FullThreadSummary,
            items: &[with_run],
            created_at: serde_json::from_value(json!(T)).unwrap(),
        });
        assert_eq!(
            handoff.history.as_ref().unwrap().messages[0]
                .run_status
                .as_deref(),
            Some("failed")
        );
        assert!(handoff.summary_text.contains("hello"));
    }
}

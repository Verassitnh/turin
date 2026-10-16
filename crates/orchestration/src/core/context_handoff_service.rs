//! Port of upstream/apps/server/src/orchestration-v2/ContextHandoffService.ts
//!
//! Builds the handoff records that carry a conversation into another provider
//! thread: the legacy import, the manual handoff, the fork merge-back delta, and
//! the provider-thread handoff. Each record holds a human-readable `summaryText`
//! and, when the history fits, the portable `history` that delivery replays.
//!
//! Deviations from upstream:
//! - Preparation is synchronous and infallible. `IdAllocator` in provider-core
//!   is a plain function, so the `ContextHandoffPrepareError` mapping on
//!   allocation has nothing to catch and is not ported.
//! - The Effect `layer` becomes `ContextHandoffService::new` (explicit token cap)
//!   or `ContextHandoffService::from_env` (reads `T3CODE_CONTEXT_HANDOFF_TOKEN_CAP`,
//!   falling back to the default on an invalid value, as `orElseSucceed` did).

use std::collections::VecDeque;

use serde::Serialize;
use turin_contracts::base_schemas::{UtcDateTime, is_js_whitespace, js_len};
use turin_contracts::orchestration_v2::{
    OrchestrationV2ContextHandoff, OrchestrationV2ContextHandoffHistory,
    OrchestrationV2ContextHandoffStatus, OrchestrationV2ContextHandoffStrategy,
    OrchestrationV2CoveredRunOrdinals, OrchestrationV2HistoricalMessage, OrchestrationV2Run,
    OrchestrationV2TurnItem,
};
use turin_contracts::provider_instance::ProviderInstanceId;
use turin_contracts::{ContextTransferId, PositiveInt, ProviderThreadId, RunId, ThreadId};
use turin_provider_core::handoff_budget::{
    DEFAULT_HANDOFF_TOKEN_CAP, SelectHistoryInput, SelectedHistory, handoff_coverage,
    handoff_token_cap_config, historical_message, render_history, select_history,
};
use turin_provider_core::id_allocator::IdAllocator;
use turin_provider_core::turn_item::turn_item_common;

/// The provider-thread handoff strategies `prepareProviderHandoff` accepts
/// (upstream `Extract<…, "delta_since_target_last_seen" | "full_thread_summary">`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderHandoffStrategy {
    DeltaSinceTargetLastSeen,
    FullThreadSummary,
}

impl ProviderHandoffStrategy {
    fn contract(self) -> OrchestrationV2ContextHandoffStrategy {
        match self {
            Self::DeltaSinceTargetLastSeen => {
                OrchestrationV2ContextHandoffStrategy::DeltaSinceTargetLastSeen
            }
            Self::FullThreadSummary => OrchestrationV2ContextHandoffStrategy::FullThreadSummary,
        }
    }
}

pub struct PrepareLegacyImportInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

pub struct PrepareManualInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
}

pub struct PrepareForkDeltaInput<'a> {
    pub source_thread_id: &'a ThreadId,
    pub target_thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub transfer_id: Option<ContextTransferId>,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub from_provider_instance_id: &'a ProviderInstanceId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub covered_run_ordinals: &'a OrchestrationV2CoveredRunOrdinals,
    pub delta_items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

pub struct PrepareProviderHandoffInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub transfer_id: Option<ContextTransferId>,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub from_provider_instance_id: &'a ProviderInstanceId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub covered_run_ordinals: &'a OrchestrationV2CoveredRunOrdinals,
    pub runs: &'a [OrchestrationV2Run],
    pub strategy: ProviderHandoffStrategy,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

/// `ContextHandoffServiceV2`: stateless apart from the history token cap.
pub struct ContextHandoffService {
    token_cap: i64,
    ids: IdAllocator,
}

impl ContextHandoffService {
    pub fn new(token_cap: i64) -> Self {
        Self {
            token_cap,
            ids: IdAllocator,
        }
    }

    /// Reads `T3CODE_CONTEXT_HANDOFF_TOKEN_CAP`. An invalid value falls back to
    /// the default, as upstream's `orElseSucceed` did.
    pub fn from_env() -> Self {
        let token_cap = handoff_token_cap_config(|key| std::env::var(key).ok())
            .unwrap_or(DEFAULT_HANDOFF_TOKEN_CAP);
        Self::new(token_cap)
    }

    /// `prepareLegacyImport`: history from the previous orchestrator, imported
    /// as a single run-1 handoff for the first native v2 turn.
    pub fn prepare_legacy_import(
        &self,
        input: PrepareLegacyImportInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let legacy_instance = ProviderInstanceId::new("legacy").expect("legacy id is non-blank");
        let id = self.ids.context_handoff(
            input.thread_id,
            &legacy_instance,
            input.to_provider_instance_id,
        );
        let covered = covered_run_ordinals(1, 1);
        let coverage = handoff_coverage(input.thread_id.as_str(), &covered, input.items);
        let messages = historical_messages(input.items, &[]);
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        OrchestrationV2ContextHandoff {
            id,
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
            history: Some(history(selected, coverage)),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    /// `prepare`: a manual handoff with no history to replay.
    pub fn prepare(&self, input: PrepareManualInput<'_>) -> OrchestrationV2ContextHandoff {
        let manual = ProviderInstanceId::new("manual").expect("manual id is non-blank");
        let id = self.ids.context_handoff(input.thread_id, &manual, &manual);
        let now = UtcDateTime::now();
        OrchestrationV2ContextHandoff {
            id,
            transfer_id: Some(None),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: input.from_provider_thread_ids.to_vec(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: covered_run_ordinals(1, 1),
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

    /// `prepareForkDelta`: the merge-back summary of what a fork did after it
    /// diverged, written into the target thread.
    pub fn prepare_fork_delta(
        &self,
        input: PrepareForkDeltaInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let id = self.ids.context_handoff(
            input.target_thread_id,
            input.from_provider_instance_id,
            input.to_provider_instance_id,
        );
        let coverage = handoff_coverage(
            input.source_thread_id.as_str(),
            input.covered_run_ordinals,
            input.delta_items,
        );
        let messages = historical_messages(input.delta_items, &[]);
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        OrchestrationV2ContextHandoff {
            id,
            transfer_id: Some(input.transfer_id),
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
                input.covered_run_ordinals,
                input.delta_items,
            ),
            history: Some(history(selected, coverage)),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    /// `prepareProviderHandoff`: a delta or full-summary handoff between two
    /// provider threads. Run statuses are attached to each message so the
    /// budget and rendering can see them.
    pub fn prepare_provider_handoff(
        &self,
        input: PrepareProviderHandoffInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let id = self.ids.context_handoff(
            input.thread_id,
            input.from_provider_instance_id,
            input.to_provider_instance_id,
        );
        let coverage = handoff_coverage(
            input.thread_id.as_str(),
            input.covered_run_ordinals,
            input.items,
        );
        let messages = historical_messages(input.items, input.runs);
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        OrchestrationV2ContextHandoff {
            id,
            transfer_id: Some(input.transfer_id),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: input.from_provider_thread_ids.to_vec(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: input.covered_run_ordinals.clone(),
            strategy: input.strategy.contract(),
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: render_history(&selected.messages, &selected.context),
            history: Some(history(selected, coverage)),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }
}

fn covered_run_ordinals(from: u64, to: u64) -> OrchestrationV2CoveredRunOrdinals {
    OrchestrationV2CoveredRunOrdinals {
        from: PositiveInt::new(from).expect("run ordinals start at 1"),
        to: PositiveInt::new(to).expect("run ordinals start at 1"),
    }
}

/// `historicalMessage` over each item, with `runStatus` set from the run the
/// item belongs to (empty `runs` leaves it unset).
fn historical_messages(
    items: &[OrchestrationV2TurnItem],
    runs: &[OrchestrationV2Run],
) -> Vec<OrchestrationV2HistoricalMessage> {
    items
        .iter()
        .filter_map(|item| {
            let mut message = historical_message(item)?;
            if let Some(run_id) = turn_item_run_id(item) {
                message.run_status = runs
                    .iter()
                    .find(|run| &run.id == run_id)
                    .map(|run| wire_name(&run.status));
            }
            Some(message)
        })
        .collect()
}

fn turn_item_run_id(item: &OrchestrationV2TurnItem) -> Option<&RunId> {
    turn_item_common(item).run_id
}

fn history(selected: SelectedHistory, coverage: String) -> OrchestrationV2ContextHandoffHistory {
    OrchestrationV2ContextHandoffHistory {
        messages: selected.messages,
        coverage,
        omitted_items: selected.omitted_items as u64,
        omitted_item_ids: Some(selected.omitted_item_ids),
    }
}

/// The wire spelling of a contract enum (`"fork_delta_summary"`, …).
fn wire_name<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `text.replace(/\s+/g, " ").trim()`, then cut to `max_length` UTF-16 code
/// units with a trailing `...`. Units, not chars: upstream measures `.length`.
fn compact_text(text: &str, max_length: usize) -> String {
    let compacted = text
        .split(is_js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if js_len(&compacted) <= max_length {
        return compacted;
    }
    let units: Vec<u16> = compacted.encode_utf16().take(max_length - 3).collect();
    format!("{}...", String::from_utf16_lossy(&units))
}

fn summarize_delta_item(item: &OrchestrationV2TurnItem) -> Option<String> {
    use OrchestrationV2TurnItem as Item;
    match item {
        Item::UserMessage { text, .. } => Some(format!("- User: {}", compact_text(text, 240))),
        Item::AssistantMessage { text, .. } => {
            Some(format!("- Assistant: {}", compact_text(text, 240)))
        }
        Item::CommandExecution { input, .. } => {
            Some(format!("- Command: {}", compact_text(input, 240)))
        }
        Item::FileChange { file_name, .. } => {
            Some(format!("- File change: {}", file_name.as_str()))
        }
        Item::Checkpoint { files, .. } => Some(format!("- Checkpoint: {} files", files.len())),
        Item::Handoff {
            summary, strategy, ..
        } => {
            let source = summary.clone().unwrap_or_else(|| wire_name(strategy));
            Some(format!("- Handoff: {}", compact_text(&source, 240)))
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
    let item_lines: Vec<String> = delta_items
        .iter()
        .filter_map(summarize_delta_item)
        .collect();
    let mut lines = vec![
        "Merge-back context from forked conversation.".to_owned(),
        format!("Source thread: {}", source_thread_id.as_str()),
        format!("Target thread: {}", target_thread_id.as_str()),
        format!(
            "Covered fork runs: {}-{}",
            covered.from.0.get(),
            covered.to.0.get()
        ),
        String::new(),
        "Fork delta:".to_owned(),
    ];
    if item_lines.is_empty() {
        lines.push("- No user-visible delta items.".to_owned());
    } else {
        lines.extend(item_lines);
    }
    lines.join("\n")
}

fn is_unit_whitespace(unit: u16) -> bool {
    char::from_u32(u32::from(unit)).is_some_and(is_js_whitespace)
}

/// The imported transcript, newest first, cut to 32,000 UTF-16 units. A section
/// that does not fit whole keeps its newest tail, started at a word boundary and
/// never inside a surrogate pair.
fn make_legacy_import_summary(items: &[OrchestrationV2TurnItem]) -> String {
    use OrchestrationV2TurnItem as Item;
    let sections: Vec<(&str, &str)> = items
        .iter()
        .filter_map(|item| match item {
            Item::UserMessage { text, .. } => Some(("User", text.as_str())),
            Item::AssistantMessage { text, .. } => Some(("Assistant", text.as_str())),
            _ => None,
        })
        .collect();
    let header = "Imported conversation history from the previous T3 Code orchestrator. Use it as context; do not repeat it unless the user asks.";
    let max_chars: i64 = 32_000;
    let mut selected: VecDeque<String> = VecDeque::new();
    let mut remaining: i64 = max_chars - js_len(header) as i64 - 2;
    for (label, body) in sections.iter().rev() {
        if remaining <= 0 {
            break;
        }
        let full_section = format!("{label}:\n{body}");
        let full_len = js_len(&full_section) as i64;
        if full_len <= remaining {
            selected.push_front(full_section);
            remaining -= full_len + 2;
            continue;
        }

        let prefix = format!("{label}:\n... ");
        let body_budget = remaining - js_len(&prefix) as i64;
        if body_budget <= 0 {
            continue;
        }

        let units: Vec<u16> = body.encode_utf16().collect();
        let is_ws_at = |index: usize| {
            units
                .get(index)
                .is_some_and(|unit| is_unit_whitespace(*unit))
        };
        let mut body_start = (units.len() as i64 - body_budget).max(0) as usize;
        if body_start > 0
            && !is_ws_at(body_start - 1)
            && !is_ws_at(body_start)
            && let Some(offset) = units[body_start..]
                .iter()
                .position(|unit| is_unit_whitespace(*unit))
        {
            body_start += offset;
        }
        let unit_at = units.get(body_start).copied();
        let unit_before = body_start
            .checked_sub(1)
            .and_then(|index| units.get(index).copied());
        if matches!(unit_at, Some(0xdc00..=0xdfff)) && matches!(unit_before, Some(0xd800..=0xdbff))
        {
            body_start += 1;
        }
        let body_suffix = String::from_utf16_lossy(units.get(body_start..).unwrap_or(&[]));
        let body_suffix = body_suffix.trim_start_matches(is_js_whitespace);
        if body_suffix.is_empty() {
            continue;
        }

        let suffix = format!("{prefix}{body_suffix}");
        remaining -= js_len(&suffix) as i64 + 2;
        selected.push_front(suffix);
    }
    std::iter::once(header.to_owned())
        .chain(selected)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `providerMessageWithContextHandoff`: the handoff summary followed by the
/// user's message, for providers that take the context as plain text.
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
            strategy => wire_name(&strategy),
        };
        lines.push(format!("Context handoff ({label}):"));
        lines.push(handoff.summary_text.clone());
        lines.push(String::new());
    }
    lines.push("User message:".to_owned());
    lines.push(user_text.to_owned());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use turin_contracts::orchestration_v2::{
        OrchestrationV2Actor, OrchestrationV2CreationSource, OrchestrationV2TurnItemStatus,
        OrchestrationV2UserMessageInputIntent,
    };
    use turin_contracts::{MessageId, TurnItemId};
    use turin_provider_core::handoff_budget::DEFAULT_HANDOFF_TOKEN_CAP;

    fn imported_item(role: &str, id: &str, text: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        let now = UtcDateTime::now();
        let base = (
            TurnItemId::new(format!("turn-item:{id}")).unwrap(),
            MessageId::new(format!("message:{id}")).unwrap(),
        );
        if role == "user" {
            OrchestrationV2TurnItem::UserMessage {
                tool_non_execution_kind: None,
                tool_surface: None,
                tool_icon: None,
                tool_source: None,
                id: base.0,
                thread_id: ThreadId::new("thread:legacy-context").unwrap(),
                run_id: None,
                node_id: None,
                provider_thread_id: None,
                provider_turn_id: None,
                native_item_ref: None,
                parent_item_id: None,
                ordinal,
                status: OrchestrationV2TurnItemStatus::Completed,
                title: None,
                started_at: Some(now),
                completed_at: Some(now),
                updated_at: now,
                created_by: OrchestrationV2Actor::User,
                creation_source: OrchestrationV2CreationSource::Server,
                message_id: base.1,
                scheduled_task_id: None,
                sender_thread_id: None,
                input_intent: OrchestrationV2UserMessageInputIntent::TurnStart,
                text: text.to_owned(),
                context: None,
                attachments: vec![],
            }
        } else {
            let value = json!({
                "type": "assistant_message",
                "id": base.0,
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
                "startedAt": null,
                "completedAt": null,
                "updatedAt": now,
                "messageId": base.1,
                "text": text,
                "attachments": null,
                "streaming": false,
            });
            serde_json::from_value(value).unwrap()
        }
    }

    fn prepare_import(items: &[OrchestrationV2TurnItem]) -> OrchestrationV2ContextHandoff {
        ContextHandoffService::new(DEFAULT_HANDOFF_TOKEN_CAP).prepare_legacy_import(
            PrepareLegacyImportInput {
                thread_id: &ThreadId::new("thread:legacy-context").unwrap(),
                target_run_id: &RunId::new("run:first-v2").unwrap(),
                to_provider_thread_id: &ProviderThreadId::new("provider-thread:first-v2").unwrap(),
                to_provider_instance_id: &ProviderInstanceId::new("codex").unwrap(),
                items,
                created_at: UtcDateTime::now(),
            },
        )
    }

    #[test]
    fn prepares_imported_history_for_the_first_native_v2_turn() {
        let handoff = prepare_import(&[
            imported_item("user", "one", "What did we decide?", 1),
            imported_item(
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
        let provider_message =
            provider_message_with_context_handoff(&handoff, "Continue from there.");
        assert!(provider_message.contains(&handoff.summary_text));
        assert!(provider_message.contains("User message:\nContinue from there."));
    }

    #[test]
    fn preserves_role_attribution_when_truncating_imported_history() {
        let long = format!("{} retained final words", "x".repeat(35_000));
        let handoff = prepare_import(&[imported_item("user", "long", &long, 1)]);

        assert!(js_len(&handoff.summary_text) <= 32_000);
        assert!(
            handoff
                .summary_text
                .contains("User:\n... retained final words")
        );
        let retained = regex::Regex::new(r"\n+x+ retained final words").unwrap();
        assert!(!retained.is_match(&handoff.summary_text));
    }

    #[test]
    fn retains_the_newest_oversized_import_even_when_it_has_no_whitespace() {
        let long = format!("{}LATEST_SINGLE_TOKEN", "🧪".repeat(20_000));
        let handoff = prepare_import(&[
            imported_item("assistant", "older", "older message", 1),
            imported_item("user", "long-single-token", &long, 2),
        ]);

        let omitted = handoff.history.as_ref().unwrap().omitted_item_ids.clone();
        assert!(
            omitted
                .unwrap_or_default()
                .contains(&TurnItemId::new("turn-item:long-single-token").unwrap())
        );
        assert!(js_len(&handoff.summary_text) <= 32_000);
        assert!(handoff.summary_text.contains("User:\n... "));
        assert!(handoff.summary_text.contains("LATEST_SINGLE_TOKEN"));
        assert!(!handoff.summary_text.contains('\u{fffd}'));
    }
}

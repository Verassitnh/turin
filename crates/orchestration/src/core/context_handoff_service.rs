//! Port of upstream/apps/server/src/orchestration-v2/ContextHandoffService.ts.
//!
//! Builds the `OrchestrationV2ContextHandoff` records that carry portable
//! history into a provider thread. Id allocation cannot fail here, so none of
//! the `prepare*` methods returns a `ContextHandoffPrepareError`; that type is
//! not ported.

use serde::Serialize;
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

use crate::projection::turn_item;

/// The strategies `prepareProviderHandoff` accepts.
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

pub struct PrepareProviderHandoffInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    /// `Some(None)` is upstream's `null`.
    pub transfer_id: Option<turin_contracts::ContextTransferId>,
    pub from_provider_thread_ids: &'a [ProviderThreadId],
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub from_provider_instance_id: &'a ProviderInstanceId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub covered_run_ordinals: OrchestrationV2CoveredRunOrdinals,
    pub runs: Option<&'a [OrchestrationV2Run]>,
    pub strategy: ProviderHandoffStrategy,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

pub struct PrepareForkDeltaInput<'a> {
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

pub struct PrepareLegacyImportInput<'a> {
    pub thread_id: &'a ThreadId,
    pub target_run_id: &'a RunId,
    pub to_provider_thread_id: &'a ProviderThreadId,
    pub to_provider_instance_id: &'a ProviderInstanceId,
    pub items: &'a [OrchestrationV2TurnItem],
    pub created_at: UtcDateTime,
}

/// `ContextHandoffServiceV2`.
#[derive(Debug, Clone, Copy)]
pub struct ContextHandoffService {
    token_cap: i64,
}

impl Default for ContextHandoffService {
    fn default() -> Self {
        Self::new(DEFAULT_HANDOFF_TOKEN_CAP)
    }
}

fn covered(from: u64, to: u64) -> OrchestrationV2CoveredRunOrdinals {
    OrchestrationV2CoveredRunOrdinals {
        from: turin_contracts::PositiveInt::new(from).expect("positive"),
        to: turin_contracts::PositiveInt::new(to).expect("positive"),
    }
}

fn history(
    messages: Vec<OrchestrationV2HistoricalMessage>,
    coverage: String,
    omitted_items: i64,
    omitted_item_ids: Vec<turin_contracts::TurnItemId>,
) -> OrchestrationV2ContextHandoffHistory {
    OrchestrationV2ContextHandoffHistory {
        messages,
        coverage,
        omitted_items: u64::try_from(omitted_items).unwrap_or(0),
        omitted_item_ids: Some(omitted_item_ids),
    }
}

impl ContextHandoffService {
    /// `tokenCap` is `handoffTokenCapConfig` read once when the layer builds.
    pub fn new(token_cap: i64) -> Self {
        Self { token_cap }
    }

    /// The service with the cap read from the environment, falling back to
    /// the default when the variable is malformed (`Effect.orElseSucceed`).
    pub fn from_environment() -> Self {
        Self::new(
            turin_provider_core::handoff_budget::handoff_token_cap_config(|name| {
                std::env::var(name).ok()
            })
            .unwrap_or(DEFAULT_HANDOFF_TOKEN_CAP),
        )
    }

    pub fn prepare_legacy_import(
        &self,
        input: &PrepareLegacyImportInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let handoff_id = IdAllocator.context_handoff(
            input.thread_id,
            &ProviderInstanceId::new("legacy").expect("static id"),
            input.to_provider_instance_id,
        );
        let covered_run_ordinals = covered(1, 1);
        let coverage =
            handoff_coverage(input.thread_id.as_str(), &covered_run_ordinals, input.items);
        let messages: Vec<_> = input.items.iter().filter_map(historical_message).collect();
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
        OrchestrationV2ContextHandoff {
            id: handoff_id,
            transfer_id: Some(None),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: Vec::new(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals,
            strategy: OrchestrationV2ContextHandoffStrategy::ManualContext,
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: make_legacy_import_summary(input.items),
            history: Some(history(
                selected.messages,
                coverage,
                selected.omitted_items,
                selected.omitted_item_ids,
            )),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    pub fn prepare(
        &self,
        thread_id: &ThreadId,
        target_run_id: &RunId,
        from_provider_thread_ids: &[ProviderThreadId],
        to_provider_thread_id: &ProviderThreadId,
    ) -> OrchestrationV2ContextHandoff {
        let now = UtcDateTime::now();
        let manual = ProviderInstanceId::new("manual").expect("static id");
        OrchestrationV2ContextHandoff {
            id: IdAllocator.context_handoff(thread_id, &manual, &manual),
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

    pub fn prepare_fork_delta(
        &self,
        input: &PrepareForkDeltaInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
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
        let messages: Vec<_> = input
            .delta_items
            .iter()
            .filter_map(historical_message)
            .collect();
        let selected = select_history(&SelectHistoryInput {
            messages: &messages,
            coverage: &coverage,
            omitted_items: None,
            budget: self.token_cap,
        });
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
            summary_text: make_fork_delta_summary(input),
            history: Some(history(
                selected.messages,
                coverage,
                selected.omitted_items,
                selected.omitted_item_ids,
            )),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }

    pub fn prepare_provider_handoff(
        &self,
        input: &PrepareProviderHandoffInput<'_>,
    ) -> OrchestrationV2ContextHandoff {
        let handoff_id = IdAllocator.context_handoff(
            input.thread_id,
            input.from_provider_instance_id,
            input.to_provider_instance_id,
        );
        let run_statuses: std::collections::HashMap<&RunId, String> = input
            .runs
            .unwrap_or_default()
            .iter()
            .map(|run| (&run.id, wire_text(&run.status)))
            .collect();
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
                if let Some(run_id) = turn_item::common(item).run_id {
                    message.run_status = run_statuses.get(run_id).cloned();
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
        OrchestrationV2ContextHandoff {
            id: handoff_id,
            transfer_id: Some(input.transfer_id.clone()),
            thread_id: input.thread_id.clone(),
            target_run_id: input.target_run_id.clone(),
            from_provider_thread_ids: input.from_provider_thread_ids.to_vec(),
            to_provider_thread_id: input.to_provider_thread_id.clone(),
            covered_run_ordinals: input.covered_run_ordinals.clone(),
            strategy: input.strategy.contract(),
            status: OrchestrationV2ContextHandoffStatus::Ready,
            summary_message_id: None,
            summary_text: render_history(&selected.messages, &selected.context),
            history: Some(history(
                selected.messages,
                coverage,
                selected.omitted_items,
                selected.omitted_item_ids,
            )),
            delivery: None,
            detail_in_turn_item: None,
            created_by_provider_instance_id: None,
            created_at: input.created_at,
            updated_at: input.created_at,
        }
    }
}

fn wire_text<T: Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `compactText`: `text.replace(/\s+/g, " ").trim()`, then a cut. `\s` is
/// JavaScript's set (it has U+FEFF and not U+0085), not Unicode White_Space.
/// Lengths are UTF-16 code units, as in JavaScript.
fn compact_text(text: &str, max_length: usize) -> String {
    let compacted = text
        .split(turin_contracts::base_schemas::is_js_whitespace)
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let units: Vec<u16> = compacted.encode_utf16().collect();
    if units.len() <= max_length {
        return compacted;
    }
    format!("{}...", String::from_utf16_lossy(&units[..max_length - 3]))
}

/// `summarizeDeltaItem`.
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
        Item::FileChange { file_name, .. } => Some(format!("- File change: {file_name}")),
        Item::Checkpoint { files, .. } => Some(format!("- Checkpoint: {} files", files.len())),
        Item::Handoff {
            summary, strategy, ..
        } => {
            let label = summary.clone().unwrap_or_else(|| wire_text(strategy));
            Some(format!("- Handoff: {}", compact_text(&label, 240)))
        }
        _ => None,
    }
}

/// `makeForkDeltaSummary`.
fn make_fork_delta_summary(input: &PrepareForkDeltaInput<'_>) -> String {
    let item_lines: Vec<String> = input
        .delta_items
        .iter()
        .filter_map(summarize_delta_item)
        .collect();
    let mut lines = vec![
        "Merge-back context from forked conversation.".to_owned(),
        format!("Source thread: {}", input.source_thread_id),
        format!("Target thread: {}", input.target_thread_id),
        format!(
            "Covered fork runs: {}-{}",
            input.covered_run_ordinals.from, input.covered_run_ordinals.to
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

/// JavaScript `\s`: what `/\s/` and `trimStart` treat as whitespace.
fn is_js_whitespace(unit: u16) -> bool {
    matches!(
        unit,
        0x09..=0x0d | 0x20 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f
            | 0x3000 | 0xfeff
    )
}

fn utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// `makeLegacyImportSummary`. Works in UTF-16 code units so the 32,000-unit
/// cap and the surrogate-pair repair match JavaScript string indexing.
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
    let mut selected: Vec<String> = Vec::new();
    let mut remaining = max_chars - utf16(header).len() as i64 - 2;
    for (label, body) in sections.into_iter().rev() {
        if remaining <= 0 {
            break;
        }
        let full_section = format!("{label}:\n{body}");
        let full_length = utf16(&full_section).len() as i64;
        if full_length <= remaining {
            selected.insert(0, full_section);
            remaining -= full_length + 2;
            continue;
        }

        let prefix = format!("{label}:\n... ");
        let body_budget = remaining - utf16(&prefix).len() as i64;
        if body_budget <= 0 {
            continue;
        }
        let body_units = utf16(body);
        let initial_start = body_units.len() as i64 - body_budget;
        let mut body_start = initial_start.max(0) as usize;
        if body_start > 0
            && !is_js_whitespace(body_units[body_start - 1])
            && !is_js_whitespace(body_units[body_start])
            && let Some(next_whitespace) = body_units[body_start..]
                .iter()
                .position(|unit| is_js_whitespace(*unit))
        {
            body_start += next_whitespace;
        }
        // Do not start on the low half of a surrogate pair.
        if body_start > 0
            && body_start < body_units.len()
            && (0xdc00..=0xdfff).contains(&body_units[body_start])
            && (0xd800..=0xdbff).contains(&body_units[body_start - 1])
        {
            body_start += 1;
        }
        let suffix_units = &body_units[body_start.min(body_units.len())..];
        let trimmed_from = suffix_units
            .iter()
            .position(|unit| !is_js_whitespace(*unit))
            .unwrap_or(suffix_units.len());
        let body_suffix = &suffix_units[trimmed_from..];
        if body_suffix.is_empty() {
            continue;
        }
        let suffix = format!("{prefix}{}", String::from_utf16_lossy(body_suffix));
        remaining -= utf16(&suffix).len() as i64 + 2;
        selected.insert(0, suffix);
    }
    std::iter::once(header.to_owned())
        .chain(selected)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// `providerMessageWithContextHandoff`.
pub fn provider_message_with_context_handoff(
    handoff: &OrchestrationV2ContextHandoff,
    user_text: &str,
) -> String {
    provider_message_with_context_handoffs(std::slice::from_ref(handoff), user_text)
}

/// `providerMessageWithContextHandoffs`.
pub fn provider_message_with_context_handoffs(
    handoffs: &[OrchestrationV2ContextHandoff],
    user_text: &str,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    for handoff in handoffs {
        let label = if handoff.strategy == OrchestrationV2ContextHandoffStrategy::ForkDeltaSummary {
            "merge_back / fork_delta_summary".to_owned()
        } else {
            wire_text(&handoff.strategy)
        };
        parts.push(format!("Context handoff ({label}):"));
        parts.push(handoff.summary_text.clone());
        parts.push(String::new());
    }
    parts.push("User message:".to_owned());
    parts.push(user_text.to_owned());
    parts.join("\n")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    const T0: &str = "2026-01-01T00:00:00.000Z";
    const CREATED: &str = "2026-01-02T00:00:00.000Z";

    fn at(value: &str) -> UtcDateTime {
        serde_json::from_value(json!(value)).expect("utc")
    }

    fn imported_item(role: &str, id: &str, text: &str, ordinal: u64) -> OrchestrationV2TurnItem {
        let mut item = json!({
            "id": format!("turn-item:{id}"), "threadId": "thread:legacy-context",
            "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal,
            "status": "completed", "title": null, "startedAt": T0, "completedAt": T0,
            "updatedAt": T0, "messageId": format!("message:{id}"), "text": text,
        });
        if role == "user" {
            item["type"] = json!("user_message");
            item["createdBy"] = json!("user");
            item["creationSource"] = json!("server");
            item["inputIntent"] = json!("turn_start");
            item["attachments"] = json!([]);
        } else {
            item["type"] = json!("assistant_message");
            item["streaming"] = json!(false);
        }
        serde_json::from_value(item).expect("item fixture")
    }

    fn prepare(items: &[OrchestrationV2TurnItem]) -> OrchestrationV2ContextHandoff {
        ContextHandoffService::default().prepare_legacy_import(&PrepareLegacyImportInput {
            thread_id: &ThreadId::new("thread:legacy-context").expect("id"),
            target_run_id: &RunId::new("run:first-v2").expect("id"),
            to_provider_thread_id: &ProviderThreadId::new("provider-thread:first-v2").expect("id"),
            to_provider_instance_id: &ProviderInstanceId::new("codex").expect("id"),
            items,
            created_at: at(CREATED),
        })
    }

    #[test]
    fn prepares_imported_history_for_the_first_native_v2_turn() {
        let handoff = prepare(&[
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
        let handoff = prepare(&[imported_item(
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
        // The truncated body starts after whitespace, not mid-token.
        assert!(!handoff.summary_text.contains("\nxxx retained final words"));
        assert!(!handoff.summary_text.contains("\nx retained final words"));
    }

    #[test]
    fn retains_the_newest_oversized_import_even_when_it_has_no_whitespace() {
        let handoff = prepare(&[
            imported_item("assistant", "older", "older message", 1),
            imported_item(
                "user",
                "long-single-token",
                &format!("{}LATEST_SINGLE_TOKEN", "🧪".repeat(20_000)),
                2,
            ),
        ]);

        let omitted = handoff
            .history
            .as_ref()
            .and_then(|history| history.omitted_item_ids.as_ref())
            .expect("omitted ids");
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
    fn provider_handoff_attributes_each_message_to_its_run_status() {
        let thread = ThreadId::new("thread:legacy-context").expect("id");
        let mut item =
            serde_json::to_value(imported_item("user", "one", "Hello", 1)).expect("item");
        item["runId"] = json!("run:failed-one");
        let item: OrchestrationV2TurnItem = serde_json::from_value(item).expect("item");
        let run: OrchestrationV2Run = serde_json::from_value(json!({
            "id": "run:failed-one", "threadId": "thread:legacy-context", "ordinal": 1,
            "providerInstanceId": "codex",
            "modelSelection": {"instanceId": "codex", "model": "gpt-5.4"},
            "providerThreadId": null, "userMessageId": "message:one", "rootNodeId": null,
            "activeAttemptId": null, "status": "failed", "requestedAt": T0, "startedAt": T0,
            "completedAt": T0, "checkpointId": null, "contextHandoffId": null,
        }))
        .expect("run");
        let handoff =
            ContextHandoffService::default().prepare_provider_handoff(
                &PrepareProviderHandoffInput {
                    thread_id: &thread,
                    target_run_id: &RunId::new("run:two").expect("id"),
                    transfer_id: None,
                    from_provider_thread_ids: &[
                        ProviderThreadId::new("provider-thread:a").expect("id")
                    ],
                    to_provider_thread_id: &ProviderThreadId::new("provider-thread:a").expect("id"),
                    from_provider_instance_id: &ProviderInstanceId::new("codex").expect("id"),
                    to_provider_instance_id: &ProviderInstanceId::new("codex").expect("id"),
                    covered_run_ordinals: covered(1, 1),
                    runs: Some(std::slice::from_ref(&run)),
                    strategy: ProviderHandoffStrategy::DeltaSinceTargetLastSeen,
                    items: std::slice::from_ref(&item),
                    created_at: at(CREATED),
                },
            );
        let messages = handoff.history.expect("history").messages;
        assert_eq!(messages[0].run_status.as_deref(), Some("failed"));
        assert_eq!(
            handoff.strategy,
            OrchestrationV2ContextHandoffStrategy::DeltaSinceTargetLastSeen
        );
    }
    #[test]
    fn compact_text_collapses_javascript_whitespace_not_unicode_whitespace() {
        // JS `\s` has U+FEFF and not U+0085 (NEL).
        assert_eq!(compact_text("\u{feff} a \n\t b\u{feff}c  ", 240), "a b c");
        assert_eq!(compact_text("a\u{85}b", 240), "a\u{85}b");
        // The cut counts UTF-16 units and appends "...".
        assert_eq!(compact_text("abcdefghij", 8), "abcde...");
    }
}

#[cfg(test)]
mod js_whitespace_tests {
    use super::compact_text;

    /// `text.replace(/\s+/g, " ").trim()`: U+FEFF is whitespace, U+0085 is not.
    #[test]
    fn compact_text_collapses_js_whitespace_only() {
        assert_eq!(compact_text("\u{feff} a \n\t b\u{a0}c ", 240), "a b c");
        assert_eq!(compact_text("a\u{85}b", 240), "a\u{85}b");
    }
}

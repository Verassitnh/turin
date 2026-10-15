//! Port of upstream/packages/client-runtime/src/state/subagentRuntime.ts
//! (`isLiveSubagentTurnItem` only; the runtime subagent model and workflow
//! grouping belong to the subagent panel, which is not ported yet).

use super::turn_item::{TurnItem, TurnItemType};

pub fn is_live_subagent_turn_item(item: &TurnItem) -> bool {
    // `isOrchestrationV2WorkActive`: pending | running | waiting.
    item.item_type() == TurnItemType::Subagent && item.status.is_live()
}

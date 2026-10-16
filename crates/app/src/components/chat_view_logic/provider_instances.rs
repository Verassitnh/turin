//! Port of upstream/apps/web/src/providerInstances.ts (selection helpers used by ChatView.logic).

use turin_contracts::provider_instance::{ProviderDriverKind, ProviderInstanceId};
use turin_provider_core::server_provider::{ServerProvider, ServerProviderState};

pub const NO_PROVIDER_INSTANCE_ID: &str = "t3code_no_provider";

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInstanceEntry {
    pub instance_id: ProviderInstanceId,
    pub driver_kind: ProviderDriverKind,
    pub continuation_group_key: Option<String>,
    pub enabled: bool,
    pub is_available: bool,
    pub status: ServerProviderState,
    pub snapshot: ServerProvider,
}

fn is_selectable_provider_instance_entry(entry: &ProviderInstanceEntry) -> bool {
    entry.enabled && entry.is_available
}

fn is_provider_instance_picker_ready(entry: &ProviderInstanceEntry) -> bool {
    entry.enabled && entry.is_available && entry.status == ServerProviderState::Ready
}

pub fn resolve_selectable_provider_instance_entry<'a>(
    entries: &'a [ProviderInstanceEntry],
    instance_id: Option<&ProviderInstanceId>,
) -> Option<&'a ProviderInstanceEntry> {
    if let Some(instance_id) = instance_id {
        let requested = entries
            .iter()
            .find(|entry| entry.instance_id == *instance_id);
        if requested.is_some_and(is_selectable_provider_instance_entry) {
            return requested;
        }
    }
    entries
        .iter()
        .find(|entry| is_provider_instance_picker_ready(entry))
        .or_else(|| {
            entries.iter().find(|entry| {
                is_selectable_provider_instance_entry(entry)
                    && entry.status != ServerProviderState::Error
            })
        })
}

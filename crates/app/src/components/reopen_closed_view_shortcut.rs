//! Port of upstream/apps/web/src/components/ReopenClosedViewShortcut.tsx (keyboard
//! handler; full restore wiring uses `reopen_closed_view` + navigation).

use crate::shell::reopen_closed_view::{
    ClosedViewEntry, ReopenOwnerState, plan_next_reopen, reopen_closed_panel_tab,
};
use crate::state::right_panel_store::RightPanelStore;

/// Handles `view.reopenClosed` when bound in the shell keymap.
pub fn try_reopen_closed_view(
    entries: &[ClosedViewEntry],
    owner_state: impl Fn(&ClosedViewEntry) -> ReopenOwnerState,
    panels: &mut RightPanelStore,
    workspace_available: bool,
) -> bool {
    let plan = plan_next_reopen(entries, owner_state);
    for dropped in &plan.dropped {
        panels.remove_closed_id(&dropped.id);
    }
    if let Some(restore) = plan.restore {
        if reopen_closed_panel_tab(panels, &restore, workspace_available) {
            panels.remove_closed_id(&restore.id);
            return true;
        }
        panels.defer_closed_id(&restore.id);
    }
    false
}

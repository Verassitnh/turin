//! Ports of upstream/apps/web/src/components/EnvironmentMachineIcon.tsx,
//! BranchToolbarEnvironmentSelector.tsx and BranchToolbarEnvModeSelector.tsx
//! (plus PreviousWorktreeItemContent.tsx).
//!
//! Turin has one environment (this machine), so the environment selector only
//! ever renders its locked indicator, and only when the active environment is
//! not the primary one (`shouldShowEnvironmentIndicator`), which never holds
//! here; it is ported for the multi-machine shape of the data seam. The
//! Workspace selector keeps its three entries (Current checkout, New worktree,
//! Previous worktree): choosing New worktree sets the draft's env mode, and
//! the launch itself is the hooks' concern (PORTING.md cut list).

// A faithful port keeps upstream's whole API (PORTING rule 3); the hosts wire
// the rest as the composer, thread details and settings tasks land.
#![allow(dead_code)]

use gpui_kit::{AnyElement, IntoElement, ParentElement as _, Styled as _, div, rems};
use turin_contracts::environment::EnvironmentMachineKind;

use super::branch_toolbar_logic::{
    EnvMode, EnvironmentOption, resolve_current_workspace_label, resolve_env_mode_label,
    resolve_locked_workspace_label,
};
use super::composer_control::composer_context_label;
use crate::theme::AppTheme;
use crate::theme::color::with_alpha;
use crate::theme::tokens::TEXT_XS;
use crate::ui::icon::{Icon, IconName};
use crate::ui::middle_truncate::MiddleTruncate;

/// `EnvironmentMachineIcon`: the glyph for a machine kind.
pub fn environment_machine_icon(kind: EnvironmentMachineKind) -> Icon {
    match kind {
        EnvironmentMachineKind::Server => Icon::new(IconName::Server),
        EnvironmentMachineKind::Cloud => Icon::new(IconName::Cloud),
        EnvironmentMachineKind::Linux => Icon::path(crate::assets::LINUX),
        EnvironmentMachineKind::Desktop => Icon::new(IconName::Monitor),
        EnvironmentMachineKind::Laptop => Icon::new(IconName::Laptop),
        EnvironmentMachineKind::MacMini => Icon::path(crate::assets::MAC_MINI),
        EnvironmentMachineKind::MacStudio => Icon::path(crate::assets::MAC_STUDIO),
    }
}

/// `ENVIRONMENT_MACHINE_KIND_LABELS`.
pub fn environment_machine_kind_label(kind: EnvironmentMachineKind) -> &'static str {
    match kind {
        EnvironmentMachineKind::Server => "Server",
        EnvironmentMachineKind::Cloud => "Cloud VM",
        EnvironmentMachineKind::Linux => "Linux/WSL",
        EnvironmentMachineKind::Desktop => "Desktop",
        EnvironmentMachineKind::Laptop => "Laptop",
        EnvironmentMachineKind::MacMini => "Mini PC",
        EnvironmentMachineKind::MacStudio => "Workstation",
    }
}

/// The locked environment row of `BranchToolbarEnvironmentSelector`:
/// `inline-flex h-7 items-center gap-1 px-1.75 text-xs text-muted-foreground/70`.
pub fn environment_indicator(
    active: Option<&EnvironmentOption>,
    compact: bool,
    theme: &AppTheme,
) -> AnyElement {
    let kind = active
        .map(|env| env.machine)
        .unwrap_or(EnvironmentMachineKind::Server);
    let label = active
        .map(|env| env.label.clone())
        .unwrap_or_else(|| "Run on".to_owned());
    div()
        .flex()
        .h(rems(1.5))
        .min_w_0()
        .max_w_full()
        .items_center()
        .gap_1()
        .border_1()
        .border_color(gpui_kit::transparent_black())
        .px(rems(0.4375))
        .text_size(TEXT_XS.size)
        .text_color(with_alpha(theme.muted_foreground, 0.7))
        .child(
            environment_machine_icon(kind)
                .size(rems(0.75))
                .color(with_alpha(theme.muted_foreground, 0.7).into()),
        )
        .child(composer_context_label(label, compact))
        .into_any_element()
}

/// `PreviousWorktreeItemContent`: the previous worktree's branch with its
/// folder glyph.
pub fn previous_worktree_item(branch: Option<&str>, theme: &AppTheme) -> AnyElement {
    div()
        .flex()
        .min_w_0()
        .items_center()
        .gap(rems(0.375))
        .child(
            Icon::new(IconName::FolderGit2)
                .size(rems(0.75))
                .color(theme.muted_foreground),
        )
        .child(div().flex_shrink_0().child("Previous worktree"))
        .children(branch.map(|branch| {
            div()
                .flex()
                .min_w_0()
                .gap_1()
                .text_color(theme.muted_foreground)
                .child("(")
                .child(MiddleTruncate::new(branch.to_owned()))
                .child(")")
        }))
        .into_any_element()
}

/// The glyph of the workspace selector: `FolderGit2` for a new worktree,
/// `FolderGit` for an attached one, `Folder` for the checkout.
pub fn workspace_icon(effective_env_mode: EnvMode, active_worktree_path: Option<&str>) -> IconName {
    if effective_env_mode == EnvMode::Worktree {
        IconName::FolderGit2
    } else if active_worktree_path.is_some() {
        IconName::FolderGit
    } else {
        IconName::Folder
    }
}

/// The selector's label (`resolveCurrentWorkspaceLabel` etc.).
pub struct WorkspaceLabels {
    pub trigger: String,
    pub locked: String,
}

pub fn workspace_labels(
    force_new_worktree: bool,
    env_mode_locked: bool,
    effective_env_mode: EnvMode,
    active_worktree_path: Option<&str>,
) -> WorkspaceLabels {
    let current = if force_new_worktree {
        resolve_env_mode_label(EnvMode::Worktree).to_owned()
    } else if env_mode_locked {
        resolve_locked_workspace_label(active_worktree_path, effective_env_mode).to_owned()
    } else if effective_env_mode == EnvMode::Worktree {
        resolve_env_mode_label(EnvMode::Worktree).to_owned()
    } else {
        resolve_current_workspace_label(active_worktree_path).to_owned()
    };
    WorkspaceLabels {
        trigger: current.clone(),
        locked: if force_new_worktree {
            resolve_env_mode_label(EnvMode::Worktree).to_owned()
        } else {
            resolve_locked_workspace_label(active_worktree_path, effective_env_mode).to_owned()
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workspace_glyph_follows_the_mode_and_worktree() {
        assert_eq!(
            workspace_icon(EnvMode::Worktree, None),
            IconName::FolderGit2
        );
        assert_eq!(
            workspace_icon(EnvMode::Local, Some("/wt")),
            IconName::FolderGit
        );
        assert_eq!(workspace_icon(EnvMode::Local, None), IconName::Folder);
    }

    #[test]
    fn workspace_labels_cover_open_locked_and_forced_states() {
        let open = workspace_labels(false, false, EnvMode::Local, None);
        assert_eq!(open.trigger, "Current checkout");
        let worktree = workspace_labels(false, false, EnvMode::Worktree, None);
        assert_eq!(worktree.trigger, "New worktree");
        let locked = workspace_labels(false, true, EnvMode::Local, Some("/wt"));
        assert_eq!(locked.trigger, "Worktree");
        let forced = workspace_labels(true, false, EnvMode::Local, None);
        assert_eq!(forced.trigger, "New worktree");
        assert_eq!(forced.locked, "New worktree");
    }

    #[test]
    fn machine_kinds_have_upstream_labels() {
        assert_eq!(
            environment_machine_kind_label(EnvironmentMachineKind::MacMini),
            "Mini PC"
        );
        assert_eq!(
            environment_machine_kind_label(EnvironmentMachineKind::Linux),
            "Linux/WSL"
        );
    }
}

//! Port of upstream/apps/server/src/provider/ProjectSettingsOverrideSweep.ts.
//!
//! Removes `projectSettingsOverrides` entries whose project no longer exists.
//! A project deleted by an older server, or one whose cleanup write failed,
//! leaves its override behind, and the instance registry folds every override
//! in unconditionally. This runs once at boot. Its best-effort: a failure is
//! logged and retried on the next boot.

use std::collections::HashSet;

use indexmap::IndexMap;
use turin_contracts::{
    ProjectId,
    settings::{ProjectSettingsOverrides, ServerSettings, ServerSettingsPatch},
};
use turin_orchestration::projection::project_store::ProjectStore;
use turin_runtime::server_settings::service::ServerSettingsService;

/// Pure so the orphan rule can be tested without a store. Keeps upstream's
/// key order.
pub fn find_orphaned_project_ids(
    settings: &ServerSettings,
    live_project_ids: &HashSet<ProjectId>,
) -> Vec<ProjectId> {
    settings
        .project_settings_overrides
        .keys()
        .filter(|project_id| !live_project_ids.contains(*project_id))
        .cloned()
        .collect()
}

/// Upstream `sweep`. Failures are logged, never returned: a sweep that does not
/// finish is simply retried on the next boot.
pub async fn sweep(settings_service: &ServerSettingsService, projects: &ProjectStore) {
    if let Err(error) = try_sweep(settings_service, projects).await {
        tracing::warn!(
            ?error,
            "Failed to sweep orphaned project settings overrides."
        );
    }
}

async fn try_sweep(
    settings_service: &ServerSettingsService,
    projects: &ProjectStore,
) -> anyhow::Result<()> {
    let settings = settings_service.get_settings().await?;
    let live_project_ids: HashSet<ProjectId> = projects
        .list_shells(None)
        .await?
        .into_iter()
        .map(|shell| shell.id)
        .collect();

    let orphaned = find_orphaned_project_ids(&settings, &live_project_ids);
    if orphaned.is_empty() {
        return Ok(());
    }

    // `null` clears an override, the same as upstream's `Object.fromEntries(... => [id, null])`.
    let overrides: IndexMap<ProjectId, Option<ProjectSettingsOverrides>> = orphaned
        .into_iter()
        .map(|project_id| (project_id, None))
        .collect();
    let patch = ServerSettingsPatch {
        project_settings_overrides: Some(overrides),
        ..ServerSettingsPatch::default()
    };
    settings_service.update_settings(&patch).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Port of upstream/apps/server/src/provider/ProjectSettingsOverrideSweep.test.ts.

    use pretty_assertions::assert_eq;
    use turin_contracts::{
        ProjectId,
        settings::{ProjectSettingsOverrides, ServerSettings},
    };

    use super::*;

    fn settings_with(overrides: &[ProjectId]) -> ServerSettings {
        let mut settings = ServerSettings::default();
        for project_id in overrides {
            settings
                .project_settings_overrides
                .insert(project_id.clone(), ProjectSettingsOverrides::default());
        }
        settings
    }

    fn project(value: &str) -> ProjectId {
        ProjectId::new(value).expect("valid project id")
    }

    #[test]
    fn reports_a_project_override_whose_project_is_no_longer_live() {
        let live = project("project-live");
        let deleted = project("project-deleted");
        let settings = settings_with(&[live.clone(), deleted.clone()]);

        let orphaned = find_orphaned_project_ids(&settings, &HashSet::from([live]));

        assert_eq!(orphaned, vec![deleted]);
    }

    #[test]
    fn reports_nothing_when_every_overrides_project_is_still_live() {
        let live = project("project-live");
        let settings = settings_with(std::slice::from_ref(&live));

        let orphaned = find_orphaned_project_ids(&settings, &HashSet::from([live]));

        assert_eq!(orphaned, Vec::<ProjectId>::new());
    }

    #[test]
    fn reports_nothing_when_there_are_no_overrides_at_all() {
        let settings = settings_with(&[]);

        let orphaned =
            find_orphaned_project_ids(&settings, &HashSet::from([project("project-live")]));

        assert_eq!(orphaned, Vec::<ProjectId>::new());
    }
}

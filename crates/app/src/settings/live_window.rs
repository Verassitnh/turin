//! Opens the settings window on a live [`SettingsStore`] (shell entry point).

use gpui_kit::{AnyWindowHandle, App, AppContext as _, Entity, Global, WindowOptions};

use crate::state::store::ClientStore;

use super::live_bindings::LiveSettingsServices;
use super::live_source::LiveSettingsSource;
use super::view::{SettingsRoute, SettingsView};
use crate::settings_logic::settings_scope::{SettingsScopeSearch, validate_settings_scope_search};

/// What the page opens on: a deep link's query (`?instanceId=<id>` on the
/// providers page, `?project=<key>` as the settings scope).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsTarget {
    pub provider_instance: Option<String>,
    pub project: Option<String>,
}

impl SettingsTarget {
    pub fn provider(instance_id: Option<&str>) -> Self {
        Self {
            provider_instance: instance_id.map(str::to_owned),
            project: None,
        }
    }

    pub fn project(project_key: &str) -> Self {
        Self {
            provider_instance: None,
            project: Some(project_key.to_owned()),
        }
    }

    /// The scope selection a project target stands for (`?project=<key>`).
    pub fn scope(&self) -> Option<SettingsScopeSearch> {
        let project = self.project.as_deref()?;
        let scope = validate_settings_scope_search(Some(project), None, None);
        scope.project.is_some().then_some(scope)
    }

    fn apply(&self, view: &mut SettingsView, cx: &mut gpui_kit::Context<SettingsView>) {
        if let Some(instance_id) = &self.provider_instance {
            view.set_provider_target(instance_id.clone(), cx);
        }
        if let Some(scope) = self.scope() {
            view.set_scope_target(scope, cx);
        }
    }
}

/// Opens the settings UI backed by [`LiveSettingsSource`]. Returns the store
/// (for follow-up routes or theme editor).
pub fn open(
    services: LiveSettingsServices,
    client: Entity<ClientStore>,
    route: SettingsRoute,
    options: WindowOptions,
    cx: &mut App,
) -> Entity<SettingsStore> {
    open_targeting(
        services,
        client,
        route,
        SettingsTarget::default(),
        options,
        cx,
    )
}

/// The one settings window: later opens focus it and change its page.
struct OpenSettingsWindow {
    handle: AnyWindowHandle,
    view: Entity<SettingsView>,
    store: Entity<SettingsStore>,
}

impl Global for OpenSettingsWindow {}

/// Focuses the settings window if it is still open and points it at `route`.
fn reuse_open_window(
    route: SettingsRoute,
    target: &SettingsTarget,
    cx: &mut App,
) -> Option<Entity<SettingsStore>> {
    let open = cx.try_global::<OpenSettingsWindow>()?;
    let (handle, view, store) = (open.handle, open.view.clone(), open.store.clone());
    let alive = handle
        .update(cx, |_, window, _| window.activate_window())
        .is_ok();
    if !alive {
        cx.remove_global::<OpenSettingsWindow>();
        return None;
    }
    view.update(cx, |view, cx| {
        view.set_route(route, cx);
        target.apply(view, cx);
    });
    cx.activate(true);
    Some(store)
}

/// [`open`], with the page opened on `target`
/// (`/settings/providers?instanceId=<id>`, a project as the settings scope).
pub fn open_targeting(
    services: LiveSettingsServices,
    client: Entity<ClientStore>,
    route: SettingsRoute,
    target: SettingsTarget,
    options: WindowOptions,
    cx: &mut App,
) -> Entity<SettingsStore> {
    if let Some(store) = reuse_open_window(route, &target, cx) {
        return store;
    }
    let store = LiveSettingsSource::start(services, client, cx);
    let store_for_view = store.clone();
    let (handle, view) = gpui_kit::open_window(options, cx, move |window, cx| {
        let view = cx.new(|cx| SettingsView::new(store_for_view, window, cx));
        view.update(cx, |view, cx| {
            view.set_route(route, cx);
            target.apply(view, cx);
        });
        view
    })
    .expect("failed to open settings window");
    cx.set_global(OpenSettingsWindow {
        handle,
        view,
        store: store.clone(),
    });
    cx.activate(true);
    store
}

use super::view_model::SettingsStore;

/// Convenience: open by page name (`"general"`, `"providers"`, …).
pub fn open_named(
    services: LiveSettingsServices,
    client: Entity<ClientStore>,
    page: &str,
    options: WindowOptions,
    cx: &mut App,
) -> Entity<SettingsStore> {
    let route = SettingsRoute::from_name(page).unwrap_or_else(|| {
        tracing::warn!(%page, "unknown settings page, showing General");
        SettingsRoute::from_name("general").expect("general route")
    });
    open(services, client, route, options, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_project_target_selects_that_project_as_the_scope() {
        let target = SettingsTarget::project("local:project-1");
        assert_eq!(
            target.scope(),
            Some(SettingsScopeSearch {
                project: Some("local:project-1".to_owned()),
                machine: None,
                checkout: None,
            })
        );
    }

    #[test]
    fn a_blank_project_or_a_provider_target_leaves_the_scope_alone() {
        assert_eq!(SettingsTarget::project("  ").scope(), None);
        assert_eq!(SettingsTarget::provider(Some("codex")).scope(), None);
        assert_eq!(SettingsTarget::default().scope(), None);
        assert_eq!(
            SettingsTarget::provider(Some("codex"))
                .provider_instance
                .as_deref(),
            Some("codex")
        );
    }
}

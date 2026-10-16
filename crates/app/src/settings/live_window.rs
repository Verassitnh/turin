//! Opens the settings window on a live [`SettingsStore`] (shell entry point).

use gpui_kit::{App, AppContext as _, Entity, WindowOptions};

use crate::state::store::ClientStore;

use super::live_bindings::LiveSettingsServices;
use super::live_source::LiveSettingsSource;
use super::view::{SettingsRoute, SettingsView};

/// Opens the settings UI backed by [`LiveSettingsSource`]. Returns the store
/// (for follow-up routes or theme editor).
pub fn open(
    services: LiveSettingsServices,
    client: Entity<ClientStore>,
    route: SettingsRoute,
    options: WindowOptions,
    cx: &mut App,
) -> Entity<SettingsStore> {
    let store = LiveSettingsSource::start(services, client, cx);
    let store_for_view = store.clone();
    let (_handle, _view) = gpui_kit::open_window(options, cx, move |window, cx| {
        let view = cx.new(|cx| SettingsView::new(store_for_view, window, cx));
        view.update(cx, |view, cx| view.set_route(route, cx));
        view
    })
    .expect("failed to open settings window");
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

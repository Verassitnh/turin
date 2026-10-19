//! Port of upstream/apps/desktop/src/window/DesktopApplicationMenu.ts (menu
//! actions, accelerators, and GPUI native menus with OS roles where supported).

use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::branding::APP_BASE_NAME;
actions!(
    desktop_menu,
    [
        OpenSettings,
        ToggleFullScreen,
        QuitApplication,
        CloseWindow,
        HideApplication,
        HideOtherApplications,
        ShowAllApplications,
    ]
);

/// Registers upstream menu accelerators app-wide (no key context), so they
/// work in the settings window too. The shell handles them when focused; the
/// app-level handlers in `main.rs` otherwise.
pub fn bind_menu_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-comma", OpenSettings, None),
        KeyBinding::new("ctrl-cmd-f", ToggleFullScreen, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
        KeyBinding::new("cmd-h", HideApplication, None),
        KeyBinding::new("alt-cmd-h", HideOtherApplications, None),
        // The shell binds Cmd-Q to the hold-to-quit flow; this is the fallback
        // with no shell focused (settings window, or no window at all).
        KeyBinding::new("cmd-q", QuitApplication, None),
    ]);
}

/// Builds the native menu bar (`Menu.buildFromTemplate` equivalent).
pub fn install_native_menus(cx: &mut App) {
    let app_name: gpui_kit::SharedString = APP_BASE_NAME.into();
    let mut menus: Vec<Menu> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        menus.push(Menu::new(app_name.clone()).items([
            MenuItem::action(format!("About {APP_BASE_NAME}"), gpui_kit::NoAction).disabled(true),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action(format!("Hide {APP_BASE_NAME}"), HideApplication),
            MenuItem::action("Hide Others", HideOtherApplications),
            MenuItem::action("Show All", ShowAllApplications),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_BASE_NAME}"), QuitApplication),
        ]));
    }

    menus.push(Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]));
    menus.push(Menu::new("Edit").items([
        MenuItem::os_action("Undo", gpui_kit::NoAction, OsAction::Undo),
        MenuItem::os_action("Redo", gpui_kit::NoAction, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", gpui_kit::NoAction, OsAction::Cut),
        MenuItem::os_action("Copy", gpui_kit::NoAction, OsAction::Copy),
        MenuItem::os_action("Paste", gpui_kit::NoAction, OsAction::Paste),
        MenuItem::os_action("Select All", gpui_kit::NoAction, OsAction::SelectAll),
        MenuItem::separator(),
        MenuItem::submenu(Menu::new("Speech").items([
            MenuItem::action("Start Speaking", gpui_kit::NoAction).disabled(true),
            MenuItem::action("Stop Speaking", gpui_kit::NoAction).disabled(true),
        ])),
    ]));
    // Reload, Force Reload, Developer Tools, zoom and Paste as Text have no
    // native equivalent (no webview; GPUI pastes plain text). See PORTING.md.
    menus.push(Menu::new("View").items([MenuItem::action("Toggle Full Screen", ToggleFullScreen)]));
    menus.push(Menu::new("Window").items([MenuItem::separator()]));
    menus.push(
        Menu::new("Help")
            .items([MenuItem::action("Documentation", gpui_kit::NoAction).disabled(true)]),
    );

    cx.set_menus(menus);
}

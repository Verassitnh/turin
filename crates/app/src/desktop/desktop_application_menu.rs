//! Port of upstream/apps/desktop/src/window/DesktopApplicationMenu.ts (menu
//! actions, accelerators, and GPUI native menus with OS roles where supported).

use gpui_kit::{App, KeyBinding, Menu, MenuItem, OsAction, SystemMenuType, actions};

use crate::branding::APP_BASE_NAME;
actions!(
    desktop_menu,
    [
        OpenSettings,
        PasteAsText,
        Reload,
        ForceReload,
        ToggleDevTools,
        ZoomIn,
        ZoomOut,
        ZoomReset,
        QuitApplication,
    ]
);

/// Registers upstream menu accelerators on the app shell context.
pub fn bind_menu_keys(cx: &mut App, context: &'static str) {
    cx.bind_keys([
        KeyBinding::new("cmd-comma", OpenSettings, Some(context)),
        KeyBinding::new("cmd-shift-v", PasteAsText, Some(context)),
        KeyBinding::new("cmd-r", Reload, Some(context)),
        KeyBinding::new("cmd-shift-r", ForceReload, Some(context)),
        KeyBinding::new("cmd-alt-i", ToggleDevTools, Some(context)),
        KeyBinding::new("cmd-0", ZoomReset, Some(context)),
        KeyBinding::new("cmd-=", ZoomIn, Some(context)),
        KeyBinding::new("cmd-plus", ZoomIn, Some(context)),
        KeyBinding::new("cmd--", ZoomOut, Some(context)),
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
            MenuItem::action(format!("Hide {APP_BASE_NAME}"), gpui_kit::NoAction).disabled(true),
            MenuItem::action("Hide Others", gpui_kit::NoAction).disabled(true),
            MenuItem::action("Show All", gpui_kit::NoAction).disabled(true),
            MenuItem::separator(),
            MenuItem::action(format!("Quit {APP_BASE_NAME}"), QuitApplication),
        ]));
    }

    menus.push(Menu::new("File").items([MenuItem::action("Close Window", gpui_kit::NoAction)]));
    menus.push(Menu::new("Edit").items([
        MenuItem::os_action("Undo", gpui_kit::NoAction, OsAction::Undo),
        MenuItem::os_action("Redo", gpui_kit::NoAction, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", gpui_kit::NoAction, OsAction::Cut),
        MenuItem::os_action("Copy", gpui_kit::NoAction, OsAction::Copy),
        MenuItem::os_action("Paste", gpui_kit::NoAction, OsAction::Paste),
        MenuItem::action("Paste as Text", PasteAsText),
        MenuItem::os_action("Select All", gpui_kit::NoAction, OsAction::SelectAll),
        MenuItem::separator(),
        MenuItem::submenu(Menu::new("Speech").items([
            MenuItem::action("Start Speaking", gpui_kit::NoAction).disabled(true),
            MenuItem::action("Stop Speaking", gpui_kit::NoAction).disabled(true),
        ])),
    ]));
    menus.push(Menu::new("View").items([
        MenuItem::action("Reload", Reload),
        MenuItem::action("Force Reload", ForceReload),
        MenuItem::action("Toggle Developer Tools", ToggleDevTools),
        MenuItem::separator(),
        MenuItem::action("Actual Size", ZoomReset),
        MenuItem::action("Zoom In", ZoomIn),
        MenuItem::action("Zoom Out", ZoomOut),
        MenuItem::separator(),
        MenuItem::action("Toggle Full Screen", gpui_kit::NoAction).disabled(true),
    ]));
    menus.push(Menu::new("Window").items([MenuItem::separator()]));
    menus.push(
        Menu::new("Help")
            .items([MenuItem::action("Documentation", gpui_kit::NoAction).disabled(true)]),
    );

    cx.set_menus(menus);
}

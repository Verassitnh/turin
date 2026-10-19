//! Where an app-level window action goes. Menu items and the key bindings
//! with no context reach the app when the shell does not handle them: the
//! settings window is focused, or no window is open at all.

/// What Cmd+Q and Cmd+W do for the window that was active when they fired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WindowActionRoute {
    /// The main window exists: its shell runs the action (hold-to-quit, or
    /// writing the window bounds before the window closes).
    MainShell,
    /// A window other than the main one is active (settings): it closes alone.
    ActiveWindow,
    /// No window is open, so there is no shell to ask.
    NoWindow,
}

/// `main` is the main window, `active` the focused one; both `None` when absent.
pub fn route_window_action<T: PartialEq>(main: Option<&T>, active: Option<&T>) -> WindowActionRoute {
    match (main, active) {
        (Some(main), Some(active)) if main == active => WindowActionRoute::MainShell,
        (Some(_), None) => WindowActionRoute::MainShell,
        (_, Some(_)) => WindowActionRoute::ActiveWindow,
        (None, None) => WindowActionRoute::NoWindow,
    }
}

/// Cmd+Q: any open main window asks the shell, which owns the hold-to-quit
/// flow and its hint. Without one, nothing is on screen to confirm, so the app quits.
pub fn quit_goes_through_shell(route: WindowActionRoute, main_window_open: bool) -> bool {
    main_window_open && route != WindowActionRoute::NoWindow
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_main_window_is_recognized_when_it_is_active() {
        assert_eq!(
            route_window_action(Some(&1), Some(&1)),
            WindowActionRoute::MainShell
        );
    }

    #[test]
    fn the_settings_window_closes_alone() {
        assert_eq!(
            route_window_action(Some(&1), Some(&2)),
            WindowActionRoute::ActiveWindow
        );
        assert_eq!(
            route_window_action(None, Some(&2)),
            WindowActionRoute::ActiveWindow
        );
    }

    #[test]
    fn no_active_window_still_finds_the_main_window() {
        assert_eq!(
            route_window_action(Some(&1), None::<&i32>),
            WindowActionRoute::MainShell
        );
        assert_eq!(
            route_window_action(None::<&i32>, None::<&i32>),
            WindowActionRoute::NoWindow
        );
    }

    #[test]
    fn quit_from_settings_asks_the_main_shell_and_quit_with_no_window_just_quits() {
        let from_settings = route_window_action(Some(&1), Some(&2));
        assert!(quit_goes_through_shell(from_settings, true));
        let nothing_open = route_window_action(None::<&i32>, None::<&i32>);
        assert!(!quit_goes_through_shell(nothing_open, false));
        // A settings window with the main window closed has no shell either.
        let settings_only = route_window_action(None, Some(&2));
        assert!(!quit_goes_through_shell(settings_only, false));
    }
}

//! The application menu's About item: the standard macOS About panel, with
//! the name and version the Electron `about` role shows
//! (upstream `DesktopApplicationMenu.ts`, `{ role: "about" }`).

use crate::branding::APP_BASE_NAME;

/// What the panel shows. A bundled app reads the same two values from its
/// Info.plist; an unbundled run (`cargo run`) has no plist, so they are passed
/// explicitly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AboutInfo {
    pub name: &'static str,
    pub version: &'static str,
}

pub fn about_info() -> AboutInfo {
    AboutInfo {
        name: APP_BASE_NAME,
        version: env!("CARGO_PKG_VERSION"),
    }
}

/// `orderFrontStandardAboutPanel` with the app name and version.
#[cfg(target_os = "macos")]
pub fn show_about_panel() {
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::NSApplication;
    use objc2_foundation::{NSDictionary, NSString};

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let info = about_info();
    let name = NSString::from_str("ApplicationName");
    let version = NSString::from_str("ApplicationVersion");
    let values: [Retained<AnyObject>; 2] = [
        // SAFETY: an `NSString` is an object; the dictionary only stores it.
        unsafe { Retained::cast_unchecked(NSString::from_str(info.name)) },
        unsafe { Retained::cast_unchecked(NSString::from_str(info.version)) },
    ];
    let options = NSDictionary::from_retained_objects(&[&*name, &*version], &values);
    // SAFETY: the keys are `NSAboutPanelOptionKey` strings and the values strings.
    unsafe {
        NSApplication::sharedApplication(mtm).orderFrontStandardAboutPanelWithOptions(&options)
    };
}

#[cfg(not(target_os = "macos"))]
pub fn show_about_panel() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_names_the_app_and_its_crate_version() {
        let info = about_info();
        assert_eq!(info.name, "Turin");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert!(!info.version.is_empty());
    }
}

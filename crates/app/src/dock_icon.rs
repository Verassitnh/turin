//! Port of T24b dock_icon.rs / unbundled macOS app icon.

#[cfg(target_os = "macos")]
mod inner {
    use objc2::{AnyThread as _, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{NSBundle, NSData};

    use crate::assets::APP_ICON_PNG;

    pub fn set_unbundled_app_icon() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        if NSBundle::mainBundle().bundleIdentifier().is_some() {
            return;
        }
        let data = NSData::with_bytes(APP_ICON_PNG);
        let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) else {
            return;
        };
        // SAFETY: main thread after NSApplication exists.
        unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
    }
}

#[cfg(target_os = "macos")]
pub use inner::set_unbundled_app_icon;

#[cfg(not(target_os = "macos"))]
pub fn set_unbundled_app_icon() {}

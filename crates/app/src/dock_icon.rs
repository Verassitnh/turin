//! Dock icon for an unbundled run (`cargo run -p turin`). A bundle from
//! `scripts/bundle-macos.sh` gets its icon from `Turin.icns` via Info.plist, so
//! this only runs when there is no bundle identifier.

use objc2::{AnyThread as _, MainThreadMarker};
use objc2_app_kit::{NSApplication, NSImage};
use objc2_foundation::{NSBundle, NSData};

use crate::assets::APP_ICON_PNG;

/// Call from the main thread after the app has started.
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
    // SAFETY: called on the main thread (`mtm`), after the application object exists.
    unsafe { NSApplication::sharedApplication(mtm).setApplicationIconImage(Some(&image)) };
}

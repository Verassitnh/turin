//! Port of `playNotificationSound` in upstream/apps/web/src/threadNotifications.ts.
//!
//! The two clips are upstream's `notification-completion.mp3` and
//! `notification-input.mp3`. They play through `NSSound` on macOS; other
//! platforms have no player here and stay silent.

/// Which clip: a thread finished, or a thread needs the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NotificationSound {
    Completion,
    Input,
}

impl NotificationSound {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Completion => include_bytes!("../../assets/sounds/notification-completion.mp3"),
            Self::Input => include_bytes!("../../assets/sounds/notification-input.mp3"),
        }
    }
}

/// Plays `sound` once, without waiting for it to end.
pub fn play_notification_sound(sound: NotificationSound) {
    #[cfg(target_os = "macos")]
    play_with_ns_sound(sound.bytes());
    #[cfg(not(target_os = "macos"))]
    let _ = sound.bytes();
}

#[cfg(target_os = "macos")]
fn play_with_ns_sound(bytes: &[u8]) {
    use std::cell::RefCell;

    use objc2::AnyThread as _;
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::NSSound;
    use objc2_foundation::NSData;

    thread_local! {
        // `NSSound` stops when it is released, so a clip is kept until it ends.
        static PLAYING: RefCell<Vec<Retained<NSSound>>> = const { RefCell::new(Vec::new()) };
    }

    if MainThreadMarker::new().is_none() {
        return;
    }
    let data = NSData::with_bytes(bytes);
    let Some(sound) = NSSound::initWithData(NSSound::alloc(), &data) else {
        return;
    };
    if sound.play() {
        PLAYING.with(|playing| {
            let mut playing = playing.borrow_mut();
            playing.retain(|clip| clip.isPlaying());
            playing.push(sound);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_clips_are_embedded() {
        assert!(NotificationSound::Completion.bytes().len() > 1000);
        assert!(NotificationSound::Input.bytes().len() > 1000);
    }
}

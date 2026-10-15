//! Port of upstream/apps/web/src/components/chat/composerMentionDrag.ts
//!
//! Upstream tags HTML drags with a MIME type; GPUI drags carry a typed
//! payload, so [`ComposerMentionDrag`] stands in for the dataTransfer entry
//! and the handlers keep the drop rules (what is allowed to touch the
//! composer, trailing space, rejected inserts).

use crate::composer::file_link::serialize_composer_file_link;

/// The payload dragged out of a workspace path row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerMentionDrag {
    /// A serialized mention, e.g. `[index.md](docs/index.md)`.
    pub mention: String,
}

/// `composerMentionFromTreePath`.
pub fn composer_mention_from_tree_path(tree_path: &str) -> Option<String> {
    let relative = tree_path.trim_end_matches('/');
    if relative.is_empty() {
        return None;
    }
    Some(serialize_composer_file_link(relative))
}

/// What a mention drop is allowed to do to the composer. Deliberately narrow:
/// there is no way to focus the editor from here.
pub trait ComposerMentionDropHost {
    fn insert_mention_at_end(&mut self, text: &str) -> bool;
    fn set_drag_active(&mut self, active: bool);
    fn on_insert_rejected(&mut self);
}

pub fn on_drag_enter(host: &mut dyn ComposerMentionDropHost) {
    host.set_drag_active(true);
}

pub fn on_drag_over(host: &mut dyn ComposerMentionDropHost) {
    host.set_drag_active(true);
}

pub fn on_drop(host: &mut dyn ComposerMentionDropHost, drag: &ComposerMentionDrag) {
    host.set_drag_active(false);
    if drag.mention.is_empty() {
        return;
    }
    if !host.insert_mention_at_end(&format!("{} ", drag.mention)) {
        host.on_insert_rejected();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Host {
        log: Vec<String>,
        accept: bool,
    }

    impl ComposerMentionDropHost for Host {
        fn insert_mention_at_end(&mut self, text: &str) -> bool {
            self.log.push(format!("insert:{text}"));
            self.accept
        }
        fn set_drag_active(&mut self, active: bool) {
            self.log.push(format!("active:{active}"));
        }
        fn on_insert_rejected(&mut self) {
            self.log.push("rejected".into());
        }
    }

    fn drag(mention: &str) -> ComposerMentionDrag {
        ComposerMentionDrag {
            mention: mention.into(),
        }
    }

    #[test]
    fn serializes_a_file_path_into_a_mention() {
        assert_eq!(
            composer_mention_from_tree_path("docs/index.md").as_deref(),
            Some("[index.md](docs/index.md)")
        );
    }

    #[test]
    fn strips_the_trailing_slash_directory_rows_carry() {
        assert_eq!(
            composer_mention_from_tree_path("docs/architecture/").as_deref(),
            Some("[architecture](docs/architecture)")
        );
    }

    #[test]
    fn rejects_drags_that_carry_no_path() {
        assert_eq!(composer_mention_from_tree_path(""), None);
        assert_eq!(composer_mention_from_tree_path("/"), None);
    }

    #[test]
    fn inserts_the_mention_with_its_trailing_space_and_clears_the_highlight() {
        let mut host = Host {
            log: vec![],
            accept: true,
        };
        on_drag_enter(&mut host);
        on_drop(&mut host, &drag("[index.md](docs/index.md)"));
        assert_eq!(
            host.log,
            [
                "active:true",
                "active:false",
                "insert:[index.md](docs/index.md) "
            ]
        );
    }

    #[test]
    fn reports_a_rejected_insert_instead_of_failing_silently() {
        let mut host = Host {
            log: vec![],
            accept: false,
        };
        on_drop(&mut host, &drag("[a](a)"));
        assert!(host.log.contains(&"rejected".to_owned()));
    }

    #[test]
    fn ignores_a_drop_whose_payload_is_empty() {
        let mut host = Host {
            log: vec![],
            accept: true,
        };
        on_drop(&mut host, &drag(""));
        assert_eq!(host.log, ["active:false"]);
    }
}

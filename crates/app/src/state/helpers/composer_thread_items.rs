//! Port of upstream/packages/client-runtime/src/composerThreadItems.ts

use turin_contracts::environment::ScopedThreadRef;
use turin_contracts::{EnvironmentId, ThreadId, js_trim};

const COMPOSER_THREAD_RESULT_LIMIT: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerThreadCandidate {
    pub environment_id: EnvironmentId,
    pub id: ThreadId,
    pub title: String,
    pub updated_at: String,
    pub archived_at: Option<String>,
}

/// `type: "thread"`, the only kind the `@` picker offers from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerThreadItemType {
    Thread,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposerThreadItem {
    pub id: String,
    pub r#type: ComposerThreadItemType,
    pub thread: ScopedThreadRef,
    pub label: String,
    pub description: String,
}

pub struct MatchComposerThreadItemsInput<'a> {
    pub shells: &'a [ComposerThreadCandidate],
    pub environment_id: &'a EnvironmentId,
    pub exclude_thread_id: Option<&'a ThreadId>,
    pub query: &'a str,
}

/// Threads the `@` picker offers next to file paths. The agent can only read threads on its
/// own server, so candidates stay within the composer's environment. A query is required:
/// bare `@` stays a file picker.
pub fn match_composer_thread_items(
    input: &MatchComposerThreadItemsInput<'_>,
) -> Vec<ComposerThreadItem> {
    let query = js_trim(input.query).to_lowercase();
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches: Vec<&ComposerThreadCandidate> = input
        .shells
        .iter()
        .filter(|shell| {
            shell.environment_id == *input.environment_id
                && input.exclude_thread_id != Some(&shell.id)
                && shell.archived_at.is_none()
                && shell.title.to_lowercase().contains(&query)
        })
        .collect();
    // `right.updatedAt.localeCompare(left.updatedAt)`: ISO timestamps, newest first. The sort is
    // stable, like JS's.
    matches.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    matches
        .into_iter()
        .take(COMPOSER_THREAD_RESULT_LIMIT)
        .map(|shell| ComposerThreadItem {
            id: format!("thread:{}:{}", shell.environment_id, shell.id),
            r#type: ComposerThreadItemType::Thread,
            thread: ScopedThreadRef {
                environment_id: shell.environment_id.clone(),
                thread_id: shell.id.clone(),
            },
            label: shell.title.clone(),
            description: "Thread".to_owned(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> EnvironmentId {
        EnvironmentId::new("env-1").unwrap()
    }
    fn other_env() -> EnvironmentId {
        EnvironmentId::new("env-2").unwrap()
    }
    fn shell(id: &str, title: &str) -> ComposerThreadCandidate {
        ComposerThreadCandidate {
            environment_id: env(),
            id: ThreadId::new(id).unwrap(),
            title: title.to_owned(),
            updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
            archived_at: None,
        }
    }

    #[test]
    fn offers_nothing_for_a_bare_at_so_the_picker_stays_a_file_picker() {
        let shells = [shell("t1", "Fix login")];
        let environment_id = env();
        assert_eq!(
            match_composer_thread_items(&MatchComposerThreadItemsInput {
                shells: &shells,
                environment_id: &environment_id,
                exclude_thread_id: None,
                query: "  ",
            }),
            Vec::new()
        );
    }

    #[test]
    fn matches_titles_within_the_environment_newest_first_skipping_self_and_archived() {
        let shells = [
            ComposerThreadCandidate {
                updated_at: "2026-01-01T00:00:00.000Z".to_owned(),
                ..shell("old", "Login flow")
            },
            ComposerThreadCandidate {
                updated_at: "2026-02-01T00:00:00.000Z".to_owned(),
                ..shell("new", "Login redesign")
            },
            shell("self", "Login self"),
            ComposerThreadCandidate {
                archived_at: Some("2026-01-02T00:00:00.000Z".to_owned()),
                ..shell("gone", "Login archived")
            },
            ComposerThreadCandidate {
                environment_id: other_env(),
                ..shell("foreign", "Login elsewhere")
            },
            shell("nope", "Unrelated"),
        ];
        let environment_id = env();
        let self_id = ThreadId::new("self").unwrap();
        let items = match_composer_thread_items(&MatchComposerThreadItemsInput {
            shells: &shells,
            environment_id: &environment_id,
            exclude_thread_id: Some(&self_id),
            query: "LOGIN",
        });
        let thread_ids: Vec<&str> = items
            .iter()
            .map(|item| item.thread.thread_id.as_str())
            .collect();
        assert_eq!(thread_ids, ["new", "old"]);
        assert_eq!(items[0].r#type, ComposerThreadItemType::Thread);
        assert_eq!(items[0].label, "Login redesign");
    }
}

//! Port of upstream/apps/web/src/components/pullRequest/pullRequestReactions.logic.ts.
use std::collections::HashMap;
use turin_contracts::PositiveInt;
use turin_contracts::pull_request::{PullRequestReaction, PullRequestReactionContent as Content};
pub const PULL_REQUEST_REACTION_ORDER: [Content; 8] = [
    Content::ThumbsUp,
    Content::ThumbsDown,
    Content::Laugh,
    Content::Hooray,
    Content::Confused,
    Content::Heart,
    Content::Rocket,
    Content::Eyes,
];
pub fn pull_request_reaction_emoji(content: Content) -> &'static str {
    match content {
        Content::ThumbsUp => "👍",
        Content::ThumbsDown => "👎",
        Content::Laugh => "😄",
        Content::Hooray => "🎉",
        Content::Confused => "😕",
        Content::Heart => "❤️",
        Content::Rocket => "🚀",
        Content::Eyes => "👀",
    }
}
pub fn pull_request_reaction_name(content: Content) -> &'static str {
    match content {
        Content::ThumbsUp => "thumbs up",
        Content::ThumbsDown => "thumbs down",
        Content::Laugh => "laugh",
        Content::Hooray => "hooray",
        Content::Confused => "confused",
        Content::Heart => "heart",
        Content::Rocket => "rocket",
        Content::Eyes => "eyes",
    }
}
const NAMED_ACTOR_LIMIT: usize = 3;
fn join_names(parts: &[String]) -> String {
    match parts.len() {
        0 => String::new(),
        1 => parts[0].clone(),
        2 => format!("{} and {}", parts[0], parts[1]),
        _ => format!(
            "{}, and {}",
            parts[..parts.len() - 1].join(", "),
            parts.last().unwrap()
        ),
    }
}
fn count_remainder(count: usize, named: bool) -> String {
    if named {
        format!("{count} {}", if count == 1 { "other" } else { "others" })
    } else {
        format!("{count} {}", if count == 1 { "person" } else { "people" })
    }
}
/// Viewer reads as You only if the host's count leaves room for them.
pub fn pull_request_reaction_tooltip(reaction: &PullRequestReaction) -> String {
    let count = reaction.count.get() as usize;
    let mut names: Vec<String> = reaction.actors.iter().map(ToString::to_string).collect();
    if reaction.viewer_has_reacted && names.len() < count {
        names.insert(0, "You".to_owned());
    }
    let shown: Vec<String> = names
        .into_iter()
        .take(NAMED_ACTOR_LIMIT.min(count))
        .collect();
    let others = count.saturating_sub(shown.len());
    let mut parts = shown;
    if others > 0 {
        parts.push(count_remainder(others, !parts.is_empty()));
    }
    format!(
        "{} reacted with {} emoji",
        join_names(&parts),
        pull_request_reaction_name(reaction.content)
    )
}
/// Apply in-flight presses before the host's slower detail read catches up.
pub fn apply_pending_pull_request_reactions(
    reactions: &[PullRequestReaction],
    pending: &HashMap<Content, bool>,
) -> Vec<PullRequestReaction> {
    if pending.is_empty() {
        return reactions.to_vec();
    }
    let mut by_content: HashMap<Content, PullRequestReaction> = reactions
        .iter()
        .map(|reaction| (reaction.content, reaction.clone()))
        .collect();
    for (&content, &reacted) in pending {
        match by_content.get(&content).cloned() {
            None if reacted => {
                by_content.insert(
                    content,
                    PullRequestReaction {
                        content,
                        count: PositiveInt::new(1).unwrap(),
                        actors: vec![],
                        viewer_has_reacted: true,
                    },
                );
            }
            None => {}
            Some(current) if current.viewer_has_reacted == reacted => {}
            Some(mut current) => {
                let count = current.count.get() as i64 + if reacted { 1 } else { -1 };
                if count <= 0 {
                    by_content.remove(&content);
                } else {
                    current.count = PositiveInt::new(count as u64).unwrap();
                    current.viewer_has_reacted = reacted;
                    by_content.insert(content, current);
                }
            }
        }
    }
    PULL_REQUEST_REACTION_ORDER
        .iter()
        .filter_map(|content| by_content.remove(content))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use turin_contracts::TrimmedNonEmptyString;
    fn reaction(
        content: Content,
        count: u64,
        actors: &[&str],
        viewer: bool,
    ) -> PullRequestReaction {
        PullRequestReaction {
            content,
            count: PositiveInt::new(count).unwrap(),
            actors: actors
                .iter()
                .map(|s| TrimmedNonEmptyString::new(*s).unwrap())
                .collect(),
            viewer_has_reacted: viewer,
        }
    }
    fn heart(count: u64, actors: &[&str], viewer: bool) -> PullRequestReaction {
        reaction(Content::Heart, count, actors, viewer)
    }
    #[test]
    fn picker_order_names_and_emoji() {
        assert_eq!(
            PULL_REQUEST_REACTION_ORDER,
            [
                Content::ThumbsUp,
                Content::ThumbsDown,
                Content::Laugh,
                Content::Hooray,
                Content::Confused,
                Content::Heart,
                Content::Rocket,
                Content::Eyes
            ]
        );
        assert_eq!(
            PULL_REQUEST_REACTION_ORDER.map(pull_request_reaction_emoji),
            ["👍", "👎", "😄", "🎉", "😕", "❤️", "🚀", "👀"]
        );
        assert_eq!(pull_request_reaction_name(Content::ThumbsUp), "thumbs up");
        assert_eq!(pull_request_reaction_name(Content::Eyes), "eyes");
    }
    #[test]
    fn one_two_three_names() {
        assert_eq!(
            pull_request_reaction_tooltip(&reaction(Content::ThumbsUp, 1, &["Bil0000"], false)),
            "Bil0000 reacted with thumbs up emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&heart(2, &["Bil0000", "octocat"], false)),
            "Bil0000 and octocat reacted with heart emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&reaction(
                Content::Eyes,
                3,
                &["Bil0000", "octocat"],
                true
            )),
            "You, Bil0000, and octocat reacted with eyes emoji"
        );
    }
    #[test]
    fn counts_past_third_and_unnamed() {
        assert_eq!(
            pull_request_reaction_tooltip(&reaction(
                Content::Rocket,
                15,
                &["a", "b", "c", "d"],
                true
            )),
            "You, a, b, and 12 others reacted with rocket emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&heart(2, &["octocat"], false)),
            "octocat and 1 other reacted with heart emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&heart(4, &[], false)),
            "4 people reacted with heart emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&heart(1, &[], false)),
            "1 person reacted with heart emoji"
        );
    }
    #[test]
    fn viewer_first_when_room() {
        assert_eq!(
            pull_request_reaction_tooltip(&heart(3, &["Bil0000", "octocat"], true)),
            "You, Bil0000, and octocat reacted with heart emoji"
        );
    }
    #[test]
    fn no_you_without_viewer_reaction() {
        assert_eq!(
            pull_request_reaction_tooltip(&heart(2, &["Bil0000", "octocat"], false)),
            "Bil0000 and octocat reacted with heart emoji"
        );
    }
    #[test]
    fn no_you_when_host_leaves_no_room() {
        assert_eq!(
            pull_request_reaction_tooltip(&heart(2, &["Bil0000", "octocat"], true)),
            "Bil0000 and octocat reacted with heart emoji"
        );
        assert_eq!(
            pull_request_reaction_tooltip(&heart(4, &["Bil0000", "octocat", "hubot", "zzz"], true)),
            "Bil0000, octocat, hubot, and 1 other reacted with heart emoji"
        );
    }
    #[test]
    fn you_when_room() {
        assert_eq!(
            pull_request_reaction_tooltip(&heart(2, &["octocat"], true)),
            "You and octocat reacted with heart emoji"
        );
    }
    #[test]
    fn optimistic_react_says_you() {
        let applied = apply_pending_pull_request_reactions(
            &[heart(2, &["a", "b"], false)],
            &HashMap::from([(Content::Heart, true)]),
        );
        assert_eq!(
            pull_request_reaction_tooltip(&applied[0]),
            "You, a, and b reacted with heart emoji"
        );
    }
    #[test]
    fn optimistic_unreact_drops_you() {
        let applied = apply_pending_pull_request_reactions(
            &[heart(2, &["octocat"], true)],
            &HashMap::from([(Content::Heart, false)]),
        );
        assert_eq!(
            pull_request_reaction_tooltip(&applied[0]),
            "octocat reacted with heart emoji"
        );
    }
    #[test]
    fn no_pending_preserves_host_values() {
        let original = vec![heart(1, &["octocat"], false)];
        assert_eq!(
            apply_pending_pull_request_reactions(&original, &HashMap::new()),
            original
        );
    }
    #[test]
    fn adds_new_reaction_in_picker_order() {
        let applied = apply_pending_pull_request_reactions(
            &[reaction(Content::Rocket, 2, &["a", "b"], false)],
            &HashMap::from([(Content::ThumbsUp, true)]),
        );
        assert_eq!(applied[0].content, Content::ThumbsUp);
        assert_eq!(applied[1].content, Content::Rocket);
    }
    #[test]
    fn joins_leaves_and_drops() {
        let joined = apply_pending_pull_request_reactions(
            &[heart(2, &["a", "b"], false)],
            &HashMap::from([(Content::Heart, true)]),
        );
        assert_eq!(joined[0].count.get(), 3);
        assert!(joined[0].viewer_has_reacted);
        let left = apply_pending_pull_request_reactions(
            &[heart(2, &["a", "b"], true)],
            &HashMap::from([(Content::Heart, false)]),
        );
        assert_eq!(left[0].count.get(), 1);
        assert!(!left[0].viewer_has_reacted);
        assert!(
            apply_pending_pull_request_reactions(
                &[heart(1, &[], true)],
                &HashMap::from([(Content::Heart, false)])
            )
            .is_empty()
        );
    }
    #[test]
    fn ignores_caught_up_pending() {
        let original = vec![heart(3, &[], true)];
        assert_eq!(
            apply_pending_pull_request_reactions(
                &original,
                &HashMap::from([(Content::Heart, true)])
            ),
            original
        );
        assert!(
            apply_pending_pull_request_reactions(&[], &HashMap::from([(Content::Heart, false)]))
                .is_empty()
        );
    }
}

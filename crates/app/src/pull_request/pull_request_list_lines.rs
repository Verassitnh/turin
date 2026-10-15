//! Port of upstream/apps/web/src/components/pullRequest/pullRequestListLines.ts

use crate::state::contracts::ThreadPullRequestLink;
use crate::state::support::thread_pull_requests::{ChainKind, ThreadPullRequestChain};
use crate::state::time::parse_ms;

/// One line of a thread's pull-request list: a link plus how deep it sits in its stack.
#[derive(Clone, Debug, PartialEq)]
pub struct PullRequestListLine<'a> {
    pub link: &'a ThreadPullRequestLink,
    /// 0 for a pull request on the base branch; each layer above steps in by one.
    pub depth: usize,
    /// Which chain the line belongs to.
    pub chain_key: String,
    /// Set on the bottom layer of a multi-layer stack.
    pub stack: Option<PullRequestLineStack>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PullRequestLineStack {
    pub kind: ChainKind,
    pub size: usize,
}

fn activity_at(link: &ThreadPullRequestLink) -> i64 {
    parse_ms(
        link.snapshot
            .as_ref()
            .and_then(|s| s.updated_at.as_deref())
            .unwrap_or(link.linked_at.as_str()),
    )
    .unwrap_or(0)
}

fn chain_key_of(chain: &ThreadPullRequestChain<'_>) -> String {
    let bottom = chain.layers[0];
    format!("{}/{}#{}", bottom.host, bottom.repository, bottom.number)
}

/// Newest first. A stack sorts by its most recent layer, then reads bottom to top.
pub fn pull_request_list_lines<'a>(
    chains: &[ThreadPullRequestChain<'a>],
) -> Vec<PullRequestListLine<'a>> {
    let mut ordered: Vec<_> = chains.iter().collect();
    ordered.sort_by(|left, right| {
        right
            .layers
            .iter()
            .map(|link| activity_at(link))
            .max()
            .unwrap_or(i64::MIN)
            .cmp(
                &left
                    .layers
                    .iter()
                    .map(|link| activity_at(link))
                    .max()
                    .unwrap_or(i64::MIN),
            )
    });
    ordered
        .into_iter()
        .flat_map(|chain| {
            let chain_key = chain_key_of(chain);
            chain
                .layers
                .iter()
                .enumerate()
                .map(move |(depth, link)| PullRequestListLine {
                    link,
                    depth,
                    chain_key: chain_key.clone(),
                    stack: if depth == 0 && chain.layers.len() > 1 {
                        Some(PullRequestLineStack {
                            kind: chain.kind,
                            size: chain.layers.len(),
                        })
                    } else {
                        None
                    },
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::support::thread_pull_requests::resolve_thread_pull_request_chains;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn link(
        number: u64,
        head: &str,
        base: &str,
        updated_at: &str,
        stack: Option<serde_json::Value>,
    ) -> ThreadPullRequestLink {
        serde_json::from_value(json!({"host":"github.com","repository":"acme/web","number":number,"url":format!("https://github.com/acme/web/pull/{number}"),"source":"manual","linkedAt":"2026-01-01T00:00:00.000Z","snapshot":{"state":"open","title":format!("PR {number}"),"headBranch":head,"baseBranch":base,"isDraft":false,"updatedAt":updated_at,"syncedAt":updated_at},"stack":stack})).unwrap()
    }
    #[test]
    fn newest_first_stack_together_under_base() {
        let links = vec![
            link(1, "a", "main", "2026-01-01T10:00:00Z", None),
            link(2, "b", "a", "2026-01-01T12:00:00Z", None),
            link(9, "solo", "main", "2026-01-01T11:00:00Z", None),
            link(5, "old", "main", "2026-01-01T09:00:00Z", None),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let lines = pull_request_list_lines(&chains);
        assert_eq!(
            lines
                .iter()
                .map(|line| (
                    line.link.number.get(),
                    line.depth,
                    line.stack.map(|s| s.size)
                ))
                .collect::<Vec<_>>(),
            vec![(1, 0, Some(2)), (2, 1, None), (9, 0, None), (5, 0, None)]
        );
    }
    #[test]
    fn native_stack_on_base_layer() {
        let stack = json!({"kind":"native","id":"1","number":1,"url":"https://github.com/acme/web/stacks/1","base":"main","layers":[{"number":3,"headBranch":"x","state":"open"},{"number":4,"headBranch":"y","state":"open"}]});
        let links = vec![
            link(4, "y", "x", "2026-01-01T10:00:00Z", Some(stack.clone())),
            link(3, "x", "main", "2026-01-01T10:00:00Z", Some(stack)),
        ];
        let chains = resolve_thread_pull_request_chains(&links);
        let lines = pull_request_list_lines(&chains);
        assert_eq!(
            lines
                .iter()
                .map(|line| (
                    line.link.number.get(),
                    line.depth,
                    line.stack.map(|s| s.kind)
                ))
                .collect::<Vec<_>>(),
            vec![(3, 0, Some(ChainKind::Native)), (4, 1, None)]
        );
    }
}

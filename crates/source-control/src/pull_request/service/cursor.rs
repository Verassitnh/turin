//! List cursors of `PullRequestService.ts`: what a listing hands back so a further page carries
//! on from where each repository stopped.

use std::sync::LazyLock;

use regex::Regex;

use crate::source_control_core::pull_request_provider::{
    ProviderChangeRequest, ProviderListCursor,
};

/// What the providers are told (`ProviderListCursor`), plus the part only the service acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ListCursor {
    pub updated_before: String,
    pub delivered: u64,
    /// The rows already handed over at exactly `updated_before`. The next read asks for that
    /// instant inclusively, so these are what keeps it from sending them a second time.
    pub seen_at: Vec<u64>,
}

impl ListCursor {
    /// Only the two fields a host can act on: which rows have already been sent at the boundary
    /// instant is this service's business, not a provider's.
    pub(crate) fn for_provider(&self) -> ProviderListCursor {
        ProviderListCursor {
            updated_before: self.updated_before.clone(),
            delivered: self.delivered,
        }
    }

    /// Whether `item` was already sent: it sits at the boundary instant and is named in `seen_at`.
    pub(crate) fn has_sent(&self, item: &ProviderChangeRequest) -> bool {
        item.updated_at == self.updated_before && self.seen_at.contains(&item.number)
    }
}

/// A continuation as it travels through the page and back. Written out rather than encoded because
/// it comes back from a client and has to be believed or refused on sight: everything a host is
/// given is either a timestamp of this shape or a number of this length, which is what lets a
/// provider drop it into a filter without checking it again.
static LIST_CURSOR_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^([0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(?:\.[0-9]{1,9})?(?:Z|[+-][0-9]{2}:[0-9]{2}))\|([0-9]{1,9})\|([0-9]{1,9}(?:,[0-9]{1,9})*)?$",
    )
    .expect("valid cursor pattern")
});

pub(crate) fn parse_list_cursor(raw: &str) -> Option<ListCursor> {
    let captures = LIST_CURSOR_PATTERN.captures(raw)?;
    Some(ListCursor {
        updated_before: captures.get(1)?.as_str().to_owned(),
        delivered: captures.get(2)?.as_str().parse().ok()?,
        seen_at: match captures.get(3) {
            None => Vec::new(),
            Some(seen) => seen
                .as_str()
                .split(',')
                .map(|number| number.parse().ok())
                .collect::<Option<Vec<u64>>>()?,
        },
    })
}

/// How a listing tells two repositories apart. The host is part of it because the same
/// `owner/repo` exists on github.com and on an Enterprise install, and they are two repositories.
pub(crate) fn list_cursor_key(host: &str, repository: &str) -> String {
    format!("{host} {}", repository.to_lowercase())
}

/// Where a repository carries on, worked out from the slice just handed over. The boundary is the
/// instant of the oldest row in it: the next read asks for that instant and everything before it,
/// and names the rows already sent at it so none of them arrives twice.
///
/// The names carry over when the boundary has not moved. A slice that ends on the same instant it
/// began on has to keep the earlier rows excluded as well as its own, or the read after it would
/// hand them over again.
///
/// `fetched` is what the host handed over, before the rows already sent were dropped from it;
/// `delivered_count` is what is being sent on, which is what the count of delivered rows is
/// about; `cursor_advance` lets a provider that consumed malformed offset-paged rows (which never
/// appear in `delivered`) say so.
pub(crate) fn next_list_cursor(
    previous: Option<&ListCursor>,
    fetched: &[ProviderChangeRequest],
    delivered_count: usize,
    cursor_advance: Option<u64>,
) -> Option<String> {
    // The host had nothing at all, so there is no row to carry on from — and repeating the cursor
    // that produced the empty slice would ask the same question forever.
    let first = fetched.first()?;
    // Taken from what the host answered rather than from what survived de-duplication: a slice can
    // be entirely rows already sent — a hundred change requests touched in the same second is one
    // repository's boring afternoon — and reading "nothing new" as "nothing left" would end the
    // walk on the instant it was stuck on, with everything older unreachable for good.
    let oldest = fetched.iter().fold(first, |left, right| {
        if right.updated_at < left.updated_at {
            right
        } else {
            left
        }
    });
    Some(list_cursor_at(
        previous,
        &oldest.updated_at,
        fetched,
        cursor_advance.unwrap_or(delivered_count as u64),
    ))
}

/// The same cursor against a boundary chosen elsewhere, which is what a slice read across several
/// repositories at once needs: every repository in it is read up to the oldest row of the whole
/// slice, including the ones that contributed nothing to it — their rows are simply all older, and
/// a repository that carried on from its own oldest row would be right about where it stopped and
/// silent about the ones that never appeared.
///
/// `fetched` is this repository's own rows in the slice, before the ones already sent were
/// dropped.
pub(crate) fn list_cursor_at(
    previous: Option<&ListCursor>,
    boundary: &str,
    fetched: &[ProviderChangeRequest],
    delivered_count: u64,
) -> String {
    let mut seen_at: Vec<u64> = match previous {
        Some(previous) if previous.updated_before == boundary => previous.seen_at.clone(),
        _ => Vec::new(),
    };
    seen_at.extend(
        fetched
            .iter()
            .filter(|item| item.updated_at == boundary)
            .map(|item| item.number),
    );
    let seen = seen_at
        .iter()
        .map(u64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{boundary}|{}|{seen}",
        previous.map_or(0, |previous| previous.delivered) + delivered_count
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_cursors_the_service_issues() {
        let cursor = parse_list_cursor("2026-07-02T00:00:00Z|3|1,2").unwrap();
        assert_eq!(cursor.updated_before, "2026-07-02T00:00:00Z");
        assert_eq!(cursor.delivered, 3);
        assert_eq!(cursor.seen_at, vec![1, 2]);
        assert_eq!(
            parse_list_cursor("2026-07-02T00:00:00.123+02:00|0|")
                .unwrap()
                .seen_at,
            vec![]
        );
        assert!(parse_list_cursor("garbage").is_none());
        assert!(parse_list_cursor("2026-07-02T00:00:00Z|1234567890|").is_none());
        assert!(parse_list_cursor("2026-07-02T00:00:00Z|1|1,").is_none());
        assert!(parse_list_cursor("2026-07-02T00:00:00Z|1|1\n").is_none());
    }

    #[test]
    fn keys_repositories_by_host_and_lowercase_name() {
        assert_eq!(
            list_cursor_key("github.com", "Acme/Web"),
            "github.com acme/web"
        );
    }
}

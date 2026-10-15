//! Port of upstream/packages/client-runtime/src/state/pullRequestRouting.ts (pure parts).
//!
//! The router itself routes a pull request read or write to whichever *environment* holds the
//! matching GitHub account, probing identities across the connection registry, with per-session
//! probe sharing and invalidation fan-out. Turin is one process with one environment, so there
//! is nothing to route between and the router (`createPullRequestRouter`, `routingAllowed`,
//! `shareMetadataRead`, `invalidateTarget`, `reportState`) is cut. What remains are the small
//! pure rules it applies to references and failures.

use std::collections::VecDeque;

use serde_json::Value;
use turin_contracts::EnvironmentId;
use turin_contracts::pull_request::PullRequestRef;
use url::Url;

/// How many routed references are remembered for invalidation fan-out.
pub const ROUTED_READ_CAPACITY: usize = 256;

/// `matchesReference`: same project, repository (ignoring case) and number, and the same host
/// (ignoring case) when the filter names one.
pub fn matches_reference(reference: &PullRequestRef, filter: &PullRequestRef) -> bool {
    reference.project_id == filter.project_id
        && reference.repository.as_str().to_lowercase() == filter.repository.as_str().to_lowercase()
        && reference.number == filter.number
        && filter.host.as_ref().is_none_or(|filter_host| {
            reference.host.as_ref().is_some_and(|host| {
                host.as_str().to_lowercase() == filter_host.as_str().to_lowercase()
            })
        })
}

/// `refKey`: identifies a routed reference within its origin environment. Host and repository
/// are case-insensitive.
pub fn routed_read_key(origin: &EnvironmentId, reference: &PullRequestRef) -> String {
    Value::Array(vec![
        Value::from(origin.as_str()),
        Value::from(reference.project_id.as_str()),
        reference.host.as_ref().map_or(Value::Null, |host| {
            Value::from(host.as_str().to_lowercase())
        }),
        Value::from(reference.repository.as_str().to_lowercase()),
        Value::from(reference.number.get().to_string()),
    ])
    .to_string()
}

/// `isLocal`: whether a connection's HTTP base URL points at this machine.
pub fn is_local_base_url(url: &str) -> bool {
    Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| ["localhost", "127.0.0.1", "[::1]"].contains(&host.as_str()))
}

/// The failures `rejectedBeforeDispatch` tells apart, by upstream error `_tag`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoutedWriteFailure {
    EnvironmentRpcUnavailable,
    EnvironmentNotRegistered,
    /// `PullRequestOperationError` with its `operation`.
    PullRequestOperation {
        operation: String,
    },
    Other,
}

/// A guard rejection is returned before the operation starts. Other write failures are ambiguous.
pub fn rejected_before_dispatch(failure: &RoutedWriteFailure) -> bool {
    match failure {
        RoutedWriteFailure::EnvironmentRpcUnavailable
        | RoutedWriteFailure::EnvironmentNotRegistered => true,
        RoutedWriteFailure::PullRequestOperation { operation } => operation == "routeIdentity",
        RoutedWriteFailure::Other => false,
    }
}

/// The remembered routed references, oldest first, bounded to [`ROUTED_READ_CAPACITY`]. A
/// reference that is already known keeps its place when it is touched again.
#[derive(Debug, Clone, Default)]
pub struct RoutedReads<V> {
    entries: VecDeque<(String, V)>,
}

impl<V> RoutedReads<V> {
    pub fn new() -> Self {
        Self {
            entries: VecDeque::new(),
        }
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut V> {
        self.entries
            .iter_mut()
            .find(|(existing, _)| existing == key)
            .map(|(_, value)| value)
    }

    /// Stores `value` under `key`, evicting the oldest reference when full and the key is new.
    pub fn set(&mut self, key: String, value: V) {
        if let Some(existing) = self.get_mut(&key) {
            *existing = value;
            return;
        }
        if self.entries.len() >= ROUTED_READ_CAPACITY {
            self.entries.pop_front();
        }
        self.entries.push_back((key, value));
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.entries.iter().map(|(_, value)| value)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reference(host: Option<&str>, repository: &str, number: u64) -> PullRequestRef {
        let mut value = json!({
            "projectId": "project-1",
            "repository": repository,
            "number": number,
        });
        if let Some(host) = host {
            value["host"] = json!(host);
        }
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn matches_references_ignoring_case_and_honoring_the_filter_host() {
        let read = reference(Some("github.com"), "Acme/Web", 7);
        assert!(matches_reference(&read, &reference(None, "acme/web", 7)));
        assert!(matches_reference(
            &read,
            &reference(Some("GITHUB.COM"), "acme/web", 7)
        ));
        assert!(!matches_reference(
            &read,
            &reference(Some("github.example.com"), "acme/web", 7)
        ));
        assert!(!matches_reference(&read, &reference(None, "acme/web", 8)));
        assert!(!matches_reference(&read, &reference(None, "acme/api", 7)));
        // A filter that names a host never matches a read that has none.
        assert!(!matches_reference(
            &reference(None, "acme/web", 7),
            &reference(Some("github.com"), "acme/web", 7)
        ));
        let mut other_project = reference(None, "acme/web", 7);
        other_project.project_id = turin_contracts::ProjectId::new("project-2").unwrap();
        assert!(!matches_reference(&read, &other_project));
    }

    #[test]
    fn keys_a_routed_read_by_origin_and_lowercased_reference() {
        let origin = EnvironmentId::new("environment-1").unwrap();
        assert_eq!(
            routed_read_key(&origin, &reference(Some("GitHub.com"), "Acme/Web", 7)),
            r#"["environment-1","project-1","github.com","acme/web","7"]"#
        );
        assert_eq!(
            routed_read_key(&origin, &reference(None, "acme/web", 7)),
            r#"["environment-1","project-1",null,"acme/web","7"]"#
        );
    }

    #[test]
    fn recognizes_loopback_base_urls() {
        for url in [
            "http://localhost:3773",
            "https://localhost",
            "http://127.0.0.1:8080/api",
            "http://[::1]:3000",
        ] {
            assert!(is_local_base_url(url), "{url}");
        }
        for url in [
            "https://environment.example.test",
            "http://192.168.1.4:3773",
            "not a url",
            "",
        ] {
            assert!(!is_local_base_url(url), "{url}");
        }
    }

    #[test]
    fn only_guard_rejections_count_as_not_dispatched() {
        assert!(rejected_before_dispatch(
            &RoutedWriteFailure::EnvironmentRpcUnavailable
        ));
        assert!(rejected_before_dispatch(
            &RoutedWriteFailure::EnvironmentNotRegistered
        ));
        assert!(rejected_before_dispatch(
            &RoutedWriteFailure::PullRequestOperation {
                operation: "routeIdentity".into()
            }
        ));
        assert!(!rejected_before_dispatch(
            &RoutedWriteFailure::PullRequestOperation {
                operation: "runAction".into()
            }
        ));
        assert!(!rejected_before_dispatch(&RoutedWriteFailure::Other));
    }

    #[test]
    fn evicts_the_oldest_reference_once_full() {
        let mut reads = RoutedReads::new();
        for index in 0..ROUTED_READ_CAPACITY {
            reads.set(format!("key-{index}"), index);
        }
        assert_eq!(reads.len(), ROUTED_READ_CAPACITY);
        // Touching a known key neither grows nor reorders the map.
        reads.set("key-0".into(), 1_000);
        assert_eq!(reads.len(), ROUTED_READ_CAPACITY);
        reads.set("key-new".into(), 2_000);
        assert_eq!(reads.len(), ROUTED_READ_CAPACITY);
        assert!(reads.get_mut("key-0").is_none());
        assert_eq!(reads.get_mut("key-1").copied(), Some(1));
        assert_eq!(reads.get_mut("key-new").copied(), Some(2_000));
        assert_eq!(reads.values().next().copied(), Some(1));
    }
}

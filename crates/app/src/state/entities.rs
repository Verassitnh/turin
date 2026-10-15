//! Port of upstream/packages/client-runtime/src/state/entities.ts
//!
//! Atom-family keys become plain string keys (`environmentId\0threadId`) so the
//! store's maps and the selectors' memo tables share upstream's identity rules.

use crate::state::contracts::{
    EnvironmentId, ProjectId, ScopedProjectRef, ScopedThreadRef, ThreadId,
};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid scoped project atom key: {key:?}.")]
pub struct InvalidScopedProjectKeyError {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid scoped thread atom key: {key:?}.")]
pub struct InvalidScopedThreadKeyError {
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("Invalid scoped project reference collection atom key: {key:?}.")]
pub struct InvalidScopedProjectRefCollectionKeyError {
    pub key: String,
}

pub fn project_key(environment_id: &EnvironmentId, project_id: &ProjectId) -> String {
    format!("{environment_id}\u{0}{project_id}")
}

pub fn thread_key(environment_id: &EnvironmentId, thread_id: &ThreadId) -> String {
    format!("{environment_id}\u{0}{thread_id}")
}

pub fn project_ref_key(reference: &ScopedProjectRef) -> String {
    project_key(&reference.environment_id, &reference.project_id)
}

pub fn thread_ref_key(reference: &ScopedThreadRef) -> String {
    thread_key(&reference.environment_id, &reference.thread_id)
}

/// `JSON.stringify(refs.map((ref) => [ref.environmentId, ref.projectId]))`.
pub fn project_ref_collection_key(refs: &[ScopedProjectRef]) -> String {
    let pairs: Vec<[&str; 2]> = refs
        .iter()
        .map(|reference| {
            [
                reference.environment_id.as_str(),
                reference.project_id.as_str(),
            ]
        })
        .collect();
    serde_json::to_string(&pairs).expect("string pairs always serialize")
}

pub fn parse_project_key(key: &str) -> Result<ScopedProjectRef, InvalidScopedProjectKeyError> {
    let invalid = || InvalidScopedProjectKeyError {
        key: key.to_owned(),
    };
    let (environment_id, project_id) = key.split_once('\u{0}').ok_or_else(invalid)?;
    Ok(ScopedProjectRef {
        environment_id: EnvironmentId::new(environment_id).map_err(|_| invalid())?,
        project_id: ProjectId::new(project_id).map_err(|_| invalid())?,
    })
}

pub fn parse_project_ref_collection_key(
    key: &str,
) -> Result<Vec<ScopedProjectRef>, InvalidScopedProjectRefCollectionKeyError> {
    let invalid = || InvalidScopedProjectRefCollectionKeyError {
        key: key.to_owned(),
    };
    let entries: Vec<(String, String)> = serde_json::from_str(key).map_err(|_| invalid())?;
    entries
        .into_iter()
        .map(|(environment_id, project_id)| {
            Ok(ScopedProjectRef {
                environment_id: EnvironmentId::new(environment_id).map_err(|_| invalid())?,
                project_id: ProjectId::new(project_id).map_err(|_| invalid())?,
            })
        })
        .collect()
}

pub fn parse_thread_key(key: &str) -> Result<ScopedThreadRef, InvalidScopedThreadKeyError> {
    let invalid = || InvalidScopedThreadKeyError {
        key: key.to_owned(),
    };
    let (environment_id, thread_id) = key.split_once('\u{0}').ok_or_else(invalid)?;
    Ok(ScopedThreadRef {
        environment_id: EnvironmentId::new(environment_id).map_err(|_| invalid())?,
        thread_id: ThreadId::new(thread_id).map_err(|_| invalid())?,
    })
}

pub fn project_refs_equal(left: &[ScopedProjectRef], right: &[ScopedProjectRef]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.environment_id == right.environment_id && left.project_id == right.project_id
        })
}

pub fn thread_refs_equal(left: &[ScopedThreadRef], right: &[ScopedThreadRef]) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(left, right)| {
            left.environment_id == right.environment_id && left.thread_id == right.thread_id
        })
}

/// `arrayElementsEqual`: element identity. Items held as `Arc`s compare by
/// pointer, which is what upstream's `===` means for objects.
pub fn arc_elements_equal<A>(left: &[std::sync::Arc<A>], right: &[std::sync::Arc<A>]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| std::sync::Arc::ptr_eq(left, right))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_ref(environment: &str, thread: &str) -> ScopedThreadRef {
        ScopedThreadRef {
            environment_id: EnvironmentId::new(environment).unwrap(),
            thread_id: ThreadId::new(thread).unwrap(),
        }
    }

    #[test]
    fn thread_keys_round_trip() {
        let reference = thread_ref("env-1", "thread-1");
        let key = thread_ref_key(&reference);
        assert_eq!(key, "env-1\u{0}thread-1");
        assert_eq!(parse_thread_key(&key).unwrap(), reference);
    }

    #[test]
    fn invalid_keys_are_rejected() {
        assert_eq!(
            parse_thread_key("no-separator").unwrap_err().to_string(),
            "Invalid scoped thread atom key: \"no-separator\"."
        );
        assert!(parse_project_key("env\u{0}").is_err());
        assert!(parse_project_ref_collection_key("not json").is_err());
    }

    #[test]
    fn project_ref_collections_round_trip() {
        let refs = vec![
            ScopedProjectRef {
                environment_id: EnvironmentId::new("env-1").unwrap(),
                project_id: ProjectId::new("p1").unwrap(),
            },
            ScopedProjectRef {
                environment_id: EnvironmentId::new("env-2").unwrap(),
                project_id: ProjectId::new("p2").unwrap(),
            },
        ];
        let key = project_ref_collection_key(&refs);
        assert_eq!(key, r#"[["env-1","p1"],["env-2","p2"]]"#);
        assert!(project_refs_equal(
            &parse_project_ref_collection_key(&key).unwrap(),
            &refs
        ));
    }

    #[test]
    fn ref_lists_compare_by_value() {
        let left = vec![thread_ref("e", "a"), thread_ref("e", "b")];
        let mut right = left.clone();
        assert!(thread_refs_equal(&left, &right));
        right[1] = thread_ref("e", "c");
        assert!(!thread_refs_equal(&left, &right));
    }
}

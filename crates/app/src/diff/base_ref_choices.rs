//! Port of upstream/apps/web/src/lib/baseRefChoices.ts (+ test).
//!
//! `VcsRef` is mirrored UI-side (the vcs contracts are not on this branch).

/// `VcsRef`, the fields the picker reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VcsRef {
    pub name: String,
    pub current: bool,
    pub is_default: bool,
    pub is_remote: bool,
    pub remote_name: Option<String>,
    pub worktree_path: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BaseRefChoice {
    pub id: String,
    pub label: String,
    pub local: Option<VcsRef>,
    pub remote: Option<VcsRef>,
}

fn remote_branch_name(reference: &VcsRef) -> &str {
    if let Some(remote) = &reference.remote_name
        && let Some(rest) = reference
            .name
            .strip_prefix(remote.as_str())
            .and_then(|rest| rest.strip_prefix('/'))
    {
        return rest;
    }
    &reference.name
}

pub fn build_base_ref_choices(local_refs: &[VcsRef], remote_refs: &[VcsRef]) -> Vec<BaseRefChoice> {
    let mut unused: Vec<bool> = vec![true; remote_refs.len()];
    let mut choices: Vec<BaseRefChoice> = local_refs
        .iter()
        .map(|local| {
            let matches: Vec<usize> = remote_refs
                .iter()
                .enumerate()
                .filter(|(i, remote)| unused[*i] && remote_branch_name(remote) == local.name)
                .map(|(i, _)| i)
                .collect();
            let chosen = matches
                .iter()
                .copied()
                .find(|i| remote_refs[*i].remote_name.as_deref() == Some("origin"))
                .or_else(|| matches.first().copied());
            if let Some(index) = chosen {
                unused[index] = false;
            }
            BaseRefChoice {
                id: format!("local:{}", local.name),
                label: local.name.clone(),
                local: Some(local.clone()),
                remote: chosen.map(|i| remote_refs[i].clone()),
            }
        })
        .collect();
    choices.extend(
        remote_refs
            .iter()
            .enumerate()
            .filter(|(i, _)| unused[*i])
            .map(|(_, remote)| BaseRefChoice {
                id: format!("remote:{}", remote.name),
                label: remote.name.clone(),
                local: None,
                remote: Some(remote.clone()),
            }),
    );
    choices
}

pub fn filter_base_ref_choices(choices: &[BaseRefChoice], query: &str) -> Vec<BaseRefChoice> {
    let normalized = query.trim().to_lowercase();
    if normalized.is_empty() {
        return choices.to_vec();
    }
    choices
        .iter()
        .filter(|choice| {
            choice.label.to_lowercase().contains(&normalized)
                || choice
                    .local
                    .as_ref()
                    .is_some_and(|r| r.name.to_lowercase().contains(&normalized))
                || choice
                    .remote
                    .as_ref()
                    .is_some_and(|r| r.name.to_lowercase().contains(&normalized))
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(name: &str, remote_name: Option<&str>) -> VcsRef {
        VcsRef {
            name: name.into(),
            current: false,
            is_default: false,
            is_remote: remote_name.is_some(),
            remote_name: remote_name.map(Into::into),
            worktree_path: None,
        }
    }

    #[test]
    fn pairs_matching_local_and_remote_branches_and_prefers_origin() {
        let choices = build_base_ref_choices(
            &[reference("main", None)],
            &[
                reference("upstream/main", Some("upstream")),
                reference("origin/main", Some("origin")),
            ],
        );
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].label, "main");
        assert_eq!(choices[0].local.as_ref().unwrap().name, "main");
        assert_eq!(choices[0].remote.as_ref().unwrap().name, "origin/main");
        assert_eq!(choices[1].label, "upstream/main");
        assert!(choices[1].local.is_none());
        assert_eq!(choices[1].remote.as_ref().unwrap().name, "upstream/main");
    }

    #[test]
    fn filters_stale_server_results_against_the_current_query() {
        let choices = build_base_ref_choices(
            &[reference("main", None), reference("feature/search", None)],
            &[
                reference("origin/main", Some("origin")),
                reference("origin/feature/search", Some("origin")),
            ],
        );
        let labels = |query: &str| -> Vec<String> {
            filter_base_ref_choices(&choices, query)
                .into_iter()
                .map(|c| c.label)
                .collect()
        };
        assert_eq!(labels("SEARCH"), vec!["feature/search"]);
        assert_eq!(labels("origin/main"), vec!["main"]);
    }
}

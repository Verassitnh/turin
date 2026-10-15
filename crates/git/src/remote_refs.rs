//! Port of upstream/apps/server/src/git/remoteRefs.ts

pub fn parse_remote_names_in_git_order(stdout: &str) -> Vec<String> {
    stdout
        .split('\n')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Longest name first, so `origin` does not steal `origin-fork/branch`.
pub fn parse_remote_names(stdout: &str) -> Vec<String> {
    let mut names = parse_remote_names_in_git_order(stdout);
    names.sort_by_key(|name| std::cmp::Reverse(name.len()));
    names
}

pub struct ParsedRemoteRef {
    pub remote_ref: String,
    pub remote_name: String,
    pub branch_name: String,
}

pub fn parse_remote_ref_with_remote_names(
    reference: &str,
    remote_names: &[String],
) -> Option<ParsedRemoteRef> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return None;
    }
    for remote_name in remote_names {
        let prefix = format!("{remote_name}/");
        if !trimmed.starts_with(&prefix) {
            continue;
        }
        let branch_name = trimmed[prefix.len()..].trim();
        if branch_name.is_empty() {
            return None;
        }
        return Some(ParsedRemoteRef {
            remote_ref: trimmed.to_owned(),
            remote_name: remote_name.clone(),
            branch_name: branch_name.to_owned(),
        });
    }
    None
}

pub fn extract_branch_name_from_remote_ref(
    reference: &str,
    remote_name: Option<&str>,
    remote_names: &[String],
) -> String {
    let normalized = reference.trim();
    if normalized.is_empty() {
        return String::new();
    }
    if let Some(rest) = normalized.strip_prefix("refs/remotes/") {
        return extract_branch_name_from_remote_ref(rest, remote_name, remote_names);
    }
    let names: Vec<String> = if let Some(remote_name) = remote_name {
        vec![remote_name.to_owned()]
    } else {
        remote_names.to_vec()
    };
    if let Some(parsed) = parse_remote_ref_with_remote_names(normalized, &names) {
        return parsed.branch_name;
    }
    match normalized.find('/') {
        Some(index) => normalized[index + 1..].trim().to_owned(),
        None => normalized.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sorts_remote_names_longest_first() {
        let names = parse_remote_names("origin\norigin-fork\nupstream\n");
        assert_eq!(
            names,
            vec![
                "origin-fork".to_owned(),
                "upstream".to_owned(),
                "origin".to_owned()
            ]
        );
    }

    #[test]
    fn prefers_the_longest_matching_remote() {
        let names = parse_remote_names("origin\norigin-fork\n");
        let parsed = parse_remote_ref_with_remote_names("origin-fork/feature", &names).unwrap();
        assert_eq!(parsed.remote_name, "origin-fork");
        assert_eq!(parsed.branch_name, "feature");
        assert_eq!(
            extract_branch_name_from_remote_ref(
                "refs/remotes/origin/feature/demo",
                Some("origin"),
                &[]
            ),
            "feature/demo"
        );
    }
}

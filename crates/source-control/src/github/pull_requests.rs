//! Port of upstream/packages/source-control-github/src/server/gitHubPullRequests.ts
//!
//! Pull request rows in `gh --json` or GraphQL node shape, normalized to one record.

use serde::Deserialize;
use serde_json::Value;
use turin_contracts::base_schemas::{TrimmedNonEmptyString, UtcDateTime};
use turin_contracts::pull_request::PullRequestState;

use super::pull_request_json::de::{self, Int};
use super::pull_request_json::{DecodeFailure, DecodeResult};
use crate::shared::js::js_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedGitHubPullRequestRecord {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    /// The head commit, when the read asked for `headRefOid`.
    pub head_sha: Option<String>,
    pub state: PullRequestState,
    pub is_draft: Option<bool>,
    /// `closedAt?: string | null` is always present on the normalized record.
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    /// `Option<DateTime.Utc>`.
    pub updated_at: Option<UtcDateTime>,
    pub is_cross_repository: Option<bool>,
    pub head_repository_name_with_owner: Option<String>,
    pub head_repository_owner_login: Option<String>,
}

/// gh < 2.47 exports `headRepository` as `{id, name}` only; `nameWithOwner` was added later. Both
/// fields stay optional so a version-drifted gh CLI can never fail the decode and silently drop
/// the PR from the list.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRepository {
    #[serde(default)]
    name_with_owner: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct HeadRepositoryOwner {
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GitHubPullRequest {
    number: Int,
    title: TrimmedNonEmptyString,
    url: TrimmedNonEmptyString,
    base_ref_name: TrimmedNonEmptyString,
    head_ref_name: TrimmedNonEmptyString,
    #[serde(default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default, deserialize_with = "de::opt")]
    is_draft: Option<bool>,
    #[serde(default)]
    closed_at: Option<String>,
    #[serde(default)]
    merged_at: Option<String>,
    /// `Schema.optional(Schema.OptionFromNullOr(Schema.DateTimeUtcFromString))`.
    #[serde(default)]
    updated_at: Option<UtcDateTime>,
    #[serde(default, deserialize_with = "de::opt")]
    is_cross_repository: Option<bool>,
    #[serde(default)]
    head_repository: Option<HeadRepository>,
    #[serde(default)]
    head_repository_owner: Option<HeadRepositoryOwner>,
}

fn trim_optional_string(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value.unwrap_or_default());
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn normalize_github_pull_request_state(
    state: Option<&str>,
    merged_at: Option<&str>,
) -> PullRequestState {
    let normalized_state = state.map(|state| js_trim(state).to_uppercase());
    if merged_at.is_some_and(|merged_at| !js_trim(merged_at).is_empty())
        || normalized_state.as_deref() == Some("MERGED")
    {
        return PullRequestState::Merged;
    }
    if normalized_state.as_deref() == Some("CLOSED") {
        return PullRequestState::Closed;
    }
    PullRequestState::Open
}

fn normalize_github_pull_request_record(
    raw: GitHubPullRequest,
) -> NormalizedGitHubPullRequestRecord {
    let explicit_name_with_owner = trim_optional_string(
        raw.head_repository
            .as_ref()
            .and_then(|repository| repository.name_with_owner.as_deref()),
    );
    let head_repository_name = trim_optional_string(
        raw.head_repository
            .as_ref()
            .and_then(|repository| repository.name.as_deref()),
    );
    let head_repository_owner_login = trim_optional_string(
        raw.head_repository_owner
            .as_ref()
            .and_then(|owner| owner.login.as_deref()),
    )
    .or_else(|| {
        explicit_name_with_owner
            .as_deref()
            .filter(|name_with_owner| name_with_owner.contains('/'))
            .and_then(|name_with_owner| name_with_owner.split('/').next())
            .map(str::to_owned)
    });
    let head_repository_name_with_owner = explicit_name_with_owner.or_else(|| {
        match (&head_repository_owner_login, &head_repository_name) {
            (Some(owner), Some(name)) if !owner.is_empty() && !name.is_empty() => {
                Some(format!("{owner}/{name}"))
            }
            _ => None,
        }
    });
    let head_sha = trim_optional_string(raw.head_ref_oid.as_deref());
    NormalizedGitHubPullRequestRecord {
        number: raw.number.non_negative(),
        title: raw.title.into_inner(),
        url: raw.url.into_inner(),
        base_ref_name: raw.base_ref_name.into_inner(),
        head_ref_name: raw.head_ref_name.into_inner(),
        head_sha,
        state: normalize_github_pull_request_state(raw.state.as_deref(), raw.merged_at.as_deref()),
        is_draft: (raw.is_draft == Some(true)).then_some(true),
        closed_at: raw.closed_at,
        merged_at: raw.merged_at,
        updated_at: raw.updated_at,
        is_cross_repository: raw.is_cross_repository,
        head_repository_name_with_owner,
        head_repository_owner_login: head_repository_owner_login.filter(|login| !login.is_empty()),
    }
}

/// Pull request rows in `gh --json` or GraphQL node shape. A row that does not decode is
/// skipped, so one malformed pull request cannot hide the rest.
pub fn decode_github_pull_request_entries(
    entries: &[Value],
) -> Vec<NormalizedGitHubPullRequestRecord> {
    entries
        .iter()
        .filter_map(|entry| GitHubPullRequest::deserialize(entry).ok())
        .filter(|raw| raw.number.0 >= 1)
        .map(normalize_github_pull_request_record)
        .collect()
}

pub fn decode_github_pull_request_list_json(
    raw: &str,
) -> DecodeResult<Vec<NormalizedGitHubPullRequestRecord>> {
    let entries: Vec<Value> =
        serde_json::from_str(raw).map_err(|error| DecodeFailure::new(error.to_string()))?;
    Ok(decode_github_pull_request_entries(&entries))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    fn row(overrides: Value) -> Value {
        let mut base = json!({
            "number": 12,
            "title": "  Add the page ",
            "url": "https://github.com/acme/web/pull/12",
            "baseRefName": "main",
            "headRefName": "feat/page",
        });
        base.as_object_mut()
            .unwrap()
            .extend(overrides.as_object().unwrap().clone());
        base
    }

    #[test]
    fn trims_required_strings_and_normalizes_a_minimal_row() {
        let records = decode_github_pull_request_entries(&[row(json!({}))]);
        assert_eq!(
            records,
            vec![NormalizedGitHubPullRequestRecord {
                number: 12,
                title: "Add the page".to_owned(),
                url: "https://github.com/acme/web/pull/12".to_owned(),
                base_ref_name: "main".to_owned(),
                head_ref_name: "feat/page".to_owned(),
                head_sha: None,
                state: PullRequestState::Open,
                is_draft: None,
                closed_at: None,
                merged_at: None,
                updated_at: None,
                is_cross_repository: None,
                head_repository_name_with_owner: None,
                head_repository_owner_login: None,
            }]
        );
    }

    #[test]
    fn a_merge_timestamp_outranks_a_closed_state() {
        let records = decode_github_pull_request_entries(&[
            row(json!({"state": "CLOSED", "mergedAt": "2026-07-03T00:00:00Z"})),
            row(json!({"state": " merged "})),
            row(json!({"state": "closed"})),
            row(json!({"state": "OPEN", "mergedAt": "  "})),
        ]);
        assert_eq!(
            records
                .iter()
                .map(|record| record.state)
                .collect::<Vec<_>>(),
            vec![
                PullRequestState::Merged,
                PullRequestState::Merged,
                PullRequestState::Closed,
                PullRequestState::Open,
            ]
        );
        assert_eq!(
            records[0].merged_at.as_deref(),
            Some("2026-07-03T00:00:00Z")
        );
    }

    #[test]
    fn skips_rows_that_do_not_decode_without_hiding_the_rest() {
        let records = decode_github_pull_request_entries(&[
            row(json!({"number": 0})),
            row(json!({"number": "7"})),
            row(json!({"title": "   "})),
            row(json!({"isDraft": null})),
            row(json!({"updatedAt": "not a date"})),
            json!(null),
            row(json!({"number": 5})),
        ]);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].number, 5);
    }

    #[test]
    fn keeps_a_draft_flag_only_when_true_and_reads_the_head_commit() {
        let records = decode_github_pull_request_entries(&[
            row(json!({"isDraft": true, "headRefOid": " abc123 "})),
            row(json!({"isDraft": false, "headRefOid": "  "})),
        ]);
        assert_eq!(records[0].is_draft, Some(true));
        assert_eq!(records[0].head_sha.as_deref(), Some("abc123"));
        assert_eq!(records[1].is_draft, None);
        assert_eq!(records[1].head_sha, None);
    }

    #[test]
    fn derives_the_head_repository_for_a_version_drifted_gh() {
        let records = decode_github_pull_request_entries(&[
            // gh >= 2.47: nameWithOwner present; the owner comes from it when none is exported.
            row(json!({
                "isCrossRepository": true,
                "headRepository": {"nameWithOwner": "octo/web", "name": "web"},
            })),
            // gh < 2.47: only {id, name} plus the owner's login.
            row(json!({
                "headRepository": {"name": "web"},
                "headRepositoryOwner": {"login": "octo"},
            })),
            // Neither enough to name the repository.
            row(json!({"headRepository": null, "headRepositoryOwner": {"login": null}})),
        ]);
        assert_eq!(
            records[0].head_repository_name_with_owner.as_deref(),
            Some("octo/web")
        );
        assert_eq!(
            records[0].head_repository_owner_login.as_deref(),
            Some("octo")
        );
        assert_eq!(records[0].is_cross_repository, Some(true));
        assert_eq!(
            records[1].head_repository_name_with_owner.as_deref(),
            Some("octo/web")
        );
        assert_eq!(
            records[1].head_repository_owner_login.as_deref(),
            Some("octo")
        );
        assert_eq!(records[2].head_repository_name_with_owner, None);
        assert_eq!(records[2].head_repository_owner_login, None);
    }

    #[test]
    fn parses_updated_at_as_a_utc_instant() {
        let records = decode_github_pull_request_entries(&[
            row(json!({"updatedAt": "2026-07-02T03:04:05Z"})),
            row(json!({"updatedAt": null})),
        ]);
        assert_eq!(
            records[0].updated_at.map(|at| at.to_string()),
            Some("2026-07-02T03:04:05.000Z".to_owned())
        );
        assert_eq!(records[1].updated_at, None);
    }

    #[test]
    fn decodes_a_list_document_and_rejects_anything_else() {
        let raw = json!([row(json!({})), {"number": "x"}]).to_string();
        assert_eq!(decode_github_pull_request_list_json(&raw).unwrap().len(), 1);
        assert!(decode_github_pull_request_list_json("{}").is_err());
        assert!(decode_github_pull_request_list_json("[").is_err());
    }
}

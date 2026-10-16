//! Port of upstream/packages/source-control-core/src/server/pullRequestChecks.ts

use std::collections::HashMap;

use turin_contracts::pull_request::PullRequestCheck;

/// One run of a check as the host listed it.
#[derive(Debug, Clone)]
pub struct CheckEntry {
    pub check: PullRequestCheck,
    pub workflow_name: Option<String>,
    pub at: Option<String>,
}

/// ISO-8601 timestamps in UTC compare correctly as plain text, which is all the ordering needs.
fn is_at_least_as_new(candidate: Option<&str>, kept: Option<&str>) -> bool {
    match candidate {
        None => kept.is_none(),
        Some(candidate) => kept.is_none_or(|kept| candidate >= kept),
    }
}

/// One row per check rather than one per run of it.
///
/// A host's rollup is a list of runs, not a list of checks: while a workflow is being re-run — or
/// while a second run of it is already live — the same check arrives twice, and both copies reach
/// the reader as what looks like a duplicate. Nothing in a check carries an id, so the name is what
/// identifies it, qualified by the workflow it belongs to since two workflows are free to name a
/// job the same thing.
///
/// Within a group the newest run is the one worth showing: a re-run is the answer that replaces the
/// one before it, and a run that never said when it happened loses to one that did. A tie goes to
/// whichever came last, because a host lists a re-run after the run it repeats.
///
/// The order is the host's own, held at the place each check first appeared, so a re-run landing
/// mid-read replaces a row where it stands instead of reshuffling the list under the reader.
///
/// Two checks that survive under the same name are then genuinely different ones, since they came
/// from different workflows — each is shown as `workflow / name`, the way GitHub writes it itself.
/// A survivor whose workflow the host did not name keeps its bare name rather than being qualified
/// with nothing.
pub fn dedupe_checks(entries: Vec<CheckEntry>) -> Vec<PullRequestCheck> {
    // Insertion-ordered map: index by key, slot holds the winning entry.
    let mut index: HashMap<String, usize> = HashMap::new();
    let mut survivors: Vec<CheckEntry> = Vec::new();
    for entry in entries {
        let key = serde_json::to_string(&(
            entry.workflow_name.as_deref().unwrap_or(""),
            entry.check.name.as_str(),
        ))
        .expect("tuple of strings serializes");
        match index.get(&key) {
            // Re-setting a key a Map already holds keeps its first position, which is the order wanted.
            Some(&slot) => {
                if is_at_least_as_new(entry.at.as_deref(), survivors[slot].at.as_deref()) {
                    survivors[slot] = entry;
                }
            }
            None => {
                index.insert(key, survivors.len());
                survivors.push(entry);
            }
        }
    }
    let mut counts_by_name: HashMap<String, usize> = HashMap::new();
    for entry in &survivors {
        *counts_by_name
            .entry(entry.check.name.as_str().to_owned())
            .or_default() += 1;
    }
    survivors
        .into_iter()
        .map(|entry| {
            let workflow_name = entry.workflow_name.as_deref().unwrap_or("");
            if !workflow_name.is_empty()
                && counts_by_name
                    .get(entry.check.name.as_str())
                    .copied()
                    .unwrap_or(0)
                    > 1
            {
                let mut check = entry.check;
                check.name = turin_contracts::TrimmedNonEmptyString::new(format!(
                    "{workflow_name} / {}",
                    check.name.as_str()
                ))
                .unwrap_or(check.name);
                check
            } else {
                entry.check
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turin_contracts::TrimmedNonEmptyString;
    use turin_contracts::pull_request::PullRequestCheckStatus as S;

    fn entry(name: &str, status: S, workflow: Option<&str>, at: Option<&str>) -> CheckEntry {
        CheckEntry {
            check: PullRequestCheck {
                name: TrimmedNonEmptyString::new(name).unwrap(),
                status,
                description: None,
                url: None,
                required: None,
            },
            workflow_name: workflow.map(str::to_owned),
            at: at.map(str::to_owned),
        }
    }
    fn view(checks: &[PullRequestCheck]) -> Vec<(String, S)> {
        checks
            .iter()
            .map(|c| (c.name.as_str().to_owned(), c.status))
            .collect()
    }

    #[test]
    fn keeps_the_newest_run_of_a_check_the_host_listed_twice() {
        let checks = dedupe_checks(vec![
            entry(
                "Prepare PR size config",
                S::Success,
                Some("PR Size"),
                Some("2026-08-11T16:06:25Z"),
            ),
            entry(
                "Prepare PR size config",
                S::Pending,
                Some("PR Size"),
                Some("2026-08-11T17:01:04Z"),
            ),
        ]);
        assert_eq!(
            view(&checks),
            vec![("Prepare PR size config".into(), S::Pending)]
        );
    }

    #[test]
    fn holds_a_check_at_the_place_it_first_appeared() {
        let checks = dedupe_checks(vec![
            entry("lint", S::Success, Some("CI"), Some("2026-08-11T16:00:00Z")),
            entry("test", S::Success, Some("CI"), Some("2026-08-11T16:00:00Z")),
            entry("lint", S::Failure, Some("CI"), Some("2026-08-11T18:00:00Z")),
        ]);
        assert_eq!(
            view(&checks),
            vec![("lint".into(), S::Failure), ("test".into(), S::Success)]
        );
    }

    #[test]
    fn keeps_distinct_workflow_and_check_pairs_even_when_their_names_join_to_the_same_text() {
        let checks = dedupe_checks(vec![
            entry(
                "Linux tests",
                S::Success,
                Some("Build"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry(
                "tests",
                S::Success,
                Some("Build Linux"),
                Some("2026-08-11T17:00:00Z"),
            ),
            entry(
                "Linux tests",
                S::Failure,
                Some("Build"),
                Some("2026-08-11T18:00:00Z"),
            ),
        ]);
        assert_eq!(
            view(&checks),
            vec![
                ("Linux tests".into(), S::Failure),
                ("tests".into(), S::Success)
            ]
        );
    }

    #[test]
    fn loses_a_run_that_never_said_when_it_happened_to_one_that_did() {
        let undated = dedupe_checks(vec![
            entry("build", S::Success, None, Some("2026-08-11T16:00:00Z")),
            entry("build", S::Pending, None, None),
        ]);
        let dated = dedupe_checks(vec![
            entry("build", S::Pending, None, None),
            entry("build", S::Success, None, Some("2026-08-11T16:00:00Z")),
        ]);
        assert_eq!(
            (undated[0].status, dated[0].status),
            (S::Success, S::Success)
        );
    }

    #[test]
    fn takes_the_last_copy_when_neither_run_is_dated() {
        let checks = dedupe_checks(vec![
            entry("build", S::Pending, None, None),
            entry("build", S::Failure, None, None),
        ]);
        assert_eq!(
            checks.iter().map(|c| c.status).collect::<Vec<_>>(),
            vec![S::Failure]
        );
    }

    #[test]
    fn keeps_two_workflows_that_name_a_job_the_same_thing_and_says_which_is_which() {
        let checks = dedupe_checks(vec![
            entry(
                "build",
                S::Success,
                Some("CI"),
                Some("2026-08-11T16:00:00Z"),
            ),
            entry(
                "build",
                S::Failure,
                Some("Release"),
                Some("2026-08-11T16:00:00Z"),
            ),
        ]);
        assert_eq!(
            view(&checks),
            vec![
                ("CI / build".into(), S::Success),
                ("Release / build".into(), S::Failure)
            ]
        );
    }

    #[test]
    fn leaves_a_colliding_check_with_no_workflow_of_its_own_unqualified() {
        let checks = dedupe_checks(vec![
            entry("build", S::Success, Some(""), Some("2026-08-11T16:00:00Z")),
            entry(
                "build",
                S::Failure,
                Some("CI"),
                Some("2026-08-11T16:00:00Z"),
            ),
        ]);
        assert_eq!(
            checks
                .iter()
                .map(|c| c.name.as_str().to_owned())
                .collect::<Vec<_>>(),
            vec!["build".to_string(), "CI / build".to_string()]
        );
    }
}

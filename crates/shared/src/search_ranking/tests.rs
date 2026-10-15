//! Port of upstream/packages/shared/src/searchRanking.test.ts

use super::*;

fn assert_normalized(input: &str, expected: &str) {
    assert_eq!(normalize_search_query(input, None), expected);
}

#[test]
fn normalize_trims_and_lowercases_queries() {
    assert_normalized("  UI  ", "ui");
}

#[test]
fn normalize_can_strip_leading_trigger_characters() {
    let pattern = regex::Regex::new(r"^\$+").expect("valid pattern");
    assert_eq!(
        normalize_search_query(
            "  $ui",
            Some(&NormalizeSearchQueryOptions { trim_leading_pattern: Some(&pattern) })
        ),
        "ui"
    );
}

#[test]
fn prefers_exact_matches_over_broader_contains_matches() {
    assert_eq!(
        score_query_match(&ScoreQueryMatchInput {
            value: "ui",
            query: "ui",
            exact_base: 0,
            prefix_base: Some(10),
            boundary_base: None,
            includes_base: Some(20),
            fuzzy_base: None,
            boundary_markers: None,
        }),
        Some(0)
    );

    assert!(
        score_query_match(&ScoreQueryMatchInput {
            value: "building native ui",
            query: "ui",
            exact_base: 0,
            prefix_base: Some(10),
            boundary_base: Some(20),
            includes_base: Some(30),
            fuzzy_base: None,
            boundary_markers: None,
        })
        .expect("matches")
            > 0
    );
}

#[test]
fn treats_boundary_matches_as_stronger_than_generic_contains_matches() {
    let boundary_score = score_query_match(&ScoreQueryMatchInput {
        value: "gh-fix-ci",
        query: "fix",
        exact_base: 0,
        prefix_base: Some(10),
        boundary_base: Some(20),
        includes_base: Some(30),
        fuzzy_base: None,
        boundary_markers: Some(&["-"]),
    });
    let contains_score = score_query_match(&ScoreQueryMatchInput {
        value: "highfixci",
        query: "fix",
        exact_base: 0,
        prefix_base: Some(10),
        boundary_base: Some(20),
        includes_base: Some(30),
        fuzzy_base: None,
        boundary_markers: Some(&["-"]),
    });

    let boundary_score = boundary_score.expect("matches");
    let contains_score = contains_score.expect("matches");
    assert!(boundary_score < contains_score);
}

#[test]
fn scores_tighter_subsequences_ahead_of_looser_ones() {
    let compact = score_subsequence_match("ghfixci", "gfc");
    let spread = score_subsequence_match("github-fix-ci", "gfc");

    assert!(compact.expect("matches") < spread.expect("matches"));
}

#[test]
fn keeps_the_best_ranked_candidates_within_the_limit() {
    let mut ranked = vec![
        RankedSearchResult { item: "b", score: 20, tie_breaker: "b".to_owned() },
        RankedSearchResult { item: "d", score: 40, tie_breaker: "d".to_owned() },
    ];

    insert_ranked_search_result(
        &mut ranked,
        RankedSearchResult { item: "a", score: 10, tie_breaker: "a".to_owned() },
        2,
    );
    insert_ranked_search_result(
        &mut ranked,
        RankedSearchResult { item: "c", score: 30, tie_breaker: "c".to_owned() },
        2,
    );

    assert_eq!(ranked.into_iter().map(|entry| entry.item).collect::<Vec<_>(), ["a", "b"]);
}

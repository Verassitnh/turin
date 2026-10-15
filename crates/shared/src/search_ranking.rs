//! Port of upstream/packages/shared/src/searchRanking.ts

use std::cmp::Ordering;

use turin_contracts::base_schemas::js_trim;

use crate::js_string::{locale_compare, utf16_len};

pub struct RankedSearchResult<T> {
    pub item: T,
    pub score: i64,
    pub tie_breaker: String,
}

pub struct NormalizeSearchQueryOptions<'a> {
    /// Trimmed from the leading edge after trimming whitespace.
    pub trim_leading_pattern: Option<&'a regex::Regex>,
}

pub fn normalize_search_query(
    input: &str,
    options: Option<&NormalizeSearchQueryOptions<'_>>,
) -> String {
    let trimmed = js_trim(input);
    if trimmed.is_empty() {
        return String::new();
    }
    match options.and_then(|options| options.trim_leading_pattern) {
        Some(pattern) => {
            // Upstream: trimmed.replace(pattern, "").toLowerCase(); a
            // non-global JS regex replaces the first match only.
            pattern.replace(trimmed, "").to_lowercase()
        }
        None => trimmed.to_lowercase(),
    }
}

pub fn score_subsequence_match(value: &str, query: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }

    // Upstream indexes both strings by UTF-16 code unit; compare units.
    let value: Vec<u16> = value.encode_utf16().collect();
    let query: Vec<u16> = query.encode_utf16().collect();
    let mut query_index = 0usize;
    let mut first_match_index: i64 = -1;
    let mut previous_match_index: i64 = -1;
    let mut gap_penalty: i64 = 0;

    for value_index in 0..value.len() {
        if value[value_index] != query[query_index] {
            continue;
        }

        if first_match_index == -1 {
            first_match_index = value_index as i64;
        }
        if previous_match_index != -1 {
            gap_penalty += value_index as i64 - previous_match_index - 1;
        }

        previous_match_index = value_index as i64;
        query_index += 1;
        if query_index == query.len() {
            let span_penalty = value_index as i64 - first_match_index + 1 - query.len() as i64;
            let length_penalty = 64.min(value.len() as i64 - query.len() as i64);
            return Some(first_match_index * 2 + gap_penalty * 3 + span_penalty + length_penalty);
        }
    }

    None
}

fn length_penalty(value: &str, query: &str) -> i64 {
    64.min(0.max(utf16_len(value) as i64 - utf16_len(query) as i64))
}

fn find_boundary_match_index(value: &str, query: &str, boundary_markers: &[&str]) -> Option<i64> {
    let mut best_index: Option<i64> = None;

    for marker in boundary_markers {
        let needle = format!("{marker}{query}");
        if let Some(index) = value.find(&needle) {
            // Upstream: index + marker.length, all in UTF-16 units.
            let unit_index = utf16_len(&value[..index]) as i64 + utf16_len(marker) as i64;
            if best_index.is_none_or(|best| unit_index < best) {
                best_index = Some(unit_index);
            }
        }
    }

    best_index
}

pub struct ScoreQueryMatchInput<'a> {
    pub value: &'a str,
    pub query: &'a str,
    pub exact_base: i64,
    pub prefix_base: Option<i64>,
    pub boundary_base: Option<i64>,
    pub includes_base: Option<i64>,
    pub fuzzy_base: Option<i64>,
    pub boundary_markers: Option<&'a [&'a str]>,
}

/// Scores how well `value` matches `query` using tiered match strategies.
///
/// **Expects pre-normalized inputs**: both `value` and `query` must already be
/// trimmed and lowercased (e.g. via [`normalize_search_query`]).
pub fn score_query_match(input: &ScoreQueryMatchInput<'_>) -> Option<i64> {
    let value = input.value;
    let query = input.query;

    if value.is_empty() || query.is_empty() {
        return None;
    }

    if value == query {
        return Some(input.exact_base);
    }

    if let Some(prefix_base) = input.prefix_base
        && value.starts_with(query)
    {
        return Some(prefix_base + length_penalty(value, query));
    }

    if let Some(boundary_base) = input.boundary_base {
        let default_markers: &[&str] = &[" ", "-", "_", "/"];
        let markers = input.boundary_markers.unwrap_or(default_markers);
        if let Some(boundary_index) = find_boundary_match_index(value, query, markers) {
            return Some(boundary_base + boundary_index * 2 + length_penalty(value, query));
        }
    }

    if let Some(includes_base) = input.includes_base
        && let Some(includes_index) = value.find(query)
    {
        let unit_index = utf16_len(&value[..includes_index]) as i64;
        return Some(includes_base + unit_index * 2 + length_penalty(value, query));
    }

    if let Some(fuzzy_base) = input.fuzzy_base
        && let Some(fuzzy_score) = score_subsequence_match(value, query)
    {
        return Some(fuzzy_base + fuzzy_score);
    }

    None
}

fn compare_ranked_search_results<T>(
    left: &RankedSearchResult<T>,
    right: &RankedSearchResult<T>,
) -> Ordering {
    let score_delta = left.score.cmp(&right.score);
    if score_delta != Ordering::Equal {
        return score_delta;
    }
    locale_compare(&left.tie_breaker, &right.tie_breaker)
}

fn find_insertion_index<T>(
    ranked_entries: &[RankedSearchResult<T>],
    candidate: &RankedSearchResult<T>,
) -> usize {
    let mut low = 0usize;
    let mut high = ranked_entries.len();

    while low < high {
        let middle = low + (high - low) / 2;
        match ranked_entries.get(middle) {
            None => break,
            Some(current) => {
                if compare_ranked_search_results(candidate, current) == Ordering::Less {
                    high = middle;
                } else {
                    low = middle + 1;
                }
            }
        }
    }

    low
}

/// Inserts `candidate` into `ranked_entries` (best-first, capped at `limit`),
/// dropping the worst entry when the limit is reached — JS `splice` semantics.
pub fn insert_ranked_search_result<T>(
    ranked_entries: &mut Vec<RankedSearchResult<T>>,
    candidate: RankedSearchResult<T>,
    limit: usize,
) {
    if limit == 0 {
        return;
    }

    let insertion_index = find_insertion_index(ranked_entries, &candidate);
    if ranked_entries.len() < limit {
        ranked_entries.insert(insertion_index, candidate);
        return;
    }

    if insertion_index >= limit {
        return;
    }

    ranked_entries.insert(insertion_index, candidate);
    ranked_entries.pop();
}

#[cfg(test)]
mod tests;

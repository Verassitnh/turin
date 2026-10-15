//! Port of upstream/apps/web/src/components/chat/modelPickerSearch.ts

use crate::composer::search_ranking::{ScoreQueryMatchInput, normalize_search_query, score_query_match};

pub struct ModelPickerSearchable<'a> {
    /// Driver kind: indexed so "codex" still matches a Codex Personal instance.
    pub driver_kind: &'a str,
    /// Instance display name (e.g. "Codex Personal"), indexed as a search field.
    pub provider_display_name: &'a str,
    pub name: &'a str,
    pub short_name: Option<&'a str>,
    pub sub_provider: Option<&'a str>,
    pub is_favorite: bool,
}

const MODEL_PICKER_FAVORITE_SCORE_BOOST: i64 = 24;

fn normalize(value: &str) -> String {
    normalize_search_query(value, None)
}

/// `buildModelPickerSearchText`.
pub fn build_model_picker_search_text(model: &ModelPickerSearchable<'_>) -> String {
    let joined = [
        Some(model.name),
        model.short_name,
        model.sub_provider,
        Some(model.driver_kind),
        Some(model.provider_display_name),
    ]
    .into_iter()
    .flatten()
    .filter(|value| !value.is_empty())
    .collect::<Vec<_>>()
    .join(" ");
    normalize(&joined)
}

fn search_fields(model: &ModelPickerSearchable<'_>) -> Vec<String> {
    let mut fields = vec![normalize(model.name)];
    if let Some(short) = model.short_name.filter(|s| !s.is_empty()) {
        fields.push(normalize(short));
    }
    if let Some(sub) = model.sub_provider.filter(|s| !s.is_empty()) {
        fields.push(normalize(sub));
    }
    fields.push(normalize(model.driver_kind));
    fields.push(normalize(model.provider_display_name));
    fields.push(build_model_picker_search_text(model));
    fields
}

fn score_token(field: &str, token: &str, field_base: i64) -> Option<i64> {
    score_query_match(&ScoreQueryMatchInput {
        value: field,
        query: token,
        exact_base: field_base,
        prefix_base: Some(field_base + 2),
        boundary_base: Some(field_base + 4),
        includes_base: Some(field_base + 6),
        fuzzy_base: (token.chars().count() >= 3).then_some(field_base + 100),
        boundary_markers: None,
    })
}

/// `scoreModelPickerSearch`: every whitespace-separated token must match some
/// field; the score sums each token's best field score.
pub fn score_model_picker_search(model: &ModelPickerSearchable<'_>, query: &str) -> Option<i64> {
    let normalized = normalize(query);
    let tokens: Vec<&str> = normalized.split_whitespace().collect();
    if tokens.is_empty() {
        return Some(0);
    }
    let fields = search_fields(model);
    let mut score = 0;
    for token in tokens {
        let best = fields
            .iter()
            .enumerate()
            .filter_map(|(index, field)| score_token(field, token, index as i64 * 10))
            .min()?;
        score += best;
    }
    Some(if model.is_favorite { score - MODEL_PICKER_FAVORITE_SCORE_BOOST } else { score })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model<'a>(driver: &'a str, provider: &'a str, name: &'a str) -> ModelPickerSearchable<'a> {
        ModelPickerSearchable {
            driver_kind: driver,
            provider_display_name: provider,
            name,
            short_name: None,
            sub_provider: None,
            is_favorite: false,
        }
    }

    fn copilot_opus() -> ModelPickerSearchable<'static> {
        ModelPickerSearchable {
            sub_provider: Some("GitHub Copilot"),
            ..model("opencode", "opencode", "Claude Opus 4.7")
        }
    }

    #[test]
    fn builds_provider_agnostic_search_text_from_generic_fields() {
        assert_eq!(
            build_model_picker_search_text(&copilot_opus()),
            "claude opus 4.7 github copilot opencode opencode"
        );
    }

    #[test]
    fn matches_typo_tolerant_multi_token_queries() {
        assert!(score_model_picker_search(&copilot_opus(), "coplt op").is_some());
    }

    #[test]
    fn rejects_results_when_any_query_token_does_not_match() {
        assert_eq!(score_model_picker_search(&model("codex", "codex", "GPT-5 Codex"), "coplt op"), None);
    }

    #[test]
    fn ranks_exact_token_matches_ahead_of_fuzzier_matches() {
        let exact = score_model_picker_search(&copilot_opus(), "copilot opus").unwrap();
        let fuzzy = score_model_picker_search(&copilot_opus(), "coplt op").unwrap();
        assert!(exact < fuzzy);
    }

    #[test]
    fn gives_favorite_models_a_strong_enough_boost_for_partial_queries() {
        let favorite = ModelPickerSearchable { is_favorite: true, ..model("claudeAgent", "Claude", "Claude Opus 4.7") };
        let plain = model("cursor", "Cursor", "Opus 4.5");
        assert!(score_model_picker_search(&favorite, "opu").unwrap() < score_model_picker_search(&plain, "opu").unwrap());
    }

    #[test]
    fn does_not_let_the_favorite_boost_outrank_clearly_better_textual_matches() {
        let favorite = ModelPickerSearchable { is_favorite: true, ..model("claudeAgent", "Claude", "Claude Opus 4.7") };
        let exact = model("cursor", "Cursor", "Opus 4.7");
        assert!(score_model_picker_search(&exact, "opus 4.7").unwrap() < score_model_picker_search(&favorite, "opus 4.7").unwrap());
    }

    #[test]
    fn matches_a_custom_instances_display_name_against_its_models() {
        assert!(score_model_picker_search(&model("codex", "Codex Personal", "GPT-5 Codex"), "personal").is_some());
    }

    #[test]
    fn an_empty_query_scores_zero() {
        assert_eq!(score_model_picker_search(&model("codex", "Codex", "GPT"), "  "), Some(0));
    }
}

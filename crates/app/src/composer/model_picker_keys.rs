//! Port of upstream/apps/web/src/components/chat/modelPickerKeys.ts

const MODEL_KEY_PREFIX: &str = "model:";
const LEGACY_SECTION_KEY_PREFIX: &str = "legacy-models:";

/// `modelPickerModelKey`: length-prefixed so instance ids containing `:` stay
/// unambiguous.
pub fn model_picker_model_key(instance_id: &str, slug: &str) -> String {
    format!(
        "{MODEL_KEY_PREFIX}{}:{instance_id}{slug}",
        instance_id.chars().count()
    )
}

/// `parseModelPickerModelKey`.
pub fn parse_model_picker_model_key(key: &str) -> Option<(String, String)> {
    let encoded = key.strip_prefix(MODEL_KEY_PREFIX)?;
    let separator = encoded.find(':')?;
    let length_text = &encoded[..separator];
    if length_text.is_empty() || !length_text.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let length: usize = length_text.parse().ok()?;
    let value = &encoded[separator + 1..];
    if length > value.chars().count() {
        return None;
    }
    let split = value
        .char_indices()
        .nth(length)
        .map_or(value.len(), |(at, _)| at);
    Some((value[..split].to_owned(), value[split..].to_owned()))
}

pub fn model_picker_legacy_section_key(instance_id: &str) -> String {
    format!("{LEGACY_SECTION_KEY_PREFIX}{instance_id}")
}

pub fn parse_model_picker_legacy_section_key(key: &str) -> Option<String> {
    key.strip_prefix(LEGACY_SECTION_KEY_PREFIX)
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_model_and_legacy_section_keys_distinct_for_colliding_instance_names() {
        let model_key = model_picker_model_key("legacy-models", "codex");
        let section_key = model_picker_legacy_section_key("codex");
        assert_ne!(model_key, section_key);
        assert_eq!(parse_model_picker_legacy_section_key(&model_key), None);
        assert_eq!(
            parse_model_picker_model_key(&model_key),
            Some(("legacy-models".to_owned(), "codex".to_owned()))
        );
    }

    #[test]
    fn round_trips_instance_ids_and_slugs_containing_separators() {
        let key = model_picker_model_key("custom", "model:\u{fffd}x:y");
        assert_eq!(
            parse_model_picker_model_key(&key),
            Some(("custom".to_owned(), "model:\u{fffd}x:y".to_owned()))
        );
        assert_eq!(parse_model_picker_model_key("model:x:abc"), None);
        assert_eq!(parse_model_picker_model_key("model:9:ab"), None);
        assert_eq!(parse_model_picker_model_key("other"), None);
    }
}

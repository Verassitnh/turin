//! Port of the `extractJsonObject` half of upstream/packages/shared/src/schemaJson.ts.
//!
//! Copied from the T05a `schema_json.rs` branch because `turin-shared` is not on
//! `stage` yet. Dedupe when it lands.

use turin_contracts::js_trim;

/// Pulls the first balanced `{...}` out of a text reply, which models often wrap in prose.
/// Text with no `{` comes back trimmed and unchanged.
pub fn extract_json_object(raw: &str) -> &str {
    let trimmed = js_trim(raw);
    if trimmed.is_empty() {
        return trimmed;
    }
    let Some(start) = trimmed.find('{') else {
        return trimmed;
    };
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escaping = false;
    for (index, char) in trimmed[start..].char_indices() {
        if in_string {
            if escaping {
                escaping = false;
            } else if char == '\\' {
                escaping = true;
            } else if char == '"' {
                in_string = false;
            }
            continue;
        }
        match char {
            '"' => in_string = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &trimmed[start..start + index + char.len_utf8()];
                }
            }
            _ => {}
        }
    }
    &trimmed[start..]
}

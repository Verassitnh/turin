//! Port of upstream/packages/client-runtime/src/state/providerInstanceDisplay.ts
//!
//! How a configured provider instance presents itself in a client: its label,
//! its accent color, and whether its icon carries the account badge.

use turin_contracts::model::provider_display_name;
use turin_contracts::provider_instance::{
    ProviderDriverKind, ProviderInstanceId, default_instance_id_for_driver,
};

use crate::state::contracts::js_trim;

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `.replace(/[_-]+/g, " ")`.
fn dashes_to_spaces(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for c in value.chars() {
        if c == '_' || c == '-' {
            if !in_run {
                out.push(' ');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    out
}

/// Title-case a slug: splits on `_` / `-` and camelCase boundaries, so
/// `codex_personal` becomes "Codex Personal" and `myCustomInstance` becomes
/// "My Custom Instance".
fn humanize_slug(slug: &str) -> String {
    let mut split = String::with_capacity(slug.len() + 4);
    let mut previous: Option<char> = None;
    for c in slug.chars() {
        if previous.is_some_and(|p| p.is_ascii_lowercase()) && c.is_ascii_uppercase() {
            split.push(' ');
        }
        split.push(c);
        previous = Some(c);
    }
    let spaced = dashes_to_spaces(&split);
    let trimmed = js_trim(&spaced);
    let mut out = String::with_capacity(trimmed.len());
    let mut previous_is_word = false;
    for c in trimmed.chars() {
        let word = is_word_char(c);
        if word && !previous_is_word {
            out.push(c.to_ascii_uppercase());
        } else {
            out.push(c);
        }
        previous_is_word = word;
    }
    out
}

/// The `Pick<ServerProvider, "instanceId" | "driver" | "displayName">` the
/// resolver reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderInstanceLabelInput<'a> {
    pub instance_id: &'a ProviderInstanceId,
    pub driver: &'a ProviderDriverKind,
    pub display_name: Option<&'a str>,
}

/// Resolve an instance's label with a tiered priority:
///
///   1. A snapshot `displayName` that differs from the driver's brand label —
///      the server has explicitly named this instance, trust it.
///   2. For non-default instances, a humanized `instanceId` — the server fell
///      back to the driver-level label (the same for every instance of that
///      kind), so the slug is what keeps "Codex" and "Codex Personal" apart.
///   3. The snapshot's `displayName`, or the brand label from contracts.
pub fn resolve_provider_instance_display_name(snapshot: ProviderInstanceLabelInput<'_>) -> String {
    let trimmed_snapshot_name = snapshot.display_name.map(js_trim);
    let kind_label = provider_display_name(snapshot.driver.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| humanize_slug(snapshot.driver.as_str()));
    if let Some(name) = trimmed_snapshot_name
        && !name.is_empty()
        && name != kind_label
    {
        return name.to_owned();
    }
    if *snapshot.instance_id != default_instance_id_for_driver(snapshot.driver) {
        let humanized = humanize_slug(snapshot.instance_id.as_str());
        if !humanized.is_empty() {
            return humanized;
        }
    }
    match trimmed_snapshot_name {
        Some(name) if !name.is_empty() => name.to_owned(),
        _ => kind_label,
    }
}

/// Turn a display name into up to two initials for the badge: the first two
/// characters of a single word, or the first character of each of the first
/// two words. Iterates by code point so an emoji never splits.
pub fn provider_instance_initials(label: &str) -> String {
    let spaced = dashes_to_spaces(label);
    let words: Vec<&str> = spaced
        .split(char::is_whitespace)
        .filter(|w| !w.is_empty())
        .collect();
    match words.as_slice() {
        [] => String::new(),
        [word] => word.chars().take(2).collect::<String>().to_uppercase(),
        words => words
            .iter()
            .take(2)
            .filter_map(|word| word.chars().next())
            .flat_map(char::to_uppercase)
            .collect(),
    }
}

/// Only `#rrggbb` accent colors render; anything else is treated as unset.
pub fn normalize_provider_accent_color(value: Option<&str>) -> Option<String> {
    let trimmed = js_trim(value?);
    if trimmed.is_empty() {
        return None;
    }
    let hex = trimmed.strip_prefix('#')?;
    (hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit())).then(|| trimmed.to_owned())
}

/// The fields [`should_show_instance_badge`] reads from a picker entry.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InstanceBadgeEntry {
    pub driver_kind: String,
    pub accent_color: Option<String>,
    pub acp_registry_agent_id: Option<String>,
}

/// Whether an instance's icon carries the account badge: accent color set, or
/// several instances sharing a provider so the brand glyph alone is ambiguous.
/// ACP agents have distinct glyphs even though they share the registry driver.
pub fn should_show_instance_badge<'a>(
    entry: &InstanceBadgeEntry,
    entries: impl IntoIterator<Item = &'a InstanceBadgeEntry>,
) -> bool {
    if entry.accent_color.as_deref().is_some_and(|c| !c.is_empty()) {
        return true;
    }
    let mut shared_provider_count = 0;
    for candidate in entries {
        if candidate.driver_kind != entry.driver_kind {
            continue;
        }
        if entry.driver_kind == "acpRegistry"
            && candidate.acp_registry_agent_id != entry.acp_registry_agent_id
        {
            continue;
        }
        shared_provider_count += 1;
        if shared_provider_count > 1 {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(instance: &str, driver: &str, display_name: Option<&str>) -> String {
        resolve_provider_instance_display_name(ProviderInstanceLabelInput {
            instance_id: &ProviderInstanceId::new(instance).unwrap(),
            driver: &ProviderDriverKind::new(driver).unwrap(),
            display_name,
        })
    }

    fn entry(driver: &str, accent: Option<&str>, agent: Option<&str>) -> InstanceBadgeEntry {
        InstanceBadgeEntry {
            driver_kind: driver.into(),
            accent_color: accent.map(Into::into),
            acp_registry_agent_id: agent.map(Into::into),
        }
    }

    #[test]
    fn keeps_a_snapshot_name_that_differs_from_the_brand_label() {
        assert_eq!(label("codex", "codex", Some("Work")), "Work");
    }

    #[test]
    fn humanizes_a_custom_instance_id_when_the_snapshot_only_carries_the_brand_label() {
        assert_eq!(
            label("codex_personal", "codex", Some("Codex")),
            "Codex Personal"
        );
        assert_eq!(
            label("myCustomInstance", "codex", None),
            "My Custom Instance"
        );
    }

    #[test]
    fn uses_the_brand_label_for_the_default_instance() {
        assert_eq!(label("codex", "codex", None), "Codex");
    }

    #[test]
    fn initials() {
        assert_eq!(provider_instance_initials("Codex"), "CO");
        assert_eq!(provider_instance_initials("Codex Personal"), "CP");
        assert_eq!(
            provider_instance_initials("Codex Personal Backup Account"),
            "CP"
        );
        assert_eq!(provider_instance_initials(""), "");
    }

    #[test]
    fn keeps_an_emoji_whole() {
        assert_eq!(provider_instance_initials("😀 Work"), "😀W");
        assert_eq!(provider_instance_initials("😀"), "😀");
    }

    #[test]
    fn normalizes_accent_colors() {
        assert_eq!(
            normalize_provider_accent_color(Some("#ff8800")).as_deref(),
            Some("#ff8800")
        );
        assert_eq!(
            normalize_provider_accent_color(Some("#FF8800")).as_deref(),
            Some("#FF8800")
        );
        assert_eq!(normalize_provider_accent_color(Some("blue")), None);
        assert_eq!(normalize_provider_accent_color(Some("#fff")), None);
        assert_eq!(normalize_provider_accent_color(None), None);
        assert_eq!(normalize_provider_accent_color(Some("   ")), None);
    }

    #[test]
    fn hides_badges_for_distinct_acp_agents_sharing_the_registry_driver() {
        let mistral = entry("acpRegistry", None, Some("mistral-vibe"));
        let devin = entry("acpRegistry", None, Some("devin"));
        let all = [mistral.clone(), devin.clone()];
        assert!(!should_show_instance_badge(&mistral, &all));
        assert!(!should_show_instance_badge(&devin, &all));
    }

    #[test]
    fn shows_badges_for_multiple_instances_of_the_same_acp_agent() {
        let first = entry("acpRegistry", None, Some("mistral-vibe"));
        assert!(should_show_instance_badge(
            &first,
            &[first.clone(), first.clone()]
        ));
    }

    #[test]
    fn keeps_an_explicit_accent_on_a_single_acp_agent() {
        let only = entry("acpRegistry", Some("#ff8800"), Some("mistral-vibe"));
        assert!(should_show_instance_badge(&only, &[only.clone()]));
    }

    #[test]
    fn distinguishes_registry_instances_whose_agent_identity_is_unavailable() {
        let only = entry("acpRegistry", None, None);
        assert!(should_show_instance_badge(
            &only,
            &[only.clone(), only.clone()]
        ));
    }

    #[test]
    fn shows_the_badge_for_an_accent_or_a_shared_driver() {
        let accented = entry("codex", Some("#ff8800"), None);
        assert!(should_show_instance_badge(&accented, &[accented.clone()]));
        let first = entry("codex", None, None);
        assert!(should_show_instance_badge(
            &first,
            &[first.clone(), first.clone()]
        ));
    }

    #[test]
    fn hides_the_badge_for_a_single_instance_of_a_driver_with_no_accent() {
        let only = entry("codex", None, None);
        let other = entry("claudeAgent", None, None);
        assert!(!should_show_instance_badge(&only, &[only.clone(), other]));
    }
}

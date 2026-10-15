//! Port of upstream/apps/web/src/components/chat/providerIconUtils.ts (the
//! name helpers; the icons are painted by the views).

use regex::Regex;

use crate::composer::provider_state::ProviderModel;

fn strip_leading_qualifier(value: &str, qualifier: Option<&str>) -> String {
    let Some(qualifier) = qualifier.map(str::trim).filter(|q| !q.is_empty()) else {
        return value.to_owned();
    };
    let pattern = format!(r"(?iu)^{}(?:\s*[.:/-]\s*|\s+)", regex::escape(qualifier));
    let Ok(re) = Regex::new(&pattern) else {
        return value.to_owned();
    };
    let stripped = re.replace(value, "");
    let trimmed = stripped.trim();
    if trimmed.is_empty() {
        value.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// `getDisplayModelName`.
pub fn get_display_model_name(model: &ProviderModel, prefer_short_name: bool) -> String {
    let name = match (&model.short_name, prefer_short_name) {
        (Some(short), true) if !short.is_empty() => short,
        _ => &model.name,
    };
    strip_leading_qualifier(name, model.sub_provider.as_deref())
}

/// `getProviderRowLabel`: the second line of a model row, naming where the
/// model comes from. A sub-provider whose name reads as its provider plus a
/// qualifier stands alone.
pub fn get_provider_row_label(provider_display_name: &str, sub_provider: Option<&str>) -> String {
    let provider = provider_display_name.trim();
    let Some(sub) = sub_provider.map(str::trim).filter(|s| !s.is_empty()) else {
        return provider_display_name.to_owned();
    };
    if !provider.is_empty()
        && Regex::new(&format!(r"(?i)^{}\s+\S", regex::escape(provider)))
            .is_ok_and(|re| re.is_match(sub))
    {
        return sub.to_owned();
    }
    format!("{provider_display_name} · {sub}")
}

/// `getTriggerDisplayModelName` / `getTriggerDisplayModelLabel`.
pub fn get_trigger_display_model_name(model: &ProviderModel) -> String {
    get_display_model_name(model, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shows_a_sub_provider_named_after_its_provider_on_its_own() {
        assert_eq!(
            get_provider_row_label("OpenCode", Some("OpenCode Zen")),
            "OpenCode Zen"
        );
        assert_eq!(
            get_provider_row_label("OpenCode", Some("OpenCode Go")),
            "OpenCode Go"
        );
        assert_eq!(
            get_provider_row_label("OpenCode", Some("opencode-go")),
            "OpenCode · opencode-go"
        );
        assert_eq!(
            get_provider_row_label("OpenCode", Some("GitHub Copilot")),
            "OpenCode · GitHub Copilot"
        );
        assert_eq!(get_provider_row_label("Claude", None), "Claude");
    }

    #[test]
    fn drops_a_leading_sub_provider_qualifier_from_the_model_name() {
        let model = ProviderModel {
            sub_provider: Some("OpenCode Zen".into()),
            ..ProviderModel::simple("a/b", "OpenCode Zen: Step 5")
        };
        assert_eq!(get_display_model_name(&model, false), "Step 5");
        let short = ProviderModel {
            short_name: Some("Step".into()),
            ..ProviderModel::simple("a", "Long Name")
        };
        assert_eq!(get_display_model_name(&short, true), "Step");
        assert_eq!(get_display_model_name(&short, false), "Long Name");
    }
}

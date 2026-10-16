//! Port of upstream/apps/web/src/components/chat/TraitsSpeed.tsx
//!
//! The speed a model option set resolves to (`fastMode`, or Codex's
//! `serviceTier`) and the bolt that marks it. The subagent tooltip shows it
//! beside the effort; the composer's own trigger reads the same rules through
//! `build_traits_trigger_display`.

use gpui_kit::{AnyElement, IntoElement as _, ParentElement as _, Styled as _, div, rems};

use crate::assets::{ULTRAFAST_ICON, ZAP_FILL_ICON};
use crate::composer::provider_options::{
    OptionDescriptor, OptionValue, get_provider_option_current_value,
};
use crate::composer::views::chrome::ControlSize;
use crate::theme::AppTheme;
use crate::theme::color::{hex, with_alpha};
use crate::ui::Icon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeedIcon {
    Fast,
    Ultrafast,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitsSpeedDisplay {
    pub label: String,
    pub speed_icon: Option<SpeedIcon>,
}

/// `getTraitsSpeedDisplay`.
pub fn get_traits_speed_display(
    provider: &str,
    descriptor: &OptionDescriptor,
) -> Option<TraitsSpeedDisplay> {
    if let OptionDescriptor::Boolean {
        id, current_value, ..
    } = descriptor
        && id == "fastMode"
    {
        let fast = *current_value == Some(true);
        return Some(TraitsSpeedDisplay {
            label: if fast { "Fast" } else { "Normal" }.to_owned(),
            speed_icon: fast.then_some(SpeedIcon::Fast),
        });
    }
    let OptionDescriptor::Select { id, options, .. } = descriptor else {
        return None;
    };
    if provider != "codex" || id != "serviceTier" {
        return None;
    }
    let current = match get_provider_option_current_value(Some(descriptor), None, None) {
        Some(OptionValue::Str(value)) => Some(value),
        _ => None,
    };
    let fast_tier = options.iter().find(|option| option.label == "Fast");
    let ultrafast_tier = options.iter().find(|option| option.label == "Ultrafast");
    let is_current = |tier: Option<&crate::composer::provider_options::OptionChoice>| {
        tier.is_some_and(|tier| current.as_deref() == Some(tier.id.as_str()))
    };
    if !(((fast_tier.is_some() || ultrafast_tier.is_some())
        && current.as_deref() == Some("default"))
        || is_current(fast_tier)
        || is_current(ultrafast_tier))
    {
        return None;
    }
    Some(TraitsSpeedDisplay {
        label: options
            .iter()
            .find(|option| Some(&option.id) == current.as_ref())
            .map_or_else(|| "Normal".to_owned(), |option| option.label.clone()),
        speed_icon: if is_current(ultrafast_tier) {
            Some(SpeedIcon::Ultrafast)
        } else if is_current(fast_tier) {
            Some(SpeedIcon::Fast)
        } else {
            None
        },
    })
}

/// `TraitsSpeedIcon`'s screen-reader text.
pub fn traits_speed_icon_label(speed_icon: SpeedIcon) -> &'static str {
    match speed_icon {
        SpeedIcon::Ultrafast => "Ultrafast mode on",
        SpeedIcon::Fast => "Fast mode on",
    }
}

/// The bolt's tone: `fill-current opacity-80`, `text-current` at `xs`, else
/// Claude's `#d97757` or `text-foreground`.
fn speed_icon_color(
    provider: &str,
    size: ControlSize,
    current: gpui_kit::Hsla,
    theme: &AppTheme,
) -> gpui_kit::Hsla {
    let base = if size == ControlSize::Xs {
        current
    } else if provider == "claudeAgent" {
        hex(0xd97757).into()
    } else {
        theme.foreground
    };
    with_alpha(base, 0.8).into()
}

/// `TraitsSpeedIcon`: `size-3` at `xs`, else `size-4`.
pub fn traits_speed_icon(
    provider: &str,
    speed_icon: SpeedIcon,
    size: ControlSize,
    current_color: gpui_kit::Hsla,
    theme: &AppTheme,
) -> AnyElement {
    let path = match speed_icon {
        SpeedIcon::Ultrafast => ULTRAFAST_ICON,
        SpeedIcon::Fast => ZAP_FILL_ICON,
    };
    div()
        .flex_shrink_0()
        .child(
            Icon::path(path)
                .size(if size == ControlSize::Xs {
                    rems(0.75)
                } else {
                    rems(1.)
                })
                .color(speed_icon_color(provider, size, current_color, theme)),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::composer::provider_options::OptionChoice;

    fn choice(id: &str, label: &str) -> OptionChoice {
        OptionChoice {
            id: id.to_owned(),
            label: label.to_owned(),
            description: None,
            is_default: false,
        }
    }

    fn fast_mode(value: Option<bool>) -> OptionDescriptor {
        OptionDescriptor::Boolean {
            id: "fastMode".to_owned(),
            label: "Fast mode".to_owned(),
            description: None,
            current_value: value,
        }
    }

    fn service_tier(current: Option<&str>, labels: &[(&str, &str)]) -> OptionDescriptor {
        OptionDescriptor::Select {
            id: "serviceTier".to_owned(),
            label: "Speed".to_owned(),
            description: None,
            options: labels.iter().map(|(id, label)| choice(id, label)).collect(),
            current_value: current.map(str::to_owned),
            prompt_injected_values: Vec::new(),
        }
    }

    #[test]
    fn fast_mode_reads_fast_or_normal() {
        assert_eq!(
            get_traits_speed_display("cursor", &fast_mode(Some(true))),
            Some(TraitsSpeedDisplay {
                label: "Fast".into(),
                speed_icon: Some(SpeedIcon::Fast)
            })
        );
        for value in [Some(false), None] {
            assert_eq!(
                get_traits_speed_display("cursor", &fast_mode(value)),
                Some(TraitsSpeedDisplay {
                    label: "Normal".into(),
                    speed_icon: None
                })
            );
        }
    }

    #[test]
    fn codex_service_tier_maps_fast_and_ultrafast() {
        let tiers = [
            ("default", "Normal"),
            ("fast", "Fast"),
            ("ultra", "Ultrafast"),
        ];
        assert_eq!(
            get_traits_speed_display("codex", &service_tier(Some("fast"), &tiers)),
            Some(TraitsSpeedDisplay {
                label: "Fast".into(),
                speed_icon: Some(SpeedIcon::Fast)
            })
        );
        assert_eq!(
            get_traits_speed_display("codex", &service_tier(Some("ultra"), &tiers)),
            Some(TraitsSpeedDisplay {
                label: "Ultrafast".into(),
                speed_icon: Some(SpeedIcon::Ultrafast)
            })
        );
        assert_eq!(
            get_traits_speed_display("codex", &service_tier(Some("default"), &tiers)),
            Some(TraitsSpeedDisplay {
                label: "Normal".into(),
                speed_icon: None
            })
        );
    }

    #[test]
    fn other_providers_and_plain_tiers_have_no_speed() {
        let tiers = [("default", "Normal"), ("fast", "Fast")];
        assert_eq!(
            get_traits_speed_display("claudeAgent", &service_tier(Some("fast"), &tiers)),
            None
        );
        let plain = [("low", "Low"), ("high", "High")];
        assert_eq!(
            get_traits_speed_display("codex", &service_tier(Some("low"), &plain)),
            None
        );
    }

    #[test]
    fn the_icon_names_its_mode() {
        assert_eq!(traits_speed_icon_label(SpeedIcon::Fast), "Fast mode on");
        assert_eq!(
            traits_speed_icon_label(SpeedIcon::Ultrafast),
            "Ultrafast mode on"
        );
    }
}

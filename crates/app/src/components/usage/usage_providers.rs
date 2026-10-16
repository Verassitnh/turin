//! Port of upstream/apps/web/src/components/usage/usageProviders.ts

use gpui_kit::Hsla;
use turin_contracts::usage::UsageProviderKind;
use turin_shared::usage_merge::ProviderTotals;

use crate::theme::AppTheme;
use crate::theme::color::{MixSpace, color_mix, hex};

/// A chart color. Upstream writes these as CSS (`var(--contrast-foreground)`,
/// `color-mix(in oklab, …)`), which only resolve against the active theme, so
/// they stay symbolic until render time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ChartColor {
    /// `var(--contrast-foreground)`.
    ContrastForeground,
    /// A fixed `#rrggbb`.
    Hex(u32),
    /// `color-mix(in oklab, var(--contrast-foreground) P%, var(--background))`.
    ContrastForegroundMix(u8),
    /// `color-mix(in oklab, var(--foreground) P%, var(--background))`: a
    /// neutral step between background and foreground.
    Ink(u8),
}

impl ChartColor {
    pub fn resolve(self, theme: &AppTheme) -> Hsla {
        match self {
            // Foreground tokens hold the `--contrast-*` value (theme module docs).
            Self::ContrastForeground => theme.foreground,
            Self::Hex(value) => hex(value).into(),
            Self::ContrastForegroundMix(percent) | Self::Ink(percent) => color_mix(
                MixSpace::Oklab,
                theme.foreground,
                f32::from(percent) / 100.,
                theme.background,
            )
            .into(),
        }
    }
}

pub struct UsageProviderPresentation {
    pub label: &'static str,
    pub color: ChartColor,
    /// `ProviderDriverKind` slug of the harness that produces this usage.
    pub driver_kind: &'static str,
}

/// Stable provider reading order across charts, summaries, tables, and hover
/// rows. Upstream derives it from the declaration order of
/// `PROVIDER_PRESENTATION`; adding a provider needs its contract support and
/// one arm in [`presentation`] plus an entry here.
pub const PROVIDER_ORDER: [UsageProviderKind; 7] = [
    UsageProviderKind::Codex,
    UsageProviderKind::Claude,
    UsageProviderKind::Grok,
    UsageProviderKind::Cursor,
    UsageProviderKind::Opencode,
    UsageProviderKind::Antigravity,
    UsageProviderKind::Pi,
];

/// Exhaustive presentation for providers supported by the usage contract.
pub fn presentation(provider: UsageProviderKind) -> UsageProviderPresentation {
    match provider {
        UsageProviderKind::Codex => UsageProviderPresentation {
            label: "Codex",
            color: ChartColor::ContrastForeground,
            driver_kind: "codex",
        },
        UsageProviderKind::Claude => UsageProviderPresentation {
            label: "Claude Code",
            color: ChartColor::Hex(0xd97757),
            driver_kind: "claudeAgent",
        },
        UsageProviderKind::Grok => UsageProviderPresentation {
            label: "Grok Build",
            // Contrast-aware neutral between the Codex series and muted chart chrome.
            color: ChartColor::ContrastForegroundMix(72),
            driver_kind: "grok",
        },
        UsageProviderKind::Cursor => UsageProviderPresentation {
            label: "Cursor",
            color: ChartColor::Hex(0x8b8b8b),
            driver_kind: "cursor",
        },
        UsageProviderKind::Opencode => UsageProviderPresentation {
            label: "OpenCode",
            color: ChartColor::Hex(0x5b9bbd),
            driver_kind: "opencode",
        },
        UsageProviderKind::Antigravity => UsageProviderPresentation {
            label: "Antigravity",
            color: ChartColor::Hex(0x8c7bd1),
            driver_kind: "antigravity",
        },
        UsageProviderKind::Pi => UsageProviderPresentation {
            label: "Pi",
            color: ChartColor::Hex(0x4d9f8a),
            driver_kind: "pi",
        },
    }
}

pub fn provider_label(provider: UsageProviderKind) -> &'static str {
    presentation(provider).label
}

/// Index in [`PROVIDER_ORDER`] (`PROVIDER_ORDER.indexOf`).
pub fn provider_slot(provider: UsageProviderKind) -> usize {
    PROVIDER_ORDER
        .iter()
        .position(|candidate| *candidate == provider)
        .expect("PROVIDER_ORDER lists every provider")
}

/// Wire name (`UsageProviderKind` literal), used as the stored key.
pub fn provider_key(provider: UsageProviderKind) -> &'static str {
    match provider {
        UsageProviderKind::Claude => "claude",
        UsageProviderKind::Codex => "codex",
        UsageProviderKind::Grok => "grok",
        UsageProviderKind::Cursor => "cursor",
        UsageProviderKind::Opencode => "opencode",
        UsageProviderKind::Antigravity => "antigravity",
        UsageProviderKind::Pi => "pi",
    }
}

pub fn provider_from_key(key: &str) -> Option<UsageProviderKind> {
    PROVIDER_ORDER
        .iter()
        .copied()
        .find(|provider| provider_key(*provider) == key)
}

/// Providers with real activity, independent of the metric currently displayed.
pub fn providers_with_usage(totals: &[ProviderTotals]) -> Vec<UsageProviderKind> {
    PROVIDER_ORDER
        .iter()
        .copied()
        .filter(|provider| {
            totals.iter().any(|entry| {
                entry.provider == *provider && (entry.total_tokens > 0 || entry.cost_usd > 0.)
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn totals(provider: UsageProviderKind, cost_usd: f64, total_tokens: u64) -> ProviderTotals {
        ProviderTotals {
            provider,
            cost_usd,
            total_tokens,
            records: 1,
            sessions: 1,
            cost_share: 0.,
            token_share: 0.,
        }
    }

    #[test]
    fn order_covers_every_provider_once() {
        for provider in PROVIDER_ORDER {
            assert_eq!(PROVIDER_ORDER[provider_slot(provider)], provider);
            assert_eq!(provider_from_key(provider_key(provider)), Some(provider));
        }
        assert_eq!(provider_from_key("nope"), None);
    }

    #[test]
    fn providers_with_usage_keeps_reading_order_and_skips_idle() {
        let active = providers_with_usage(&[
            totals(UsageProviderKind::Cursor, 0., 10),
            totals(UsageProviderKind::Claude, 1.5, 0),
            totals(UsageProviderKind::Codex, 0., 0),
        ]);
        assert_eq!(
            active,
            vec![UsageProviderKind::Claude, UsageProviderKind::Cursor]
        );
    }
}

//! Port of upstream/apps/web/src/components/chat/ProviderInstanceIcon.tsx
//! (glyph only: the status dot and account badge belong to the model picker).
//!
//! Marks are monochrome SVGs tinted with the provider's text color
//! (`providerTextColor` / the provider packages' light and dark fills). Cut
//! providers have no bundled glyph and fall back to initials, as upstream does
//! for a driver without an icon.

use gpui_kit::{
    AnyElement, App, FontWeight, Hsla, IntoElement as _, ParentElement as _, Rems, Styled as _,
    div, rems,
};

use crate::state::helpers::provider_instance_display::provider_instance_initials;
use crate::theme::ActiveAppTheme;
use crate::theme::color::hex;
use crate::theme::tokens::TEXT_3XS;
use crate::ui::Icon;

/// The bundled glyph for a driver kind.
pub fn provider_icon_path(driver_kind: &str) -> Option<&'static str> {
    match driver_kind {
        "codex" => Some("brand/openai.svg"),
        "claudeAgent" => Some("brand/claude.svg"),
        "cursor" => Some("brand/cursor.svg"),
        "opencode" => Some("brand/opencode.svg"),
        _ => None,
    }
}

/// The glyph's fill. Codex is `text-black dark:text-white`; Cursor and
/// OpenCode carry `--icon-light` / `--icon-dark` from their package fills.
pub fn provider_icon_color(driver_kind: &str, cx: &App) -> Hsla {
    let theme = cx.app_theme();
    let dark = theme.is_dark();
    let pick = |light: u32, dark_fill: u32| -> Hsla {
        if dark {
            hex(dark_fill).into()
        } else {
            hex(light).into()
        }
    };
    match driver_kind {
        "codex" => pick(0x000000, 0xffffff),
        "claudeAgent" => hex(0xd97757).into(),
        "cursor" => pick(0x26251e, 0xedecec),
        "opencode" => pick(0x211e1e, 0xf1ecec),
        "antigravity" => hex(0x5b87bf).into(),
        _ => theme.foreground,
    }
}

/// `ProviderInstanceIcon`: the glyph in a `size`-sized slot, or the display
/// name's initials when the driver has no bundled mark.
pub fn provider_instance_icon(
    driver_kind: &str,
    display_name: &str,
    size: Rems,
    tint: Option<Hsla>,
    cx: &App,
) -> AnyElement {
    let color = tint.unwrap_or_else(|| provider_icon_color(driver_kind, cx));
    match provider_icon_path(driver_kind) {
        Some(path) => Icon::path(path).size(size).color(color).into_any_element(),
        None => div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .justify_center()
            .size(size)
            .text_size(TEXT_3XS.size)
            .line_height(rems(0.625))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(tint.unwrap_or(cx.app_theme().foreground))
            .child(provider_instance_initials(display_name))
            .into_any_element(),
    }
}

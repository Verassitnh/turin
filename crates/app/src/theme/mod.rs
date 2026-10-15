//! Port of upstream/apps/web/src/index.css (the semantic token layer: `:root`
//! defaults, `html[data-theme-id]` palette mapping, `--contrast-*`) and of
//! `applyThemePalette` in upstream/apps/web/src/themePalette.ts, which
//! writes a palette's roles into those tokens.
//!
//! Turin's design system: the tokens are resolved once into concrete colors
//! ([`AppTheme`]) and pushed into gpui-component's [`Theme`] so built-in
//! components paint with them too.
//!
//! - [`theme_palettes`]: port of `packages/shared/src/themePalettes.ts`.
//! - [`theme_palette`]: port of `apps/web/src/themePalette.ts` resolution.
//! - [`theme_preview`]: port of `packages/shared/src/themePreview.ts`.
//! - [`color`]: color parsing / `color-mix()`.
//! - [`sidebar_scope`]: the `[data-app-sidebar]` token scope.
//! - [`tokens`]: radii, spacing, text sizes, shadows from `index.css`.
//! - [`appearance_fonts`], [`appearance_contrast`]: Settings → Appearance logic.
//! - [`component_theme`]: [`AppTheme`] → gpui-component `ThemeConfig`.
//!
//! UI code reads semantic tokens with `cx.app_theme()`; the names follow the
//! Tailwind color utilities upstream uses (`bg-sidebar` → `sidebar`,
//! `text-muted-foreground` → `muted_foreground`, …). Foreground tokens hold
//! the `--contrast-*` value, as the Tailwind `--color-*` aliases do.

pub mod appearance_contrast;
pub mod appearance_fonts;
pub mod color;
pub mod component_theme;
pub mod sidebar_scope;
pub mod theme_palette;
pub mod theme_palettes;
pub mod theme_preview;
pub mod tokens;

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::{App, Global, Hsla, Rgba, Window, WindowAppearance, px};

use appearance_contrast::AppearanceContrast;
use appearance_fonts::{AppearanceFontPreferences, ResolvedFonts, resolve_appearance_fonts};
use color::{MixSpace, color, color_mix, with_alpha};
use sidebar_scope::SidebarScope;
use theme_palette::{
    ThemePreferenceMode, get_standard_theme_colors, get_theme_colors_for_mode,
    get_theme_definition, resolve_theme_appearance,
};
use theme_palettes::{ThemeAppearance, ThemeColors, ThemeDefinition};
use tokens::tw;

/// Which palette the user picked and how it follows the OS.
#[derive(Clone, Debug, PartialEq)]
pub struct AppearancePreferences {
    /// Upstream `ThemePreference`: `"system"` / `"light"` / `"dark"` (the
    /// standard palette) or a theme id (legacy aliases accepted).
    pub theme: String,
    /// Upstream `THEME_APPEARANCE_MODE_STORAGE_KEY`; `None` derives it from
    /// `theme` (`"system"` follows the OS).
    pub appearance_mode: Option<ThemePreferenceMode>,
    pub contrast: u32,
    pub fonts: AppearanceFontPreferences,
}

impl Default for AppearancePreferences {
    fn default() -> Self {
        Self {
            theme: "system".to_string(),
            appearance_mode: None,
            contrast: appearance_contrast::DEFAULT_APPEARANCE_CONTRAST,
            fonts: AppearanceFontPreferences::default(),
        }
    }
}

/// Resolved semantic tokens. Field names mirror the CSS custom properties
/// (`--sidebar-row-hover` → `sidebar_row_hover`).
#[derive(Clone, Debug, PartialEq)]
pub struct AppTheme {
    /// The applied theme's id; `None` is the standard look (no
    /// `data-theme-id`).
    pub theme_id: Option<&'static str>,
    pub appearance: ThemeAppearance,
    pub fonts: ResolvedFonts,

    pub background: Hsla,
    pub app_chrome_background: Hsla,
    pub toolbar_background: Hsla,
    pub toolbar_foreground: Hsla,
    pub toolbar_border: Hsla,
    pub toolbar_control: Hsla,
    pub toolbar_control_foreground: Hsla,
    pub toolbar_control_hover: Hsla,
    pub surface_raised: Hsla,
    pub foreground: Hsla,
    pub card: Hsla,
    pub card_foreground: Hsla,
    pub popover: Hsla,
    pub popover_foreground: Hsla,
    pub primary: Hsla,
    pub primary_foreground: Hsla,
    pub secondary: Hsla,
    pub secondary_foreground: Hsla,
    pub muted: Hsla,
    pub muted_foreground: Hsla,
    pub placeholder: Hsla,
    pub secondary_label: Hsla,
    pub icon_muted: Hsla,
    /// `--accent`: the quiet hover/selected surface (theme role `accentSurface`).
    pub accent: Hsla,
    pub accent_foreground: Hsla,
    /// The theme's brand accent (role `accent`): selection, focus, artwork.
    pub theme_accent: Hsla,
    pub theme_accent_foreground: Hsla,
    pub message_surface: Hsla,
    pub message_foreground: Hsla,
    pub message_action: Hsla,
    pub message_action_foreground: Hsla,
    pub message_action_hover: Hsla,
    pub error: Hsla,
    pub error_foreground: Hsla,
    pub error_surface: Hsla,
    pub tool_error_icon: Hsla,
    pub destructive: Hsla,
    pub destructive_foreground: Hsla,
    pub warning: Hsla,
    pub warning_foreground: Hsla,
    pub warning_surface: Hsla,
    pub update: Hsla,
    pub update_foreground: Hsla,
    pub update_surface: Hsla,
    /// Success and info stay independent of themes (index.css).
    pub info: Hsla,
    pub info_foreground: Hsla,
    pub success: Hsla,
    pub success_foreground: Hsla,
    /// `--diff-addition-foreground` / `--diff-deletion-foreground`
    /// (`light-dark(emerald-600, emerald-400)`, red likewise).
    pub diff_addition_foreground: Hsla,
    pub diff_deletion_foreground: Hsla,
    pub border: Hsla,
    pub input: Hsla,
    pub ring: Hsla,
    pub code_background: Hsla,
    pub code_foreground: Hsla,
    pub search_match_background: Hsla,
    pub search_match_foreground: Hsla,
    pub search_match_active_background: Hsla,
    pub search_match_active_foreground: Hsla,
    pub sidebar: Hsla,
    pub sidebar_foreground: Hsla,
    pub sidebar_muted_foreground: Hsla,
    pub sidebar_control_surface: Hsla,
    pub sidebar_row_hover: Hsla,
    pub sidebar_row_active: Hsla,
    pub sidebar_row_selected: Hsla,
    pub sidebar_border: Hsla,
    /// `--sidebar-icon-color`: 60% sidebar-muted-foreground over the sidebar.
    pub sidebar_icon_color: Hsla,
    /// `--app-scrollbar-thumb` / `--app-scrollbar-thumb-hover`.
    pub scrollbar_thumb: Hsla,
    pub scrollbar_thumb_hover: Hsla,
    /// Tokens inside the sidebar (`[data-app-sidebar]`); see [`Self::sidebar_scope`].
    sidebar_scope: SidebarScope,
}

impl Global for AppTheme {}

fn rgba(value: &str) -> Rgba {
    color(value).into()
}

impl AppTheme {
    /// `definition: None` is the standard look. `appearance` should come
    /// from [`resolve_theme_appearance`], which keeps a single-appearance
    /// theme on its own half; any other half falls back to the standard
    /// palette, as upstream clears `data-theme-id` when a theme cannot render.
    pub fn resolve(
        definition: Option<&'static ThemeDefinition>,
        appearance: ThemeAppearance,
        contrast: u32,
        fonts: ResolvedFonts,
    ) -> Self {
        let themed = definition
            .and_then(|definition| get_theme_colors_for_mode(definition, appearance))
            .map(|colors| (definition.map(|d| d.id), colors));
        let (theme_id, palette): (Option<&'static str>, ThemeColors) =
            themed.unwrap_or((None, get_standard_theme_colors(appearance)));
        let dark = appearance.is_dark();
        let contrast = AppearanceContrast::new(contrast);
        let target = if dark { tw::WHITE } else { tw::BLACK };
        let c = |value: &str| -> Hsla { color(value) };
        let fg = |value: &str, backdrop: &str| -> Hsla {
            contrast
                .foreground(rgba(value), rgba(backdrop), target)
                .into()
        };
        let line = |value: &str, foreground: &str| -> Hsla {
            contrast.border(rgba(value), rgba(foreground)).into()
        };
        let p = &palette;

        let sidebar_muted_foreground = fg(p.sidebar_muted_foreground, p.sidebar);
        let sidebar_icon_color: Hsla = color_mix(
            MixSpace::Srgb,
            sidebar_muted_foreground,
            0.6,
            rgba(p.sidebar),
        )
        .into();

        let (info, info_foreground, success, success_foreground) = if dark {
            (tw::BLUE_500, tw::BLUE_400, tw::EMERALD_500, tw::EMERALD_400)
        } else {
            (tw::BLUE_500, tw::BLUE_700, tw::EMERALD_500, tw::EMERALD_700)
        };
        let (scrollbar_thumb, scrollbar_thumb_hover): (Hsla, Hsla) = if dark {
            (
                with_alpha(tw::WHITE, 0.08).into(),
                with_alpha(tw::WHITE, 0.12).into(),
            )
        } else {
            (color::hex(0xd9d9d9).into(), color::hex(0xbfbfbf).into())
        };

        Self {
            theme_id,
            appearance,
            fonts,
            background: c(p.canvas),
            app_chrome_background: c(p.chrome),
            toolbar_background: c(p.toolbar),
            toolbar_foreground: fg(p.toolbar_foreground, p.toolbar),
            toolbar_border: line(p.toolbar_border, p.toolbar_foreground),
            toolbar_control: c(p.toolbar_control),
            toolbar_control_foreground: fg(p.toolbar_control_foreground, p.toolbar_control),
            toolbar_control_hover: c(p.toolbar_control_hover),
            surface_raised: c(p.surface_raised),
            foreground: fg(p.text, p.canvas),
            card: c(p.surface),
            card_foreground: fg(p.text, p.surface),
            popover: c(p.surface_overlay),
            popover_foreground: fg(p.text, p.surface_overlay),
            primary: c(p.message_action),
            primary_foreground: c(p.message_action_foreground),
            secondary: c(p.secondary),
            secondary_foreground: fg(p.secondary_foreground, p.secondary),
            muted: c(p.muted),
            muted_foreground: fg(p.muted_foreground, p.canvas),
            placeholder: fg(p.placeholder, p.canvas),
            secondary_label: fg(p.secondary_label, p.canvas),
            icon_muted: fg(p.icon_muted, p.canvas),
            accent: c(p.accent_surface),
            accent_foreground: fg(p.accent_surface_foreground, p.accent_surface),
            theme_accent: c(p.accent),
            theme_accent_foreground: c(p.accent_foreground),
            message_surface: c(p.message_surface),
            message_foreground: fg(p.message_foreground, p.message_surface),
            message_action: c(p.message_action),
            message_action_foreground: c(p.message_action_foreground),
            message_action_hover: c(p.message_action_hover),
            error: c(p.error),
            error_foreground: c(p.error_foreground),
            error_surface: c(p.error_surface),
            tool_error_icon: if theme_id.is_none() {
                if dark {
                    // index.css dark `--tool-error-icon: #fca5a5` (a literal).
                    color::hex(0xfca5a5).into()
                } else {
                    c(p.error)
                }
            } else {
                c(p.error_foreground)
            },
            destructive: c(p.error),
            destructive_foreground: c(p.error_foreground),
            warning: c(p.warning),
            warning_foreground: c(p.warning_foreground),
            warning_surface: c(p.warning_surface),
            update: c(p.update),
            update_foreground: c(p.update_foreground),
            update_surface: c(p.update_surface),
            info: info.into(),
            info_foreground: info_foreground.into(),
            success: success.into(),
            success_foreground: success_foreground.into(),
            diff_addition_foreground: if dark {
                tw::EMERALD_400
            } else {
                tw::EMERALD_600
            }
            .into(),
            diff_deletion_foreground: if dark { tw::RED_400 } else { tw::RED_600 }.into(),
            border: line(p.border, p.text),
            input: line(p.input, p.text),
            ring: c(p.focus),
            code_background: c(p.code_background),
            code_foreground: c(p.code_foreground),
            search_match_background: c(p.search_match_background),
            search_match_foreground: c(p.search_match_foreground),
            search_match_active_background: c(p.search_match_active_background),
            search_match_active_foreground: c(p.search_match_active_foreground),
            sidebar: c(p.sidebar),
            sidebar_foreground: fg(p.sidebar_foreground, p.sidebar),
            sidebar_muted_foreground,
            sidebar_control_surface: c(p.sidebar_control_surface),
            sidebar_row_hover: c(p.sidebar_row_hover),
            sidebar_row_active: c(p.sidebar_row_active),
            sidebar_row_selected: c(p.sidebar_row_selected),
            sidebar_border: line(p.sidebar_border, p.sidebar_foreground),
            sidebar_icon_color,
            scrollbar_thumb,
            scrollbar_thumb_hover,
            sidebar_scope: SidebarScope::resolve(p, theme_id.is_none(), theme_id, dark, contrast),
        }
    }

    /// The `[data-app-sidebar]` token scope: what `text-foreground`,
    /// `text-muted-foreground`, `bg-accent`, `border-border`, … resolve to
    /// for anything rendered inside the sidebar.
    #[allow(dead_code)] // sidebar call sites move to it in T15.
    pub fn sidebar_scope(&self) -> &SidebarScope {
        &self.sidebar_scope
    }

    pub fn is_dark(&self) -> bool {
        self.appearance.is_dark()
    }

    pub fn global(cx: &App) -> &Self {
        cx.global::<Self>()
    }
}

/// `cx.app_theme()` for any context that derefs to [`App`].
pub trait ActiveAppTheme {
    fn app_theme(&self) -> &AppTheme;
}

impl ActiveAppTheme for App {
    #[inline]
    fn app_theme(&self) -> &AppTheme {
        AppTheme::global(self)
    }
}

/// The preferences the theme was last resolved from.
#[derive(Clone, Debug, Default)]
struct ThemeState {
    preferences: AppearancePreferences,
}

impl Global for ThemeState {}

fn system_appearance(appearance: WindowAppearance) -> ThemeAppearance {
    match appearance {
        WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeAppearance::Dark,
        WindowAppearance::Light | WindowAppearance::VibrantLight => ThemeAppearance::Light,
    }
}

fn resolve_fonts(preferences: &AppearanceFontPreferences, cx: &App) -> ResolvedFonts {
    let installed = cx.text_system().all_font_names();
    resolve_appearance_fonts(preferences, |name| installed.iter().any(|f| f == name))
}

/// Installs the default appearance. Call after `gpui_kit::init`.
pub fn init(cx: &mut App) {
    cx.set_global(ThemeState::default());
    apply(None, cx);
}

/// Re-resolves the theme for the current preferences and the given (or app)
/// system appearance, and pushes it into gpui-component.
pub fn apply(window_appearance: Option<WindowAppearance>, cx: &mut App) {
    let preferences = cx.global::<ThemeState>().preferences.clone();
    let definition = get_theme_definition(&preferences.theme);
    let system_dark =
        system_appearance(window_appearance.unwrap_or_else(|| cx.window_appearance())).is_dark();
    let appearance = resolve_theme_appearance(
        &preferences.theme,
        system_dark,
        None,
        preferences.appearance_mode,
    );
    let fonts = resolve_fonts(&preferences.fonts, cx);

    let light = AppTheme::resolve(
        definition,
        ThemeAppearance::Light,
        preferences.contrast,
        fonts.clone(),
    );
    let dark = AppTheme::resolve(
        definition,
        ThemeAppearance::Dark,
        preferences.contrast,
        fonts,
    );
    let active = if appearance.is_dark() { &dark } else { &light };
    cx.set_global(active.clone());

    let light_config = component_theme::theme_config(&light);
    let dark_config = component_theme::theme_config(&dark);
    let size_interface = active.fonts.size_interface;
    Theme::update(cx, |component| {
        component.light_theme = light_config;
        component.dark_theme = dark_config;
    });
    Theme::change(
        if appearance.is_dark() {
            ThemeMode::Dark
        } else {
            ThemeMode::Light
        },
        None,
        cx,
    );
    Theme::update(cx, |component| {
        component.font_size = px(size_interface);
    });
}

/// Replaces the appearance preferences (Settings → Appearance) and re-applies.
pub fn set_preferences(preferences: AppearancePreferences, cx: &mut App) {
    cx.global_mut::<ThemeState>().preferences = preferences;
    apply(None, cx);
}

/// Follow the OS light/dark switch for `window`, as upstream's
/// `prefers-color-scheme` handling does.
pub fn follow_system_appearance(window: &mut Window, cx: &mut App) {
    apply(Some(window.appearance()), cx);
}

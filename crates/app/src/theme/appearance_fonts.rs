//! Port of upstream/apps/web/src/appearanceFonts.ts
//!
//! Font preferences from Settings → Appearance. The default stacks mirror the
//! `--font-sans` / `--font-mono` definitions in `index.css`; a custom family
//! is always prepended to the matching default stack so glyph coverage never
//! regresses.
//!
//! GPUI takes one family per text run rather than a CSS stack, so
//! [`resolve_font_family`] walks a stack and picks the first installed family
//! (CSS generics map to their GPUI/macOS equivalents). Canvas metric probing
//! (`isFontFamilyAvailable`, `isMonospaceFamily`) is replaced by asking the
//! text system for installed family names. Terminal font preferences are cut
//! along with the terminal.

/// Font size preferences, in CSS pixels (upstream contracts/settings.ts). The
/// ranges are deliberately narrow: the interface size scales every rem-based
/// dimension in the app, so the bounds keep layouts intact rather than
/// offering unusable extremes.
pub const MIN_INTERFACE_FONT_SIZE: u32 = 12;
pub const MAX_INTERFACE_FONT_SIZE: u32 = 20;
pub const DEFAULT_INTERFACE_FONT_SIZE: u32 = 16;
pub const MIN_PROMPT_FONT_SIZE: u32 = 12;
pub const MAX_PROMPT_FONT_SIZE: u32 = 20;
pub const DEFAULT_PROMPT_FONT_SIZE: u32 = 14;
pub const MIN_CODE_FONT_SIZE: u32 = 10;
pub const MAX_CODE_FONT_SIZE: u32 = 18;
pub const DEFAULT_CODE_FONT_SIZE: u32 = 13;

pub const DEFAULT_SANS_FONT_STACK: &str =
    r#"-apple-system, BlinkMacSystemFont, "Segoe UI", system-ui, sans-serif"#;

// Concrete names first: some engines alias `ui-monospace` to the
// proportional system UI font, which would break every code surface.
pub const DEFAULT_CODE_FONT_STACK: &str =
    r#""SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace"#;

/// index.css `--font-mono`, what code surfaces use while no custom code
/// font is set (`applyAppearanceFontVariables` removes the override).
pub const INDEX_CSS_MONO_STACK: &str =
    r#"ui-monospace, "SF Mono", "SFMono-Regular", Menlo, Consolas, "Liberation Mono", monospace"#;

/// GPUI's name for the platform UI face (San Francisco on macOS).
pub const SYSTEM_UI_FONT: &str = ".SystemUIFont";

/// CoreText's alias for the system monospaced face (SF Mono), which is what
/// `ui-monospace` renders in WebKit/Chromium on macOS. SF Mono itself is a
/// hidden system family, so `"SF Mono"` / `"SFMono-Regular"` never appear in
/// the installed-family list and must map here too.
pub const SYSTEM_MONO_FONT: &str = ".AppleSystemUIFontMonospaced";

fn quote_font_family_name(name: &str) -> String {
    let bare = name.trim();
    if bare.is_empty() {
        return String::new();
    }
    // Already quoted, or a single ident that needs no quoting.
    let quoted = bare.len() >= 2
        && ((bare.starts_with('"') && bare.ends_with('"'))
            || (bare.starts_with('\'') && bare.ends_with('\'')));
    if quoted {
        return bare.to_string();
    }
    let mut chars = bare.chars();
    let ident = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-');
    if ident {
        return bare.to_string();
    }
    format!("\"{}\"", bare.replace('"', ""))
}

/// Normalize a user-entered family (single name or comma-separated list) into a
/// safe CSS font-family list, or `None` when the input is effectively empty.
pub fn css_font_families(input: &str) -> Option<String> {
    let families: Vec<String> = input
        .split(',')
        .map(quote_font_family_name)
        .filter(|name| !name.is_empty())
        .collect();
    (!families.is_empty()).then(|| families.join(", "))
}

/// The full stack a preference resolves to: custom families before the default.
pub fn appearance_font_stack(custom: &str, default_stack: &str) -> String {
    match css_font_families(custom) {
        None => default_stack.to_string(),
        Some(families) => format!("{families}, {default_stack}"),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppearanceFontPreferences {
    pub sans: String,
    pub code: String,
    pub composer: String,
    pub size_interface: u32,
    pub size_prompt: u32,
    pub size_code: u32,
    /// Grayscale `antialiased` rendering; false keeps the heavier platform default.
    pub smoothing: bool,
}

impl Default for AppearanceFontPreferences {
    fn default() -> Self {
        Self {
            sans: String::new(),
            code: String::new(),
            composer: String::new(),
            size_interface: DEFAULT_INTERFACE_FONT_SIZE,
            size_prompt: DEFAULT_PROMPT_FONT_SIZE,
            size_code: DEFAULT_CODE_FONT_SIZE,
            // contracts settings.ts: `fontSmoothing` decodes to `true`.
            smoothing: true,
        }
    }
}

fn clamp_font_size(value: f64, minimum: u32, maximum: u32, fallback: u32) -> u32 {
    if !value.is_finite() {
        return fallback;
    }
    // JS Math.round: halves round up.
    ((value + 0.5).floor() as i64).clamp(minimum as i64, maximum as i64) as u32
}

pub fn clamp_interface_font_size(value: f64) -> u32 {
    clamp_font_size(
        value,
        MIN_INTERFACE_FONT_SIZE,
        MAX_INTERFACE_FONT_SIZE,
        DEFAULT_INTERFACE_FONT_SIZE,
    )
}

pub fn clamp_prompt_font_size(value: f64) -> u32 {
    clamp_font_size(
        value,
        MIN_PROMPT_FONT_SIZE,
        MAX_PROMPT_FONT_SIZE,
        DEFAULT_PROMPT_FONT_SIZE,
    )
}

pub fn clamp_code_font_size(value: f64) -> u32 {
    clamp_font_size(
        value,
        MIN_CODE_FONT_SIZE,
        MAX_CODE_FONT_SIZE,
        DEFAULT_CODE_FONT_SIZE,
    )
}

const MONOSPACE_ADVANCE_TOLERANCE: f32 = 0.01;

/// Whether measured glyph advances share one width. Unmeasurable input
/// (empty, non-finite, non-positive) answers true so a missing measurement
/// never blocks a legitimate font.
pub fn are_font_advances_monospace(advances: &[f32]) -> bool {
    let Some(&reference) = advances.first() else {
        return true;
    };
    if reference <= 0. || advances.iter().any(|a| !a.is_finite() || *a <= 0.) {
        return true;
    }
    advances
        .iter()
        .all(|advance| (advance - reference).abs() < MONOSPACE_ADVANCE_TOLERANCE)
}

/// The first family of a default stack that will actually render - what the
/// "Default" choice means on this machine. Concrete names are checked with
/// `is_installed`; generic keywords go to `resolve_generic` (upstream measures
/// DOM probes; on macOS that names SF Pro / SF Mono). `None` when nothing
/// can be named.
pub fn resolve_default_family_label(
    stack: &str,
    is_installed: impl Fn(&str) -> bool,
    resolve_generic: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    for family in split_font_stack(stack) {
        let generic = matches!(
            family.to_ascii_lowercase().as_str(),
            "system-ui"
                | "sans-serif"
                | "serif"
                | "monospace"
                | "ui-monospace"
                | "-apple-system"
                | "blinkmacsystemfont"
        );
        if generic {
            if let Some(resolved) = resolve_generic(&family) {
                return Some(resolved);
            }
            continue;
        }
        if is_installed(&family) {
            return Some(family);
        }
    }
    None
}

/// Splits a CSS family list into bare names (quotes removed).
pub fn split_font_stack(stack: &str) -> Vec<String> {
    stack
        .split(',')
        .map(|name| {
            name.trim()
                .trim_matches(|c| c == '"' || c == '\'')
                .to_string()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// Picks the first family of `stack` that `is_installed` accepts. CSS
/// generics and the platform UI aliases resolve to GPUI's system face (sans)
/// or `None` (monospace, so the caller's mono default applies).
pub fn resolve_font_family(stack: &str, is_installed: impl Fn(&str) -> bool) -> Option<String> {
    for family in split_font_stack(stack) {
        match family.as_str() {
            "-apple-system" | "BlinkMacSystemFont" | "system-ui" | "sans-serif" => {
                return Some(SYSTEM_UI_FONT.to_string());
            }
            "ui-monospace" if cfg!(target_os = "macos") => {
                return Some(SYSTEM_MONO_FONT.to_string());
            }
            "ui-monospace" | "monospace" => return None,
            name if is_installed(name) => return Some(name.to_string()),
            "SF Mono" | "SFMono-Regular" if cfg!(target_os = "macos") => {
                return Some(SYSTEM_MONO_FONT.to_string());
            }
            _ => {}
        }
    }
    None
}

/// The resolved families and sizes the UI paints with.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFonts {
    pub sans: String,
    pub mono: String,
    /// The composer falls back to whatever the sans preference resolves to.
    pub composer: String,
    pub size_interface: f32,
    pub size_prompt: f32,
    pub size_code: f32,
}

/// The mono face to use when nothing in the stack resolves (non-macOS
/// without any listed family).
pub const FALLBACK_MONO_FONT: &str = "Menlo";

pub fn resolve_appearance_fonts(
    preferences: &AppearanceFontPreferences,
    is_installed: impl Fn(&str) -> bool,
) -> ResolvedFonts {
    let sans = resolve_font_family(
        &appearance_font_stack(&preferences.sans, DEFAULT_SANS_FONT_STACK),
        &is_installed,
    )
    .unwrap_or_else(|| SYSTEM_UI_FONT.to_string());
    // A custom code font is prepended to DEFAULT_CODE_FONT_STACK; without
    // one, index.css's `--font-mono` (ui-monospace first) applies.
    let mono_stack = match css_font_families(&preferences.code) {
        Some(families) => format!("{families}, {DEFAULT_CODE_FONT_STACK}"),
        None => INDEX_CSS_MONO_STACK.to_string(),
    };
    let mono = resolve_font_family(&mono_stack, &is_installed)
        .unwrap_or_else(|| FALLBACK_MONO_FONT.to_string());
    let composer = css_font_families(&preferences.composer)
        .and_then(|stack| resolve_font_family(&stack, &is_installed))
        .unwrap_or_else(|| sans.clone());
    ResolvedFonts {
        sans,
        mono,
        composer,
        size_interface: clamp_interface_font_size(preferences.size_interface as f64) as f32,
        size_prompt: clamp_prompt_font_size(preferences.size_prompt as f64) as f32,
        size_code: clamp_code_font_size(preferences.size_code as f64) as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // describe("areFontAdvancesMonospace")

    #[test]
    fn accepts_a_fixed_advance_and_rejects_any_proportional_glyph() {
        assert!(are_font_advances_monospace(&[10., 10., 10., 10.]));
        assert!(!are_font_advances_monospace(&[10., 10., 7., 10.]));
        assert!(!are_font_advances_monospace(&[10., 10.02]));
    }

    #[test]
    fn fails_open_when_canvas_metrics_are_unavailable() {
        assert!(are_font_advances_monospace(&[]));
        assert!(are_font_advances_monospace(&[f32::NAN, f32::NAN]));
    }

    // describe("cssFontFamilies")

    #[test]
    fn returns_null_for_effectively_empty_input() {
        assert_eq!(css_font_families(""), None);
        assert_eq!(css_font_families("   "), None);
        assert_eq!(css_font_families(" , , "), None);
    }

    #[test]
    fn quotes_names_with_spaces_and_keeps_single_idents_bare() {
        assert_eq!(
            css_font_families("Fira Code").as_deref(),
            Some(r#""Fira Code""#)
        );
        assert_eq!(css_font_families("monospace").as_deref(), Some("monospace"));
        assert_eq!(
            css_font_families(r#""Comic Mono""#).as_deref(),
            Some(r#""Comic Mono""#)
        );
    }

    #[test]
    fn normalizes_comma_separated_lists_and_strips_embedded_quotes() {
        assert_eq!(
            css_font_families(" Fira Code , Menlo ").as_deref(),
            Some(r#""Fira Code", Menlo"#)
        );
        assert_eq!(
            css_font_families(r#"Bad"Name"#).as_deref(),
            Some(r#""BadName""#)
        );
    }

    #[test]
    fn quotes_names_that_are_not_single_css_idents() {
        assert_eq!(
            css_font_families("3270 Nerd Font").as_deref(),
            Some(r#""3270 Nerd Font""#)
        );
        assert_eq!(css_font_families("M+ 1m").as_deref(), Some(r#""M+ 1m""#));
    }

    // describe("resolveDefaultFamilyLabel")

    #[test]
    fn skips_generic_keywords_and_returns_null_for_a_stack_of_only_generics() {
        let none = |_: &str| None;
        assert_eq!(
            resolve_default_family_label("system-ui, sans-serif", |_| true, none),
            None
        );
        assert_eq!(
            resolve_default_family_label("ui-monospace, monospace", |_| true, none),
            None
        );
    }

    // describe("appearanceFontStack")

    #[test]
    fn prepends_the_custom_family_to_the_default_stack() {
        assert_eq!(
            appearance_font_stack("Fira Code", DEFAULT_CODE_FONT_STACK),
            format!(r#""Fira Code", {DEFAULT_CODE_FONT_STACK}"#)
        );
    }

    #[test]
    fn falls_back_to_the_default_stack_when_unset() {
        assert_eq!(
            appearance_font_stack("", DEFAULT_SANS_FONT_STACK),
            DEFAULT_SANS_FONT_STACK
        );
    }

    // describe("font size clamping")

    #[test]
    fn keeps_sizes_inside_the_ranges_the_ui_can_absorb() {
        assert_eq!(clamp_interface_font_size(16.), 16);
        assert_eq!(clamp_interface_font_size(2.), 12);
        assert_eq!(clamp_interface_font_size(96.), 20);
        assert_eq!(clamp_prompt_font_size(40.), 20);
        assert_eq!(clamp_code_font_size(1.), 10);
    }

    #[test]
    fn rounds_fractional_values_and_falls_back_for_unusable_input() {
        assert_eq!(clamp_code_font_size(13.4), 13);
        assert_eq!(clamp_interface_font_size(f64::NAN), 16);
        assert_eq!(clamp_prompt_font_size(f64::INFINITY), 14);
    }

    // Turin: GPUI family resolution (no upstream counterpart).

    #[test]
    fn resolves_defaults_to_system_faces() {
        let fonts = resolve_appearance_fonts(&AppearanceFontPreferences::default(), |name| {
            name == "Menlo"
        });
        assert_eq!(fonts.sans, SYSTEM_UI_FONT);
        // index.css `ui-monospace` first: SF Mono on macOS, Menlo elsewhere.
        let expected_mono = if cfg!(target_os = "macos") {
            SYSTEM_MONO_FONT
        } else {
            "Menlo"
        };
        assert_eq!(fonts.mono, expected_mono);
        assert_eq!(fonts.composer, SYSTEM_UI_FONT);
        assert_eq!(fonts.size_interface, 16.);
        assert_eq!(fonts.size_prompt, 14.);
        assert_eq!(fonts.size_code, 13.);
    }

    #[test]
    fn uninstalled_custom_family_falls_through_to_defaults() {
        let preferences = AppearanceFontPreferences {
            sans: "Nope Sans".into(),
            code: "SF Mono".into(),
            ..Default::default()
        };
        let fonts = resolve_appearance_fonts(&preferences, |name| name == "SF Mono");
        assert_eq!(fonts.sans, SYSTEM_UI_FONT);
        assert_eq!(fonts.mono, "SF Mono");
    }

    #[test]
    fn hidden_sf_mono_maps_to_the_system_monospaced_face() {
        let preferences = AppearanceFontPreferences {
            code: "SF Mono".into(),
            ..Default::default()
        };
        let fonts = resolve_appearance_fonts(&preferences, |name| name == "Menlo");
        if cfg!(target_os = "macos") {
            assert_eq!(fonts.mono, SYSTEM_MONO_FONT);
        } else {
            assert_eq!(fonts.mono, "Menlo");
        }
    }
}

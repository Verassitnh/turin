//! Port of upstream/apps/web/src/themePalette.ts (custom-theme library)

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::{Arc, OnceLock};

use super::theme_palettes::{
    BUILT_IN_THEMES, RESERVED_THEME_IDS, T3_CHAT_THEME, THEME_COLOR_ROLES, ThemeAppearance,
    ThemeCollection, ThemeColors, ThemeDefinition, ThemeVariants,
};
use csscolorparser::Color;
use gpui_kit::SharedString;
use parking_lot::Mutex;
use serde_json::{Map, Value};

pub const THEME_FILE_VERSION: u32 = 1;
pub const CUSTOM_THEMES_STORAGE_KEY: &str = "t3code:themes:v1";

#[derive(Debug, PartialEq, Eq)]
pub struct ThemeLibraryStorageError {
    pub storage_key: String,
    pub operation: ThemeLibraryStorageOperation,
    pub reason: ThemeLibraryStorageReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeLibraryStorageOperation {
    Read,
    Write,
}

impl fmt::Display for ThemeLibraryStorageOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Read => write!(f, "read"),
            Self::Write => write!(f, "write"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemeLibraryStorageReason {
    Malformed,
    StorageUnavailable,
}

impl ThemeLibraryStorageError {
    fn read(reason: ThemeLibraryStorageReason) -> Self {
        Self {
            storage_key: CUSTOM_THEMES_STORAGE_KEY.to_string(),
            operation: ThemeLibraryStorageOperation::Read,
            reason,
        }
    }

    fn write_unavailable() -> Self {
        Self {
            storage_key: CUSTOM_THEMES_STORAGE_KEY.to_string(),
            operation: ThemeLibraryStorageOperation::Write,
            reason: ThemeLibraryStorageReason::StorageUnavailable,
        }
    }

    fn direction(&self) -> &'static str {
        match self.operation {
            ThemeLibraryStorageOperation::Read => "from",
            ThemeLibraryStorageOperation::Write => "to",
        }
    }
}

impl fmt::Display for ThemeLibraryStorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Failed to {} the theme library {} {}.",
            self.operation,
            self.direction(),
            self.storage_key
        )
    }
}

pub trait ThemeLibraryStorage: Send + Sync {
    fn read(&self) -> Result<Option<String>, ThemeLibraryStorageError>;
    fn write(&self, raw: &str) -> Result<(), ThemeLibraryStorageError>;
}

pub struct InMemoryThemeLibraryStorage {
    value: Mutex<Option<String>>,
}

impl InMemoryThemeLibraryStorage {
    fn new() -> Self {
        Self {
            value: Mutex::new(None),
        }
    }
}

impl ThemeLibraryStorage for InMemoryThemeLibraryStorage {
    fn read(&self) -> Result<Option<String>, ThemeLibraryStorageError> {
        Ok(self.value.lock().clone())
    }

    fn write(&self, raw: &str) -> Result<(), ThemeLibraryStorageError> {
        *self.value.lock() = Some(raw.to_string());
        Ok(())
    }
}

static THEME_LIBRARY_STORAGE: OnceLock<Mutex<Arc<dyn ThemeLibraryStorage>>> = OnceLock::new();

fn theme_library_storage() -> Arc<dyn ThemeLibraryStorage> {
    THEME_LIBRARY_STORAGE
        .get_or_init(|| Mutex::new(Arc::new(InMemoryThemeLibraryStorage::new())))
        .lock()
        .clone()
}

/// Tests can swap the backing store (for example to simulate read failures).
pub fn set_theme_library_storage(storage: Arc<dyn ThemeLibraryStorage>) {
    THEME_LIBRARY_STORAGE
        .get_or_init(|| Mutex::new(Arc::new(InMemoryThemeLibraryStorage::new())))
        .lock()
        .clone_from(&storage);
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum CustomThemeLibrarySnapshot {
    Ready {
        stored_themes: Vec<Value>,
        themes: Vec<ThemeDefinition>,
    },
    UnavailableMalformed,
    UnavailableStorage,
}

static CUSTOM_THEME_LIBRARY_SNAPSHOT: Mutex<Option<CustomThemeLibrarySnapshot>> = Mutex::new(None);

static ENVIRONMENT_THEME_DEFINITIONS: Mutex<Vec<ThemeDefinition>> = Mutex::new(Vec::new());

fn is_record(value: &Value) -> bool {
    value.is_object()
}

fn is_theme_appearance(value: &str) -> bool {
    matches!(value, "light" | "dark")
}

fn appearance_from_str(value: &str) -> Option<ThemeAppearance> {
    match value {
        "light" => Some(ThemeAppearance::Light),
        "dark" => Some(ThemeAppearance::Dark),
        _ => None,
    }
}

fn appearance_to_str(appearance: ThemeAppearance) -> &'static str {
    match appearance {
        ThemeAppearance::Light => "light",
        ThemeAppearance::Dark => "dark",
    }
}

fn is_theme_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 48
        && value
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_theme_label(value: &str) -> bool {
    let trimmed = value.trim();
    !trimmed.is_empty() && trimmed.len() <= 48
}

fn snake_to_camel_role(role: &str) -> String {
    let mut out = String::new();
    for (index, part) in role.split('_').enumerate() {
        if index == 0 {
            out.push_str(part);
        } else {
            let mut chars = part.chars();
            if let Some(first) = chars.next() {
                out.extend(first.to_uppercase());
                out.extend(chars);
            }
        }
    }
    out
}

fn camel_to_snake_role(camel: &str) -> Option<&'static str> {
    let mut snake = String::new();
    for (index, ch) in camel.chars().enumerate() {
        if ch.is_uppercase() {
            if index > 0 {
                snake.push('_');
            }
            snake.extend(ch.to_lowercase());
        } else {
            snake.push(ch);
        }
    }
    THEME_COLOR_ROLES
        .iter()
        .copied()
        .find(|role| *role == snake.as_str())
}

fn theme_color_role_set() -> HashSet<&'static str> {
    THEME_COLOR_ROLES.iter().copied().collect()
}

fn get_color_role<'a>(colors: &'a ThemeColors, role: &str) -> Option<&'a SharedString> {
    match role {
        "canvas" => Some(&colors.canvas),
        "chrome" => Some(&colors.chrome),
        "toolbar" => Some(&colors.toolbar),
        "toolbar_foreground" => Some(&colors.toolbar_foreground),
        "toolbar_border" => Some(&colors.toolbar_border),
        "toolbar_control" => Some(&colors.toolbar_control),
        "toolbar_control_foreground" => Some(&colors.toolbar_control_foreground),
        "toolbar_control_hover" => Some(&colors.toolbar_control_hover),
        "surface" => Some(&colors.surface),
        "surface_raised" => Some(&colors.surface_raised),
        "surface_overlay" => Some(&colors.surface_overlay),
        "text" => Some(&colors.text),
        "text_muted" => Some(&colors.text_muted),
        "border" => Some(&colors.border),
        "input" => Some(&colors.input),
        "focus" => Some(&colors.focus),
        "accent" => Some(&colors.accent),
        "accent_foreground" => Some(&colors.accent_foreground),
        "secondary" => Some(&colors.secondary),
        "secondary_foreground" => Some(&colors.secondary_foreground),
        "muted" => Some(&colors.muted),
        "muted_foreground" => Some(&colors.muted_foreground),
        "placeholder" => Some(&colors.placeholder),
        "secondary_label" => Some(&colors.secondary_label),
        "icon_muted" => Some(&colors.icon_muted),
        "error" => Some(&colors.error),
        "error_foreground" => Some(&colors.error_foreground),
        "error_surface" => Some(&colors.error_surface),
        "warning" => Some(&colors.warning),
        "warning_foreground" => Some(&colors.warning_foreground),
        "warning_surface" => Some(&colors.warning_surface),
        "update" => Some(&colors.update),
        "update_foreground" => Some(&colors.update_foreground),
        "update_surface" => Some(&colors.update_surface),
        "accent_surface" => Some(&colors.accent_surface),
        "accent_surface_foreground" => Some(&colors.accent_surface_foreground),
        "message_surface" => Some(&colors.message_surface),
        "message_foreground" => Some(&colors.message_foreground),
        "message_action" => Some(&colors.message_action),
        "message_action_foreground" => Some(&colors.message_action_foreground),
        "message_action_hover" => Some(&colors.message_action_hover),
        "code_background" => Some(&colors.code_background),
        "code_foreground" => Some(&colors.code_foreground),
        "search_match_background" => Some(&colors.search_match_background),
        "search_match_foreground" => Some(&colors.search_match_foreground),
        "search_match_active_background" => Some(&colors.search_match_active_background),
        "search_match_active_foreground" => Some(&colors.search_match_active_foreground),
        "sidebar" => Some(&colors.sidebar),
        "sidebar_foreground" => Some(&colors.sidebar_foreground),
        "sidebar_muted_foreground" => Some(&colors.sidebar_muted_foreground),
        "sidebar_control_surface" => Some(&colors.sidebar_control_surface),
        "sidebar_row_hover" => Some(&colors.sidebar_row_hover),
        "sidebar_row_active" => Some(&colors.sidebar_row_active),
        "sidebar_row_selected" => Some(&colors.sidebar_row_selected),
        "sidebar_border" => Some(&colors.sidebar_border),
        "terminal_background" => Some(&colors.terminal_background),
        "terminal_foreground" => Some(&colors.terminal_foreground),
        "terminal_cursor" => Some(&colors.terminal_cursor),
        "terminal_selection" => Some(&colors.terminal_selection),
        "terminal_scrollbar" => Some(&colors.terminal_scrollbar),
        "terminal_scrollbar_hover" => Some(&colors.terminal_scrollbar_hover),
        _ => None,
    }
}

fn set_color_role(colors: &mut ThemeColors, role: &str, value: SharedString) -> bool {
    match role {
        "canvas" => colors.canvas = value,
        "chrome" => colors.chrome = value,
        "toolbar" => colors.toolbar = value,
        "toolbar_foreground" => colors.toolbar_foreground = value,
        "toolbar_border" => colors.toolbar_border = value,
        "toolbar_control" => colors.toolbar_control = value,
        "toolbar_control_foreground" => colors.toolbar_control_foreground = value,
        "toolbar_control_hover" => colors.toolbar_control_hover = value,
        "surface" => colors.surface = value,
        "surface_raised" => colors.surface_raised = value,
        "surface_overlay" => colors.surface_overlay = value,
        "text" => colors.text = value,
        "text_muted" => colors.text_muted = value,
        "border" => colors.border = value,
        "input" => colors.input = value,
        "focus" => colors.focus = value,
        "accent" => colors.accent = value,
        "accent_foreground" => colors.accent_foreground = value,
        "secondary" => colors.secondary = value,
        "secondary_foreground" => colors.secondary_foreground = value,
        "muted" => colors.muted = value,
        "muted_foreground" => colors.muted_foreground = value,
        "placeholder" => colors.placeholder = value,
        "secondary_label" => colors.secondary_label = value,
        "icon_muted" => colors.icon_muted = value,
        "error" => colors.error = value,
        "error_foreground" => colors.error_foreground = value,
        "error_surface" => colors.error_surface = value,
        "warning" => colors.warning = value,
        "warning_foreground" => colors.warning_foreground = value,
        "warning_surface" => colors.warning_surface = value,
        "update" => colors.update = value,
        "update_foreground" => colors.update_foreground = value,
        "update_surface" => colors.update_surface = value,
        "accent_surface" => colors.accent_surface = value,
        "accent_surface_foreground" => colors.accent_surface_foreground = value,
        "message_surface" => colors.message_surface = value,
        "message_foreground" => colors.message_foreground = value,
        "message_action" => colors.message_action = value,
        "message_action_foreground" => colors.message_action_foreground = value,
        "message_action_hover" => colors.message_action_hover = value,
        "code_background" => colors.code_background = value,
        "code_foreground" => colors.code_foreground = value,
        "search_match_background" => colors.search_match_background = value,
        "search_match_foreground" => colors.search_match_foreground = value,
        "search_match_active_background" => colors.search_match_active_background = value,
        "search_match_active_foreground" => colors.search_match_active_foreground = value,
        "sidebar" => colors.sidebar = value,
        "sidebar_foreground" => colors.sidebar_foreground = value,
        "sidebar_muted_foreground" => colors.sidebar_muted_foreground = value,
        "sidebar_control_surface" => colors.sidebar_control_surface = value,
        "sidebar_row_hover" => colors.sidebar_row_hover = value,
        "sidebar_row_active" => colors.sidebar_row_active = value,
        "sidebar_row_selected" => colors.sidebar_row_selected = value,
        "sidebar_border" => colors.sidebar_border = value,
        "terminal_background" => colors.terminal_background = value,
        "terminal_foreground" => colors.terminal_foreground = value,
        "terminal_cursor" => colors.terminal_cursor = value,
        "terminal_selection" => colors.terminal_selection = value,
        "terminal_scrollbar" => colors.terminal_scrollbar = value,
        "terminal_scrollbar_hover" => colors.terminal_scrollbar_hover = value,
        _ => return false,
    }
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThemeRgbColor {
    r: f64,
    g: f64,
    b: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ThemeOklch {
    l: f64,
    c: f64,
    h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ParsedThemeColor {
    color: ThemeOklch,
    alpha: f64,
}

const THEME_LIGHT_FOREGROUND: ThemeRgbColor = ThemeRgbColor {
    r: 255.0,
    g: 250.0,
    b: 255.0,
};
const THEME_DARK_FOREGROUND: ThemeRgbColor = ThemeRgbColor {
    r: 36.0,
    g: 21.0,
    b: 35.0,
};
const THEME_WHITE_FOREGROUND: ThemeRgbColor = ThemeRgbColor {
    r: 255.0,
    g: 255.0,
    b: 255.0,
};
const THEME_BLACK_FOREGROUND: ThemeRgbColor = ThemeRgbColor {
    r: 0.0,
    g: 0.0,
    b: 0.0,
};

fn format_theme_color_number(value: f64, precision: usize) -> String {
    let threshold = 10_f64.powi(-(precision as i32)) / 2.0;
    let rounded = if value.abs() < threshold { 0.0 } else { value };
    let mut formatted = format!("{rounded:.precision$}", precision = precision);
    if let Some(dot) = formatted.find('.') {
        while formatted.ends_with('0') {
            formatted.pop();
        }
        if formatted.ends_with('.') {
            formatted.pop();
        }
        let _ = dot;
    }
    formatted
}

fn format_oklch_theme_color(color: ThemeOklch, alpha: f64) -> String {
    let normalized_hue = if color.c < 0.0000005 {
        0.0
    } else {
        ((color.h % 360.0) + 360.0) % 360.0
    };
    let body = format!(
        "{} {} {}",
        format_theme_color_number(color.l, 6),
        format_theme_color_number(color.c, 6),
        format_theme_color_number(normalized_hue, 3)
    );
    if alpha < 1.0 {
        format!("oklch({body} / {})", format_theme_color_number(alpha, 4))
    } else {
        format!("oklch({body})")
    }
}

fn srgb_channel_to_linear(channel: f64) -> f64 {
    let c = channel / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_channel_to_srgb(channel: f64) -> f64 {
    let c = if channel <= 0.0031308 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round()
}

fn theme_rgb_to_oklch(color: ThemeRgbColor) -> ThemeOklch {
    let r = srgb_channel_to_linear(color.r);
    let g = srgb_channel_to_linear(color.g);
    let b = srgb_channel_to_linear(color.b);
    let l = (0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b).cbrt();
    let m = (0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b).cbrt();
    let s = (0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b).cbrt();
    let ok_l = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
    let a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
    let bb = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
    ThemeOklch {
        l: ok_l,
        c: (a * a + bb * bb).sqrt(),
        h: bb.atan2(a).to_degrees(),
    }
}

fn oklch_to_rgb_unclamped(color: ThemeOklch) -> ThemeRgbColor {
    let hr = color.h.to_radians();
    let a = color.c * hr.cos();
    let bb = color.c * hr.sin();
    let l = (color.l + 0.3963377774 * a + 0.2158037573 * bb).powi(3);
    let m = (color.l - 0.1055613458 * a - 0.0638541728 * bb).powi(3);
    let s = (color.l - 0.0894841775 * a - 1.291485548 * bb).powi(3);
    ThemeRgbColor {
        r: 4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        g: -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        b: -0.0041960863 * l - 0.7034186147 * m + 1.707614701 * s,
    }
}

fn map_theme_oklch_to_srgb_gamut(color: ThemeOklch) -> ThemeOklch {
    let is_in_gamut = |chroma: f64| {
        let linear = oklch_to_rgb_unclamped(ThemeOklch { c: chroma, ..color });
        [linear.r, linear.g, linear.b]
            .into_iter()
            .all(|channel| (-0.0001..=1.0001).contains(&channel))
    };
    if is_in_gamut(color.c) {
        return color;
    }
    let mut low = 0.0;
    let mut high = color.c;
    let chroma_resolution = 0.000001;
    let steps =
        ((color.c.max(chroma_resolution).log2() - chroma_resolution.log2()).ceil() as i32).max(1);
    for _ in 0..steps {
        let mid = (low + high) / 2.0;
        if is_in_gamut(mid) {
            low = mid;
        } else {
            high = mid;
        }
    }
    ThemeOklch { c: low, ..color }
}

fn theme_oklch_to_rgb(color: ThemeOklch) -> ThemeRgbColor {
    let linear = oklch_to_rgb_unclamped(map_theme_oklch_to_srgb_gamut(color));
    ThemeRgbColor {
        r: linear_channel_to_srgb(linear.r),
        g: linear_channel_to_srgb(linear.g),
        b: linear_channel_to_srgb(linear.b),
    }
}

fn theme_oklch_to_theme_color(color: ThemeOklch) -> SharedString {
    SharedString::from(format_oklch_theme_color(
        map_theme_oklch_to_srgb_gamut(color),
        1.0,
    ))
}

fn theme_rgb_to_hex_color(color: ThemeRgbColor) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        color.r.round().clamp(0.0, 255.0) as u8,
        color.g.round().clamp(0.0, 255.0) as u8,
        color.b.round().clamp(0.0, 255.0) as u8,
    )
}

fn theme_rgb_to_theme_color(color: ThemeRgbColor) -> SharedString {
    SharedString::from(format_oklch_theme_color(theme_rgb_to_oklch(color), 1.0))
}

fn parse_oklch_components(input: &str) -> Option<(ThemeOklch, f64)> {
    let trimmed = input.trim();
    if !trimmed.starts_with("oklch(") || !trimmed.ends_with(')') {
        return None;
    }
    let inner = &trimmed[6..trimmed.len() - 1];
    let (body, alpha) = if let Some(slash) = inner.rfind('/') {
        let (left, right) = inner.split_at(slash);
        let alpha_str = right.trim_start_matches('/').trim();
        let alpha = if alpha_str.eq_ignore_ascii_case("none") {
            0.0
        } else {
            parse_css_number(alpha_str)?
        };
        (left.trim(), alpha)
    } else {
        (inner.trim(), 1.0)
    };
    let mut parts = body.split_whitespace();
    let l_raw = parts.next()?;
    let c_raw = parts.next()?;
    let h_raw = parts.next()?.trim_end_matches("deg");
    let l = parse_css_number(l_raw)?;
    let c = parse_css_number(c_raw)?;
    let h = parse_css_number(h_raw)?;
    if ![l, c, h, alpha].iter().all(|value| value.is_finite()) {
        return None;
    }
    Some((
        ThemeOklch {
            l: l.clamp(0.0, 1.0),
            c: c.max(0.0),
            h,
        },
        alpha.clamp(0.0, 1.0),
    ))
}

fn parse_css_number(raw: &str) -> Option<f64> {
    let value = raw.trim();
    if value.ends_with('%') {
        let percent = value.trim_end_matches('%').parse::<f64>().ok()?;
        Some(percent / 100.0)
    } else {
        value.parse::<f64>().ok()
    }
}

fn display_p3_to_rgb(r: f64, g: f64, b: f64) -> ThemeRgbColor {
    let linearize = |channel: f64| {
        if channel <= 0.04045 {
            channel / 12.92
        } else {
            ((channel + 0.055) / 1.055).powf(2.4)
        }
    };
    let encode = |channel: f64| {
        let channel = channel.clamp(0.0, 1.0);
        if channel <= 0.0031308 {
            12.92 * channel
        } else {
            1.055 * channel.powf(1.0 / 2.4) - 0.055
        }
    };
    let (lr, lg, lb) = (linearize(r), linearize(g), linearize(b));
    let x = 0.486570948648216 * lr + 0.265667693168093 * lg + 0.198217285234362 * lb;
    let y = 0.228974564069748 * lr + 0.691738521836556 * lg + 0.079286914093745 * lb;
    let z = 0.045113381962903 * lg + 1.043944368900976 * lb;
    let sr = 3.2404542 * x - 1.5371385 * y - 0.4985314 * z;
    let sg = -0.969266 * x + 1.8760108 * y + 0.041556 * z;
    let sb = 0.0556434 * x - 0.2040259 * y + 1.0572252 * z;
    ThemeRgbColor {
        r: encode(sr) * 255.0,
        g: encode(sg) * 255.0,
        b: encode(sb) * 255.0,
    }
}

fn parse_display_p3_color(input: &str) -> Option<ThemeRgbColor> {
    let trimmed = input.trim();
    let open = trimmed.strip_prefix("color(display-p3")?;
    let inner = open.trim_start_matches('(').trim_end_matches(')').trim();
    let mut parts = inner.split_whitespace();
    let r = parse_css_number(parts.next()?)?;
    let g = parse_css_number(parts.next()?)?;
    let b = parse_css_number(parts.next()?)?;
    Some(display_p3_to_rgb(r, g, b))
}

fn parse_theme_color(value: &str) -> Option<ParsedThemeColor> {
    let input = value.trim();
    if let Some((color, alpha)) = parse_oklch_components(input) {
        return Some(ParsedThemeColor { color, alpha });
    }
    if let Some(rgb) = parse_display_p3_color(input) {
        return Some(ParsedThemeColor {
            color: theme_rgb_to_oklch(rgb),
            alpha: 1.0,
        });
    }
    let lower = input.to_ascii_lowercase();
    let normalized = if lower.contains("/ none") {
        input
            .replace("/ none", "/ 0")
            .replace("/none", "/0")
            .replace("/ NONE", "/ 0")
    } else {
        input.to_string()
    };
    let parsed: Color = csscolorparser::parse(&normalized).ok()?;
    let alpha = if lower.contains("/ none") {
        0.0
    } else {
        f64::from(parsed.a)
    };
    let rgb = ThemeRgbColor {
        r: f64::from(parsed.r) * 255.0,
        g: f64::from(parsed.g) * 255.0,
        b: f64::from(parsed.b) * 255.0,
    };
    Some(ParsedThemeColor {
        color: theme_rgb_to_oklch(rgb),
        alpha: alpha.clamp(0.0, 1.0),
    })
}

pub fn to_canonical_theme_color(value: &str) -> Option<SharedString> {
    parse_theme_color(value)
        .map(|parsed| SharedString::from(format_oklch_theme_color(parsed.color, parsed.alpha)))
}

pub fn theme_color_to_hex(value: &str) -> Option<String> {
    let parsed = parse_theme_color(value)?;
    let opaque = theme_rgb_to_hex_color(theme_oklch_to_rgb(parsed.color));
    if parsed.alpha >= 1.0 {
        return Some(opaque);
    }
    let alpha = (parsed.alpha * 255.0).round().clamp(0.0, 255.0) as u8;
    Some(format!("{opaque}{alpha:02x}"))
}

pub fn is_theme_color(value: &str) -> bool {
    to_canonical_theme_color(value).is_some()
}

pub fn lenient_theme_color_overrides(value: &Map<String, Value>) -> HashMap<String, SharedString> {
    let roles = theme_color_role_set();
    let mut overrides = HashMap::new();
    for (role, color) in value {
        let snake = camel_to_snake_role(role).unwrap_or(role.as_str());
        if !roles.contains(snake) {
            continue;
        }
        let Some(normalized) = color.as_str().and_then(to_canonical_theme_color) else {
            continue;
        };
        overrides.insert(snake.to_string(), normalized);
    }
    overrides
}

pub fn get_default_theme_colors(appearance: ThemeAppearance) -> ThemeColors {
    match appearance {
        ThemeAppearance::Light => T3_CHAT_THEME.colors.clone(),
        ThemeAppearance::Dark => T3_CHAT_THEME
            .variants
            .dark
            .clone()
            .expect("T3 Chat dark variant"),
    }
}

fn decode_theme_colors(colors: &ThemeColors) -> Result<ThemeColors, String> {
    let mut decoded = colors.clone();
    for role in THEME_COLOR_ROLES {
        let value = get_color_role(&decoded, role).expect("role");
        let canonical = to_canonical_theme_color(value.as_str()).ok_or_else(|| {
            format!(
                "The color for \"{}\" must be a literal CSS color such as oklch(0.62 0.2 280).",
                snake_to_camel_role(role)
            )
        })?;
        set_color_role(&mut decoded, role, canonical);
    }
    Ok(decoded)
}

fn canonicalize_theme_definition(theme: ThemeDefinition) -> Result<ThemeDefinition, String> {
    let mut next = theme;
    next.colors = decode_theme_colors(&next.colors)?;
    if next.variants.light.is_some() {
        next.variants.light = Some(decode_theme_colors(
            next.variants.light.as_ref().expect("light"),
        )?);
    }
    if next.variants.dark.is_some() {
        next.variants.dark = Some(decode_theme_colors(
            next.variants.dark.as_ref().expect("dark"),
        )?);
    }
    Ok(next)
}

fn merge_overrides(base: ThemeColors, overrides: HashMap<String, SharedString>) -> ThemeColors {
    let mut merged = base;
    for (role, value) in overrides {
        set_color_role(&mut merged, role.as_str(), value);
    }
    merged
}

fn parse_stored_theme_colors(value: &Value, appearance: ThemeAppearance) -> Option<ThemeColors> {
    let object = value.as_object()?;
    let overrides = lenient_theme_color_overrides(object);
    Some(merge_overrides(
        get_default_theme_colors(appearance),
        overrides,
    ))
}

fn parse_stored_theme_variants(
    value: &Value,
    base_appearance: ThemeAppearance,
) -> Result<Option<ThemeVariants>, ()> {
    if value.is_null() {
        return Ok(None);
    }
    let Some(object) = value.as_object() else {
        return Err(());
    };
    let mut variants = ThemeVariants {
        light: None,
        dark: None,
    };
    let mut any = false;
    for (appearance, colors) in object {
        let Some(mode) = appearance_from_str(appearance) else {
            return Err(());
        };
        if mode == base_appearance {
            continue;
        }
        let Some(parsed) = parse_stored_theme_colors(colors, mode) else {
            return Err(());
        };
        any = true;
        match mode {
            ThemeAppearance::Light => variants.light = Some(parsed),
            ThemeAppearance::Dark => variants.dark = Some(parsed),
        }
    }
    Ok(if any { Some(variants) } else { None })
}

fn parse_theme_collection(value: &Value) -> Option<ThemeCollection> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    if id.is_empty()
        || id.len() > 128
        || !id.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".:-_".contains(c))
    {
        return None;
    }
    let label = object.get("label")?.as_str()?;
    if !is_theme_label(label) {
        return None;
    }
    Some(ThemeCollection {
        id: SharedString::from(id),
        label: SharedString::from(label.trim()),
    })
}

fn is_reserved_theme_id_inner(theme_id: &str) -> bool {
    RESERVED_THEME_IDS.contains(&theme_id)
}

pub fn is_reserved_theme_id(theme_id: &str) -> bool {
    is_reserved_theme_id_inner(theme_id)
}

fn parse_stored_theme(value: &Value) -> Option<ThemeDefinition> {
    let object = value.as_object()?;
    let id = object.get("id")?.as_str()?;
    if !is_theme_id(id) || is_reserved_theme_id_inner(id) {
        return None;
    }
    let label = object.get("label")?.as_str()?;
    if !is_theme_label(label) {
        return None;
    }
    let appearance_raw = object.get("appearance")?.as_str()?;
    let appearance = appearance_from_str(appearance_raw)?;
    let colors = parse_stored_theme_colors(object.get("colors")?, appearance)?;
    let variants = match object.get("variants") {
        None => ThemeVariants {
            light: None,
            dark: None,
        },
        Some(value) => parse_stored_theme_variants(value, appearance)
            .ok()?
            .unwrap_or(ThemeVariants {
                light: None,
                dark: None,
            }),
    };
    let collection = object.get("collection").and_then(parse_theme_collection);
    let managed = object.get("managed").and_then(Value::as_bool) == Some(true);
    Some(ThemeDefinition {
        id: SharedString::from(id),
        label: SharedString::from(label.trim()),
        appearance,
        colors,
        variants,
        collection,
        managed,
        sidebar_artwork: false,
    })
}

fn parse_stored_themes(stored_themes: &[Value]) -> Vec<ThemeDefinition> {
    let mut themes: Vec<ThemeDefinition> = Vec::new();
    for value in stored_themes {
        let Some(theme) = parse_stored_theme(value) else {
            continue;
        };
        if themes.iter().any(|existing| existing.id == theme.id) {
            continue;
        }
        themes.push(theme);
    }
    themes
}

fn read_custom_theme_library_snapshot() -> CustomThemeLibrarySnapshot {
    match theme_library_storage().read() {
        Err(ThemeLibraryStorageError {
            reason: ThemeLibraryStorageReason::StorageUnavailable,
            ..
        }) => CustomThemeLibrarySnapshot::UnavailableStorage,
        Err(ThemeLibraryStorageError {
            reason: ThemeLibraryStorageReason::Malformed,
            ..
        }) => CustomThemeLibrarySnapshot::UnavailableMalformed,
        Ok(None) => CustomThemeLibrarySnapshot::Ready {
            stored_themes: Vec::new(),
            themes: Vec::new(),
        },
        Ok(Some(raw)) => {
            let parsed: Value = match serde_json::from_str(&raw) {
                Ok(value) => value,
                Err(_) => return CustomThemeLibrarySnapshot::UnavailableMalformed,
            };
            let Some(array) = parsed.as_array() else {
                return CustomThemeLibrarySnapshot::UnavailableMalformed;
            };
            let stored_themes = array.clone();
            let themes = parse_stored_themes(array);
            CustomThemeLibrarySnapshot::Ready {
                stored_themes,
                themes,
            }
        }
    }
}

fn get_custom_theme_library_snapshot() -> CustomThemeLibrarySnapshot {
    let mut cache = CUSTOM_THEME_LIBRARY_SNAPSHOT.lock();
    if cache.is_none() {
        *cache = Some(read_custom_theme_library_snapshot());
    }
    cache.clone().expect("snapshot")
}

pub fn invalidate_custom_themes() {
    *CUSTOM_THEME_LIBRARY_SNAPSHOT.lock() = None;
}

pub fn get_custom_themes() -> Vec<ThemeDefinition> {
    match get_custom_theme_library_snapshot() {
        CustomThemeLibrarySnapshot::Ready { themes, .. } => themes,
        _ => Vec::new(),
    }
}

pub fn get_environment_themes() -> Vec<ThemeDefinition> {
    ENVIRONMENT_THEME_DEFINITIONS.lock().clone()
}

pub fn set_environment_themes(themes: Vec<ThemeDefinition>) -> bool {
    let mut slot = ENVIRONMENT_THEME_DEFINITIONS.lock();
    if *slot == themes {
        return false;
    }
    *slot = themes;
    true
}

pub fn get_stored_custom_theme_collection(collection_id: &str) -> Vec<ThemeDefinition> {
    read_writable_custom_theme_library()
        .map(|(_, themes)| {
            themes
                .into_iter()
                .filter(|theme| {
                    theme
                        .collection
                        .as_ref()
                        .is_some_and(|collection| collection.id.as_str() == collection_id)
                })
                .collect()
        })
        .unwrap_or_default()
}

fn require_writable_custom_theme_library(
    snapshot: CustomThemeLibrarySnapshot,
) -> Result<(Vec<Value>, Vec<ThemeDefinition>), ThemeLibraryStorageError> {
    match snapshot {
        CustomThemeLibrarySnapshot::Ready {
            stored_themes,
            themes,
        } => Ok((stored_themes, themes)),
        CustomThemeLibrarySnapshot::UnavailableMalformed => Err(ThemeLibraryStorageError::read(
            ThemeLibraryStorageReason::Malformed,
        )),
        CustomThemeLibrarySnapshot::UnavailableStorage => Err(ThemeLibraryStorageError::read(
            ThemeLibraryStorageReason::StorageUnavailable,
        )),
    }
}

fn get_writable_custom_theme_library()
-> Result<(Vec<Value>, Vec<ThemeDefinition>), ThemeLibraryStorageError> {
    require_writable_custom_theme_library(get_custom_theme_library_snapshot())
}

fn read_writable_custom_theme_library()
-> Result<(Vec<Value>, Vec<ThemeDefinition>), ThemeLibraryStorageError> {
    require_writable_custom_theme_library(read_custom_theme_library_snapshot())
}

fn save_custom_themes(
    stored_themes: Vec<Value>,
    themes: Vec<ThemeDefinition>,
) -> Result<(), ThemeLibraryStorageError> {
    let raw = serde_json::to_string(&stored_themes)
        .map_err(|_| ThemeLibraryStorageError::write_unavailable())?;
    theme_library_storage().write(&raw)?;
    *CUSTOM_THEME_LIBRARY_SNAPSHOT.lock() = Some(CustomThemeLibrarySnapshot::Ready {
        stored_themes,
        themes,
    });
    Ok(())
}

fn stored_theme_has_id(stored_theme: &Value, theme_id: &str) -> bool {
    stored_theme
        .as_object()
        .and_then(|object| object.get("id"))
        .and_then(Value::as_str)
        == Some(theme_id)
}

fn stored_theme_has_collection_id(stored_theme: &Value, collection_id: &str) -> bool {
    stored_theme
        .as_object()
        .and_then(|object| object.get("collection"))
        .and_then(Value::as_object)
        .and_then(|collection| collection.get("id"))
        .and_then(Value::as_str)
        == Some(collection_id)
}

pub fn install_custom_theme(theme: ThemeDefinition) -> Result<ThemeDefinition, String> {
    if is_reserved_theme_id_inner(theme.id.as_str()) {
        return Err(format!("The theme id \"{}\" is reserved.", theme.id));
    }
    let (stored_themes, themes) =
        get_writable_custom_theme_library().map_err(|error| error.to_string())?;
    if BUILT_IN_THEMES
        .iter()
        .any(|existing| existing.id == theme.id)
        || stored_themes
            .iter()
            .any(|stored| stored_theme_has_id(stored, theme.id.as_str()))
    {
        return Err(format!(
            "A theme named \"{}\" is already installed.",
            theme.label
        ));
    }
    let canonical = canonicalize_theme_definition(theme)?;
    let mut next_stored = stored_themes;
    let mut next_themes = themes;
    next_stored.push(stored_theme_from_definition(&canonical));
    next_themes.push(canonical.clone());
    save_custom_themes(next_stored, next_themes).map_err(|error| error.to_string())?;
    Ok(canonical)
}

pub fn update_custom_theme(theme: ThemeDefinition) -> Result<ThemeDefinition, String> {
    if is_reserved_theme_id_inner(theme.id.as_str()) {
        return Err(format!("The theme id \"{}\" is reserved.", theme.id));
    }
    let (stored_themes, themes) =
        get_writable_custom_theme_library().map_err(|error| error.to_string())?;
    let Some(index) = themes.iter().position(|existing| existing.id == theme.id) else {
        return Err(format!("The theme \"{}\" is not installed.", theme.label));
    };
    let canonical = canonicalize_theme_definition(theme)?;
    let mut next_themes = themes;
    next_themes[index] = canonical.clone();
    let mut next_stored = Vec::new();
    let mut replaced = false;
    for stored in stored_themes {
        if !stored_theme_has_id(&stored, canonical.id.as_str()) {
            next_stored.push(stored);
        } else if !replaced {
            next_stored.push(stored_theme_from_definition(&canonical));
            replaced = true;
        }
    }
    save_custom_themes(next_stored, next_themes).map_err(|error| error.to_string())?;
    Ok(canonical)
}

pub fn remove_custom_theme(theme_id: &str) {
    remove_custom_themes(&[theme_id.to_string()]);
}

pub fn remove_custom_themes(theme_ids: &[String]) {
    let removed: HashSet<&str> = theme_ids.iter().map(String::as_str).collect();
    if removed.is_empty() {
        return;
    }
    let Ok((stored_themes, themes)) = get_writable_custom_theme_library() else {
        return;
    };
    let previous_len = themes.len();
    let next_themes: Vec<_> = themes
        .into_iter()
        .filter(|theme| !removed.contains(theme.id.as_str()))
        .collect();
    if next_themes.len() == previous_len {
        return;
    }
    let next_stored: Vec<_> = stored_themes
        .into_iter()
        .filter(|stored| {
            !stored
                .as_object()
                .and_then(|object| object.get("id"))
                .and_then(Value::as_str)
                .is_some_and(|id| removed.contains(id))
        })
        .collect();
    let _ = save_custom_themes(next_stored, next_themes);
}

pub fn replace_custom_theme_collection(
    collection_id: &str,
    themes: Vec<ThemeDefinition>,
    expected_collection: Option<&[ThemeDefinition]>,
) -> Result<Vec<ThemeDefinition>, String> {
    if themes.is_empty() {
        return Err("A theme collection cannot be empty.".to_string());
    }
    let validated: Vec<_> = themes
        .iter()
        .map(|theme| parse_stored_theme(&stored_theme_from_definition(theme)))
        .collect();
    if validated.iter().any(|theme| theme.is_none())
        || validated.iter().any(|theme| {
            theme
                .as_ref()
                .and_then(|theme| theme.collection.as_ref())
                .is_none_or(|collection| collection.id.as_str() != collection_id)
        })
        || validated.len()
            != validated
                .iter()
                .filter_map(|theme| theme.as_ref().map(|theme| theme.id.as_str()))
                .collect::<HashSet<_>>()
                .len()
    {
        return Err("That theme collection is invalid.".to_string());
    }
    let replacement: Vec<ThemeDefinition> = validated.into_iter().flatten().collect();
    let (stored_themes, current) =
        read_writable_custom_theme_library().map_err(|error| error.to_string())?;
    let current_collection: Vec<_> = current
        .iter()
        .filter(|theme| {
            theme
                .collection
                .as_ref()
                .is_some_and(|collection| collection.id.as_str() == collection_id)
        })
        .cloned()
        .collect();
    if let Some(expected) = expected_collection {
        let current_json = serde_json::to_string(
            &current_collection
                .iter()
                .map(stored_theme_from_definition)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        let expected_json = serde_json::to_string(
            &expected
                .iter()
                .map(stored_theme_from_definition)
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        if current_json != expected_json {
            return Err(
                "Your installed themes changed while this package was downloading. Try again."
                    .to_string(),
            );
        }
    }
    let mut occupied: HashSet<String> = BUILT_IN_THEMES
        .iter()
        .map(|theme| theme.id.to_string())
        .collect();
    for stored in &stored_themes {
        if stored_theme_has_collection_id(stored, collection_id) {
            continue;
        }
        if let Some(id) = stored
            .as_object()
            .and_then(|object| object.get("id"))
            .and_then(Value::as_str)
        {
            occupied.insert(id.to_string());
        }
    }
    if let Some(conflict) = replacement.iter().find(|theme| {
        is_reserved_theme_id_inner(theme.id.as_str()) || occupied.contains(theme.id.as_str())
    }) {
        return Err(format!(
            "A theme named \"{}\" is already installed.",
            conflict.label
        ));
    }
    let mut next_stored = Vec::new();
    let mut inserted = false;
    for stored in stored_themes {
        if !stored_theme_has_collection_id(&stored, collection_id) {
            next_stored.push(stored);
        } else if !inserted {
            for theme in &replacement {
                next_stored.push(stored_theme_from_definition(theme));
            }
            inserted = true;
        }
    }
    if !inserted {
        for theme in &replacement {
            next_stored.push(stored_theme_from_definition(theme));
        }
    }
    let parsed = parse_stored_themes(&next_stored);
    save_custom_themes(next_stored, parsed).map_err(|error| error.to_string())?;
    Ok(replacement)
}

fn parse_theme_color_overrides(
    value: &Map<String, Value>,
) -> Result<HashMap<String, SharedString>, String> {
    let roles = theme_color_role_set();
    let mut overrides = HashMap::new();
    for (role, color) in value {
        let snake = camel_to_snake_role(role)
            .ok_or_else(|| format!("\"{role}\" is not a supported theme color role."))?;
        if !roles.contains(snake) {
            return Err(format!("\"{role}\" is not a supported theme color role."));
        }
        let raw = color.as_str().ok_or_else(|| {
            format!(
                "The color for \"{role}\" must be a literal CSS color such as oklch(0.62 0.2 280)."
            )
        })?;
        let normalized = to_canonical_theme_color(raw).ok_or_else(|| {
            format!(
                "The color for \"{role}\" must be a literal CSS color such as oklch(0.62 0.2 280)."
            )
        })?;
        overrides.insert(snake.to_string(), normalized);
    }
    if overrides.is_empty() {
        return Err("Add at least one color role to the theme file.".to_string());
    }
    Ok(overrides)
}

pub fn parse_theme_file(value: Value) -> Result<ThemeDefinition, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Theme files must contain a JSON object.".to_string())?;
    let version = object
        .get("version")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            format!("This theme file uses an unsupported version. Expected {THEME_FILE_VERSION}.")
        })?;
    if version != u64::from(THEME_FILE_VERSION) {
        return Err(format!(
            "This theme file uses an unsupported version. Expected {THEME_FILE_VERSION}."
        ));
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "Theme files need a name (48 characters or fewer).".to_string())?;
    if !is_theme_label(name) {
        return Err("Theme files need a name (48 characters or fewer).".to_string());
    }
    let appearance_raw = object
        .get("appearance")
        .and_then(Value::as_str)
        .ok_or_else(|| "Theme files need an appearance of \"light\" or \"dark\".".to_string())?;
    let appearance = appearance_from_str(appearance_raw)
        .ok_or_else(|| "Theme files need an appearance of \"light\" or \"dark\".".to_string())?;
    let raw_colors = object
        .get("colors")
        .and_then(Value::as_object)
        .ok_or_else(|| "Theme files need a colors object.".to_string())?;
    let id = match object.get("id") {
        Some(Value::String(id)) => id.clone(),
        None => theme_id_from_name(name),
        _ => {
            return Err(
                "Theme ids may only contain lowercase letters, numbers, and hyphens.".to_string(),
            );
        }
    };
    if !is_theme_id(&id) {
        return Err(
            "Theme ids may only contain lowercase letters, numbers, and hyphens.".to_string(),
        );
    }
    if is_reserved_theme_id_inner(&id) {
        return Err(format!("The theme id \"{id}\" is reserved."));
    }
    let overrides = parse_theme_color_overrides(raw_colors)?;
    let collection = match object.get("collection") {
        None => None,
        Some(value) => Some(
            parse_theme_collection(value)
                .ok_or_else(|| "Theme collections need a valid id and label.".to_string())?,
        ),
    };
    let mut variants = ThemeVariants {
        light: None,
        dark: None,
    };
    if let Some(raw_variants) = object.get("variants") {
        let variant_object = raw_variants
            .as_object()
            .ok_or_else(|| "Theme variants must be an object.".to_string())?;
        for (variant_appearance, variant_colors) in variant_object {
            let Some(mode) = appearance_from_str(variant_appearance) else {
                return Err("Theme variants may only be named \"light\" or \"dark\".".to_string());
            };
            if mode == appearance {
                return Err(format!(
                    "Theme variants must not repeat the base appearance \"{}\".",
                    appearance_to_str(appearance)
                ));
            }
            let variant_overrides = parse_theme_color_overrides(
                variant_colors
                    .as_object()
                    .ok_or_else(|| "Theme variants must be an object.".to_string())?,
            )?;
            let merged = merge_overrides(get_default_theme_colors(mode), variant_overrides);
            match mode {
                ThemeAppearance::Light => variants.light = Some(merged),
                ThemeAppearance::Dark => variants.dark = Some(merged),
            }
        }
    }
    let managed = object.get("managed").and_then(Value::as_bool) == Some(true);
    Ok(ThemeDefinition {
        id: SharedString::from(id),
        label: SharedString::from(name.trim()),
        appearance,
        colors: merge_overrides(get_default_theme_colors(appearance), overrides),
        variants,
        collection,
        managed,
        sidebar_artwork: false,
    })
}

fn theme_colors_to_camel_map(colors: &ThemeColors) -> Map<String, Value> {
    let mut map = Map::new();
    for (role, value) in colors.entries() {
        map.insert(snake_to_camel_role(role), Value::String(value.to_string()));
    }
    map
}

fn stored_theme_from_definition(theme: &ThemeDefinition) -> Value {
    let mut object = Map::new();
    object.insert("id".to_string(), Value::String(theme.id.to_string()));
    object.insert("label".to_string(), Value::String(theme.label.to_string()));
    object.insert(
        "appearance".to_string(),
        Value::String(appearance_to_str(theme.appearance).to_string()),
    );
    object.insert(
        "colors".to_string(),
        Value::Object(theme_colors_to_camel_map(&theme.colors)),
    );
    let mut variants = Map::new();
    if let Some(light) = &theme.variants.light {
        variants.insert(
            "light".to_string(),
            Value::Object(theme_colors_to_camel_map(light)),
        );
    }
    if let Some(dark) = &theme.variants.dark {
        variants.insert(
            "dark".to_string(),
            Value::Object(theme_colors_to_camel_map(dark)),
        );
    }
    if !variants.is_empty() {
        object.insert("variants".to_string(), Value::Object(variants));
    }
    if let Some(collection) = &theme.collection {
        let mut collection_object = Map::new();
        collection_object.insert("id".to_string(), Value::String(collection.id.to_string()));
        collection_object.insert(
            "label".to_string(),
            Value::String(collection.label.to_string()),
        );
        object.insert("collection".to_string(), Value::Object(collection_object));
    }
    if theme.managed {
        object.insert("managed".to_string(), Value::Bool(true));
    }
    Value::Object(object)
}

pub fn serialize_theme_file(theme: ThemeDefinition) -> Result<String, String> {
    let canonical = canonicalize_theme_definition(theme)?;
    let mut file = Map::new();
    file.insert(
        "version".to_string(),
        Value::Number(THEME_FILE_VERSION.into()),
    );
    file.insert("id".to_string(), Value::String(canonical.id.to_string()));
    file.insert(
        "name".to_string(),
        Value::String(canonical.label.to_string()),
    );
    file.insert(
        "appearance".to_string(),
        Value::String(appearance_to_str(canonical.appearance).to_string()),
    );
    file.insert(
        "colors".to_string(),
        Value::Object(theme_colors_to_camel_map(&canonical.colors)),
    );
    let mut variants = Map::new();
    if let Some(light) = &canonical.variants.light {
        variants.insert(
            "light".to_string(),
            Value::Object(theme_colors_to_camel_map(light)),
        );
    }
    if let Some(dark) = &canonical.variants.dark {
        variants.insert(
            "dark".to_string(),
            Value::Object(theme_colors_to_camel_map(dark)),
        );
    }
    if !variants.is_empty() {
        file.insert("variants".to_string(), Value::Object(variants));
    }
    if let Some(collection) = &canonical.collection {
        let mut collection_object = Map::new();
        collection_object.insert("id".to_string(), Value::String(collection.id.to_string()));
        collection_object.insert(
            "label".to_string(),
            Value::String(collection.label.to_string()),
        );
        file.insert("collection".to_string(), Value::Object(collection_object));
    }
    if canonical.managed {
        file.insert("managed".to_string(), Value::Bool(true));
    }
    Ok(format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(file)).map_err(|error| error.to_string())?
    ))
}

pub fn theme_id_from_name(name: &str) -> String {
    let mut normalized = String::new();
    let mut last_hyphen = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            normalized.push(ch);
            last_hyphen = false;
        } else if !last_hyphen {
            normalized.push('-');
            last_hyphen = true;
        }
    }
    let trimmed = normalized.trim_matches('-');
    let truncated: String = trimmed.chars().take(48).collect();
    if truncated.is_empty() {
        "custom-theme".to_string()
    } else {
        truncated
    }
}

fn mix_theme_rgb_colors(base: ThemeRgbColor, overlay: ThemeRgbColor, amount: f64) -> ThemeRgbColor {
    ThemeRgbColor {
        r: base.r + (overlay.r - base.r) * amount,
        g: base.g + (overlay.g - base.g) * amount,
        b: base.b + (overlay.b - base.b) * amount,
    }
}

fn theme_relative_luminance(color: ThemeRgbColor) -> f64 {
    let linearize = |channel: f64| {
        let normalized = channel / 255.0;
        if normalized <= 0.03928 {
            normalized / 12.92
        } else {
            ((normalized + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linearize(color.r) + 0.7152 * linearize(color.g) + 0.0722 * linearize(color.b)
}

fn theme_contrast_ratio(first: ThemeRgbColor, second: ThemeRgbColor) -> f64 {
    let first_luminance = theme_relative_luminance(first);
    let second_luminance = theme_relative_luminance(second);
    let lighter = first_luminance.max(second_luminance);
    let darker = first_luminance.min(second_luminance);
    (lighter + 0.05) / (darker + 0.05)
}

fn parse_theme_rgb_color(value: &str, fallback: ThemeRgbColor) -> ThemeRgbColor {
    parse_theme_color(value)
        .map(|parsed| theme_oklch_to_rgb(parsed.color))
        .unwrap_or(fallback)
}

fn solve_oklch_lightness(
    base: ThemeOklch,
    against: ThemeRgbColor,
    min_contrast: f64,
    direction: &str,
) -> ThemeOklch {
    let (mut low, mut high) = if direction == "lighter" {
        (base.l, 1.0)
    } else {
        (0.0, base.l)
    };
    let mut candidate = base;
    if theme_contrast_ratio(theme_oklch_to_rgb(candidate), against) >= min_contrast {
        return candidate;
    }
    for _ in 0..18 {
        let mid = (low + high) / 2.0;
        candidate = ThemeOklch { l: mid, ..base };
        let contrast = theme_contrast_ratio(theme_oklch_to_rgb(candidate), against);
        if contrast >= min_contrast {
            if direction == "lighter" {
                high = mid;
            } else {
                low = mid;
            }
        } else if direction == "lighter" {
            low = mid;
        } else {
            high = mid;
        }
    }
    ThemeOklch {
        l: if direction == "lighter" { high } else { low },
        ..base
    }
}

const STANDARD_LIGHT_MUTED_CONTRAST: f64 = 4.705;
const STANDARD_DARK_MUTED_CONTRAST: f64 = 5.082;

fn readable_theme_foreground(background: ThemeRgbColor) -> ThemeRgbColor {
    let light_contrast = theme_contrast_ratio(background, THEME_LIGHT_FOREGROUND);
    let dark_contrast = theme_contrast_ratio(background, THEME_DARK_FOREGROUND);
    if light_contrast.max(dark_contrast) >= 4.5 {
        return if light_contrast >= dark_contrast {
            THEME_LIGHT_FOREGROUND
        } else {
            THEME_DARK_FOREGROUND
        };
    }
    if theme_contrast_ratio(background, THEME_WHITE_FOREGROUND)
        >= theme_contrast_ratio(background, THEME_BLACK_FOREGROUND)
    {
        THEME_WHITE_FOREGROUND
    } else {
        THEME_BLACK_FOREGROUND
    }
}

fn readable_theme_text(
    background: ThemeRgbColor,
    foreground: ThemeRgbColor,
    amount: f64,
    minimum_ratio: f64,
) -> ThemeRgbColor {
    let softened = mix_theme_rgb_colors(foreground, background, amount);
    if theme_contrast_ratio(softened, background) >= minimum_ratio {
        return softened;
    }
    let mut readable = foreground;
    let mut lower_amount = 0.0;
    let mut upper_amount = amount;
    for _ in 0..12 {
        let candidate_amount = (lower_amount + upper_amount) / 2.0;
        let candidate = mix_theme_rgb_colors(foreground, background, candidate_amount);
        if theme_contrast_ratio(candidate, background) >= minimum_ratio {
            readable = candidate;
            lower_amount = candidate_amount;
        } else {
            upper_amount = candidate_amount;
        }
    }
    readable
}

fn standard_muted_theme_text(
    background: ThemeRgbColor,
    foreground: ThemeRgbColor,
) -> ThemeRgbColor {
    let target = if theme_relative_luminance(background) < 0.179 {
        STANDARD_DARK_MUTED_CONTRAST
    } else {
        STANDARD_LIGHT_MUTED_CONTRAST
    };
    readable_theme_text(background, foreground, 1.0, target)
}

fn standard_status_colors(canvas: ThemeRgbColor) -> HashMap<String, SharedString> {
    let appearance = if theme_relative_luminance(canvas) < 0.179 {
        ThemeAppearance::Dark
    } else {
        ThemeAppearance::Light
    };
    let (error, error_foreground, warning, warning_foreground) = match appearance {
        ThemeAppearance::Light => ("#fb2c36", "#c10007", "#fe9a00", "#bb4d00"),
        ThemeAppearance::Dark => ("#fb414a", "#ff6467", "#fe9a00", "#ffb900"),
    };
    let surface_mix = if appearance == ThemeAppearance::Dark {
        0.16
    } else {
        0.08
    };
    let surface_of = |value: &str| {
        mix_theme_rgb_colors(canvas, parse_theme_rgb_color(value, canvas), surface_mix)
    };
    let readable_on = |foreground: &str, surface: ThemeRgbColor| {
        theme_oklch_to_theme_color(solve_oklch_lightness(
            theme_rgb_to_oklch(parse_theme_rgb_color(foreground, canvas)),
            surface,
            4.6,
            if appearance == ThemeAppearance::Dark {
                "lighter"
            } else {
                "darker"
            },
        ))
    };
    let error_surface = surface_of(error);
    let warning_surface = surface_of(warning);
    let mut map = HashMap::new();
    map.insert(
        "error".to_string(),
        to_canonical_theme_color(error).expect("standard error"),
    );
    map.insert(
        "error_foreground".to_string(),
        readable_on(error_foreground, error_surface),
    );
    map.insert(
        "error_surface".to_string(),
        theme_rgb_to_theme_color(error_surface),
    );
    map.insert(
        "warning".to_string(),
        to_canonical_theme_color(warning).expect("standard warning"),
    );
    map.insert(
        "warning_foreground".to_string(),
        readable_on(warning_foreground, warning_surface),
    );
    map.insert(
        "warning_surface".to_string(),
        theme_rgb_to_theme_color(warning_surface),
    );
    map
}

pub fn create_vivid_theme_colors(
    appearance: ThemeAppearance,
    background_value: &str,
    accent_value: &str,
) -> ThemeColors {
    let defaults = get_default_theme_colors(appearance);
    let canvas_rgb = parse_theme_rgb_color(
        background_value,
        if appearance == ThemeAppearance::Dark {
            ThemeRgbColor {
                r: 24.0,
                g: 15.0,
                b: 27.0,
            }
        } else {
            ThemeRgbColor {
                r: 250.0,
                g: 245.0,
                b: 250.0,
            }
        },
    );
    let accent_rgb = parse_theme_rgb_color(
        accent_value,
        ThemeRgbColor {
            r: 168.0,
            g: 67.0,
            b: 112.0,
        },
    );
    let canvas = theme_rgb_to_oklch(canvas_rgb);
    let accent = theme_rgb_to_oklch(accent_rgb);
    let dark = theme_relative_luminance(canvas_rgb) < 0.179;
    let hue = if accent.c < 0.02 { canvas.h } else { accent.h };
    let tint_c = accent.c.mul_add(0.22, 0.0).clamp(0.008, 0.045);
    let step = if dark { 1.0 } else { -1.0 };
    let surface_at = |delta_l: f64, chroma: f64| ThemeOklch {
        l: (canvas.l + step * delta_l).clamp(0.05, 0.98),
        c: chroma,
        h: hue,
    };
    let theme_color = |color: ThemeOklch| theme_oklch_to_theme_color(color);
    let text_base = ThemeOklch {
        l: if dark { 0.95 } else { 0.2 },
        c: (accent.c * 0.25).min(0.035),
        h: hue,
    };
    let text = solve_oklch_lightness(
        text_base,
        canvas_rgb,
        7.0,
        if dark { "lighter" } else { "darker" },
    );
    let text_rgb = theme_oklch_to_rgb(text);
    let text_muted_rgb = standard_muted_theme_text(canvas_rgb, text_rgb);
    let action = ThemeOklch {
        l: (accent.l + if dark { 0.06 } else { -0.02 }).clamp(0.35, 0.85),
        c: accent.c.mul_add(0.9, 0.0).max(0.06),
        h: (hue + 50.0) % 360.0,
    };
    let action_rgb = theme_oklch_to_rgb(action);
    let action_foreground = readable_theme_foreground(action_rgb);
    let accent_foreground = readable_theme_foreground(accent_rgb);
    let sidebar = surface_at(0.045, tint_c * 1.4);
    let sidebar_rgb = theme_oklch_to_rgb(sidebar);
    let surface = surface_at(0.015, tint_c);
    let surface_raised = surface_at(0.05, tint_c);
    let surface_raised_rgb = theme_oklch_to_rgb(surface_raised);
    let surface_overlay = surface_at(0.075, tint_c);
    let border = surface_at(if dark { 0.16 } else { 0.12 }, (accent.c * 0.35).min(0.07));
    let input = surface_at(if dark { 0.21 } else { 0.16 }, (accent.c * 0.4).min(0.08));
    let secondary = surface_at(if dark { 0.1 } else { 0.06 }, (accent.c * 0.5).min(0.09));
    let secondary_rgb = theme_oklch_to_rgb(secondary);
    let muted = surface_at(if dark { 0.06 } else { 0.04 }, (accent.c * 0.35).min(0.06));
    let muted_rgb = theme_oklch_to_rgb(muted);
    let accent_surface = surface_at(if dark { 0.13 } else { 0.08 }, (accent.c * 0.55).min(0.11));
    let accent_surface_rgb = theme_oklch_to_rgb(accent_surface);
    let message_surface = surface_at(if dark { 0.16 } else { 0.1 }, (accent.c * 0.6).min(0.13));
    let message_surface_rgb = theme_oklch_to_rgb(message_surface);
    let code_background = surface_at(0.035, tint_c * 0.8);
    let update_surface = surface_at(if dark { 0.14 } else { 0.09 }, (accent.c * 0.55).min(0.12));
    let foreground_on = |surface_rgb: ThemeRgbColor| {
        theme_oklch_to_theme_color(solve_oklch_lightness(
            text_base,
            surface_rgb,
            4.6,
            if dark { "lighter" } else { "darker" },
        ))
    };
    let muted_foreground =
        theme_rgb_to_theme_color(readable_theme_text(muted_rgb, text_rgb, 1.0, 4.6));
    let placeholder =
        theme_rgb_to_theme_color(readable_theme_text(surface_raised_rgb, text_rgb, 1.0, 4.6));
    let search_match = theme_oklch_to_rgb(surface_at(0.22, (accent.c * 0.12).min(0.12)));
    let search_match_active = theme_oklch_to_rgb(surface_at(0.4, (accent.c * 0.18).min(0.18)));
    let action_hover = ThemeOklch {
        l: action.l + if dark { 0.06 } else { -0.06 },
        ..action
    };
    let status = standard_status_colors(canvas_rgb);
    let mut colors = defaults;
    for (role, value) in status {
        set_color_role(&mut colors, role.as_str(), value);
    }
    colors.canvas = theme_rgb_to_theme_color(canvas_rgb);
    colors.chrome = colors.canvas.clone();
    colors.toolbar = colors.canvas.clone();
    colors.toolbar_foreground = theme_rgb_to_theme_color(text_rgb);
    colors.toolbar_border = theme_color(surface_at(
        if dark { 0.14 } else { 0.1 },
        (accent.c * 0.4).min(0.08),
    ));
    colors.toolbar_control = theme_color(surface_at(if dark { 0.09 } else { 0.05 }, tint_c * 1.3));
    colors.toolbar_control_foreground = theme_rgb_to_theme_color(text_rgb);
    colors.toolbar_control_hover =
        theme_color(surface_at(if dark { 0.14 } else { 0.09 }, tint_c * 1.6));
    colors.surface = theme_color(surface);
    colors.surface_raised = theme_color(surface_raised);
    colors.surface_overlay = theme_color(surface_overlay);
    colors.text = theme_rgb_to_theme_color(text_rgb);
    colors.text_muted = theme_rgb_to_theme_color(text_muted_rgb);
    colors.border = theme_color(border);
    colors.input = theme_color(input);
    colors.focus = theme_rgb_to_theme_color(accent_rgb);
    colors.accent = theme_rgb_to_theme_color(accent_rgb);
    colors.accent_foreground = theme_rgb_to_theme_color(accent_foreground);
    colors.secondary = theme_color(secondary);
    colors.secondary_foreground = foreground_on(secondary_rgb);
    colors.muted = theme_color(muted);
    colors.muted_foreground = muted_foreground;
    colors.placeholder = placeholder.clone();
    colors.secondary_label = theme_rgb_to_theme_color(text_muted_rgb);
    colors.icon_muted = theme_rgb_to_theme_color(text_muted_rgb);
    colors.update = theme_rgb_to_theme_color(accent_rgb);
    colors.update_foreground = foreground_on(theme_oklch_to_rgb(update_surface));
    colors.update_surface = theme_color(update_surface);
    colors.accent_surface = theme_color(accent_surface);
    colors.accent_surface_foreground = foreground_on(accent_surface_rgb);
    colors.message_surface = theme_color(message_surface);
    colors.message_foreground = foreground_on(message_surface_rgb);
    colors.message_action = theme_rgb_to_theme_color(action_rgb);
    colors.message_action_foreground = theme_rgb_to_theme_color(action_foreground);
    colors.message_action_hover = theme_color(action_hover);
    colors.code_background = theme_color(code_background);
    colors.code_foreground = theme_rgb_to_theme_color(text_rgb);
    colors.search_match_background = theme_rgb_to_theme_color(search_match);
    colors.search_match_foreground =
        theme_rgb_to_theme_color(readable_theme_foreground(search_match));
    colors.search_match_active_background = theme_rgb_to_theme_color(search_match_active);
    colors.search_match_active_foreground =
        theme_rgb_to_theme_color(readable_theme_foreground(search_match_active));
    colors.sidebar = theme_color(sidebar);
    colors.sidebar_foreground = foreground_on(sidebar_rgb);
    colors.sidebar_muted_foreground =
        theme_rgb_to_theme_color(standard_muted_theme_text(sidebar_rgb, text_rgb));
    colors.sidebar_control_surface =
        theme_color(surface_at(if dark { 0.1 } else { 0.07 }, tint_c * 1.5));
    colors.sidebar_row_hover = theme_color(surface_at(
        if dark { 0.08 } else { 0.06 },
        (accent.c * 0.45).min(0.08),
    ));
    colors.sidebar_row_active = theme_color(surface_at(
        if dark { 0.12 } else { 0.09 },
        (accent.c * 0.55).min(0.1),
    ));
    colors.sidebar_row_selected = theme_color(surface_at(
        if dark { 0.14 } else { 0.1 },
        (accent.c * 0.6).min(0.11),
    ));
    colors.sidebar_border = theme_color(surface_at(
        if dark { 0.17 } else { 0.12 },
        (accent.c * 0.4).min(0.08),
    ));
    colors.terminal_background = theme_rgb_to_theme_color(canvas_rgb);
    colors.terminal_foreground = theme_rgb_to_theme_color(text_rgb);
    colors.terminal_cursor = theme_rgb_to_theme_color(accent_rgb);
    colors.terminal_selection = theme_color(surface_at(
        if dark { 0.18 } else { 0.12 },
        (accent.c * 0.55).min(0.12),
    ));
    colors.terminal_scrollbar = theme_color(surface_at(if dark { 0.22 } else { 0.16 }, tint_c));
    colors.terminal_scrollbar_hover =
        theme_color(surface_at(if dark { 0.3 } else { 0.22 }, tint_c));
    colors
}

pub fn update_theme_color_family(
    appearance: ThemeAppearance,
    colors: ThemeColors,
    role: &str,
    value: &str,
) -> ThemeColors {
    let Some(parsed_selected) = parse_theme_color(value) else {
        let mut next = colors.clone();
        if set_color_role(&mut next, role, SharedString::from(value)) {
            return next;
        }
        return colors;
    };
    let normalized = SharedString::from(format_oklch_theme_color(
        parsed_selected.color,
        parsed_selected.alpha,
    ));
    let canvas = parse_theme_rgb_color(
        colors.canvas.as_str(),
        if appearance == ThemeAppearance::Dark {
            ThemeRgbColor {
                r: 24.0,
                g: 15.0,
                b: 27.0,
            }
        } else {
            ThemeRgbColor {
                r: 250.0,
                g: 245.0,
                b: 250.0,
            }
        },
    );
    let selected = theme_oklch_to_rgb(parsed_selected.color);
    let selected_on = |background: ThemeRgbColor| {
        mix_theme_rgb_colors(background, selected, parsed_selected.alpha)
    };
    let selected_on_canvas = selected_on(canvas);
    let accent = parse_theme_rgb_color(
        colors.accent.as_str(),
        ThemeRgbColor {
            r: 168.0,
            g: 67.0,
            b: 112.0,
        },
    );
    let canvas_is_dark = theme_relative_luminance(canvas) < 0.179;
    let terminal_is_dark = theme_relative_luminance(selected_on_canvas) < 0.179;
    let color_of = |color: ThemeRgbColor| theme_rgb_to_theme_color(color);
    let foreground_on = |background: ThemeRgbColor| color_of(readable_theme_foreground(background));
    let selected_tone_on = |background: ThemeRgbColor| {
        theme_oklch_to_theme_color(solve_oklch_lightness(
            parsed_selected.color,
            background,
            4.6,
            if theme_relative_luminance(background) < 0.179 {
                "lighter"
            } else {
                "darker"
            },
        ))
    };
    let status_colors = || {
        let surface = mix_theme_rgb_colors(
            canvas,
            selected_on_canvas,
            if canvas_is_dark { 0.16 } else { 0.08 },
        );
        (selected_tone_on(surface), color_of(surface))
    };
    let sidebar_source = colors.sidebar.clone();
    let mut next = colors;
    match role {
        "canvas" => {
            next.canvas = normalized.clone();
            next.chrome = normalized.clone();
            next.toolbar = normalized;
        }
        "surface" | "surface_raised" | "surface_overlay" | "input" | "sidebar_control_surface" => {
            set_color_role(&mut next, role, normalized);
        }
        "text" => {
            next.text = normalized.clone();
            next.toolbar_foreground = normalized.clone();
            next.toolbar_control_foreground = normalized;
        }
        "muted_foreground" => {
            next.text_muted = normalized.clone();
            next.muted_foreground = normalized.clone();
            next.placeholder = normalized.clone();
            next.secondary_label = normalized.clone();
            next.icon_muted = normalized.clone();
            next.sidebar_muted_foreground = normalized;
        }
        "border" => {
            next.border = normalized.clone();
            next.toolbar_border = normalized.clone();
            next.sidebar_border = normalized;
        }
        "secondary" => {
            next.secondary = normalized.clone();
            next.secondary_foreground = foreground_on(selected_on_canvas);
            next.muted = normalized.clone();
            next.toolbar_control = normalized;
        }
        "accent_surface" => {
            next.accent_surface = normalized.clone();
            next.accent_surface_foreground = foreground_on(selected_on_canvas);
            next.toolbar_control_hover = normalized;
        }
        "accent" => {
            let update_surface = mix_theme_rgb_colors(
                canvas,
                selected_on_canvas,
                if canvas_is_dark { 0.32 } else { 0.16 },
            );
            next.accent = normalized.clone();
            next.accent_foreground = foreground_on(selected_on_canvas);
            next.focus = normalized.clone();
            next.update = normalized.clone();
            next.update_foreground = selected_tone_on(update_surface);
            next.update_surface = color_of(update_surface);
            next.terminal_cursor = normalized;
        }
        "message_action" => {
            let action_foreground = readable_theme_foreground(selected_on_canvas);
            let toward_opposite = if action_foreground == THEME_LIGHT_FOREGROUND
                || action_foreground == THEME_WHITE_FOREGROUND
            {
                THEME_BLACK_FOREGROUND
            } else {
                THEME_WHITE_FOREGROUND
            };
            let action_hover = mix_theme_rgb_colors(selected, toward_opposite, 0.12);
            next.message_action = normalized;
            next.message_action_foreground = color_of(action_foreground);
            next.message_action_hover = SharedString::from(format_oklch_theme_color(
                theme_rgb_to_oklch(action_hover),
                parsed_selected.alpha,
            ));
        }
        "message_surface" => {
            next.message_surface = normalized;
            next.message_foreground = foreground_on(selected_on_canvas);
        }
        "search_match_background" => {
            next.search_match_background = color_of(selected_on_canvas);
            next.search_match_foreground = foreground_on(selected_on_canvas);
        }
        "search_match_active_background" => {
            next.search_match_active_background = color_of(selected_on_canvas);
            next.search_match_active_foreground = foreground_on(selected_on_canvas);
        }
        "code_background" => {
            next.code_background = normalized;
            next.code_foreground = foreground_on(selected_on_canvas);
        }
        "sidebar" => {
            next.sidebar = normalized;
            next.sidebar_foreground = foreground_on(selected_on_canvas);
        }
        "sidebar_row_selected" => {
            let sidebar = parse_theme_rgb_color(sidebar_source.as_str(), canvas);
            let selected_on_sidebar = selected_on(sidebar);
            next.sidebar_row_hover =
                color_of(mix_theme_rgb_colors(sidebar, selected_on_sidebar, 0.5));
            next.sidebar_row_active =
                color_of(mix_theme_rgb_colors(sidebar, selected_on_sidebar, 0.8));
            next.sidebar_row_selected = normalized;
        }
        "terminal_background" => {
            let terminal_foreground = readable_theme_foreground(selected_on_canvas);
            next.terminal_background = normalized;
            next.terminal_foreground = color_of(terminal_foreground);
            next.terminal_selection = color_of(mix_theme_rgb_colors(
                selected_on_canvas,
                accent,
                if terminal_is_dark { 0.35 } else { 0.18 },
            ));
            next.terminal_scrollbar = color_of(mix_theme_rgb_colors(
                selected_on_canvas,
                terminal_foreground,
                if terminal_is_dark { 0.42 } else { 0.22 },
            ));
            next.terminal_scrollbar_hover = color_of(mix_theme_rgb_colors(
                selected_on_canvas,
                terminal_foreground,
                if terminal_is_dark { 0.55 } else { 0.32 },
            ));
        }
        "error" => {
            let (foreground, surface) = status_colors();
            next.error = normalized;
            next.error_foreground = foreground;
            next.error_surface = surface;
        }
        "warning" => {
            let (foreground, surface) = status_colors();
            next.warning = normalized;
            next.warning_foreground = foreground;
            next.warning_surface = surface;
        }
        _ => {
            set_color_role(&mut next, role, normalized);
        }
    }
    next
}

#[cfg(test)]
mod tests {
    use super::*;
    fn as_hex(value: &str) -> String {
        theme_color_to_hex(value)
            .expect("theme color")
            .chars()
            .take(7)
            .collect()
    }

    fn canonical(value: &str) -> String {
        to_canonical_theme_color(value)
            .expect("theme color")
            .to_string()
    }

    fn contrast_ratio(first: &str, second: &str) -> f64 {
        let parse = |value: &str| {
            let hex = as_hex(value);
            [1, 3, 5].map(|index| {
                u8::from_str_radix(&hex[index..index + 2], 16).expect("hex") as f64 / 255.0
            })
        };
        let luminance = |value: &str| {
            parse(value)
                .map(|channel| {
                    if channel <= 0.03928 {
                        channel / 12.92
                    } else {
                        ((channel + 0.055) / 1.055).powf(2.4)
                    }
                })
                .iter()
                .zip([0.2126, 0.7152, 0.0722])
                .map(|(channel, weight)| channel * weight)
                .sum::<f64>()
        };
        let lighter = luminance(first).max(luminance(second));
        let darker = luminance(first).min(luminance(second));
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn round_trips_custom_search_colours() {
        let legacy = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Legacy",
            "appearance": "dark",
            "colors": { "canvas": "#101010" },
        }))
        .expect("legacy");
        assert_eq!(
            legacy.colors.search_match_background,
            get_default_theme_colors(ThemeAppearance::Dark).search_match_background
        );
        let theme = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Search",
            "appearance": "dark",
            "colors": {
                "searchMatchBackground": "#224466",
                "searchMatchForeground": "#ffffff",
                "searchMatchActiveBackground": "#aaddff",
                "searchMatchActiveForeground": "#112233",
            },
            "variants": { "light": { "searchMatchBackground": "#ddeeff", "searchMatchForeground": "#112233" } },
        }))
        .expect("search");
        let restored: ThemeDefinition = parse_theme_file(
            serde_json::from_str(&serialize_theme_file(theme.clone()).expect("serialize"))
                .expect("json"),
        )
        .expect("restored");
        assert_eq!(restored.colors, theme.colors);
        assert_eq!(restored.variants.light, theme.variants.light);
    }

    #[test]
    fn merges_small_user_file_onto_base_palette() {
        let theme = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Ocean dusk",
            "appearance": "dark",
            "colors": { "canvas": "#07152f", "accent": "#67c2ff" },
        }))
        .expect("theme");
        assert_eq!(theme.id.as_str(), "ocean-dusk");
        assert_eq!(theme.label.as_str(), "Ocean dusk");
        assert_eq!(theme.appearance, ThemeAppearance::Dark);
        assert_eq!(theme.colors.canvas.as_str(), canonical("#07152f"));
        assert_eq!(theme.colors.accent.as_str(), canonical("#67c2ff"));
        assert_eq!(theme.colors.placeholder.as_str(), canonical("#968d9f"));
    }

    #[test]
    fn decodes_literal_css_color_formats() {
        let theme = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Translucent",
            "appearance": "light",
            "colors": {
                "canvas": "oklch(62% 0.2 280deg / 50%)",
                "accent": "#abcd",
                "focus": "rgb(10 20 30 / 50%)",
                "error": "hsl(350 80% 50%)",
                "warning": "hwb(45 10% 20%)",
                "update": "lab(60% 40 30)",
                "messageAction": "lch(60% 50 120)",
                "sidebar": "oklab(0.6 0.1 -0.1)",
                "terminalCursor": "color(display-p3 0.8 0.2 0.3)",
                "terminalSelection": "rebeccapurple",
                "terminalScrollbar": "transparent",
                "terminalScrollbarHover": "rgb(10 20 30 / none)",
            },
        }))
        .expect("theme");
        assert_eq!(theme.colors.canvas.as_str(), "oklch(0.62 0.2 280 / 0.5)");
        assert_eq!(theme.colors.accent.as_str(), canonical("#abcd"));
        assert_eq!(
            theme_color_to_hex(theme.colors.accent.as_str()),
            Some("#aabbccdd".to_string())
        );
        assert_eq!(
            theme_color_to_hex(theme.colors.focus.as_str()),
            Some("#0a141e80".to_string())
        );
        assert_eq!(
            theme_color_to_hex(theme.colors.terminal_selection.as_str()),
            Some("#663399".to_string())
        );
        assert_eq!(theme.colors.terminal_scrollbar.as_str(), "oklch(0 0 0 / 0)");
        assert_eq!(
            theme_color_to_hex(theme.colors.terminal_scrollbar_hover.as_str()),
            Some("#0a141e00".to_string())
        );
    }

    #[test]
    fn rejects_unknown_roles_and_invalid_colors() {
        let background = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Broken",
            "appearance": "light",
            "colors": { "background": "#ffffff" },
        }));
        assert!(background.is_err());
        let accent = parse_theme_file(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Broken",
            "appearance": "light",
            "colors": { "accent": "var(--danger)" },
        }));
        assert!(accent.is_err());
    }

    #[test]
    fn derives_vivid_palettes_from_seeds() {
        let seeds = [
            ("light", "#f4f9f2", "#1d8a4e"),
            ("dark", "#101a2c", "#4f8fe8"),
            ("dark", "#211a23", "#df5398"),
            ("light", "#fdf6ec", "#c2571b"),
            ("light", "#111827", "#8ab4f8"),
            ("dark", "#f5ecf5", "#a84370"),
        ];
        for (appearance, canvas, accent) in seeds {
            let appearance = if appearance == "light" {
                ThemeAppearance::Light
            } else {
                ThemeAppearance::Dark
            };
            let colors = create_vivid_theme_colors(appearance, canvas, accent);
            assert!(colors.canvas.as_str().starts_with("oklch("));
            assert_eq!(as_hex(colors.canvas.as_str()), canvas);
            assert_eq!(as_hex(colors.accent.as_str()), accent);
            assert!(contrast_ratio(colors.text.as_str(), colors.canvas.as_str()) >= 7.0);
            assert!(contrast_ratio(colors.text_muted.as_str(), colors.canvas.as_str()) >= 4.5);
            assert!(
                contrast_ratio(
                    colors.error_foreground.as_str(),
                    colors.error_surface.as_str()
                ) >= 4.5
            );
        }
    }

    #[test]
    fn canonicalizes_explicit_writes_without_migrating_untouched() {
        let storage = Arc::new(InMemoryThemeLibraryStorage::new());
        set_theme_library_storage(storage.clone());
        invalidate_custom_themes();
        let untouched = serde_json::json!({
            "id": "legacy",
            "label": "Legacy",
            "appearance": "dark",
            "colors": { "accent": "#5b6cff", "futureRole": "hsl(10 20% 30%)" },
            "futureMetadata": { "version": 2 },
        });
        storage
            .write(&serde_json::to_string(&vec![untouched.clone()]).unwrap())
            .expect("write");
        invalidate_custom_themes();
        let created = install_custom_theme(
            parse_theme_file(serde_json::json!({
                "version": THEME_FILE_VERSION,
                "id": "aurora",
                "name": "Aurora",
                "appearance": "light",
                "colors": { "canvas": "#f8fbff", "accent": "#5b6cff" },
            }))
            .expect("parse"),
        )
        .expect("install");
        let updated = update_custom_theme(ThemeDefinition {
            label: SharedString::from("Aurora Night"),
            colors: ThemeColors {
                accent: SharedString::from("hsl(263 70% 58%)"),
                ..created.colors
            },
            ..created
        })
        .expect("update");
        assert_eq!(updated.label.as_str(), "Aurora Night");
        assert_eq!(
            updated.colors.accent.as_str(),
            canonical("hsl(263 70% 58%)")
        );
        remove_custom_theme("aurora");
        invalidate_custom_themes();
    }
}

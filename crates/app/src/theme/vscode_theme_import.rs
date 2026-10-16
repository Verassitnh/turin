//! Port of upstream/apps/web/src/vscodeThemeImport.ts

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use indexmap::IndexMap;
use regex::Regex;
use serde_json::{Map, Value};

use super::theme_palette::{get_standard_theme_colors, get_theme_modes};
use super::theme_palette_library::{
    THEME_FILE_VERSION, create_vivid_theme_colors, is_reserved_theme_id, parse_theme_file,
    theme_color_to_hex, theme_id_from_name,
};
use super::theme_palettes::{ThemeAppearance, ThemeColors, ThemeDefinition, ThemeVariants};

#[derive(Clone, Copy, Debug, PartialEq)]
struct VsCodeRgba {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct VsCodeRgb {
    r: f64,
    g: f64,
    b: f64,
}

fn decode_gamma(value: f64) -> f64 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn encode_gamma(value: f64) -> f64 {
    let clamped = value.clamp(0.0, 1.0);
    if clamped <= 0.0031308 {
        clamped * 12.92
    } else {
        1.055 * clamped.powf(1.0 / 2.4) - 0.055
    }
}

fn parse_color_function(value: &str) -> Option<VsCodeRgba> {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        Regex::new(r"(?i)^color\(\s*(display-p3|srgb)\s+([^)]+)\)$").expect("color() regex")
    });
    let captures = re.captures(value)?;
    let space = captures.get(1)?.as_str().to_ascii_lowercase();
    let body = captures.get(2)?.as_str();
    let parts: Vec<&str> = body.split('/').collect();
    let channel_part = parts.first()?;
    let alpha_part = parts.get(1);
    let channels: Vec<f64> = channel_part
        .split_whitespace()
        .map(|part| {
            if let Some(stripped) = part.strip_suffix('%') {
                stripped.parse::<f64>().ok().map(|v| v / 100.0)
            } else {
                part.parse::<f64>().ok()
            }
        })
        .collect::<Option<_>>()?;
    if channels.len() != 3 {
        return None;
    }
    let alpha = match alpha_part.as_ref().map(|part| part.trim()) {
        None => 1.0,
        Some(raw) if raw.ends_with('%') => raw.trim_end_matches('%').parse::<f64>().ok()? / 100.0,
        Some(raw) => raw.parse::<f64>().ok()?,
    };
    if !alpha.is_finite() {
        return None;
    }
    let [red, green, blue] = [channels[0], channels[1], channels[2]];
    if space == "srgb" {
        return Some(VsCodeRgba {
            r: red * 255.0,
            g: green * 255.0,
            b: blue * 255.0,
            a: alpha.clamp(0.0, 1.0),
        });
    }
    let linear_red = decode_gamma(red);
    let linear_green = decode_gamma(green);
    let linear_blue = decode_gamma(blue);
    let srgb = [
        1.2249401762805 * linear_red - 0.2249401762805 * linear_green,
        -0.042056961239 * linear_red + 1.042056961239 * linear_green,
        -0.0196375547643 * linear_red - 0.0786360655012 * linear_green
            + 1.0982736202656 * linear_blue,
    ]
    .map(|channel| encode_gamma(channel) * 255.0);
    Some(VsCodeRgba {
        r: srgb[0],
        g: srgb[1],
        b: srgb[2],
        a: alpha.clamp(0.0, 1.0),
    })
}

fn parse_vscode_color(value: &Value) -> Option<VsCodeRgba> {
    let trimmed = value.as_str()?.trim();
    if trimmed.starts_with("color(") {
        return parse_color_function(trimmed);
    }
    let hex = trimmed.trim_start_matches('#');
    static HEX_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let re = HEX_RE.get_or_init(|| {
        Regex::new(r"(?i)^(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$").expect("hex regex")
    });
    if !re.is_match(hex) {
        return None;
    }
    let expand = |part: &str| -> u8 {
        if part.len() == 1 {
            u8::from_str_radix(&format!("{part}{part}"), 16).unwrap_or(0)
        } else {
            u8::from_str_radix(part, 16).unwrap_or(0)
        }
    };
    if hex.len() <= 4 {
        let chars: Vec<char> = hex.chars().collect();
        return Some(VsCodeRgba {
            r: f64::from(expand(&chars[0].to_string())),
            g: f64::from(expand(&chars[1].to_string())),
            b: f64::from(expand(&chars[2].to_string())),
            a: if hex.len() == 4 {
                f64::from(expand(&chars[3].to_string())) / 255.0
            } else {
                1.0
            },
        });
    }
    Some(VsCodeRgba {
        r: f64::from(u8::from_str_radix(&hex[0..2], 16).ok()?),
        g: f64::from(u8::from_str_radix(&hex[2..4], 16).ok()?),
        b: f64::from(u8::from_str_radix(&hex[4..6], 16).ok()?),
        a: if hex.len() == 8 {
            f64::from(u8::from_str_radix(&hex[6..8], 16).ok()?) / 255.0
        } else {
            1.0
        },
    })
}

fn to_hex(color: VsCodeRgb) -> String {
    let channel = |value: f64| format!("{:02x}", (value.round() as i32).clamp(0, 255));
    format!(
        "#{}{}{}",
        channel(color.r),
        channel(color.g),
        channel(color.b)
    )
}

fn flatten_over(color: VsCodeRgba, base: VsCodeRgb) -> String {
    if color.a >= 1.0 {
        return to_hex(VsCodeRgb {
            r: color.r,
            g: color.g,
            b: color.b,
        });
    }
    to_hex(VsCodeRgb {
        r: color.r * color.a + base.r * (1.0 - color.a),
        g: color.g * color.a + base.g * (1.0 - color.a),
        b: color.b * color.a + base.b * (1.0 - color.a),
    })
}

fn relative_luminance(color: VsCodeRgb) -> f64 {
    let channel = |value: f64| {
        let ratio = value / 255.0;
        if ratio <= 0.03928 {
            ratio / 12.92
        } else {
            ((ratio + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

fn contrast_ratio(first: VsCodeRgb, second: VsCodeRgb) -> f64 {
    let a = relative_luminance(first);
    let b = relative_luminance(second);
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn hex_to_rgb(value: &str) -> VsCodeRgb {
    let hex = theme_color_to_hex(value).unwrap_or_else(|| value.to_string());
    let parsed = parse_vscode_color(&Value::String(hex)).unwrap_or(VsCodeRgba {
        r: 0.0,
        g: 0.0,
        b: 0.0,
        a: 1.0,
    });
    VsCodeRgb {
        r: parsed.r,
        g: parsed.g,
        b: parsed.b,
    }
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

fn theme_colors_to_camel_map(colors: &ThemeColors) -> Map<String, Value> {
    let mut map = Map::new();
    for (role, value) in colors.entries() {
        map.insert(snake_to_camel_role(role), Value::String(value.to_string()));
    }
    map
}

fn appearance_to_str(appearance: ThemeAppearance) -> &'static str {
    match appearance {
        ThemeAppearance::Light => "light",
        ThemeAppearance::Dark => "dark",
    }
}

fn build_theme_file(
    name: &str,
    appearance: ThemeAppearance,
    id: Option<&str>,
    colors: Map<String, Value>,
    variants: Option<&ThemeVariants>,
    managed: bool,
) -> Result<ThemeDefinition, String> {
    let mut file = Map::new();
    file.insert(
        "version".to_string(),
        Value::Number(THEME_FILE_VERSION.into()),
    );
    if let Some(id) = id {
        file.insert("id".to_string(), Value::String(id.to_string()));
    }
    file.insert("name".to_string(), Value::String(name.to_string()));
    file.insert(
        "appearance".to_string(),
        Value::String(appearance_to_str(appearance).to_string()),
    );
    file.insert("colors".to_string(), Value::Object(colors));
    if let Some(variants) = variants {
        let mut variant_map = Map::new();
        if let Some(light) = &variants.light {
            variant_map.insert(
                "light".to_string(),
                Value::Object(theme_colors_to_camel_map(light)),
            );
        }
        if let Some(dark) = &variants.dark {
            variant_map.insert(
                "dark".to_string(),
                Value::Object(theme_colors_to_camel_map(dark)),
            );
        }
        if !variant_map.is_empty() {
            file.insert("variants".to_string(), Value::Object(variant_map));
        }
    }
    if managed {
        file.insert("managed".to_string(), Value::Bool(true));
    }
    parse_theme_file(Value::Object(file))
}

/// A VS Code color theme is recognised by workbench colors or tokenColors.
pub fn is_vscode_theme_file(value: &Value) -> bool {
    let Some(object) = value.as_object() else {
        return false;
    };
    if object.get("version").and_then(Value::as_u64) == Some(u64::from(THEME_FILE_VERSION)) {
        return false;
    }
    let has_workbench_colors = object
        .get("colors")
        .and_then(Value::as_object)
        .is_some_and(|colors| colors.keys().any(|key| key.contains('.')));
    has_workbench_colors
        || object
            .get("tokenColors")
            .and_then(Value::as_array)
            .is_some()
}

fn resolve_appearance(value: &Map<String, Value>, canvas: VsCodeRgb) -> ThemeAppearance {
    let theme_type = value
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_ascii_lowercase);
    match theme_type.as_deref() {
        Some("light") | Some("hc-light") => ThemeAppearance::Light,
        Some("dark") | Some("hc-black") => ThemeAppearance::Dark,
        _ => {
            if relative_luminance(canvas) < 0.179 {
                ThemeAppearance::Dark
            } else {
                ThemeAppearance::Light
            }
        }
    }
}

/// Extension `name` fields are often package slugs; read them as words.
pub fn humanize_theme_name(raw: &str) -> String {
    let trimmed = raw.trim();
    static WS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    static SEP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let ws = WS.get_or_init(|| Regex::new(r"\s").expect("ws"));
    let sep = SEP.get_or_init(|| Regex::new(r"[-_.]").expect("sep"));
    if ws.is_match(trimmed) || !sep.is_match(trimmed) {
        return trimmed.to_string();
    }
    static SPLIT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let split = SPLIT.get_or_init(|| Regex::new(r"[-_.]+").expect("split"));
    split
        .split(trimmed)
        .filter(|word| !word.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => {
                    let mut out = first.to_uppercase().collect::<String>();
                    out.extend(chars);
                    out
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn resolve_name(value: &Map<String, Value>) -> String {
    for key in ["displayName", "name"] {
        let Some(candidate) = value.get(key).and_then(Value::as_str) else {
            continue;
        };
        let humanized = humanize_theme_name(candidate);
        if !humanized.is_empty() {
            let truncated: String = humanized.chars().take(48).collect();
            return truncated;
        }
    }
    "VS Code theme".to_string()
}

fn set_color(map: &mut Map<String, Value>, role: &str, value: String) {
    map.insert(snake_to_camel_role(role), Value::String(value));
}

pub fn parse_vscode_theme_file(value: Value) -> Result<ThemeDefinition, String> {
    let object = value
        .as_object()
        .ok_or_else(|| "Theme files must contain a JSON object.".to_string())?;
    let colors = object
        .get("colors")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let pick = |keys: &[&str]| -> Option<VsCodeRgba> {
        for key in keys {
            if let Some(parsed) = colors.get(*key).and_then(parse_vscode_color) {
                return Some(parsed);
            }
        }
        None
    };
    let solid_over = |base: VsCodeRgb, keys: &[&str]| -> Option<String> {
        pick(keys).map(|parsed| flatten_over(parsed, base))
    };

    let canvas_color = pick(&["editor.background", "editorPane.background"]).ok_or_else(|| {
        "That VS Code theme has no \"editor.background\" color, so there is nothing to build a palette from.".to_string()
    })?;
    let canvas = VsCodeRgb {
        r: canvas_color.r,
        g: canvas_color.g,
        b: canvas_color.b,
    };
    let appearance = resolve_appearance(object, canvas);

    let canvas_hex = to_hex(canvas);
    let raised_surface_candidate_hex =
        solid_over(canvas, &["editorWidget.background", "dropdown.background"]);

    let stands_apart =
        |first: &str, second: &str| contrast_ratio(hex_to_rgb(first), hex_to_rgb(second)) >= 1.1;

    let accent_keys = [
        "focusBorder",
        "button.background",
        "textLink.foreground",
        "activityBarBadge.background",
        "progressBar.background",
        "badge.background",
    ];
    let mut accent_color: Option<VsCodeRgba> = None;
    let mut accent_hex: Option<String> = None;
    for key in accent_keys {
        let Some(candidate) = colors.get(key).and_then(parse_vscode_color) else {
            continue;
        };
        let candidate_hex = flatten_over(candidate, canvas);
        if !stands_apart(&candidate_hex, &canvas_hex)
            || raised_surface_candidate_hex
                .as_ref()
                .is_some_and(|raised| !stands_apart(&candidate_hex, raised))
        {
            continue;
        }
        accent_color = Some(candidate);
        accent_hex = Some(candidate_hex);
        break;
    }
    let (accent_color, accent_hex) =
        if let (Some(accent_color), Some(accent_hex)) = (accent_color, accent_hex) {
            (accent_color, accent_hex)
        } else {
            let standard = get_standard_theme_colors(appearance);
            let standard_accent_hex = theme_color_to_hex(standard.accent.as_ref())
                .ok_or_else(|| "standard accent".to_string())?;
            let accent_hex = [standard_accent_hex.as_str(), "#ffffff", "#000000"]
                .iter()
                .find(|candidate| {
                    stands_apart(candidate, &canvas_hex)
                        && raised_surface_candidate_hex
                            .as_ref()
                            .is_none_or(|raised| stands_apart(candidate, raised))
                })
                .copied()
                .unwrap_or(standard_accent_hex.as_str())
                .to_string();
            let accent_color = parse_vscode_color(&Value::String(accent_hex.clone()))
                .ok_or_else(|| "accent color".to_string())?;
            (accent_color, accent_hex)
        };

    let muted_accent_hex = flatten_over(
        VsCodeRgba {
            r: accent_color.r,
            g: accent_color.g,
            b: accent_color.b,
            a: 0.2,
        },
        canvas,
    );
    let derived = create_vivid_theme_colors(appearance, &canvas_hex, &muted_accent_hex);
    let sidebar_hex = solid_over(canvas, &["sideBar.background", "activityBar.background"])
        .unwrap_or_else(|| derived.sidebar.to_string());
    let sidebar = hex_to_rgb(&sidebar_hex);
    let terminal_hex = solid_over(canvas, &["terminal.background", "panel.background"])
        .unwrap_or_else(|| derived.terminal_background.to_string());
    let terminal = hex_to_rgb(&terminal_hex);

    let readable_on = |surface: &str, fallback: &str, keys: &[&str]| -> String {
        let surface_rgb = hex_to_rgb(surface);
        let is_readable =
            |candidate: &str| contrast_ratio(hex_to_rgb(candidate), surface_rgb) >= 4.5;
        if let Some(specified) = solid_over(surface_rgb, keys)
            && is_readable(&specified)
        {
            return specified;
        }
        if is_readable(fallback) {
            return fallback.to_string();
        }
        if relative_luminance(surface_rgb) < 0.179 {
            "#ffffff".to_string()
        } else {
            "#000000".to_string()
        }
    };

    let surface_raised_hex = raised_surface_candidate_hex
        .clone()
        .unwrap_or_else(|| derived.surface_raised.to_string());
    let button_hex = solid_over(canvas, &["button.background"]);
    let action_hex = button_hex
        .as_ref()
        .filter(|button| {
            stands_apart(button, &canvas_hex) && stands_apart(button, &surface_raised_hex)
        })
        .cloned()
        .unwrap_or_else(|| accent_hex.clone());

    let standard = get_standard_theme_colors(appearance);
    let input_candidates = [
        derived.input.to_string(),
        derived.surface_raised.to_string(),
        standard.input.to_string(),
        "#000000".to_string(),
        "#ffffff".to_string(),
        "#808080".to_string(),
    ];
    let mut input_hex = input_candidates
        .iter()
        .find(|candidate| {
            stands_apart(candidate, &canvas_hex) && stands_apart(candidate, &action_hex)
        })
        .cloned()
        .unwrap_or_else(|| "#808080".to_string());
    for key in ["input.background", "input.border"] {
        if let Some(candidate) = solid_over(canvas, &[key])
            && stands_apart(&candidate, &canvas_hex)
            && stands_apart(&candidate, &action_hex)
        {
            input_hex = candidate;
            break;
        }
    }

    let search_match_background = solid_over(canvas, &["editor.findMatchHighlightBackground"])
        .unwrap_or_else(|| derived.search_match_background.to_string());
    let search_match_active_background = solid_over(canvas, &["editor.findMatchBackground"])
        .unwrap_or_else(|| derived.search_match_active_background.to_string());

    let mut color_map = theme_colors_to_camel_map(&derived);
    set_color(&mut color_map, "canvas", canvas_hex.clone());
    set_color(
        &mut color_map,
        "text",
        readable_on(
            &canvas_hex,
            derived.text.as_ref(),
            &["editor.foreground", "foreground"],
        ),
    );
    set_color(
        &mut color_map,
        "text_muted",
        readable_on(
            &canvas_hex,
            derived.text_muted.as_ref(),
            &["descriptionForeground", "disabledForeground"],
        ),
    );
    if let Some(value) = solid_over(canvas, &["editorWidget.background"]) {
        set_color(&mut color_map, "surface", value);
    }
    set_color(&mut color_map, "surface_raised", surface_raised_hex.clone());
    if let Some(value) = solid_over(
        canvas,
        &[
            "menu.background",
            "quickInput.background",
            "dropdown.background",
        ],
    ) {
        set_color(&mut color_map, "surface_overlay", value);
    }
    if let Some(value) = solid_over(
        canvas,
        &["panel.border", "editorGroup.border", "contrastBorder"],
    ) {
        set_color(&mut color_map, "border", value);
    }
    set_color(&mut color_map, "input", input_hex);
    set_color(
        &mut color_map,
        "placeholder",
        readable_on(
            &surface_raised_hex,
            derived.placeholder.as_ref(),
            &["input.placeholderForeground"],
        ),
    );
    set_color(
        &mut color_map,
        "error",
        readable_on(
            &canvas_hex,
            derived.error.as_ref(),
            &["editorError.foreground", "errorForeground"],
        ),
    );
    set_color(
        &mut color_map,
        "warning",
        readable_on(
            &canvas_hex,
            derived.warning.as_ref(),
            &["editorWarning.foreground"],
        ),
    );
    if let Some(value) = solid_over(
        canvas,
        &["list.activeSelectionBackground", "list.hoverBackground"],
    ) {
        set_color(&mut color_map, "accent_surface", value);
    }
    if let Some(value) = solid_over(canvas, &["textCodeBlock.background"]) {
        set_color(&mut color_map, "code_background", value);
    }
    set_color(
        &mut color_map,
        "search_match_background",
        search_match_background.clone(),
    );
    set_color(
        &mut color_map,
        "search_match_foreground",
        readable_on(
            &search_match_background,
            derived.search_match_foreground.as_ref(),
            &[
                "editor.findMatchHighlightForeground",
                "editor.foreground",
                "foreground",
            ],
        ),
    );
    set_color(
        &mut color_map,
        "search_match_active_background",
        search_match_active_background.clone(),
    );
    set_color(
        &mut color_map,
        "search_match_active_foreground",
        readable_on(
            &search_match_active_background,
            derived.search_match_active_foreground.as_ref(),
            &[
                "editor.findMatchForeground",
                "editor.foreground",
                "foreground",
            ],
        ),
    );
    set_color(&mut color_map, "sidebar", sidebar_hex.clone());
    set_color(
        &mut color_map,
        "sidebar_foreground",
        readable_on(
            &sidebar_hex,
            derived.sidebar_foreground.as_ref(),
            &["sideBar.foreground"],
        ),
    );
    if let Some(value) = solid_over(sidebar, &["sideBar.border"]) {
        set_color(&mut color_map, "sidebar_border", value);
    }
    if let Some(value) = solid_over(sidebar, &["list.hoverBackground"]) {
        set_color(&mut color_map, "sidebar_row_hover", value);
    }
    if let Some(value) = solid_over(
        sidebar,
        &["list.inactiveSelectionBackground", "list.hoverBackground"],
    ) {
        set_color(&mut color_map, "sidebar_row_active", value);
    }
    if let Some(value) = solid_over(sidebar, &["list.activeSelectionBackground"]) {
        set_color(&mut color_map, "sidebar_row_selected", value);
    }
    set_color(&mut color_map, "terminal_background", terminal_hex.clone());
    set_color(
        &mut color_map,
        "terminal_foreground",
        readable_on(
            &terminal_hex,
            derived.terminal_foreground.as_ref(),
            &["terminal.foreground"],
        ),
    );
    if let Some(value) = solid_over(
        terminal,
        &["terminalCursor.foreground", "editorCursor.foreground"],
    ) {
        set_color(&mut color_map, "terminal_cursor", value);
    }
    if let Some(value) = solid_over(
        terminal,
        &["terminal.selectionBackground", "editor.selectionBackground"],
    ) {
        set_color(&mut color_map, "terminal_selection", value);
    }
    if let Some(value) = solid_over(terminal, &["scrollbarSlider.background"]) {
        set_color(&mut color_map, "terminal_scrollbar", value);
    }
    set_color(&mut color_map, "accent", accent_hex.clone());
    set_color(&mut color_map, "focus", accent_hex.clone());
    set_color(&mut color_map, "message_action", action_hex.clone());
    set_color(
        &mut color_map,
        "message_action_foreground",
        readable_on(
            &action_hex,
            derived.message_action_foreground.as_ref(),
            &["button.foreground"],
        ),
    );
    set_color(
        &mut color_map,
        "accent_foreground",
        readable_on(
            &accent_hex,
            derived.accent_foreground.as_ref(),
            &["button.foreground"],
        ),
    );

    let name = resolve_name(object);
    let generated_id = theme_id_from_name(&name);
    let id = if is_reserved_theme_id(&generated_id) {
        Some(format!("{generated_id}-vscode"))
    } else {
        None
    };
    build_theme_file(&name, appearance, id.as_deref(), color_map, None, false)
}

pub fn pair_vscode_themes(
    themes: &[ThemeDefinition],
    paired_id: Option<fn(&ThemeDefinition, &ThemeDefinition) -> String>,
) -> Vec<ThemeDefinition> {
    static STRIP: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let strip = STRIP.get_or_init(|| Regex::new(r"(?i)\b(?:light|dark)\b").expect("strip"));
    static ALNUM: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    let alnum = ALNUM.get_or_init(|| Regex::new(r"(?i)[a-z0-9]").expect("alnum"));

    let strip_appearance = |label: &str| -> String {
        let replaced = strip.replace_all(label, " ");
        static WS: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
        let ws = WS.get_or_init(|| Regex::new(r"\s+").expect("ws"));
        ws.replace_all(replaced.trim(), " ").trim().to_string()
    };

    struct Group {
        light: Vec<ThemeDefinition>,
        dark: Vec<ThemeDefinition>,
        order: usize,
    }
    let mut groups: IndexMap<String, Group> = IndexMap::new();
    let mut passthrough: Vec<(ThemeDefinition, usize)> = Vec::new();

    for (order, theme) in themes.iter().cloned().enumerate() {
        let key = strip_appearance(theme.label.as_ref());
        if get_theme_modes(&theme).len() != 1
            || key == theme.label.as_ref()
            || !alnum.is_match(&key)
        {
            passthrough.push((theme, order));
            continue;
        }
        let entry = groups.entry(key).or_insert(Group {
            light: Vec::new(),
            dark: Vec::new(),
            order,
        });
        match theme.appearance {
            ThemeAppearance::Light => entry.light.push(theme),
            ThemeAppearance::Dark => entry.dark.push(theme),
        }
    }

    let mut paired: Vec<(ThemeDefinition, usize)> = Vec::new();
    for (key, group) in groups {
        if group.light.len() == 1 && group.dark.len() == 1 {
            let light = group.light[0].clone();
            let dark = group.dark[0].clone();
            let mut file = Map::new();
            file.insert(
                "version".to_string(),
                Value::Number(THEME_FILE_VERSION.into()),
            );
            if let Some(paired_id) = &paired_id {
                file.insert("id".to_string(), Value::String(paired_id(&light, &dark)));
            }
            file.insert("name".to_string(), Value::String(key));
            file.insert("appearance".to_string(), Value::String("light".to_string()));
            file.insert(
                "colors".to_string(),
                Value::Object(theme_colors_to_camel_map(&light.colors)),
            );
            let mut variants = Map::new();
            variants.insert(
                "dark".to_string(),
                Value::Object(theme_colors_to_camel_map(&dark.colors)),
            );
            file.insert("variants".to_string(), Value::Object(variants));
            if let Ok(theme) = parse_theme_file(Value::Object(file)) {
                paired.push((theme, group.order));
                continue;
            }
        }
        for theme in group.light.iter().chain(&group.dark).cloned() {
            paired.push((theme, group.order));
        }
    }

    let mut combined = passthrough;
    combined.extend(paired);
    combined.sort_by_key(|(_, order)| *order);
    combined.into_iter().map(|(theme, _)| theme).collect()
}

pub struct ThemeImportEntry {
    pub theme: ThemeDefinition,
    pub source_name: Option<String>,
}

pub fn resolve_theme_label_collisions(entries: &[ThemeImportEntry]) -> Vec<ThemeDefinition> {
    let rename = |theme: &ThemeDefinition, name: &str| -> Option<ThemeDefinition> {
        build_theme_file(
            &name.chars().take(48).collect::<String>(),
            theme.appearance,
            None,
            theme_colors_to_camel_map(&theme.colors),
            if theme.variants.light.is_some() || theme.variants.dark.is_some() {
                Some(&theme.variants)
            } else {
                None
            },
            theme.managed,
        )
        .ok()
    };

    let mut counts: HashMap<String, usize> = HashMap::new();
    for entry in entries {
        *counts.entry(entry.theme.id.to_string()).or_default() += 1;
    }

    let relabelled: Vec<ThemeDefinition> = entries
        .iter()
        .map(|entry| {
            if counts.get(entry.theme.id.as_ref()).copied().unwrap_or(0) < 2 {
                return entry.theme.clone();
            }
            let stem = entry.source_name.as_ref().map(|name| {
                static EXT: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
                let ext = EXT.get_or_init(|| Regex::new(r"\.[^.]+$").expect("ext"));
                ext.replace(name, "").to_string()
            });
            let from_file = stem.as_ref().map(|stem| humanize_theme_name(stem));
            if let Some(from_file) = from_file
                && from_file.to_ascii_lowercase() != entry.theme.label.to_ascii_lowercase()
                && let Some(renamed) = rename(&entry.theme, &from_file)
            {
                return renamed;
            }
            entry.theme.clone()
        })
        .collect();

    let mut seen = HashSet::new();
    relabelled
        .into_iter()
        .map(|theme| {
            if seen.insert(theme.id.to_string()) {
                return theme;
            }
            for suffix in 2..100 {
                let candidate_name = format!("{} {suffix}", theme.label);
                if let Some(candidate) = rename(&theme, &candidate_name)
                    && seen.insert(candidate.id.to_string())
                {
                    return candidate;
                }
            }
            theme
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::theme_palette::get_theme_colors_for_mode;

    fn json(value: serde_json::Value) -> Value {
        value
    }

    fn as_hex(value: &str) -> String {
        theme_color_to_hex(value)
            .unwrap_or_else(|| panic!("Expected a theme color, received {value}"))
    }

    fn contrast_ratio_theme(first: &str, second: &str) -> f64 {
        let to_channels = |value: &str| {
            let hex = as_hex(value);
            let hex = hex.trim_start_matches('#');
            [0, 1, 2].map(|channel| {
                f64::from(u8::from_str_radix(&hex[channel * 2..channel * 2 + 2], 16).unwrap())
                    / 255.0
            })
        };
        let luminance = |value: &str| {
            to_channels(value)
                .iter()
                .enumerate()
                .map(|(index, channel)| {
                    let transformed = if *channel <= 0.03928 {
                        *channel / 12.92
                    } else {
                        ((*channel + 0.055) / 1.055).powf(2.4)
                    };
                    transformed * [0.2126, 0.7152, 0.0722][index]
                })
                .sum::<f64>()
        };
        let lighter = luminance(first).max(luminance(second));
        let darker = luminance(first).min(luminance(second));
        (lighter + 0.05) / (darker + 0.05)
    }

    fn vscode_dark() -> Value {
        json(serde_json::json!({
            "name": "pierre-dark-soft",
            "type": "dark",
            "colors": {
                "editor.background": "#171717",
                "editor.foreground": "#d4d4d4",
                "foreground": "#d4d4d4",
                "sideBar.background": "#101010",
                "sideBar.foreground": "#8a8a8a",
                "sideBar.border": "#1d1d1d",
                "focusBorder": "#69b1ff",
                "button.background": "#69b1ff",
                "button.foreground": "#171717",
                "input.border": "#2c2c2c",
                "input.placeholderForeground": "#525252",
                "terminal.background": "#101010",
                "terminal.foreground": "#8a8a8a",
                "list.hoverBackground": "#1f3e5e59",
                "list.activeSelectionBackground": "#1f3e5e99",
            },
            "tokenColors": [],
        }))
    }

    #[test]
    fn recognises_workbench_themes_and_rejects_our_own_files() {
        assert!(is_vscode_theme_file(&vscode_dark()));
        assert!(is_vscode_theme_file(&json(serde_json::json!({
            "type": "dark",
            "tokenColors": [],
        }))));
        assert!(!is_vscode_theme_file(&json(serde_json::json!({
            "version": THEME_FILE_VERSION,
            "name": "Aurora",
            "appearance": "light",
            "colors": { "canvas": "#ffffff" },
        }))));
        assert!(!is_vscode_theme_file(&Value::String("nope".into())));
    }

    #[test]
    fn carries_editor_surfaces_and_accent_across() {
        let theme = parse_vscode_theme_file(vscode_dark()).expect("theme");
        assert_eq!(theme.label.as_ref(), "Pierre Dark Soft");
        assert_eq!(theme.appearance, ThemeAppearance::Dark);
        assert_eq!(as_hex(theme.colors.canvas.as_ref()), "#171717");
        assert_eq!(as_hex(theme.colors.text.as_ref()), "#d4d4d4");
        assert_eq!(as_hex(theme.colors.accent.as_ref()), "#69b1ff");
        assert_eq!(as_hex(theme.colors.sidebar.as_ref()), "#101010");
        assert_eq!(as_hex(theme.colors.terminal_background.as_ref()), "#101010");
    }

    #[test]
    fn flattens_alpha_overlays_onto_the_surface_they_sit_on() {
        let theme = parse_vscode_theme_file(vscode_dark()).expect("theme");
        assert!(
            theme
                .colors
                .sidebar_row_hover
                .as_ref()
                .starts_with("oklch(")
        );
        assert_ne!(as_hex(theme.colors.sidebar_row_hover.as_ref()), "#1f3e5e59");
        assert_ne!(
            theme.colors.sidebar_row_selected.as_ref(),
            theme.colors.sidebar.as_ref()
        );
    }

    #[test]
    fn keeps_fallback_placeholder_dimmer_than_entered_text() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Dark placeholder fallback",
            "type": "dark",
            "colors": {
                "editor.background": "#1e1e2e",
                "editor.foreground": "#cdd6f4",
                "input.placeholderForeground": "#cdd6f473",
            },
        })))
        .expect("theme");
        assert!(
            contrast_ratio_theme(
                theme.colors.placeholder.as_ref(),
                theme.colors.surface_raised.as_ref()
            ) >= 4.5
        );
        assert!(
            contrast_ratio_theme(
                theme.colors.placeholder.as_ref(),
                theme.colors.canvas.as_ref()
            ) >= 4.5
        );
        assert!(
            contrast_ratio_theme(
                theme.colors.placeholder.as_ref(),
                theme.colors.canvas.as_ref()
            ) < contrast_ratio_theme(theme.colors.text.as_ref(), theme.colors.canvas.as_ref())
        );
    }

    #[test]
    fn moves_reserved_vscode_names_to_a_non_reserved_id() {
        let dark_plus = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Dark+",
            "type": "dark",
            "colors": { "editor.background": "#1e1e1e" },
        })))
        .expect("theme");
        assert_eq!(dark_plus.label.as_ref(), "Dark+");
        assert_ne!(dark_plus.id.as_ref(), "dark");
        assert_eq!(dark_plus.id.as_ref(), "dark-vscode");
    }

    #[test]
    fn keeps_dark_plus_and_light_plus_as_separate_themes_instead_of_pairing_as_plus() {
        let themes = pair_vscode_themes(
            &[
                parse_vscode_theme_file(json(serde_json::json!({
                    "name": "Light+",
                    "type": "light",
                    "colors": { "editor.background": "#ffffff" },
                })))
                .expect("light"),
                parse_vscode_theme_file(json(serde_json::json!({
                    "name": "Dark+",
                    "type": "dark",
                    "colors": { "editor.background": "#1e1e1e" },
                })))
                .expect("dark"),
            ],
            None,
        );
        let ids_labels: Vec<_> = themes
            .iter()
            .map(|theme| (theme.id.to_string(), theme.label.to_string()))
            .collect();
        assert_eq!(
            ids_labels,
            vec![
                ("light-vscode".to_string(), "Light+".to_string()),
                ("dark-vscode".to_string(), "Dark+".to_string()),
            ]
        );
    }

    #[test]
    fn prefers_visible_input_background_over_transparent_border() {
        let catppuccin = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Catppuccin Mocha",
            "type": "dark",
            "colors": {
                "editor.background": "#1e1e2e",
                "input.border": "#00000000",
                "input.background": "#313244",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(catppuccin.colors.input.as_ref()), "#313244");
    }

    #[test]
    fn keeps_unchecked_inputs_distinct_from_checked_action_color() {
        let gruvbox = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Gruvbox Light Soft",
            "type": "light",
            "colors": {
                "editor.background": "#f2e5bc",
                "button.background": "#45858880",
                "input.background": "#45858880",
                "input.border": "#928374",
            },
        })))
        .expect("theme");
        assert!(
            contrast_ratio_theme(
                gruvbox.colors.input.as_ref(),
                gruvbox.colors.message_action.as_ref()
            ) >= 1.1
        );
    }

    #[test]
    fn keeps_derived_input_distinct_when_button_reuses_it() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Derived input collision",
            "type": "dark",
            "colors": {
                "editor.background": "#1e1e2e",
                "focusBorder": "#89b4fa",
                "button.background": "#525661",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(theme.colors.message_action.as_ref()), "#525661");
        assert_ne!(as_hex(theme.colors.input.as_ref()), "#525661");
        assert!(
            contrast_ratio_theme(theme.colors.input.as_ref(), theme.colors.canvas.as_ref()) >= 1.1
        );
        assert!(
            contrast_ratio_theme(
                theme.colors.input.as_ref(),
                theme.colors.message_action.as_ref()
            ) >= 1.1
        );
    }

    #[test]
    fn skips_transparent_focus_border_for_visible_accent_key() {
        let vitesse = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Vitesse Dark",
            "type": "dark",
            "colors": {
                "editor.background": "#121212",
                "focusBorder": "#00000000",
                "button.background": "#4d9375",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(vitesse.colors.accent.as_ref()), "#4d9375");
        assert_eq!(as_hex(vitesse.colors.focus.as_ref()), "#4d9375");
        assert!(
            contrast_ratio_theme(
                vitesse.colors.focus.as_ref(),
                vitesse.colors.canvas.as_ref()
            ) >= 1.1
        );
    }

    #[test]
    fn keeps_focus_visible_against_explicit_raised_surface() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Raised focus",
            "type": "dark",
            "colors": {
                "editor.background": "#000000",
                "focusBorder": "#111111",
                "editorWidget.background": "#111111",
                "button.background": "#4d9375",
            },
        })))
        .expect("theme");
        assert!(
            contrast_ratio_theme(
                theme.colors.focus.as_ref(),
                theme.colors.surface_raised.as_ref()
            ) >= 1.1
        );
        assert_eq!(as_hex(theme.colors.focus.as_ref()), "#4d9375");
    }

    #[test]
    fn keeps_fallback_focus_visible_against_explicit_raised_surface() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Raised fallback focus",
            "type": "dark",
            "colors": {
                "editor.background": "#121212",
                "editorWidget.background": "#346bf1",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(theme.colors.focus.as_ref()), "#ffffff");
        assert!(
            contrast_ratio_theme(theme.colors.focus.as_ref(), theme.colors.canvas.as_ref()) >= 1.1
        );
        assert!(
            contrast_ratio_theme(
                theme.colors.focus.as_ref(),
                theme.colors.surface_raised.as_ref()
            ) >= 1.1
        );
    }

    #[test]
    fn skips_transparent_button_background_for_action_color() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Transparent button",
            "type": "dark",
            "colors": {
                "editor.background": "#121212",
                "focusBorder": "#4d9375",
                "button.background": "#00000000",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(theme.colors.message_action.as_ref()), "#4d9375");
    }

    #[test]
    fn skips_button_background_that_matches_raised_surface() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Raised button",
            "type": "dark",
            "colors": {
                "editor.background": "#121212",
                "focusBorder": "#4d9375",
                "editorWidget.background": "#2a2d3a",
                "button.background": "#2a2d3a",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(theme.colors.message_action.as_ref()), "#4d9375");
    }

    #[test]
    fn uses_visible_default_accent_when_file_has_no_usable_accent_key() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "No accent",
            "type": "dark",
            "colors": { "editor.background": "#121212", "focusBorder": "#121212" },
        })))
        .expect("theme");
        assert!(
            contrast_ratio_theme(theme.colors.focus.as_ref(), theme.colors.canvas.as_ref()) >= 1.1
        );
    }

    #[test]
    fn validates_placeholders_against_resolved_raised_surface() {
        let light_plus = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Light Plus Shape",
            "type": "light",
            "colors": {
                "editor.background": "#eaeff3",
                "editor.foreground": "#1f1f1f",
                "editorWidget.background": "#ffffff",
                "input.placeholderForeground": "#767676",
            },
        })))
        .expect("theme");
        assert!(
            contrast_ratio_theme(
                light_plus.colors.placeholder.as_ref(),
                light_plus.colors.surface_raised.as_ref()
            ) >= 4.5
        );
        assert!(
            contrast_ratio_theme(
                light_plus.colors.placeholder.as_ref(),
                light_plus.colors.surface_raised.as_ref()
            ) < contrast_ratio_theme(
                light_plus.colors.text.as_ref(),
                light_plus.colors.surface_raised.as_ref()
            )
        );
    }

    #[test]
    fn imports_current_and_other_find_matches_as_independent_background_text_pairs() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "type": "dark",
            "colors": {
                "editor.background": "#202020",
                "editor.findMatchBackground": "#663399",
                "editor.findMatchForeground": "#ffffff",
                "editor.findMatchHighlightBackground": "#bce4ff",
                "editor.findMatchHighlightForeground": "#17314a",
            },
        })))
        .expect("theme");
        let colors = &theme.colors;
        assert_eq!(
            as_hex(colors.search_match_active_background.as_ref()),
            "#663399"
        );
        assert_eq!(
            as_hex(colors.search_match_active_foreground.as_ref()),
            "#ffffff"
        );
        assert_eq!(as_hex(colors.search_match_background.as_ref()), "#bce4ff");
        assert_eq!(as_hex(colors.search_match_foreground.as_ref()), "#17314a");
    }

    #[test]
    fn composites_find_backgrounds_onto_canvas_and_foregrounds_onto_match() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "type": "dark",
            "colors": {
                "editor.background": "#202020",
                "editor.findMatchBackground": "#33669980",
                "editor.findMatchForeground": "#ffffffcc",
                "editor.findMatchHighlightBackground": "#ffcc0040",
                "editor.findMatchHighlightForeground": "#ffffff",
            },
        })))
        .expect("theme");
        let colors = &theme.colors;
        assert_eq!(
            as_hex(colors.search_match_active_background.as_ref()),
            "#2a435d"
        );
        assert_eq!(
            as_hex(colors.search_match_active_foreground.as_ref()),
            "#d4d9df"
        );
        assert_eq!(as_hex(colors.search_match_background.as_ref()), "#584b18");
        assert_eq!(as_hex(colors.search_match_foreground.as_ref()), "#ffffff");
    }

    #[test]
    fn uses_editor_text_when_no_find_foreground_is_specified() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "type": "dark",
            "colors": {
                "editor.background": "#101010",
                "editor.foreground": "#eeeeee",
                "editor.findMatchBackground": "#304050",
                "editor.findMatchHighlightBackground": "#403020",
            },
        })))
        .expect("theme");
        let colors = &theme.colors;
        assert_eq!(
            as_hex(colors.search_match_active_foreground.as_ref()),
            "#eeeeee"
        );
        assert_eq!(as_hex(colors.search_match_foreground.as_ref()), "#eeeeee");
    }

    fn assert_find_colours_readable(type_name: &str) {
        let base = serde_json::json!({
            "editor.background": if type_name == "dark" { "#101010" } else { "#fafafa" },
        });
        let missing = parse_vscode_theme_file(json(serde_json::json!({
            "type": type_name,
            "colors": base,
        })))
        .expect("missing")
        .colors;
        let invalid = parse_vscode_theme_file(json(serde_json::json!({
            "type": type_name,
            "colors": {
                "editor.background": base["editor.background"].clone(),
                "editor.findMatchBackground": "not-a-colour",
                "editor.findMatchForeground": 42,
                "editor.findMatchHighlightBackground": null,
                "editor.findMatchHighlightForeground": "invalid",
            },
        })))
        .expect("invalid")
        .colors;
        let unreadable = parse_vscode_theme_file(json(serde_json::json!({
            "type": type_name,
            "colors": {
                "editor.background": base["editor.background"].clone(),
                "editor.findMatchBackground": "#ffffcc",
                "editor.findMatchForeground": "#ffffff",
                "editor.findMatchHighlightBackground": "#101020",
                "editor.findMatchHighlightForeground": "#111111",
            },
        })))
        .expect("unreadable")
        .colors;

        for colors in [&missing, &invalid, &unreadable] {
            assert!(
                contrast_ratio_theme(
                    colors.search_match_foreground.as_ref(),
                    colors.search_match_background.as_ref()
                ) >= 4.5
            );
            assert!(
                contrast_ratio_theme(
                    colors.search_match_active_foreground.as_ref(),
                    colors.search_match_active_background.as_ref()
                ) >= 4.5
            );
        }
        assert_eq!(
            invalid.search_match_background.as_ref(),
            missing.search_match_background.as_ref()
        );
        assert_eq!(
            invalid.search_match_active_background.as_ref(),
            missing.search_match_active_background.as_ref()
        );
        assert_ne!(
            missing.search_match_background.as_ref(),
            missing.search_match_active_background.as_ref()
        );
        assert_ne!(
            missing.search_match_background.as_ref(),
            missing.canvas.as_ref()
        );
        assert_ne!(
            missing.search_match_active_background.as_ref(),
            missing.canvas.as_ref()
        );
    }

    #[test]
    fn keeps_missing_or_invalid_find_colours_readable_in_light_themes() {
        assert_find_colours_readable("light");
    }

    #[test]
    fn keeps_missing_or_invalid_find_colours_readable_in_dark_themes() {
        assert_find_colours_readable("dark");
    }

    #[test]
    fn fills_every_role_the_file_omits_with_readable_derived_value() {
        let theme = parse_vscode_theme_file(vscode_dark()).expect("theme");
        let colors = get_theme_colors_for_mode(&theme, ThemeAppearance::Dark).expect("colors");
        for (_, value) in colors.entries() {
            assert!(value.starts_with("oklch("));
        }
        assert!(contrast_ratio_theme(colors.text.as_ref(), colors.canvas.as_ref()) >= 4.5);
        assert!(
            contrast_ratio_theme(colors.sidebar_foreground.as_ref(), colors.sidebar.as_ref())
                >= 4.5
        );
        assert!(
            contrast_ratio_theme(
                colors.message_action_foreground.as_ref(),
                colors.message_action.as_ref()
            ) >= 4.5
        );
    }

    #[test]
    fn falls_back_to_editor_background_when_type_is_missing_or_odd() {
        let untyped = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Untyped",
            "colors": { "editor.background": "#fdfdfd", "editor.foreground": "#202020" },
        })))
        .expect("untyped");
        assert_eq!(untyped.appearance, ThemeAppearance::Light);
        let hc = parse_vscode_theme_file(json(serde_json::json!({
            "name": "High contrast",
            "type": "hc-light",
            "colors": { "editor.background": "#ffffff" },
        })))
        .expect("hc");
        assert_eq!(hc.appearance, ThemeAppearance::Light);
    }

    #[test]
    fn keeps_unreadable_foreground_out_of_palette() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Unreadable",
            "type": "dark",
            "colors": { "editor.background": "#101010", "editor.foreground": "#111111" },
        })))
        .expect("theme");
        assert_ne!(as_hex(theme.colors.text.as_ref()), "#111111");
        assert!(
            contrast_ratio_theme(theme.colors.text.as_ref(), theme.colors.canvas.as_ref()) >= 4.5
        );
    }

    #[test]
    fn stays_readable_when_file_replaces_surface_but_not_foreground() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Split",
            "type": "dark",
            "colors": {
                "editor.background": "#101010",
                "editor.foreground": "#f5f5f5",
                "sideBar.background": "#fafafa",
                "terminal.background": "#fbfbfb",
            },
        })))
        .expect("theme");
        assert_eq!(as_hex(theme.colors.sidebar.as_ref()), "#fafafa");
        assert!(
            contrast_ratio_theme(
                theme.colors.sidebar_foreground.as_ref(),
                theme.colors.sidebar.as_ref()
            ) >= 4.5
        );
        assert!(
            contrast_ratio_theme(
                theme.colors.terminal_foreground.as_ref(),
                theme.colors.terminal_background.as_ref()
            ) >= 4.5
        );
    }

    #[test]
    fn reads_wide_gamut_color_notation() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Vibrant",
            "type": "dark",
            "colors": {
                "editor.background": "color(display-p3 0.039216 0.039216 0.039216)",
                "editor.foreground": "color(display-p3 0.980392 0.980392 0.980392)",
                "focusBorder": "color(display-p3 0.308664 0.645271 1.000000)",
                "editor.selectionBackground": "color(display-p3 0.308664 0.645271 1.000000 / 0.300000)",
            },
        })))
        .expect("theme");
        assert!(as_hex(theme.colors.canvas.as_ref()).starts_with("#0"));
        assert!(
            as_hex(theme.colors.text.as_ref())
                .to_ascii_lowercase()
                .starts_with("#f")
        );
        let accent = as_hex(theme.colors.accent.as_ref());
        let red = u8::from_str_radix(&accent[1..3], 16).unwrap();
        let green = u8::from_str_radix(&accent[3..5], 16).unwrap();
        let blue = u8::from_str_radix(&accent[5..7], 16).unwrap();
        assert!(blue > 200);
        assert!(blue > red);
        assert!(green > red);
    }

    fn make_github_theme(name: &str, type_name: &str) -> ThemeDefinition {
        parse_vscode_theme_file(json(serde_json::json!({
            "name": name,
            "type": type_name,
            "colors": {
                "editor.background": if type_name == "dark" { "#101014" } else { "#fdfdfd" },
                "editor.foreground": if type_name == "dark" { "#e6e6e6" } else { "#1f1f1f" },
                "focusBorder": "#69b1ff",
            },
        })))
        .expect("theme")
    }

    #[test]
    fn pairs_light_and_dark_files_from_one_family_into_dual_mode_themes() {
        let themes = pair_vscode_themes(
            &[
                make_github_theme("github-dark", "dark"),
                make_github_theme("github-light", "light"),
                make_github_theme("github-dark-colorblind", "dark"),
                make_github_theme("github-light-colorblind", "light"),
                make_github_theme("github-dark-dimmed", "dark"),
            ],
            None,
        );
        let labels: Vec<_> = themes.iter().map(|theme| theme.label.to_string()).collect();
        assert_eq!(
            labels,
            vec![
                "Github".to_string(),
                "Github Colorblind".to_string(),
                "Github Dark Dimmed".to_string(),
            ]
        );
        let github = &themes[0];
        assert_eq!(github.appearance, ThemeAppearance::Light);
        assert!(get_theme_colors_for_mode(github, ThemeAppearance::Dark).is_some());
        assert_eq!(
            as_hex(
                get_theme_colors_for_mode(github, ThemeAppearance::Dark)
                    .expect("dark")
                    .canvas
                    .as_ref()
            ),
            "#101014"
        );
        assert_eq!(as_hex(github.colors.canvas.as_ref()), "#fdfdfd");
        assert!(get_theme_colors_for_mode(&themes[2], ThemeAppearance::Light).is_none());
    }

    #[test]
    fn does_not_guess_when_family_is_ambiguous() {
        let make = |name: &str, type_name: &str| {
            parse_vscode_theme_file(json(serde_json::json!({
                "name": name,
                "type": type_name,
                "colors": { "editor.background": if type_name == "dark" { "#101014" } else { "#fdfdfd" } },
            })))
            .expect("theme")
        };
        let themes = pair_vscode_themes(
            &[
                make("solar-dark", "dark"),
                make("solar-dark-soft", "dark"),
                make("solar-light", "light"),
            ],
            None,
        );
        let mut labels: Vec<_> = themes.iter().map(|theme| theme.label.to_string()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["Solar".to_string(), "Solar Dark Soft".to_string()]
        );
    }

    #[test]
    fn keeps_derived_surfaces_neutral_instead_of_washing_them_with_accent() {
        let theme = parse_vscode_theme_file(vscode_dark()).expect("theme");
        let spread = |value: &str| {
            let hex = as_hex(value);
            let channels = [1, 3, 5]
                .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).unwrap() as i32);
            channels.iter().max().unwrap() - channels.iter().min().unwrap()
        };
        assert!(spread(theme.colors.code_background.as_ref()) <= 8);
        assert!(spread(theme.colors.surface.as_ref()) <= 8);
        assert!(spread(theme.colors.text.as_ref()) <= 12);
        assert_eq!(as_hex(theme.colors.accent.as_ref()), "#69b1ff");
    }

    fn dracula(bg: &str) -> ThemeDefinition {
        parse_vscode_theme_file(json(serde_json::json!({
            "name": "Dracula",
            "type": "dark",
            "colors": { "editor.background": bg, "editor.foreground": "#f8f8f2" },
        })))
        .expect("dracula")
    }

    #[test]
    fn tells_same_named_variants_apart_by_file_names() {
        let themes = resolve_theme_label_collisions(&[
            ThemeImportEntry {
                theme: dracula("#282a36"),
                source_name: Some("dracula.json".into()),
            },
            ThemeImportEntry {
                theme: dracula("#22232e"),
                source_name: Some("dracula-soft.json".into()),
            },
        ]);
        assert_eq!(
            themes
                .iter()
                .map(|t| t.label.to_string())
                .collect::<Vec<_>>(),
            vec!["Dracula".to_string(), "Dracula Soft".to_string()]
        );
        assert_eq!(
            themes.iter().map(|t| t.id.to_string()).collect::<Vec<_>>(),
            vec!["dracula".to_string(), "dracula-soft".to_string()]
        );
        let numbered = resolve_theme_label_collisions(&[
            ThemeImportEntry {
                theme: dracula("#282a36"),
                source_name: None,
            },
            ThemeImportEntry {
                theme: dracula("#22232e"),
                source_name: None,
            },
        ]);
        assert_eq!(
            numbered
                .iter()
                .map(|t| t.label.to_string())
                .collect::<Vec<_>>(),
            vec!["Dracula".to_string(), "Dracula 2".to_string()]
        );
    }

    #[test]
    fn falls_back_to_name_when_display_name_humanizes_to_nothing() {
        let theme = parse_vscode_theme_file(json(serde_json::json!({
            "displayName": "---",
            "name": "night-owl",
            "type": "dark",
            "colors": { "editor.background": "#011627" },
        })))
        .expect("theme");
        assert_eq!(theme.label.as_ref(), "Night Owl");
    }

    #[test]
    fn keeps_pair_whose_stripped_name_is_reserved_as_two_single_themes() {
        let make = |name: &str, type_name: &str| {
            parse_vscode_theme_file(json(serde_json::json!({
                "name": name,
                "type": type_name,
                "colors": { "editor.background": if type_name == "dark" { "#101014" } else { "#fdfdfd" } },
            })))
            .expect("theme")
        };
        let themes = pair_vscode_themes(
            &[make("grove-light", "light"), make("grove-dark", "dark")],
            None,
        );
        let mut labels: Vec<_> = themes.iter().map(|theme| theme.label.to_string()).collect();
        labels.sort();
        assert_eq!(
            labels,
            vec!["Grove Dark".to_string(), "Grove Light".to_string()]
        );
    }

    #[test]
    fn explains_file_with_no_editor_background() {
        let error = parse_vscode_theme_file(json(serde_json::json!({
            "name": "Empty",
            "type": "dark",
            "colors": {},
        })))
        .expect_err("error");
        assert!(error.contains("editor.background"));
    }
}

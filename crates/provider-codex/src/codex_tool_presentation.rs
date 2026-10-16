//! Port of upstream/apps/server/src/provider/CodexToolPresentation.ts
//!
//! How a Codex `mcpToolCall` item shows up in the timeline. Codex tags browser and computer use
//! calls with `result._meta["codex/toolSurface"]`; those get the browser or native app identity.
//! Everything else falls through to the generic integration presentation in
//! `turin_provider_core::mcp_tool_presentation`.
//!
//! Deviation: the item is the raw JSON object (`serde_json::Value`). The generated app-server
//! types wrap `McpToolCallResult` and `McpToolCallAppContext` as opaque values, and this reads
//! `_meta`, which they do not model.
//!
//! Deviation: upstream is an `Effect` needing `Crypto` for the SHA-256 of an over-long native app
//! key; here it is a plain function over `sha2`, so it cannot fail.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use turin_contracts::{
    TrimmedNonEmptyString, js_trim,
    provider_runtime::{
        ToolActivityIcon, ToolActivityNativeAppReference, ToolActivitySource,
        ToolActivitySourceKind,
    },
};
use turin_provider_core::{
    js::utf16_len,
    mcp_tool_presentation::{
        mcp_tool_presentation as integration_tool_presentation, normalize_mcp_http_url,
        normalize_mcp_text,
    },
};

/// `McpToolPresentation` as Codex returns it: the generic one plus the surface.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CodexMcpToolPresentation {
    pub title: Option<String>,
    pub tool_surface: Option<CodexToolSurface>,
    pub tool_icon: Option<ToolActivityIcon>,
    pub tool_source: Option<ToolActivitySource>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexToolSurface {
    Browser,
    Computer,
}

fn text(value: &str) -> TrimmedNonEmptyString {
    TrimmedNonEmptyString::new(value).expect("normalized text is nonempty")
}

/// `asUnknownRecord`: any non-null object, arrays included (their named fields read as absent).
fn as_record(value: &Value) -> Option<&Value> {
    (value.is_object() || value.is_array()).then_some(value)
}

fn field<'a>(record: Option<&'a Value>, name: &str) -> &'a Value {
    record
        .and_then(|record| record.get(name))
        .unwrap_or(&Value::Null)
}

/// JS `a ?? b` over JSON: falls through on absent and `null`.
fn nullish_or<'a>(value: &'a Value, fallback: &'a Value) -> &'a Value {
    if value.is_null() { fallback } else { value }
}

fn normalized_image_url(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if utf16_len(raw) > 4096 {
        return None;
    }
    let url = url::Url::parse(raw).ok()?;
    matches!(url.scheme(), "http" | "https" | "data").then(|| url.to_string())
}

fn normalized_app_id(value: &Value) -> Option<String> {
    let app_id = js_trim(value.as_str()?);
    let length = utf16_len(app_id);
    (length > 0
        && length <= 512
        && app_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')))
    .then(|| app_id.to_owned())
}

fn normalized_source_key_part(value: &str) -> String {
    js_trim(value).to_lowercase()
}

fn native_app_source_key(app_id: &str) -> String {
    let key = format!("native-app:{}", app_id.to_lowercase());
    if utf16_len(&key) <= 512 {
        return key;
    }
    let digest: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    // The key is ASCII (app ids are), so byte and UTF-16 offsets agree.
    format!("{}:{digest}", &key[..512 - digest.len() - 1])
}

fn display_name(value: &Value) -> Option<String> {
    normalize_mcp_text(value, 160)
}

fn browser_display_name(value: &Value) -> Option<String> {
    let normalized = display_name(value)?.to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    if normalized.contains("chrome") || normalized == "chromium" {
        return Some("Chrome".to_owned());
    }
    if normalized.contains("edge") {
        return Some("Microsoft Edge".to_owned());
    }
    if normalized.contains("firefox") {
        return Some("Firefox".to_owned());
    }
    if normalized.contains("safari") {
        return Some("Safari".to_owned());
    }
    if normalized.contains("arc") {
        return Some("Arc".to_owned());
    }
    if normalized == "iab" || normalized.contains("in-app") {
        return Some("Browser".to_owned());
    }
    display_name(value)
}

fn browser_native_app_reference(name: &str) -> Option<ToolActivityNativeAppReference> {
    let display = match name {
        "Chrome" => "Google Chrome",
        "Microsoft Edge" | "Firefox" | "Safari" | "Arc" => name,
        _ => return None,
    };
    Some(ToolActivityNativeAppReference::DisplayName {
        display_name: text(display),
    })
}

fn app_display_name_from_id(app_id: &str) -> Option<&'static str> {
    match app_id.to_lowercase().as_str() {
        "com.apple.finder" => Some("Finder"),
        "com.apple.safari" => Some("Safari"),
        "com.google.chrome" => Some("Chrome"),
        "com.microsoft.edgemac" => Some("Microsoft Edge"),
        "org.mozilla.firefox" => Some("Firefox"),
        "company.thebrowser.browser" => Some("Arc"),
        _ => None,
    }
}

fn native_app_reference(value: &Value) -> Option<ToolActivityNativeAppReference> {
    let app = as_record(value);
    match field(app, "kind").as_str() {
        Some("appId") => normalized_app_id(field(app, "appId")).map(|app_id| {
            ToolActivityNativeAppReference::AppId {
                app_id: text(&app_id),
            }
        }),
        Some("displayName") => display_name(field(app, "displayName")).map(|name| {
            ToolActivityNativeAppReference::DisplayName {
                display_name: text(&name),
            }
        }),
        _ => None,
    }
}

fn themed_logo_icon(records: &[Option<&Value>]) -> Option<ToolActivityIcon> {
    for record in records {
        let Some(logo_url) = normalized_image_url(field(*record, "logoUrl")) else {
            continue;
        };
        let logo_url_dark = normalized_image_url(nullish_or(
            field(*record, "logoUrlDark"),
            field(*record, "logoDarkUrl"),
        ));
        return Some(ToolActivityIcon::ThemedLogo {
            logo_url: text(&logo_url),
            logo_url_dark: logo_url_dark.as_deref().map(text),
        });
    }
    None
}

/// The timeline presentation of a Codex `mcpToolCall` item (`type: "mcpToolCall"`).
pub fn mcp_tool_presentation(item: &Value) -> CodexMcpToolPresentation {
    let result = as_record(field(Some(item), "result"));
    let metadata = as_record(field(result, "_meta"));
    let surface = as_record(field(metadata, "codex/toolSurface"));
    let source_metadata = as_record(field(metadata, "source"));
    let app_context = as_record(field(Some(item), "appContext"));
    let source_logo = themed_logo_icon(&[surface, source_metadata, app_context]);

    if field(surface, "kind").as_str() == Some("browserUse") {
        return browser_use_presentation(surface, metadata, app_context, source_logo);
    }
    if field(surface, "kind").as_str() == Some("computerUse") {
        return computer_use_presentation(item, surface, app_context, source_logo);
    }

    let (icon_url, icon_url_dark) = match &source_logo {
        Some(ToolActivityIcon::ThemedLogo {
            logo_url,
            logo_url_dark,
        }) => (
            Some(logo_url.as_str().to_owned()),
            logo_url_dark.as_ref().map(|url| url.as_str().to_owned()),
        ),
        _ => (None, None),
    };
    let generic = integration_tool_presentation(&json!({
        "serverName": nullish_or(
            field(app_context, "connectorId"),
            field(Some(item), "server"),
        ),
        "toolName": field(Some(item), "tool"),
        "title": field(app_context, "actionName"),
        "serverDisplayName": field(app_context, "appName"),
        "source": source_metadata,
        "iconUrl": icon_url,
        "iconUrlDark": icon_url_dark,
    }));
    CodexMcpToolPresentation {
        title: generic.title,
        tool_surface: None,
        tool_icon: generic.tool_icon,
        tool_source: generic.tool_source,
    }
}

fn browser_use_presentation(
    surface: Option<&Value>,
    metadata: Option<&Value>,
    app_context: Option<&Value>,
    source_logo: Option<ToolActivityIcon>,
) -> CodexMcpToolPresentation {
    let screenshot = as_record(field(surface, "screenshot"));
    let browser_use = as_record(field(metadata, "browser_use"));
    let empty = Vec::new();
    let open_tabs = field(surface, "openTabs").as_array().unwrap_or(&empty);
    let latest_open_tab = open_tabs
        .iter()
        .rev()
        .map(as_record)
        .find(|tab| normalize_mcp_http_url(field(*tab, "url")).is_some())
        .flatten();
    let selected_page = [
        (screenshot, field(screenshot, "pageUrl")),
        (browser_use, field(browser_use, "url")),
        (latest_open_tab, field(latest_open_tab, "url")),
    ]
    .into_iter()
    .find_map(|(record, url)| normalize_mcp_http_url(url).map(|page_url| (record, page_url)));
    let page_url = selected_page.as_ref().map(|(_, page_url)| page_url.clone());
    let selected_record = selected_page.as_ref().and_then(|(record, _)| *record);
    let favicon_url = normalized_image_url(nullish_or(
        field(selected_record, "faviconUrl"),
        field(selected_record, "favIconUrl"),
    ));
    let favicon_url_dark = normalized_image_url(nullish_or(
        field(selected_record, "faviconUrlDark"),
        field(selected_record, "favIconUrlDark"),
    ));
    let name = browser_display_name(field(app_context, "appName"))
        .or_else(|| browser_display_name(field(surface, "browserFamily")))
        .or_else(|| browser_display_name(field(surface, "backend")))
        .unwrap_or_else(|| "Browser".to_owned());
    let native_browser_icon = browser_native_app_reference(&name);
    let source_icon =
        source_logo.or_else(|| native_browser_icon.map(|app| ToolActivityIcon::NativeApp { app }));
    let key_part = normalized_source_key_part(&name);
    let key_part = if key_part.is_empty() {
        "browser".to_owned()
    } else {
        key_part
    };
    CodexMcpToolPresentation {
        title: None,
        tool_surface: Some(CodexToolSurface::Browser),
        tool_icon: page_url.map(|page_url| ToolActivityIcon::Website {
            page_url: text(&page_url),
            favicon_url: favicon_url.as_deref().map(text),
            favicon_url_dark: favicon_url_dark.as_deref().map(text),
        }),
        tool_source: Some(ToolActivitySource {
            key: text(&format!("browser-use:{key_part}")),
            name: text(&name),
            kind: if name == "Browser" {
                ToolActivitySourceKind::Browser
            } else {
                ToolActivitySourceKind::Integration
            },
            icon: source_icon,
        }),
    }
}

fn computer_use_presentation(
    item: &Value,
    surface: Option<&Value>,
    app_context: Option<&Value>,
    source_logo: Option<ToolActivityIcon>,
) -> CodexMcpToolPresentation {
    let app = native_app_reference(field(surface, "app"));
    let args = as_record(field(Some(item), "arguments"));
    let argument_app_name = display_name(field(args, "appName"))
        .or_else(|| display_name(field(args, "application")))
        .or_else(|| {
            let app_arg = field(args, "app");
            app_arg.is_string().then(|| display_name(app_arg)).flatten()
        });
    let name = display_name(field(app_context, "appName"))
        .or(argument_app_name)
        .or_else(|| match &app {
            Some(ToolActivityNativeAppReference::DisplayName { display_name }) => {
                Some(display_name.as_str().to_owned())
            }
            _ => None,
        })
        .or_else(|| match &app {
            Some(ToolActivityNativeAppReference::AppId { app_id }) => {
                app_display_name_from_id(app_id.as_str()).map(str::to_owned)
            }
            _ => None,
        })
        .unwrap_or_else(|| "Computer Use".to_owned());
    let source_icon =
        source_logo.or_else(|| app.clone().map(|app| ToolActivityIcon::NativeApp { app }));
    let source_key = match &app {
        Some(ToolActivityNativeAppReference::AppId { app_id }) => {
            native_app_source_key(app_id.as_str())
        }
        Some(ToolActivityNativeAppReference::DisplayName { display_name }) => {
            format!(
                "native-app-name:{}",
                normalized_source_key_part(display_name.as_str())
            )
        }
        None => "computer-use".to_owned(),
    };
    CodexMcpToolPresentation {
        title: None,
        tool_surface: Some(CodexToolSurface::Computer),
        tool_icon: app.map(|app| ToolActivityIcon::NativeApp { app }),
        tool_source: Some(ToolActivitySource {
            key: text(&source_key),
            name: text(&name),
            kind: ToolActivitySourceKind::Computer,
            icon: source_icon,
        }),
    }
}

#[cfg(test)]
mod tests {
    //! Port of the `CodexToolPresentation` cases in
    //! upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.test.ts ("dynamic tool
    //! projection"): the presentation fields of `projectCodexDynamicToolItem`. The adapter-level
    //! projection (title from `js` arguments, structured output) belongs to the Codex adapter.

    use pretty_assertions::assert_eq;

    use super::*;

    fn website(page_url: &str, favicon_url: Option<&str>) -> ToolActivityIcon {
        ToolActivityIcon::Website {
            page_url: text(page_url),
            favicon_url: favicon_url.map(text),
            favicon_url_dark: None,
        }
    }

    fn app_id(id: &str) -> ToolActivityNativeAppReference {
        ToolActivityNativeAppReference::AppId { app_id: text(id) }
    }

    fn source(
        key: &str,
        name: &str,
        kind: ToolActivitySourceKind,
        icon: Option<ToolActivityIcon>,
    ) -> ToolActivitySource {
        ToolActivitySource {
            key: text(key),
            name: text(name),
            kind,
            icon,
        }
    }

    #[test]
    fn presents_ordinary_mcp_calls() {
        for status in ["inProgress", "completed", "failed"] {
            let presentation = mcp_tool_presentation(&json!({
                "type": "mcpToolCall",
                "id": "weather-call",
                "server": "weather",
                "tool": "get_weather",
                "status": status,
                "arguments": { "city": "Berlin" },
            }));
            assert_eq!(presentation.title.as_deref(), Some("get weather"));
            assert_eq!(
                presentation.tool_source,
                Some(source(
                    "mcp:weather",
                    "weather",
                    ToolActivitySourceKind::Integration,
                    None
                ))
            );
            assert_eq!(presentation.tool_surface, None);
        }
    }

    #[test]
    fn uses_codex_connector_names_without_reading_a_display_title_from_arguments() {
        let presentation = mcp_tool_presentation(&json!({
            "type": "mcpToolCall",
            "id": "connector-call",
            "server": "_apps",
            "tool": "connector_get_weather",
            "status": "completed",
            "arguments": { "title": "Argument, not display metadata" },
            "appContext": {
                "connectorId": "weather-app",
                "appName": "Weather",
                "actionName": "Check weather",
            },
            "result": {
                "content": [],
                "_meta": { "source": { "logoUrl": "https://example.com/weather.png" } },
            },
        }));
        assert_eq!(presentation.title.as_deref(), Some("Check weather"));
        assert_eq!(
            presentation.tool_source.as_ref().map(|s| s.name.as_str()),
            Some("Weather")
        );
        let icon = ToolActivityIcon::ThemedLogo {
            logo_url: text("https://example.com/weather.png"),
            logo_url_dark: None,
        };
        assert_eq!(presentation.tool_icon, Some(icon.clone()));
        assert_eq!(
            presentation.tool_source.and_then(|source| source.icon),
            Some(icon)
        );
    }

    #[test]
    fn preserves_native_browser_and_app_icons_alongside_mcp_tool_output() {
        let browser = mcp_tool_presentation(&json!({
            "type": "mcpToolCall",
            "id": "browser",
            "server": "browser",
            "tool": "open",
            "status": "completed",
            "arguments": {},
            "result": {
                "content": [],
                "_meta": {
                    "codex/toolSurface": {
                        "kind": "browserUse",
                        "browserFamily": "Chrome",
                        "screenshot": {
                            "pageUrl": "https://example.com/docs",
                            "faviconUrl": "https://example.com/icon.png",
                        },
                    },
                },
            },
        }));
        assert_eq!(browser.tool_surface, Some(CodexToolSurface::Browser));
        assert_eq!(
            browser.tool_icon,
            Some(website(
                "https://example.com/docs",
                Some("https://example.com/icon.png")
            ))
        );
        let browser_source = browser.tool_source.unwrap();
        assert_eq!(browser_source.name.as_str(), "Chrome");
        assert_eq!(browser_source.key.as_str(), "browser-use:chrome");
        assert_eq!(browser_source.kind, ToolActivitySourceKind::Integration);
        assert_eq!(
            browser_source.icon,
            Some(ToolActivityIcon::NativeApp {
                app: ToolActivityNativeAppReference::DisplayName {
                    display_name: text("Google Chrome")
                }
            })
        );

        let app = mcp_tool_presentation(&json!({
            "type": "mcpToolCall",
            "id": "app",
            "server": "computer",
            "tool": "click",
            "status": "completed",
            "arguments": {},
            "result": {
                "content": [],
                "_meta": {
                    "codex/toolSurface": {
                        "kind": "computerUse",
                        "app": { "kind": "appId", "appId": "com.apple.finder" },
                    },
                },
            },
        }));
        assert_eq!(app.tool_surface, Some(CodexToolSurface::Computer));
        assert_eq!(
            app.tool_icon,
            Some(ToolActivityIcon::NativeApp {
                app: app_id("com.apple.finder")
            })
        );
        let app_source = app.tool_source.unwrap();
        assert_eq!(app_source.name.as_str(), "Finder");
        assert_eq!(app_source.key.as_str(), "native-app:com.apple.finder");
        assert_eq!(app_source.kind, ToolActivitySourceKind::Computer);
    }

    #[test]
    fn computer_use_without_an_app_falls_back_to_the_computer_use_source() {
        let presentation = mcp_tool_presentation(&json!({
            "type": "mcpToolCall",
            "server": "node_repl",
            "tool": "js",
            "arguments": {},
            "result": { "_meta": { "codex/toolSurface": { "kind": "computerUse" } } },
        }));
        assert_eq!(presentation.tool_surface, Some(CodexToolSurface::Computer));
        assert_eq!(presentation.tool_icon, None);
        assert_eq!(
            presentation.tool_source,
            Some(source(
                "computer-use",
                "Computer Use",
                ToolActivitySourceKind::Computer,
                None
            ))
        );
    }

    #[test]
    fn computer_use_names_the_app_from_context_then_arguments_then_the_reference() {
        let surface = |app: Value| json!({ "result": { "_meta": { "codex/toolSurface": { "kind": "computerUse", "app": app } } } });
        let named = |mut item: Value, extra: Value| {
            for (key, value) in extra.as_object().unwrap() {
                item[key] = value.clone();
            }
            mcp_tool_presentation(&item)
                .tool_source
                .unwrap()
                .name
                .as_str()
                .to_owned()
        };
        let by_display = surface(json!({ "kind": "displayName", "displayName": "Notes" }));
        assert_eq!(named(by_display.clone(), json!({})), "Notes");
        assert_eq!(
            named(
                by_display.clone(),
                json!({ "arguments": { "appName": "From args" } })
            ),
            "From args"
        );
        assert_eq!(
            named(
                by_display,
                json!({ "arguments": { "appName": "From args" }, "appContext": { "appName": "Context" } })
            ),
            "Context"
        );
        assert_eq!(
            named(
                surface(json!({ "kind": "appId", "appId": "company.thebrowser.browser" })),
                json!({})
            ),
            "Arc"
        );
        // An app id with characters outside `[A-Za-z0-9._-]` is not an app reference.
        let invalid =
            mcp_tool_presentation(&surface(json!({ "kind": "appId", "appId": "no spaces" })));
        assert_eq!(invalid.tool_icon, None);
        assert_eq!(invalid.tool_source.unwrap().key.as_str(), "computer-use");
    }

    #[test]
    fn over_long_native_app_keys_are_hashed_to_512_characters() {
        let long_id = "a".repeat(600);
        let key = native_app_source_key(&long_id);
        assert_eq!(key.len(), 512);
        let (prefix, digest) = key.rsplit_once(':').unwrap();
        assert_eq!(digest.len(), 64);
        assert!(digest.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(prefix.starts_with("native-app:aaaa"));
        assert_eq!(
            native_app_source_key("Com.Example"),
            "native-app:com.example"
        );
    }

    #[test]
    fn browser_use_picks_the_first_http_page_among_screenshot_browser_use_and_open_tabs() {
        let presentation = mcp_tool_presentation(&json!({
            "result": {
                "_meta": {
                    "codex/toolSurface": {
                        "kind": "browserUse",
                        "backend": "iab",
                        "screenshot": { "pageUrl": "file:///not-http" },
                        "openTabs": [
                            { "url": "https://first.example/" },
                            { "url": "chrome://settings" },
                            { "url": "https://last.example/page", "favIconUrl": "https://last.example/f.png" },
                        ],
                    },
                },
            },
        }));
        assert_eq!(
            presentation.tool_icon,
            Some(website(
                "https://last.example/page",
                Some("https://last.example/f.png")
            ))
        );
        let source = presentation.tool_source.unwrap();
        assert_eq!(source.name.as_str(), "Browser");
        assert_eq!(source.kind, ToolActivitySourceKind::Browser);
        assert_eq!(source.key.as_str(), "browser-use:browser");
        assert_eq!(source.icon, None);
    }

    #[test]
    fn image_urls_accept_http_https_and_data_only() {
        assert_eq!(
            normalized_image_url(&json!("https://example.com")),
            Some("https://example.com/".to_owned())
        );
        assert!(normalized_image_url(&json!("data:image/png;base64,AAAA")).is_some());
        assert_eq!(normalized_image_url(&json!("javascript:alert(1)")), None);
        assert_eq!(normalized_image_url(&json!("not a url")), None);
        assert_eq!(normalized_image_url(&json!(5)), None);
        assert_eq!(
            normalized_image_url(&json!(format!("https://e.com/{}", "a".repeat(4100)))),
            None
        );
    }
}

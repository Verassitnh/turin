//! Port of the presentation half of `projectCodexDynamicToolItem` in
//! upstream/apps/server/src/orchestration-v2/Adapters/CodexAdapterV2.ts: the
//! integration path of `provider/CodexToolPresentation.ts` `mcpToolPresentation`
//! (over `provider-core/src/server/mcpToolPresentation.ts`), and the
//! `dynamicToolSurface` / `dynamicToolTitle` helpers from
//! `packages/shared/src/toolActivity.ts`.
//!
//! Not ported: the browser-use and computer-use MCP surfaces. Those calls take
//! the integration path here (see the Deviations table in PORTING.md).
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;
use turin_contracts::{
    TrimmedNonEmptyString,
    base_schemas::{js_len, js_trim},
    provider_runtime::{
        ToolActivityIcon, ToolActivitySource, ToolActivitySourceKind, ToolActivitySurface,
    },
};

/// Upstream `normalizeMcpText` default `maxLength`.
const MCP_TEXT_MAX_LEN: usize = 160;
/// Upstream `normalizedImageUrl` / `normalizeMcpHttpUrl` length bound.
const URL_MAX_LEN: usize = 4096;

static QUALIFIED_MCP_TOOL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^mcp__(.+?)__(.+)$").expect("qualified tool regex"));
static SKY_CALL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\bsky\s*\.").expect("sky call regex"));
static OAI_SKY_IMPORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"["']@oai/sky["']"#).expect("oai sky regex"));

/// Upstream `McpToolPresentation`, without the browser and computer surfaces.
#[derive(Default)]
pub struct ToolPresentation {
    pub title: Option<String>,
    pub tool_surface: Option<ToolActivitySurface>,
    pub tool_icon: Option<ToolActivityIcon>,
    pub tool_source: Option<ToolActivitySource>,
}

/// Upstream `COMPUTER_USE_TOOL_SOURCE`.
pub fn computer_use_tool_source() -> ToolActivitySource {
    ToolActivitySource {
        key: TrimmedNonEmptyString::new("computer-use").expect("computer-use key"),
        name: TrimmedNonEmptyString::new("Computer Use").expect("computer-use name"),
        kind: ToolActivitySourceKind::Computer,
        icon: None,
    }
}

fn is_javascript_repl_tool(tool_name: &str) -> bool {
    matches!(tool_name, "js" | "cua_repl.js" | "node_repl.js")
}

/// Upstream `dynamicToolSurface`. Computer-use calls may fail or still run before
/// result metadata exists, so the surface comes from the name or the REPL code.
pub fn dynamic_tool_surface(tool_name: &str, input: &Value) -> Option<ToolActivitySurface> {
    if tool_name == "cua_repl.js" {
        return Some(ToolActivitySurface::Computer);
    }
    let code = input.get("code")?.as_str()?;
    (is_javascript_repl_tool(tool_name)
        && (SKY_CALL.is_match(code) || OAI_SKY_IMPORT.is_match(code)))
    .then_some(ToolActivitySurface::Computer)
}

/// Upstream `asTrimmedString`: a non-empty string, trimmed.
fn trimmed_text(text: &str) -> Option<&str> {
    Some(js_trim(text)).filter(|text| !text.is_empty())
}

fn trimmed_str(value: Option<&Value>) -> Option<&str> {
    value.and_then(Value::as_str).and_then(trimmed_text)
}

/// Upstream `dynamicToolTitle`: a REPL's `title`, or the skill a `Skill` call loads.
pub fn dynamic_tool_title(
    tool_name: &str,
    input: &Value,
    retained_title: Option<&str>,
) -> Option<String> {
    if is_javascript_repl_tool(tool_name) {
        // A retained title that only repeats the tool name says nothing about the call.
        let meaningful = retained_title.and_then(trimmed_text).filter(|title| {
            let lower = title.to_lowercase();
            lower != "js" && lower != tool_name.to_lowercase()
        });
        return Some(
            trimmed_str(input.get("title"))
                .or(meaningful)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    if dynamic_tool_surface(tool_name, input) == Some(ToolActivitySurface::Computer)
                    {
                        "Computer use".to_owned()
                    } else {
                        "Run JavaScript".to_owned()
                    }
                }),
        );
    }
    if tool_name == "Skill" {
        return trimmed_str(input.get("skill")).map(|skill| format!("Skill: {skill}"));
    }
    None
}

/// Upstream `normalizeMcpText`: whitespace collapsed, at most 160 chars.
fn normalize_text(text: &str) -> Option<String> {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty() && js_len(&collapsed) <= MCP_TEXT_MAX_LEN).then_some(collapsed)
}

fn normalize_value_text(value: &Value) -> Option<String> {
    value.as_str().and_then(normalize_text)
}

/// Upstream `normalizeMcpHttpUrl`: http(s) only, normalized through `URL`.
fn normalize_http_url(text: &str) -> Option<String> {
    if js_len(text) > URL_MAX_LEN {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let is_http = matches!(url.scheme(), "http" | "https");
    (is_http && url.as_str().len() <= URL_MAX_LEN).then(|| url.as_str().to_owned())
}

/// Upstream `normalizedImageUrl`: http(s) or data URLs only.
fn normalize_image_url(text: &str) -> Option<String> {
    if js_len(text) > URL_MAX_LEN {
        return None;
    }
    let url = url::Url::parse(text).ok()?;
    let allowed = matches!(url.scheme(), "http" | "https" | "data");
    allowed.then(|| url.as_str().to_owned())
}

fn image_url_of(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).and_then(normalize_image_url)
}

fn record(value: Option<&Value>) -> Option<&Value> {
    value.filter(|value| value.is_object())
}

/// Upstream `themedLogoIcon`: the first record with a usable `logoUrl`.
fn themed_logo(records: &[Option<&Value>]) -> Option<ToolActivityIcon> {
    records.iter().flatten().find_map(|record| {
        let logo_url = image_url_of(record.get("logoUrl"))?;
        let dark = record
            .get("logoUrlDark")
            .filter(|value| !value.is_null())
            .or_else(|| record.get("logoDarkUrl"));
        Some(ToolActivityIcon::ThemedLogo {
            logo_url: TrimmedNonEmptyString::new(logo_url).ok()?,
            logo_url_dark: image_url_of(dark).and_then(|url| TrimmedNonEmptyString::new(url).ok()),
        })
    })
}

/// Upstream `mcpToolPresentation` (provider-core), over the values the Codex
/// adapter passes it. `None` means the value was absent or null in the item.
struct IntegrationInput<'a> {
    server_name: Option<&'a Value>,
    tool_name: Option<&'a Value>,
    title: Option<&'a Value>,
    server_display_name: Option<&'a Value>,
    source: Option<&'a Value>,
    icon_url: Option<String>,
    icon_url_dark: Option<String>,
}

fn integration_presentation(input: IntegrationInput<'_>) -> ToolPresentation {
    let qualified = input
        .tool_name
        .and_then(Value::as_str)
        .and_then(|name| QUALIFIED_MCP_TOOL.captures(name));
    let server = match input.server_name.filter(|value| !value.is_null()) {
        Some(value) => normalize_value_text(value),
        None => match &qualified {
            Some(captures) => normalize_text(&captures[1]),
            None => input.server_display_name.and_then(normalize_value_text),
        },
    };
    let tool = match &qualified {
        Some(captures) => normalize_text(&captures[2]),
        None => input.tool_name.and_then(normalize_value_text),
    };
    let title = input
        .title
        .and_then(normalize_value_text)
        .or_else(|| match (&server, &tool) {
            (Some(_), Some(tool)) => normalize_text(&tool.replace(['_', '-'], " ")),
            _ => None,
        });
    let Some(server) = server else {
        return ToolPresentation {
            title,
            ..ToolPresentation::default()
        };
    };
    let name = input
        .server_display_name
        .and_then(normalize_value_text)
        .or_else(|| {
            input
                .source
                .and_then(|s| s.get("name"))
                .and_then(normalize_value_text)
        })
        .or_else(|| normalize_text(&server.replace(['_', '-'], " ")))
        .unwrap_or_else(|| server.clone());
    let source_url = |key: &str| {
        input
            .source
            .and_then(|s| s.get(key))
            .and_then(Value::as_str)
            .and_then(normalize_http_url)
    };
    let logo_url = input
        .icon_url
        .as_deref()
        .and_then(normalize_http_url)
        .or_else(|| source_url("logoUrl"));
    let logo_url_dark = input
        .icon_url_dark
        .as_deref()
        .and_then(normalize_http_url)
        .or_else(|| source_url("logoUrlDark"));
    let icon = logo_url.and_then(|logo_url| {
        Some(ToolActivityIcon::ThemedLogo {
            logo_url: TrimmedNonEmptyString::new(logo_url).ok()?,
            logo_url_dark: logo_url_dark.and_then(|url| TrimmedNonEmptyString::new(url).ok()),
        })
    });
    ToolPresentation {
        title,
        tool_surface: None,
        tool_icon: icon.clone(),
        tool_source: integration_source(&server, name, icon),
    }
}

fn integration_source(
    server: &str,
    name: String,
    icon: Option<ToolActivityIcon>,
) -> Option<ToolActivitySource> {
    Some(ToolActivitySource {
        key: TrimmedNonEmptyString::new(format!("mcp:{}", server.to_lowercase())).ok()?,
        name: TrimmedNonEmptyString::new(name).ok()?,
        kind: ToolActivitySourceKind::Integration,
        icon,
    })
}

/// Upstream `mcpToolPresentation` (CodexToolPresentation) on the integration path.
pub fn mcp_tool_presentation(item: &Value) -> ToolPresentation {
    let metadata = record(item.get("result")).and_then(|result| record(result.get("_meta")));
    let surface = metadata.and_then(|meta| record(meta.get("codex/toolSurface")));
    let source = metadata.and_then(|meta| record(meta.get("source")));
    let app_context = record(item.get("appContext"));
    let source_logo = themed_logo(&[surface, source, app_context]);
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
    // Upstream `appContext?.connectorId ?? item.server`: null falls through.
    let connector = app_context
        .and_then(|ctx| ctx.get("connectorId"))
        .filter(|value| !value.is_null());
    integration_presentation(IntegrationInput {
        server_name: connector.or_else(|| item.get("server")),
        tool_name: item.get("tool"),
        title: app_context.and_then(|ctx| ctx.get("actionName")),
        server_display_name: app_context.and_then(|ctx| ctx.get("appName")),
        source,
        icon_url,
        icon_url_dark,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repl_title_prefers_input_title_then_retained_then_default() {
        assert_eq!(
            dynamic_tool_title("js", &json!({"title": "  Run tests  "}), None).as_deref(),
            Some("Run tests")
        );
        // A retained title that only repeats the tool name is not meaningful.
        assert_eq!(
            dynamic_tool_title("js", &json!({}), Some("JS")).as_deref(),
            Some("Run JavaScript")
        );
        assert_eq!(
            dynamic_tool_title("node_repl.js", &json!({}), Some("Lint pass")).as_deref(),
            Some("Lint pass")
        );
    }

    #[test]
    fn computer_surface_comes_from_sky_calls() {
        assert_eq!(
            dynamic_tool_surface("cua_repl.js", &json!({})),
            Some(ToolActivitySurface::Computer)
        );
        assert_eq!(
            dynamic_tool_surface("js", &json!({"code": "await sky.click(1)"})),
            Some(ToolActivitySurface::Computer)
        );
        assert_eq!(
            dynamic_tool_surface("js", &json!({"code": "console.log(1)"})),
            None
        );
        assert_eq!(
            dynamic_tool_title("js", &json!({"code": "await sky.click(1)"}), None).as_deref(),
            Some("Computer use")
        );
    }

    #[test]
    fn skill_title_only_for_skill_tool() {
        assert_eq!(
            dynamic_tool_title("Skill", &json!({"skill": "review"}), None).as_deref(),
            Some("Skill: review")
        );
        assert_eq!(
            dynamic_tool_title("Read", &json!({"skill": "x"}), None),
            None
        );
    }

    #[test]
    fn mcp_integration_presentation_names_server_and_source() {
        let item = json!({
            "server": "linear_app",
            "tool": "create_issue",
            "result": null,
            "appContext": null,
        });
        let presentation = mcp_tool_presentation(&item);
        assert_eq!(presentation.title.as_deref(), Some("create issue"));
        let source = presentation.tool_source.expect("integration source");
        assert_eq!(source.key.as_str(), "mcp:linear_app");
        assert_eq!(source.name.as_str(), "linear app");
        assert_eq!(source.kind, ToolActivitySourceKind::Integration);
        assert!(presentation.tool_icon.is_none());
    }

    #[test]
    fn mcp_logo_from_source_metadata_is_used_when_http() {
        let item = json!({
            "server": "docs",
            "tool": "search",
            "result": {"_meta": {"source": {
                "name": "Docs Hub",
                "logoUrl": "https://example.com/logo.png",
                "logoUrlDark": "https://example.com/logo-dark.png",
            }}},
        });
        let presentation = mcp_tool_presentation(&item);
        let source = presentation.tool_source.expect("integration source");
        assert_eq!(source.name.as_str(), "Docs Hub");
        assert!(matches!(
            presentation.tool_icon,
            Some(ToolActivityIcon::ThemedLogo { .. })
        ));
    }

    #[test]
    fn qualified_tool_names_supply_server_when_item_has_none() {
        let item = json!({"server": null, "tool": "mcp__github__list_prs"});
        let presentation = mcp_tool_presentation(&item);
        let source = presentation.tool_source.expect("integration source");
        assert_eq!(source.key.as_str(), "mcp:github");
        assert_eq!(presentation.title.as_deref(), Some("list prs"));
    }

    #[test]
    fn item_server_wins_over_qualified_name() {
        // Upstream `serverName ?? qualified[1]`: the item's server is used as given.
        let item = json!({"server": "ignored", "tool": "mcp__github__list_prs"});
        let source = mcp_tool_presentation(&item)
            .tool_source
            .expect("integration source");
        assert_eq!(source.key.as_str(), "mcp:ignored");
    }
}

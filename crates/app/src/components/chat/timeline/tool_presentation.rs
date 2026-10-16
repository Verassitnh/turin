//! Port of upstream/packages/client-runtime/src/work-log/toolPresentation.ts
//!
//! Upstream reads the presentation fields off the raw (wire-decoded) item
//! and re-validates them. Turin's turn items are the typed mirror in
//! [`super::turn_item`], so this reads the decoded fields and keeps the same
//! validation: bounded trimmed strings, http(s)/data image URLs, http(s) page
//! URLs, safe app ids, and the computer-use source recovery. URL validation is
//! a scheme check; upstream's `new URL(..).href` normalization (e.g. a trailing
//! `/` on a bare origin) belongs to the decode seam.

use std::sync::LazyLock;

use regex::Regex;

use super::js_text::{js_trim, utf16_len};
use super::tool_activity::{computer_use_tool_source, dynamic_tool_surface};
use super::turn_item::{
    ToolActivityIcon, ToolActivitySource, ToolActivitySurface, TurnItem, TurnItemPayload,
};
use turin_shared::file_preview::is_workspace_image_preview_path;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExtractedToolActivityPresentation {
    pub viewed_image_path: Option<String>,
    pub tool_surface: Option<ToolActivitySurface>,
    pub tool_icon: Option<ToolActivityIcon>,
    pub tool_source: Option<ToolActivitySource>,
}

fn trimmed_string(value: &str, max_length: usize) -> Option<String> {
    let trimmed = js_trim(value);
    (!trimmed.is_empty() && utf16_len(trimmed) <= max_length).then(|| trimmed.to_string())
}

fn has_scheme(url: &str, schemes: &[&str]) -> bool {
    let lower = url.to_ascii_lowercase();
    schemes.iter().any(|scheme| lower.starts_with(scheme))
}

fn image_url(value: &str) -> Option<String> {
    trimmed_string(value, 4096).filter(|url| has_scheme(url, &["http://", "https://", "data:"]))
}

fn page_url(value: &str) -> Option<String> {
    trimmed_string(value, 4096).filter(|url| has_scheme(url, &["http://", "https://"]))
}

static APP_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+$").expect("static regex"));

/// The mirror keeps one app string for both `app-id` and `display-name`
/// references; an id-shaped value keeps the id bound, anything else is a
/// display name.
fn native_app_reference(app: &str) -> Option<String> {
    if let Some(app_id) = trimmed_string(app, 512).filter(|app_id| APP_ID.is_match(app_id)) {
        return Some(app_id);
    }
    trimmed_string(app, 160)
}

fn activity_icon(icon: Option<&ToolActivityIcon>) -> Option<ToolActivityIcon> {
    match icon? {
        ToolActivityIcon::Website {
            page_url: raw_page_url,
            favicon_url,
        } => {
            let resolved_page_url = page_url(raw_page_url)?;
            Some(ToolActivityIcon::Website {
                page_url: resolved_page_url,
                favicon_url: favicon_url.as_deref().and_then(image_url),
            })
        }
        ToolActivityIcon::NativeApp { app } => {
            native_app_reference(app).map(|app| ToolActivityIcon::NativeApp { app })
        }
        ToolActivityIcon::ThemedLogo { logo_url } => {
            image_url(logo_url).map(|logo_url| ToolActivityIcon::ThemedLogo { logo_url })
        }
    }
}

fn activity_source(source: Option<&ToolActivitySource>) -> Option<ToolActivitySource> {
    let source = source?;
    let key = trimmed_string(&source.key, 512)?;
    let name = trimmed_string(&source.name, 160)?;
    if !matches!(source.kind.as_str(), "browser" | "computer" | "integration") {
        return None;
    }
    Some(ToolActivitySource {
        key,
        name,
        kind: source.kind.clone(),
        icon: activity_icon(source.icon.as_ref()),
    })
}

pub fn extract_tool_activity_presentation(item: &TurnItem) -> ExtractedToolActivityPresentation {
    let retained_source = activity_source(item.tool_source.as_ref());
    let tool_surface = item
        .tool_surface
        .or_else(
            || match retained_source.as_ref().map(|source| source.kind.as_str()) {
                Some("computer") => Some(ToolActivitySurface::Computer),
                Some("browser") => Some(ToolActivitySurface::Browser),
                _ => None,
            },
        )
        .or_else(|| match &item.payload {
            TurnItemPayload::DynamicTool {
                tool_name, input, ..
            } => dynamic_tool_surface(tool_name.as_deref(), input),
            _ => None,
        });
    let tool_source = if tool_surface == Some(ToolActivitySurface::Computer)
        && retained_source.as_ref().map(|source| source.kind.as_str()) != Some("computer")
    {
        Some(computer_use_tool_source())
    } else {
        retained_source
    };
    let tool_icon = activity_icon(item.tool_icon.as_ref());
    let viewed_image_path = match &item.payload {
        TurnItemPayload::DynamicTool {
            viewed_image_path: Some(path),
            ..
        } => trimmed_string(path, 4096),
        _ => None,
    }
    .filter(|path| !path.contains(['\r', '\n']) && is_workspace_image_preview_path(path));
    ExtractedToolActivityPresentation {
        viewed_image_path,
        tool_surface,
        tool_icon,
        tool_source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    use super::super::turn_item::TurnItemStatus;

    fn dynamic_tool(tool_name: &str, input: serde_json::Value) -> TurnItem {
        TurnItem::new(
            "item",
            "thread",
            TurnItemStatus::Completed,
            "2026-01-01T00:00:00.000Z",
            TurnItemPayload::DynamicTool {
                tool_name: Some(tool_name.to_string()),
                viewed_image_path: None,
                input,
                output: None,
                output_omitted: false,
            },
        )
    }

    fn computer_use() -> ToolActivitySource {
        ToolActivitySource {
            key: "computer-use".into(),
            name: "Computer Use".into(),
            kind: "computer".into(),
            icon: None,
        }
    }

    #[test]
    fn recovers_computer_use_presentation_for_retained_calls_without_result_metadata() {
        let item = dynamic_tool("js", json!({ "code": "await sky.get_app_state(app);" }));
        assert_eq!(
            extract_tool_activity_presentation(&item),
            ExtractedToolActivityPresentation {
                tool_surface: Some(ToolActivitySurface::Computer),
                tool_source: Some(computer_use()),
                ..Default::default()
            }
        );
        let browser = TurnItem {
            tool_surface: Some(ToolActivitySurface::Browser),
            ..item.clone()
        };
        assert_eq!(
            extract_tool_activity_presentation(&browser),
            ExtractedToolActivityPresentation {
                tool_surface: Some(ToolActivitySurface::Browser),
                ..Default::default()
            }
        );
        let plain = dynamic_tool("js", json!({ "code": "await fs.readFile(path);" }));
        assert_eq!(
            extract_tool_activity_presentation(&plain),
            ExtractedToolActivityPresentation::default()
        );
    }

    #[test]
    fn replaces_a_failed_sky_calls_generic_node_repl_source() {
        let source = ToolActivitySource {
            key: "mcp:node_repl".into(),
            name: "node repl".into(),
            kind: "integration".into(),
            icon: None,
        };
        let item = TurnItem {
            status: TurnItemStatus::Failed,
            title: Some("js".into()),
            tool_source: Some(source.clone()),
            ..dynamic_tool(
                "node_repl.js",
                json!({
                    "title": "Restrict the breakpoint to Git actions",
                    "code": "await sky.type_text({app:'T3 Code (Nightly)',text:'debug(ga.run)'});",
                }),
            )
        };
        assert_eq!(
            extract_tool_activity_presentation(&item),
            ExtractedToolActivityPresentation {
                tool_surface: Some(ToolActivitySurface::Computer),
                tool_source: Some(computer_use()),
                ..Default::default()
            }
        );
        let fetch = TurnItem {
            payload: dynamic_tool(
                "node_repl.js",
                json!({ "code": "await fetch('http://127.0.0.1/json/list');" }),
            )
            .payload,
            ..item
        };
        assert_eq!(
            extract_tool_activity_presentation(&fetch),
            ExtractedToolActivityPresentation {
                tool_source: Some(source),
                ..Default::default()
            }
        );
    }

    fn viewing(path: &str) -> TurnItem {
        let mut item = dynamic_tool("Read", json!({}));
        if let TurnItemPayload::DynamicTool {
            viewed_image_path, ..
        } = &mut item.payload
        {
            *viewed_image_path = Some(path.to_string());
        }
        item
    }

    #[test]
    fn retains_valid_image_paths_independently_of_redacted_output() {
        assert_eq!(
            extract_tool_activity_presentation(&viewing(" /workspace/reference.webp ")),
            ExtractedToolActivityPresentation {
                viewed_image_path: Some("/workspace/reference.webp".into()),
                ..Default::default()
            }
        );
        for path in [
            "/workspace/README.md".to_string(),
            "/workspace/a.png\nother".to_string(),
            format!("{}.png", "x".repeat(4097)),
        ] {
            assert_eq!(
                extract_tool_activity_presentation(&viewing(&path)),
                ExtractedToolActivityPresentation::default()
            );
        }
    }

    #[test]
    fn reads_provider_neutral_presentation_fields() {
        let icon = ToolActivityIcon::Website {
            page_url: "https://example.com/docs".into(),
            favicon_url: Some("https://example.com/favicon.png".into()),
        };
        let source = ToolActivitySource {
            key: "integration:example".into(),
            name: "Example".into(),
            kind: "integration".into(),
            icon: Some(ToolActivityIcon::ThemedLogo {
                logo_url: "https://example.com/logo-light.png".into(),
            }),
        };
        let item = TurnItem {
            tool_surface: Some(ToolActivitySurface::Browser),
            tool_icon: Some(icon.clone()),
            tool_source: Some(source.clone()),
            ..dynamic_tool("tool", json!({}))
        };
        assert_eq!(
            extract_tool_activity_presentation(&item),
            ExtractedToolActivityPresentation {
                tool_surface: Some(ToolActivitySurface::Browser),
                tool_icon: Some(icon),
                tool_source: Some(source),
                ..Default::default()
            }
        );
    }

    #[test]
    fn reads_provider_neutral_native_app_icons() {
        let icon = ToolActivityIcon::NativeApp {
            app: "com.example.Editor".into(),
        };
        let source = ToolActivitySource {
            key: "native-app:com.example.editor".into(),
            name: "Editor".into(),
            kind: "computer".into(),
            icon: None,
        };
        let item = TurnItem {
            tool_surface: Some(ToolActivitySurface::Computer),
            tool_icon: Some(icon.clone()),
            tool_source: Some(source.clone()),
            ..dynamic_tool("tool", json!({}))
        };
        assert_eq!(
            extract_tool_activity_presentation(&item),
            ExtractedToolActivityPresentation {
                tool_surface: Some(ToolActivitySurface::Computer),
                tool_icon: Some(icon),
                tool_source: Some(source),
                ..Default::default()
            }
        );
    }

    #[test]
    fn rejects_unsafe_icon_urls() {
        let item = TurnItem {
            tool_icon: Some(ToolActivityIcon::Website {
                page_url: "javascript:alert(1)".into(),
                favicon_url: None,
            }),
            ..dynamic_tool("tool", json!({}))
        };
        assert_eq!(extract_tool_activity_presentation(&item).tool_icon, None);
    }
}

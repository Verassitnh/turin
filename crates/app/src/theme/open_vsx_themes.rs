//! Port of upstream/apps/web/src/openVsxThemes.ts

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read};
use std::sync::OnceLock;
use std::time::Duration;

use gpui_kit::SharedString;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zip::ZipArchive;
use zip::read::ZipFile;

use super::open_vsx::{
    OpenVsxClient, OpenVsxClientError, OpenVsxHttpResponse, ReqwestOpenVsxClient,
};
use super::theme_palettes::{ThemeCollection, ThemeDefinition};
use super::vscode_theme_import::{
    ThemeImportEntry, is_vscode_theme_file, pair_vscode_themes, parse_vscode_theme_file,
    resolve_theme_label_collisions,
};

pub(crate) const OPEN_VSX_SEARCH_URL: &str = "https://open-vsx.org/api/-/search";
const MAX_VSIX_BYTES: usize = 20 * 1024 * 1024;
const MAX_SEARCH_BYTES: usize = 512 * 1024;
const MAX_DETAIL_BYTES: usize = 256 * 1024;
const MAX_MANIFEST_BYTES: usize = 256 * 1024;
const SEARCH_REQUEST_TIMEOUT_MS: u64 = 10_000;
const MAX_THEME_BYTES: usize = 256 * 1024;
const MAX_ZIP_ENTRIES: usize = 5_000;
const MAX_UNCOMPRESSED_BYTES: u64 = 100 * 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 200;
const MAX_THEMES_PER_EXTENSION: usize = 40;
const MAX_INCLUDE_DEPTH: usize = 8;
const MAX_PACKAGE_PATH_LENGTH: usize = 1_024;
const MAX_COLOR_VALUE_LENGTH: usize = 128;
const MAX_RESOLVED_THEME_FILES: usize = MAX_THEMES_PER_EXTENSION * MAX_INCLUDE_DEPTH;

static SUPPORTED_LICENSES: OnceLock<HashSet<&'static str>> = OnceLock::new();
static USED_WORKBENCH_COLORS: OnceLock<HashSet<&'static str>> = OnceLock::new();

fn supported_licenses() -> &'static HashSet<&'static str> {
    SUPPORTED_LICENSES.get_or_init(|| {
        [
            "0BSD",
            "Apache-2.0",
            "BSD-2-Clause",
            "BSD-3-Clause",
            "CC0-1.0",
            "ISC",
            "MIT",
            "MPL-2.0",
            "Unlicense",
        ]
        .into_iter()
        .collect()
    })
}

fn used_workbench_colors() -> &'static HashSet<&'static str> {
    USED_WORKBENCH_COLORS.get_or_init(|| {
        [
            "activityBar.background",
            "activityBarBadge.background",
            "badge.background",
            "button.background",
            "button.foreground",
            "contrastBorder",
            "descriptionForeground",
            "disabledForeground",
            "dropdown.background",
            "dropdown.border",
            "editor.background",
            "editor.foreground",
            "editor.findMatchBackground",
            "editor.findMatchForeground",
            "editor.findMatchHighlightBackground",
            "editor.findMatchHighlightForeground",
            "editor.selectionBackground",
            "editorCursor.foreground",
            "editorError.foreground",
            "editorGroup.border",
            "editorPane.background",
            "editorWarning.foreground",
            "editorWidget.background",
            "errorForeground",
            "focusBorder",
            "foreground",
            "input.border",
            "input.placeholderForeground",
            "list.activeSelectionBackground",
            "list.hoverBackground",
            "list.inactiveSelectionBackground",
            "menu.background",
            "panel.background",
            "panel.border",
            "progressBar.background",
            "quickInput.background",
            "scrollbarSlider.background",
            "sideBar.background",
            "sideBar.border",
            "sideBar.foreground",
            "terminal.background",
            "terminal.foreground",
            "terminal.selectionBackground",
            "terminalCursor.foreground",
            "textCodeBlock.background",
            "textLink.foreground",
        ]
        .into_iter()
        .collect()
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OpenVsxThemeSort {
    #[default]
    DownloadCount,
    Rating,
    Timestamp,
    Relevance,
}

impl OpenVsxThemeSort {
    fn as_str(self) -> &'static str {
        match self {
            Self::DownloadCount => "downloadCount",
            Self::Rating => "rating",
            Self::Timestamp => "timestamp",
            Self::Relevance => "relevance",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenVsxThemeExtension {
    pub id: String,
    pub collection_id: String,
    pub name: String,
    pub publisher: String,
    pub description: String,
    pub download_count: u64,
    pub icon_url: Option<String>,
    pub source_url: Option<String>,
    pub manifest_url: String,
    pub sha256_url: String,
    pub vsix_url: String,
    pub version: String,
    pub license: String,
}

pub struct OpenVsxThemeSearchOptions<'a> {
    pub cancel: Option<&'a CancellationToken>,
    pub sort_by: OpenVsxThemeSort,
}

impl<'a> Default for OpenVsxThemeSearchOptions<'a> {
    fn default() -> Self {
        Self {
            cancel: None,
            sort_by: OpenVsxThemeSort::DownloadCount,
        }
    }
}

fn is_record(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

fn short_hash(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    digest
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn open_vsx_theme_id(extension_id: &str, source: &str) -> String {
    format!(
        "ovx-theme-{}",
        short_hash(&format!("{extension_id}:{source}"))
    )
}

fn open_vsx_collection_id(extension_id: &str) -> String {
    let normalized = format!("open-vsx:{}", extension_id.to_ascii_lowercase());
    static COLLECTION_ID: OnceLock<Regex> = OnceLock::new();
    let re = COLLECTION_ID
        .get_or_init(|| Regex::new(r"^[a-z0-9][a-z0-9.:-]{0,127}$").expect("collection id"));
    if re.is_match(&normalized) {
        normalized
    } else {
        format!("open-vsx:{}", short_hash(extension_id))
    }
}

fn trusted_open_vsx_url(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    let url = url::Url::parse(text).ok()?;
    if url.scheme() == "https" && url.host_str()?.eq_ignore_ascii_case("open-vsx.org") {
        Some(url.to_string())
    } else {
        None
    }
}

fn public_source_url(value: &Value) -> Option<String> {
    let raw = value.as_str().map(str::to_string).or_else(|| {
        value
            .as_object()
            .and_then(|object| object.get("url"))
            .and_then(|url| url.as_str().map(str::to_string))
    })?;
    let url = url::Url::parse(&raw).ok()?;
    if url.scheme() == "https" && url.username().is_empty() && url.password().is_none() {
        Some(url.to_string())
    } else {
        None
    }
}

struct ThemeContribution {
    label: Option<String>,
    ui_theme: Option<String>,
    path: Option<String>,
}

fn theme_contributions(manifest: &Map<String, Value>) -> Vec<ThemeContribution> {
    let contributes = manifest
        .get("contributes")
        .and_then(|value| value.as_object());
    let Some(themes) = contributes
        .and_then(|object| object.get("themes"))
        .and_then(|value| value.as_array())
    else {
        return Vec::new();
    };
    themes
        .iter()
        .filter_map(|entry| {
            let object = entry.as_object()?;
            Some(ThemeContribution {
                label: object
                    .get("label")
                    .and_then(|value| value.as_str().map(str::to_string)),
                ui_theme: object
                    .get("uiTheme")
                    .and_then(|value| value.as_str().map(str::to_string)),
                path: object
                    .get("path")
                    .and_then(|value| value.as_str().map(str::to_string)),
            })
        })
        .collect()
}

fn manifest_license_matches(manifest: &Map<String, Value>, license: &str) -> bool {
    manifest
        .get("license")
        .and_then(|value| value.as_str())
        .is_some_and(|value| value.trim().eq_ignore_ascii_case(license.trim()))
}

fn extension_from_detail(value: &Value) -> Result<Option<OpenVsxThemeExtension>, String> {
    let object =
        is_record(value).ok_or_else(|| "Open VSX returned malformed theme details.".to_string())?;
    let files = object
        .get("files")
        .and_then(|value| value.as_object())
        .ok_or_else(|| "Open VSX returned malformed theme details.".to_string())?;
    let namespace = object
        .get("namespace")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    let extension_name = object
        .get("name")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    let display_name = object
        .get("displayName")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(extension_name);
    let version = object
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    let license = object
        .get("license")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .unwrap_or("");
    let manifest_url = files
        .get("manifest")
        .and_then(trusted_open_vsx_url)
        .ok_or_else(|| "Open VSX returned malformed theme details.".to_string())?;
    let sha256_url = files
        .get("sha256")
        .and_then(trusted_open_vsx_url)
        .ok_or_else(|| "Open VSX returned malformed theme details.".to_string())?;
    let vsix_url = files
        .get("download")
        .and_then(trusted_open_vsx_url)
        .ok_or_else(|| "Open VSX returned malformed theme details.".to_string())?;
    if namespace.is_empty() || extension_name.is_empty() || version.is_empty() {
        return Err("Open VSX returned malformed theme details.".to_string());
    }
    if !supported_licenses().contains(license) {
        return Ok(None);
    }
    let id = format!("{namespace}.{extension_name}");
    Ok(Some(OpenVsxThemeExtension {
        id: id.clone(),
        collection_id: open_vsx_collection_id(&id),
        name: display_name.to_string(),
        publisher: namespace.to_string(),
        description: object
            .get("description")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        download_count: object
            .get("downloadCount")
            .and_then(|value| value.as_f64())
            .filter(|value| value.is_finite())
            .map(|value| value as u64)
            .unwrap_or(0),
        icon_url: files.get("icon").and_then(trusted_open_vsx_url),
        source_url: object
            .get("repository")
            .and_then(public_source_url)
            .or_else(|| object.get("homepage").and_then(public_source_url))
            .or_else(|| object.get("url").and_then(public_source_url)),
        manifest_url,
        sha256_url,
        vsix_url,
        version: version.to_string(),
        license: license.to_string(),
    }))
}

fn client_error_message(error: OpenVsxClientError) -> String {
    match error {
        OpenVsxClientError::Aborted => "The operation was aborted.".to_string(),
        OpenVsxClientError::Message(message) => message,
        OpenVsxClientError::Http(error) => error.to_string(),
    }
}

async fn with_search_timeout<C, F, T>(
    client: &C,
    parent: Option<&CancellationToken>,
    operation: F,
) -> Result<T, String>
where
    C: OpenVsxClient + ?Sized,
    F: for<'a> FnOnce(
        &'a C,
        CancellationToken,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<T, OpenVsxClientError>> + Send + 'a>,
    >,
{
    let timeout_token = CancellationToken::new();
    if let Some(parent) = parent {
        if parent.is_cancelled() {
            return Err(client_error_message(OpenVsxClientError::Aborted));
        }
        let child = timeout_token.clone();
        let parent = parent.clone();
        tokio::spawn(async move {
            parent.cancelled().await;
            child.cancel();
        });
    }
    let timeout_child = timeout_token.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(SEARCH_REQUEST_TIMEOUT_MS)).await;
        timeout_child.cancel();
    });

    match operation(client, timeout_token.clone()).await {
        Ok(value) => Ok(value),
        Err(error) => {
            if matches!(error, OpenVsxClientError::Aborted)
                && parent.is_none_or(|token| !token.is_cancelled())
            {
                Err("Open VSX took too long to respond.".to_string())
            } else {
                Err(client_error_message(error))
            }
        }
    }
}

fn read_capped_bytes(
    body: &[u8],
    limit: usize,
    too_large_message: &str,
) -> Result<Vec<u8>, String> {
    if body.len() > limit {
        return Err(too_large_message.to_string());
    }
    Ok(body.to_vec())
}

fn read_capped_response(
    response: OpenVsxHttpResponse,
    limit: usize,
    too_large_message: &str,
) -> Result<Vec<u8>, String> {
    if response
        .content_length
        .is_some_and(|length| length as usize > limit)
    {
        return Err(too_large_message.to_string());
    }
    read_capped_bytes(&response.body, limit, too_large_message)
}

pub async fn search_open_vsx_themes<C: OpenVsxClient + ?Sized>(
    client: &C,
    query: &str,
    options: OpenVsxThemeSearchOptions<'_>,
) -> Result<Vec<OpenVsxThemeExtension>, String> {
    let search_text = query.trim();
    if search_text.is_empty() {
        return Ok(Vec::new());
    }
    let mut url = url::Url::parse(OPEN_VSX_SEARCH_URL).expect("search url");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("query", search_text);
        pairs.append_pair("category", "Themes");
        pairs.append_pair("sortBy", options.sort_by.as_str());
        pairs.append_pair("sortOrder", "desc");
        pairs.append_pair("size", "16");
    }
    let search_url = url.to_string();

    let value = with_search_timeout(client, options.cancel, |client, cancel| {
        Box::pin(async move {
            let response = client.get(&search_url, &cancel).await?;
            if response.status != 200 {
                return Err(OpenVsxClientError::Message(
                    "Open VSX search is unavailable right now.".to_string(),
                ));
            }
            let search_bytes = read_capped_response(
                response,
                MAX_SEARCH_BYTES,
                "Open VSX returned an unexpectedly large response.",
            )
            .map_err(OpenVsxClientError::Message)?;
            serde_json::from_slice::<Value>(&search_bytes).map_err(|_| {
                OpenVsxClientError::Message("Open VSX returned an unreadable response.".to_string())
            })
        })
    })
    .await?;

    let object = is_record(&value)
        .ok_or_else(|| "Open VSX returned an unreadable search response.".to_string())?;
    let extensions = object
        .get("extensions")
        .and_then(|value| value.as_array())
        .ok_or_else(|| "Open VSX returned an unreadable search response.".to_string())?;

    let identities: Vec<(String, String)> = extensions
        .iter()
        .flat_map(|candidate| {
            let object = candidate.as_object()?;
            let namespace = object
                .get("namespace")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let name = object
                .get("name")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if namespace.is_empty() || name.is_empty() {
                None
            } else {
                Some((namespace.to_string(), name.to_string()))
            }
        })
        .collect();

    let mut completed = Vec::new();
    let mut fulfilled_count = 0usize;
    for (namespace, name) in identities.iter().take(16) {
        let detail = with_search_timeout(client, options.cancel, |client, cancel| {
            let namespace = namespace.clone();
            let name = name.clone();
            Box::pin(async move {
                let detail_url = format!(
                    "https://open-vsx.org/api/{}/{}",
                    utf8_percent_encode(&namespace, NON_ALPHANUMERIC),
                    utf8_percent_encode(&name, NON_ALPHANUMERIC)
                );
                let detail_response = client.get(&detail_url, &cancel).await?;
                if detail_response.status != 200 {
                    return Err(OpenVsxClientError::Message(
                        "Open VSX theme details are unavailable.".to_string(),
                    ));
                }
                let detail_bytes = read_capped_response(
                    detail_response,
                    MAX_DETAIL_BYTES,
                    "Open VSX returned an unexpectedly large detail response.",
                )
                .map_err(OpenVsxClientError::Message)?;
                let parsed: Value = serde_json::from_slice(&detail_bytes).map_err(|_| {
                    OpenVsxClientError::Message(
                        "Open VSX returned unreadable theme details.".to_string(),
                    )
                })?;
                let extension =
                    extension_from_detail(&parsed).map_err(OpenVsxClientError::Message)?;
                let Some(extension) = extension else {
                    return Ok(None);
                };
                let manifest_response = client.get(&extension.manifest_url, &cancel).await?;
                if manifest_response.status != 200 {
                    return Err(OpenVsxClientError::Message(
                        "manifest unavailable".to_string(),
                    ));
                }
                let package_response = client.head(&extension.vsix_url, &cancel).await?;
                if package_response.status != 200 {
                    return Ok(None);
                }
                if package_response
                    .content_length
                    .is_some_and(|length| length as usize > MAX_VSIX_BYTES)
                {
                    return Ok(None);
                }
                let manifest_bytes = read_capped_response(
                    manifest_response,
                    MAX_MANIFEST_BYTES,
                    "Open VSX returned an unexpectedly large manifest.",
                )
                .map_err(OpenVsxClientError::Message)?;
                let manifest = parse_jsonc_object(
                    std::str::from_utf8(&manifest_bytes).map_err(|_| {
                        OpenVsxClientError::Message(
                            "Extension manifest is not valid JSON.".to_string(),
                        )
                    })?,
                    "Extension manifest",
                )
                .map_err(OpenVsxClientError::Message)?;
                if theme_contributions(&manifest).is_empty()
                    || !manifest_license_matches(&manifest, &extension.license)
                {
                    return Ok(None);
                }
                Ok(Some(extension))
            })
        })
        .await;

        if let Ok(maybe_extension) = detail {
            fulfilled_count += 1;
            if let Some(extension) = maybe_extension {
                completed.push(extension);
            }
        }
    }

    if options.cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err("The operation was aborted.".to_string());
    }
    if !identities.is_empty() && fulfilled_count == 0 {
        return Err("Open VSX theme details are unavailable right now.".to_string());
    }
    Ok(completed.into_iter().take(8).collect())
}

fn line_comments() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|//[^\n]*"#).expect("line comments"))
}

fn block_comments() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|/\*(?s:.*?)\*/"#).expect("block comments"))
}

fn trailing_commas() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"("(?:[^"\\]|\\.)*")|,(\s*[}\]])"#).expect("trailing commas"))
}

fn strip_jsonc(input: &str) -> String {
    let stripped = line_comments()
        .replace_all(input, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    let stripped = block_comments()
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                String::new()
            }
        })
        .into_owned();
    trailing_commas()
        .replace_all(&stripped, |captures: &regex::Captures| {
            if captures.get(1).is_some() {
                captures[0].to_owned()
            } else {
                captures
                    .get(2)
                    .map(|group| group.as_str().to_owned())
                    .unwrap_or_default()
            }
        })
        .into_owned()
}

fn parse_jsonc_object(source: &str, description: &str) -> Result<Map<String, Value>, String> {
    let stripped = strip_jsonc(source);
    let value: Value =
        serde_json::from_str(&stripped).map_err(|_| format!("{description} is not valid JSON."))?;
    is_record(&value)
        .cloned()
        .ok_or_else(|| format!("{description} is not valid JSON."))
}

fn sanitize_theme_object(value: &Map<String, Value>) -> Map<String, Value> {
    let mut colors = Map::new();
    if let Some(object) = value.get("colors").and_then(|value| value.as_object()) {
        for (key, color) in object {
            if used_workbench_colors().contains(key.as_str())
                && color
                    .as_str()
                    .is_some_and(|color| color.len() <= MAX_COLOR_VALUE_LENGTH)
            {
                colors.insert(key.clone(), color.clone());
            }
        }
    }
    let mut sanitized = Map::new();
    if let Some(include) = value.get("include").and_then(|value| value.as_str()) {
        sanitized.insert("include".to_string(), Value::String(include.to_string()));
    }
    sanitized.insert("colors".to_string(), Value::Object(colors));
    sanitized
}

fn normalize_package_path(path: &str, relative_to: &str) -> Result<String, String> {
    if path.len() > MAX_PACKAGE_PATH_LENGTH
        || path.contains('\0')
        || path.starts_with('/')
        || Regex::new(r"^[a-zA-Z]:")
            .expect("drive letter")
            .is_match(path)
    {
        return Err("Theme path is not a safe relative package path.".to_string());
    }
    let normalized_input = path.replace('\\', "/");
    let base_segments: Vec<&str> = relative_to.split('/').collect();
    let mut segments: Vec<&str> = base_segments[..base_segments.len().saturating_sub(1)].to_vec();
    for segment in normalized_input.split('/') {
        if segment.is_empty() || segment == "." {
            continue;
        }
        if segment == ".." {
            if segments.len() <= 1 {
                return Err("Theme path escapes the extension package.".to_string());
            }
            segments.pop();
            continue;
        }
        segments.push(segment);
    }
    if segments.first().copied() != Some("extension") {
        segments.insert(0, "extension");
    }
    Ok(segments.join("/"))
}

fn contribution_type(ui_theme: Option<&str>) -> Option<String> {
    match ui_theme {
        Some("vs") => Some("light".to_string()),
        Some("vs-dark") => Some("dark".to_string()),
        Some("hc-black") => Some("hc-black".to_string()),
        Some("hc-light") => Some("hc-light".to_string()),
        _ => None,
    }
}

fn inspect_zip_directory(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() < 22 {
        return Err("That extension package has no ZIP directory.".to_string());
    }
    let minimum_offset = bytes.len().saturating_sub(65_557);
    let mut end_offset = bytes.len().saturating_sub(22);
    while end_offset >= minimum_offset {
        let signature =
            u32::from_le_bytes(bytes[end_offset..end_offset + 4].try_into().expect("sig"));
        let comment_length = u16::from_le_bytes(
            bytes[end_offset + 20..end_offset + 22]
                .try_into()
                .expect("comment"),
        ) as usize;
        if signature == 0x0605_4b50 && end_offset + 22 + comment_length == bytes.len() {
            break;
        }
        if end_offset == 0 {
            break;
        }
        end_offset -= 1;
    }
    if end_offset < minimum_offset {
        return Err("That extension package has no ZIP directory.".to_string());
    }
    let directory_size = u32::from_le_bytes(
        bytes[end_offset + 12..end_offset + 16]
            .try_into()
            .expect("dir size"),
    );
    let directory_offset = u32::from_le_bytes(
        bytes[end_offset + 16..end_offset + 20]
            .try_into()
            .expect("dir offset"),
    );
    let directory_end = directory_offset as usize + directory_size as usize;
    if directory_end != end_offset || directory_end > bytes.len() {
        return Err("That extension package has an invalid ZIP directory.".to_string());
    }
    let mut entry_count = 0usize;
    let mut total_uncompressed = 0u64;
    let mut offset = directory_offset as usize;
    while offset < directory_end {
        if offset + 46 > directory_end
            || u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("cd sig"))
                != 0x0201_4b50
        {
            return Err("That extension package has an invalid ZIP directory.".to_string());
        }
        entry_count += 1;
        if entry_count > MAX_ZIP_ENTRIES {
            return Err("That extension package has too many files.".to_string());
        }
        let compressed = u32::from_le_bytes(
            bytes[offset + 20..offset + 24]
                .try_into()
                .expect("compressed"),
        );
        let uncompressed = u32::from_le_bytes(
            bytes[offset + 24..offset + 28]
                .try_into()
                .expect("uncompressed"),
        );
        if compressed == 0xffff_ffff || uncompressed == 0xffff_ffff {
            return Err("That extension package has unsupported ZIP64 metadata.".to_string());
        }
        total_uncompressed += uncompressed as u64;
        if total_uncompressed > MAX_UNCOMPRESSED_BYTES {
            return Err("That extension package expands beyond the safe import limit.".to_string());
        }
        if uncompressed > 0
            && (compressed == 0
                || u64::from(uncompressed) / u64::from(compressed) > MAX_COMPRESSION_RATIO)
        {
            return Err("That extension package has an unsafe compression ratio.".to_string());
        }
        let name_length = u16::from_le_bytes(
            bytes[offset + 28..offset + 30]
                .try_into()
                .expect("name len"),
        ) as usize;
        let extra_length = u16::from_le_bytes(
            bytes[offset + 30..offset + 32]
                .try_into()
                .expect("extra len"),
        ) as usize;
        let comment_length = u16::from_le_bytes(
            bytes[offset + 32..offset + 34]
                .try_into()
                .expect("comment len"),
        ) as usize;
        offset += 46 + name_length + extra_length + comment_length;
    }
    if offset != directory_end {
        return Err("That extension package has an invalid ZIP directory.".to_string());
    }
    let comment_length = u16::from_le_bytes(
        bytes[end_offset + 20..end_offset + 22]
            .try_into()
            .expect("eocd comment"),
    ) as usize;
    if comment_length == 0 {
        return Ok(bytes.to_vec());
    }
    let mut without_comment = bytes.to_vec();
    without_comment.truncate(end_offset + 22);
    without_comment[end_offset + 20] = 0;
    without_comment[end_offset + 21] = 0;
    Ok(without_comment)
}

fn inspect_zip<R: Read + std::io::Seek>(archive: &mut ZipArchive<R>) -> Result<(), String> {
    if archive.len() > MAX_ZIP_ENTRIES {
        return Err("That extension package has too many files.".to_string());
    }
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|_| "That Open VSX extension package could not be opened.".to_string())?;
        let _ = normalize_package_path(file.name(), "extension/")?;
    }
    Ok(())
}

fn read_zip_text<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
    description: &str,
) -> Result<String, String> {
    let mut file: ZipFile<'_> = archive
        .by_name(path)
        .map_err(|_| format!("{description} is missing from the extension package."))?;
    let size = file.size();
    if size > MAX_THEME_BYTES as u64 {
        return Err(format!("{description} is too large."));
    }
    let mut buffer = Vec::with_capacity(size.min(MAX_THEME_BYTES as u64) as usize);
    file.read_to_end(&mut buffer)
        .map_err(|_| format!("{description} is too large."))?;
    if buffer.len() > MAX_THEME_BYTES {
        return Err(format!("{description} is too large."));
    }
    String::from_utf8(buffer).map_err(|_| format!("{description} is not valid JSON."))
}

fn load_theme_object<R: Read + std::io::Seek>(
    archive: &mut ZipArchive<R>,
    path: &str,
    cache: &mut HashMap<String, Map<String, Value>>,
    budget: &mut usize,
    ancestors: &HashSet<String>,
) -> Result<Map<String, Value>, String> {
    if ancestors.len() >= MAX_INCLUDE_DEPTH {
        return Err("Theme includes are nested too deeply.".to_string());
    }
    if ancestors.contains(path) {
        return Err("Theme includes contain a cycle.".to_string());
    }
    if let Some(cached) = cache.get(path) {
        return Ok(cached.clone());
    }
    *budget += 1;
    if *budget > MAX_RESOLVED_THEME_FILES {
        return Err("That extension references too many theme files.".to_string());
    }
    let mut value = sanitize_theme_object(&parse_jsonc_object(
        &read_zip_text(archive, path, path)?,
        path,
    )?);
    let include = value
        .get("include")
        .and_then(|value| value.as_str().map(str::to_string));
    if include.is_none() {
        cache.insert(path.to_string(), value.clone());
        return Ok(value);
    }
    let include_path = normalize_package_path(&include.expect("include checked"), path)?;
    let mut next_ancestors = ancestors.clone();
    next_ancestors.insert(path.to_string());
    let base = load_theme_object(archive, &include_path, cache, budget, &next_ancestors)?;
    let mut merged = base.clone();
    for (key, item) in value.iter() {
        if key == "colors" {
            continue;
        }
        merged.insert(key.clone(), item.clone());
    }
    let mut colors = base
        .get("colors")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    if let Some(overlay) = value.get("colors").and_then(|value| value.as_object()) {
        for (key, color) in overlay {
            colors.insert(key.clone(), color.clone());
        }
    }
    merged.insert("colors".to_string(), Value::Object(colors));
    value = merged;
    cache.insert(path.to_string(), value.clone());
    Ok(value)
}

async fn fetch_package<C: OpenVsxClient + ?Sized>(
    client: &C,
    url: &str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let response = client
        .get(url, cancel)
        .await
        .map_err(client_error_message)?;
    if response.status != 200 {
        return Err("That Open VSX theme could not be downloaded.".to_string());
    }
    read_capped_response(
        response,
        MAX_VSIX_BYTES,
        "That theme extension is too large to import safely.",
    )
}

pub async fn import_open_vsx_theme_extension<C: OpenVsxClient + ?Sized>(
    client: &C,
    extension: &OpenVsxThemeExtension,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<ThemeDefinition>, String> {
    let cancel_token = cancel.cloned().unwrap_or_default();
    if cancel_token.is_cancelled() {
        return Err("The operation was aborted.".to_string());
    }

    let manifest_response = client
        .get(&extension.manifest_url, &cancel_token)
        .await
        .map_err(client_error_message)?;
    if manifest_response.status != 200 {
        return Err("That Open VSX extension has no readable manifest.".to_string());
    }
    let manifest_bytes = read_capped_response(
        manifest_response,
        MAX_MANIFEST_BYTES,
        "That Open VSX extension manifest is too large.",
    )?;
    let manifest = parse_jsonc_object(
        std::str::from_utf8(&manifest_bytes)
            .map_err(|_| "Extension manifest is not valid JSON.".to_string())?,
        "Extension manifest",
    )?;
    let advertised = theme_contributions(&manifest);
    if advertised.is_empty() {
        return Err("That extension does not contain color themes.".to_string());
    }
    if advertised.len() > MAX_THEMES_PER_EXTENSION {
        return Err("That extension contains too many color themes to import safely.".to_string());
    }

    let package_bytes = fetch_package(client, &extension.vsix_url, &cancel_token).await?;
    if cancel_token.is_cancelled() {
        return Err("The operation was aborted.".to_string());
    }
    let checksum_response = client
        .get(&extension.sha256_url, &cancel_token)
        .await
        .map_err(client_error_message)?;
    if checksum_response.status != 200 {
        return Err("That Open VSX theme has no readable checksum.".to_string());
    }
    let checksum_text = std::str::from_utf8(&read_capped_response(
        checksum_response,
        256,
        "That Open VSX checksum response is invalid.",
    )?)
    .map_err(|_| "That Open VSX checksum response is invalid.".to_string())?
    .trim()
    .to_string();
    let expected_checksum = checksum_text
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string();
    static CHECKSUM: OnceLock<Regex> = OnceLock::new();
    let checksum_re = CHECKSUM.get_or_init(|| Regex::new(r"^[a-f\d]{64}$").expect("checksum"));
    if expected_checksum.is_empty() || !checksum_re.is_match(&expected_checksum) {
        return Err("That Open VSX theme has an invalid checksum.".to_string());
    }
    if cancel_token.is_cancelled() {
        return Err("The operation was aborted.".to_string());
    }
    let actual_checksum = Sha256::digest(&package_bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if !actual_checksum.eq_ignore_ascii_case(&expected_checksum) {
        return Err("That Open VSX theme failed its integrity check.".to_string());
    }
    if cancel_token.is_cancelled() {
        return Err("The operation was aborted.".to_string());
    }

    let inspected = inspect_zip_directory(&package_bytes).map_err(|error| {
        if error.starts_with("That extension package") {
            error
        } else {
            "That Open VSX extension package could not be opened.".to_string()
        }
    })?;
    let cursor = Cursor::new(inspected);
    let mut archive = ZipArchive::new(cursor)
        .map_err(|_| "That Open VSX extension package could not be opened.".to_string())?;
    inspect_zip(&mut archive)?;

    let packaged_manifest = parse_jsonc_object(
        &read_zip_text(&mut archive, "extension/package.json", "Extension manifest")?,
        "Extension manifest",
    )?;
    let packaged_publisher = packaged_manifest
        .get("publisher")
        .and_then(|value| value.as_str());
    let packaged_name = packaged_manifest
        .get("name")
        .and_then(|value| value.as_str());
    let packaged_id = packaged_publisher
        .zip(packaged_name)
        .map(|(publisher, name)| format!("{publisher}.{name}"));
    if packaged_publisher
        .is_none_or(|publisher| !publisher.eq_ignore_ascii_case(&extension.publisher))
        || packaged_id
            .as_ref()
            .is_none_or(|id| !id.eq_ignore_ascii_case(&extension.id))
        || packaged_manifest
            .get("version")
            .and_then(|value| value.as_str())
            != Some(extension.version.as_str())
    {
        return Err(
            "That extension package does not match the selected Open VSX theme.".to_string(),
        );
    }
    if !manifest_license_matches(&packaged_manifest, &extension.license) {
        return Err("That extension package does not match its advertised license.".to_string());
    }
    let contributions = theme_contributions(&packaged_manifest);
    if contributions.is_empty() {
        return Err("That extension does not contain color themes.".to_string());
    }
    if contributions.len() > MAX_THEMES_PER_EXTENSION {
        return Err("That extension contains too many color themes to import safely.".to_string());
    }

    struct ParsedTheme {
        theme: ThemeDefinition,
        source_name: String,
        source_path: String,
    }

    let mut parsed = Vec::new();
    let mut failures = Vec::new();
    let mut theme_cache = HashMap::new();
    let mut theme_budget = 0usize;
    for contribution in contributions {
        if cancel_token.is_cancelled() {
            return Err("The operation was aborted.".to_string());
        }
        let Some(path_raw) = contribution.path else {
            failures.push("theme path is missing".to_string());
            continue;
        };
        let result: Result<ParsedTheme, String> = (|| {
            let path = normalize_package_path(&path_raw, "extension/")?;
            let theme_value = load_theme_object(
                &mut archive,
                &path,
                &mut theme_cache,
                &mut theme_budget,
                &HashSet::new(),
            )?;
            let mut decorated = theme_value.clone();
            let label = contribution
                .label
                .as_ref()
                .map(|label| label.trim().to_string())
                .filter(|label| !label.is_empty())
                .unwrap_or_else(|| extension.name.clone());
            decorated.insert("displayName".to_string(), Value::String(label));
            if let Some(theme_type) = contribution_type(contribution.ui_theme.as_deref()) {
                decorated.insert("type".to_string(), Value::String(theme_type));
            }
            let decorated_value = Value::Object(decorated);
            if !is_vscode_theme_file(&decorated_value) {
                return Err("not a VS Code color theme".to_string());
            }
            let theme = parse_vscode_theme_file(decorated_value)?;
            let source_name = path.split('/').next_back().unwrap_or(&path).to_string();
            Ok(ParsedTheme {
                theme,
                source_name,
                source_path: path,
            })
        })();
        match result {
            Ok(entry) => parsed.push(entry),
            Err(message) => {
                if cancel_token.is_cancelled() {
                    return Err("The operation was aborted.".to_string());
                }
                failures.push(message);
            }
        }
    }
    if !failures.is_empty() {
        return Err(
            "One or more color themes in that extension could not be imported safely.".to_string(),
        );
    }
    if parsed.is_empty() {
        return Err("That extension has no compatible color themes.".to_string());
    }

    let extension_id = extension.id.to_ascii_lowercase();
    let mut source_path_counts = HashMap::<String, usize>::new();
    for entry in &parsed {
        *source_path_counts
            .entry(entry.source_path.clone())
            .or_default() += 1;
    }
    let mut source_path_occurrences = HashMap::<String, usize>::new();
    let source_identities: Vec<String> = parsed
        .iter()
        .map(|entry| {
            if source_path_counts
                .get(&entry.source_path)
                .copied()
                .unwrap_or(0)
                == 1
            {
                return entry.source_path.clone();
            }
            let occurrence = source_path_occurrences
                .entry(entry.source_path.clone())
                .or_insert(0);
            let identity = if *occurrence == 0 {
                entry.source_path.clone()
            } else {
                format!("{}\0{occurrence}", entry.source_path)
            };
            *occurrence += 1;
            identity
        })
        .collect();

    let resolved = resolve_theme_label_collisions(
        &parsed
            .iter()
            .map(|entry| ThemeImportEntry {
                theme: entry.theme.clone(),
                source_name: Some(entry.source_name.clone()),
            })
            .collect::<Vec<_>>(),
    )
    .into_iter()
    .enumerate()
    .map(|(index, theme)| {
        let mut theme = theme;
        theme.id = SharedString::from(open_vsx_theme_id(&extension_id, &source_identities[index]));
        theme
    })
    .collect::<Vec<_>>();

    let extension_id_for_pair = extension_id.clone();
    let paired_id = move |light: &ThemeDefinition, dark: &ThemeDefinition| -> String {
        let mut ids = [light.id.to_string(), dark.id.to_string()];
        ids.sort();
        open_vsx_theme_id(&extension_id_for_pair, &ids.join(":"))
    };
    let paired = pair_vscode_themes(&resolved, Some(&paired_id));

    let themes = resolve_theme_label_collisions(
        &paired
            .iter()
            .map(|theme| ThemeImportEntry {
                theme: theme.clone(),
                source_name: None,
            })
            .collect::<Vec<_>>(),
    );

    let collection = ThemeCollection {
        id: SharedString::from(extension.collection_id.as_str()),
        label: SharedString::from(extension.name.chars().take(48).collect::<String>()),
    };
    Ok(themes
        .into_iter()
        .map(|mut theme| {
            theme.collection = Some(collection.clone());
            theme
        })
        .collect())
}

pub async fn search_open_vsx_themes_live(
    query: &str,
    options: OpenVsxThemeSearchOptions<'_>,
) -> Result<Vec<OpenVsxThemeExtension>, String> {
    let client = ReqwestOpenVsxClient::new();
    search_open_vsx_themes(&client, query, options).await
}

pub async fn import_open_vsx_theme_extension_live(
    extension: &OpenVsxThemeExtension,
    cancel: Option<&CancellationToken>,
) -> Result<Vec<ThemeDefinition>, String> {
    let client = ReqwestOpenVsxClient::new();
    import_open_vsx_theme_extension(&client, extension, cancel).await
}

#[cfg(test)]
mod tests;

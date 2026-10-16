use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use super::super::open_vsx::{
    FakeOpenVsxClient, OpenVsxClient, OpenVsxClientError, OpenVsxHeadResponse, OpenVsxHttpResponse,
};
use super::super::theme_palette::get_theme_colors_for_mode;
use super::super::theme_palette_library::theme_color_to_hex;
use super::super::theme_palettes::ThemeAppearance;
use super::*;

const ASSET_ROOT: &str = "https://open-vsx.org/api/demo/theme/1.0.0/file";

fn json_response(value: serde_json::Value) -> OpenVsxHttpResponse {
    let body = serde_json::to_vec(&value).expect("json");
    OpenVsxHttpResponse {
        status: 200,
        body,
        content_length: None,
    }
}

fn extension_detail(overrides: serde_json::Value) -> serde_json::Value {
    let mut value = serde_json::json!({
        "namespace": "demo",
        "name": "theme",
        "displayName": "Demo Theme",
        "description": "A nice theme",
        "version": "1.0.0",
        "downloadCount": 123456,
        "license": "MIT",
        "repository": "https://github.com/demo/theme",
        "files": {
            "icon": format!("{ASSET_ROOT}/icon.png"),
            "manifest": format!("{ASSET_ROOT}/package.json"),
            "sha256": format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
            "download": format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
        }
    });
    if let Some(object) = overrides.as_object() {
        for (key, item) in object {
            value[key] = item.clone();
        }
    }
    value
}

fn search_url(query: &str, sort: OpenVsxThemeSort) -> String {
    let mut url = url::Url::parse(OPEN_VSX_SEARCH_URL).expect("search url");
    {
        let mut pairs = url.query_pairs_mut();
        pairs.append_pair("query", query);
        pairs.append_pair("category", "Themes");
        pairs.append_pair("sortBy", sort.as_str());
        pairs.append_pair("sortOrder", "desc");
        pairs.append_pair("size", "16");
    }
    url.to_string()
}

fn search_fixture_client() -> FakeOpenVsxClient {
    let mut client = FakeOpenVsxClient::new();
    client = client.register_get(
        &search_url("dracula", OpenVsxThemeSort::DownloadCount),
        json_response(serde_json::json!({
            "extensions": [
                { "namespace": "demo", "name": "theme" },
                { "namespace": "icons", "name": "theme" },
                { "namespace": "oversized", "name": "theme" },
                { "namespace": "unlicensed", "name": "theme" },
                { "namespace": "unavailable", "name": "theme" },
                { "namespace": "closed", "name": "theme" },
                { "namespace": "huge", "name": "theme" },
            ]
        })),
    );
    client = client.register_get(
        "https://open-vsx.org/api/icons/theme",
        json_response(extension_detail(serde_json::json!({
            "namespace": "icons",
            "files": {
                "icon": format!("{ASSET_ROOT}/icon.png"),
                "manifest": "https://open-vsx.org/api/icons/theme/1.0.0/file/package.json",
                "sha256": format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
                "download": format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
            }
        }))),
    );
    client = client.register_get(
        "https://open-vsx.org/api/oversized/theme",
        json_response(extension_detail(serde_json::json!({
            "namespace": "oversized",
            "files": {
                "icon": format!("{ASSET_ROOT}/icon.png"),
                "manifest": format!("{ASSET_ROOT}/package.json"),
                "sha256": format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
                "download": "https://open-vsx.org/api/oversized/theme/1.0.0/file/oversized.vsix",
            }
        }))),
    );
    client = client.register_get(
        "https://open-vsx.org/api/unlicensed/theme",
        json_response(extension_detail(serde_json::json!({
            "namespace": "unlicensed",
            "files": {
                "icon": format!("{ASSET_ROOT}/icon.png"),
                "manifest": "https://open-vsx.org/api/unlicensed/theme/1.0.0/file/package.json",
                "sha256": format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
                "download": format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
            }
        }))),
    );
    client = client.register_get(
        "https://open-vsx.org/api/unavailable/theme",
        json_response(extension_detail(serde_json::json!({
            "namespace": "unavailable",
            "files": {
                "icon": format!("{ASSET_ROOT}/icon.png"),
                "manifest": "https://open-vsx.org/api/unavailable/theme/1.0.0/file/package.json",
                "sha256": format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
                "download": "https://open-vsx.org/api/unavailable/theme/1.0.0/file/unavailable.vsix",
            }
        }))),
    );
    client = client.register_get(
        "https://open-vsx.org/api/closed/theme",
        json_response(extension_detail(serde_json::json!({
            "namespace": "closed",
            "license": "All Rights Reserved",
        }))),
    );
    client = client.register_get(
        "https://open-vsx.org/api/huge/theme",
        json_response(serde_json::json!({ "padding": "x".repeat(256 * 1024) })),
    );
    client = client.register_get(
        "https://open-vsx.org/api/demo/theme",
        json_response(extension_detail(serde_json::json!({}))),
    );
    client = client.register_head(
        "https://open-vsx.org/api/oversized/theme/1.0.0/file/oversized.vsix",
        OpenVsxHeadResponse {
            status: 200,
            content_length: Some(20 * 1024 * 1024 + 1),
        },
    );
    client = client.register_head(
        "https://open-vsx.org/api/unavailable/theme/1.0.0/file/unavailable.vsix",
        OpenVsxHeadResponse {
            status: 404,
            content_length: None,
        },
    );
    client = client.register_head(
        &format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
        OpenVsxHeadResponse {
            status: 200,
            content_length: Some(1024),
        },
    );
    client = client.register_get(
        "https://open-vsx.org/api/icons/theme/1.0.0/file/package.json",
        json_response(serde_json::json!({ "contributes": { "iconThemes": [{}] } })),
    );
    client = client.register_get(
        "https://open-vsx.org/api/unlicensed/theme/1.0.0/file/package.json",
        json_response(serde_json::json!({
            "contributes": { "themes": [{ "path": "./theme.json" }] }
        })),
    );
    client = client.register_get(
        "https://open-vsx.org/api/unavailable/theme/1.0.0/file/package.json",
        json_response(serde_json::json!({
            "license": "MIT",
            "contributes": { "themes": [{ "path": "./theme.json" }] }
        })),
    );
    client.register_get(
        &format!("{ASSET_ROOT}/package.json"),
        json_response(serde_json::json!({
            "license": "MIT",
            "contributes": { "themes": [{ "path": "./theme.json" }] }
        })),
    )
}

struct AbortOnChecksumClient {
    inner: FakeOpenVsxClient,
    sha256_url: String,
    cancel: CancellationToken,
}

#[async_trait::async_trait]
impl OpenVsxClient for AbortOnChecksumClient {
    async fn get(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<OpenVsxHttpResponse, OpenVsxClientError> {
        if url == self.sha256_url {
            self.cancel.cancel();
        }
        self.inner.get(url, cancel).await
    }

    async fn head(
        &self,
        url: &str,
        cancel: &CancellationToken,
    ) -> Result<OpenVsxHeadResponse, OpenVsxClientError> {
        self.inner.head(url, cancel).await
    }
}

struct PackageState {
    bytes: Vec<u8>,
    checksum: String,
}

fn build_demo_vsix(manifest: &serde_json::Value) -> Vec<u8> {
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
    let mut zip = ZipWriter::new(std::io::Cursor::new(Vec::new()));
    zip.start_file("extension/.gitkeep", options)
        .expect("gitkeep");
    zip.write_all(b"").expect("gitkeep write");
    for index in 0..3_000 {
        zip.start_file(
            format!("extension/node_modules/package-{index}.js"),
            options,
        )
        .expect("node file");
        zip.write_all(b"").expect("node write");
    }
    zip.start_file("extension/themes/base.jsonc", options)
        .expect("base");
    zip.write_all(
        br##"{
        // inherited workbench colors
        "colors": {
          "editor.foreground": "#eeeeee",
          "focusBorder": "#8b5cf6",
          "editor.findMatchBackground": "#663399",
          "editor.findMatchForeground": "#ffffff",
          "editor.findMatchHighlightBackground": "#445566",
          "editor.findMatchHighlightForeground": "#ffffff",
        },
      }"##,
    )
    .expect("base write");
    zip.start_file("extension/themes/demo-dark.json", options)
        .expect("dark");
    zip.write_all(
        br##"{
        "include": "./base.jsonc",
        "colors": {
          "editor.background": "#111111",
          "editor.findMatchHighlightBackground": "#ffffff20"
        }
      }"##,
    )
    .expect("dark write");
    zip.start_file("extension/themes/demo-light.json", options)
        .expect("light");
    zip.write_all(
        br##"{
        "colors": {
          "editor.background": "#fafafa",
          "editor.foreground": "#222222",
          "focusBorder": "#8b5cf6",
          "editor.findMatchBackground": "#335577",
          "editor.findMatchForeground": "#ffffff",
          "editor.findMatchHighlightBackground": "#bbddff",
          "editor.findMatchHighlightForeground": "#112233"
        }
      }"##,
    )
    .expect("light write");
    zip.start_file("extension/themes/demo.json", options)
        .expect("demo");
    zip.write_all(
        br##"{
        "colors": {
          "editor.background": "#181818",
          "editor.foreground": "#eeeeee"
        }
      }"##,
    )
    .expect("demo write");
    zip.start_file("extension/package.json", options)
        .expect("manifest");
    zip.write_all(
        serde_json::to_vec(manifest)
            .expect("manifest json")
            .as_slice(),
    )
    .expect("manifest write");
    zip.set_comment(format!("PK\u{05}\u{06}{}", "x".repeat(26)));
    zip.finish().expect("finish").into_inner()
}

fn checksum_for(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn import_extension() -> OpenVsxThemeExtension {
    OpenVsxThemeExtension {
        collection_id: "open-vsx:demo.theme".to_string(),
        id: "demo.theme".to_string(),
        name: "Demo Theme".to_string(),
        publisher: "demo".to_string(),
        description: String::new(),
        download_count: 1,
        icon_url: None,
        source_url: None,
        license: "MIT".to_string(),
        manifest_url: format!("{ASSET_ROOT}/package.json"),
        sha256_url: format!("{ASSET_ROOT}/demo.theme-1.0.0.sha256"),
        version: "1.0.0".to_string(),
        vsix_url: format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
    }
}

fn import_client(state: Arc<Mutex<PackageState>>) -> FakeOpenVsxClient {
    let extension = import_extension();
    let wrong_manifest = serde_json::json!({
        "contributes": { "themes": [{ "label": "Wrong", "path": "./themes/missing.json" }] }
    });
    FakeOpenVsxClient::new()
        .register_get(&extension.manifest_url, json_response(wrong_manifest))
        .register_get(
            &extension.sha256_url,
            OpenVsxHttpResponse {
                status: 200,
                body: {
                    let checksum = state.lock().expect("state").checksum.clone();
                    format!("{checksum}  demo.vsix").into_bytes()
                },
                content_length: None,
            },
        )
        .register_get(
            &extension.vsix_url,
            OpenVsxHttpResponse {
                status: 200,
                body: state.lock().expect("state").bytes.clone(),
                content_length: None,
            },
        )
}

#[tokio::test]
async fn searches_theme_extensions_and_keeps_only_supported_licenses() {
    let client = search_fixture_client();
    let results =
        search_open_vsx_themes(&client, "  dracula  ", OpenVsxThemeSearchOptions::default())
            .await
            .expect("search");
    assert_eq!(results.len(), 1);
    let theme = &results[0];
    assert_eq!(theme.id, "demo.theme");
    assert_eq!(theme.name, "Demo Theme");
    assert_eq!(theme.publisher, "demo");
    assert_eq!(theme.download_count, 123456);
    assert_eq!(
        theme.icon_url.as_deref(),
        Some(format!("{ASSET_ROOT}/icon.png").as_str())
    );
    assert_eq!(
        theme.source_url.as_deref(),
        Some("https://github.com/demo/theme")
    );
    assert_eq!(theme.license, "MIT");
}

#[tokio::test]
async fn supports_sorting_theme_searches() {
    let client = FakeOpenVsxClient::new().register_get(
        &search_url("nord", OpenVsxThemeSort::Rating),
        json_response(serde_json::json!({ "extensions": [] })),
    );
    search_open_vsx_themes(
        &client,
        "nord",
        OpenVsxThemeSearchOptions {
            cancel: None,
            sort_by: OpenVsxThemeSort::Rating,
        },
    )
    .await
    .expect("search");
}

#[tokio::test]
async fn rejects_malformed_search_responses() {
    let client = FakeOpenVsxClient::new().register_get(
        &search_url("nord", OpenVsxThemeSort::DownloadCount),
        json_response(serde_json::json!({ "error": "unavailable" })),
    );
    let error = search_open_vsx_themes(&client, "nord", OpenVsxThemeSearchOptions::default())
        .await
        .expect_err("malformed");
    assert!(error.contains("unreadable search response"));
}

#[tokio::test]
async fn reports_unavailable_search_when_every_detail_request_fails() {
    let client = FakeOpenVsxClient::new()
        .register_get(
            &search_url("dracula", OpenVsxThemeSort::DownloadCount),
            json_response(
                serde_json::json!({ "extensions": [{ "namespace": "demo", "name": "theme" }] }),
            ),
        )
        .register_get(
            "https://open-vsx.org/api/demo/theme",
            OpenVsxHttpResponse {
                status: 429,
                body: Vec::new(),
                content_length: None,
            },
        );
    let error = search_open_vsx_themes(&client, "dracula", OpenVsxThemeSearchOptions::default())
        .await
        .expect_err("unavailable");
    assert!(error.contains("Open VSX theme details are unavailable"));
}

#[tokio::test]
async fn reports_unavailable_search_when_every_detail_response_is_malformed() {
    let client = FakeOpenVsxClient::new()
        .register_get(
            &search_url("dracula", OpenVsxThemeSort::DownloadCount),
            json_response(
                serde_json::json!({ "extensions": [{ "namespace": "demo", "name": "theme" }] }),
            ),
        )
        .register_get(
            "https://open-vsx.org/api/demo/theme",
            json_response(serde_json::json!({})),
        );
    let error = search_open_vsx_themes(&client, "dracula", OpenVsxThemeSearchOptions::default())
        .await
        .expect_err("malformed detail");
    assert!(error.contains("Open VSX theme details are unavailable"));
}

#[tokio::test]
async fn returns_no_results_when_every_theme_package_is_unavailable() {
    let client = FakeOpenVsxClient::new()
        .register_get(
            &search_url("dracula", OpenVsxThemeSort::DownloadCount),
            json_response(
                serde_json::json!({ "extensions": [{ "namespace": "demo", "name": "theme" }] }),
            ),
        )
        .register_get(
            "https://open-vsx.org/api/demo/theme",
            json_response(extension_detail(serde_json::json!({}))),
        )
        .register_head(
            &format!("{ASSET_ROOT}/demo.theme-1.0.0.vsix"),
            OpenVsxHeadResponse {
                status: 404,
                content_length: None,
            },
        )
        .register_get(
            &format!("{ASSET_ROOT}/package.json"),
            json_response(serde_json::json!({
                "license": "MIT",
                "contributes": { "themes": [{ "path": "./theme.json" }] }
            })),
        );
    let results = search_open_vsx_themes(&client, "dracula", OpenVsxThemeSearchOptions::default())
        .await
        .expect("empty");
    assert!(results.is_empty());
}

#[tokio::test(start_paused = true)]
async fn times_out_a_stalled_search_request() {
    let client = FakeOpenVsxClient::new()
        .register_hang_on_get(&search_url("dracula", OpenVsxThemeSort::DownloadCount));
    let search = tokio::spawn(async move {
        search_open_vsx_themes(&client, "dracula", OpenVsxThemeSearchOptions::default()).await
    });
    tokio::time::advance(Duration::from_millis(10_001)).await;
    let error = search.await.expect("join").expect_err("timeout");
    assert!(error.contains("Open VSX took too long to respond"));
}

#[tokio::test]
async fn downloads_verified_vsix_reads_jsonc_includes_and_pairs_variants() {
    let mut manifest = serde_json::json!({
        "publisher": "demo",
        "name": "theme",
        "version": "1.0.0",
        "license": "MIT",
        "contributes": {
            "themes": [
                { "label": "Demo Dark", "uiTheme": "vs-dark", "path": "./themes/demo-dark.json" },
                { "label": "Demo Light", "uiTheme": "vs", "path": "./themes/demo-light.json" },
                { "label": "Demo", "uiTheme": "vs-dark", "path": "./themes/demo.json" },
            ]
        }
    });
    let state = Arc::new(Mutex::new(PackageState {
        bytes: Vec::new(),
        checksum: String::new(),
    }));
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let client = import_client(state.clone());
    let extension = import_extension();
    let themes = import_open_vsx_theme_extension(&client, &extension, None)
        .await
        .expect("import");
    assert_eq!(themes.len(), 2);
    assert_eq!(
        themes
            .iter()
            .map(|theme| theme.id.to_string())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        2
    );
    static ID_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let id_re =
        ID_RE.get_or_init(|| regex::Regex::new(r"^ovx-theme-[0-9a-f]{12}$").expect("id re"));
    assert!(themes.iter().all(|theme| id_re.is_match(theme.id.as_ref())));
    assert!(themes.iter().all(|theme| {
        theme
            .collection
            .as_ref()
            .is_some_and(|collection| collection.id.as_ref() == "open-vsx:demo.theme")
    }));
    let paired = themes
        .iter()
        .find(|theme| {
            get_theme_colors_for_mode(theme, ThemeAppearance::Light).is_some()
                && get_theme_colors_for_mode(theme, ThemeAppearance::Dark).is_some()
        })
        .expect("paired");
    assert_eq!(paired.label.as_ref(), "Demo");
    assert_eq!(
        theme_color_to_hex(&paired.colors.canvas).as_deref(),
        Some("#fafafa")
    );
    let dark = get_theme_colors_for_mode(paired, ThemeAppearance::Dark).expect("dark");
    assert_eq!(theme_color_to_hex(&dark.canvas).as_deref(), Some("#111111"));
    assert_eq!(theme_color_to_hex(&dark.text).as_deref(), Some("#eeeeee"));
    assert_eq!(
        theme_color_to_hex(&dark.search_match_active_background).as_deref(),
        Some("#663399")
    );
    assert_eq!(
        theme_color_to_hex(&dark.search_match_active_foreground).as_deref(),
        Some("#ffffff")
    );
    assert_eq!(
        theme_color_to_hex(&dark.search_match_background).as_deref(),
        Some("#2f2f2f")
    );
    assert_eq!(
        theme_color_to_hex(&dark.search_match_foreground).as_deref(),
        Some("#ffffff")
    );
    assert_eq!(
        theme_color_to_hex(&paired.colors.search_match_active_background).as_deref(),
        Some("#335577")
    );
    assert_eq!(
        theme_color_to_hex(&paired.colors.search_match_active_foreground).as_deref(),
        Some("#ffffff")
    );
    assert_eq!(
        theme_color_to_hex(&paired.colors.search_match_background).as_deref(),
        Some("#bbddff")
    );
    assert_eq!(
        theme_color_to_hex(&paired.colors.search_match_foreground).as_deref(),
        Some("#112233")
    );

    let original_ids: Vec<String> = themes.iter().map(|theme| theme.id.to_string()).collect();
    manifest["contributes"]["themes"][0]["label"] = "Renamed Dark".into();
    manifest["contributes"]["themes"][1]["label"] = "Renamed Light".into();
    manifest["contributes"]["themes"][2]["label"] = "Renamed Solo".into();
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let renamed = import_open_vsx_theme_extension(&import_client(state.clone()), &extension, None)
        .await
        .expect("renamed");
    assert_eq!(
        renamed
            .iter()
            .map(|theme| theme.id.to_string())
            .collect::<Vec<_>>(),
        original_ids
    );

    manifest["contributes"]["themes"]
        .as_array_mut()
        .expect("themes")
        .push(serde_json::json!({
            "label": "Duplicate Demo",
            "uiTheme": "vs-dark",
            "path": "./themes/demo.json",
        }));
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let duplicate =
        import_open_vsx_theme_extension(&import_client(state.clone()), &extension, None)
            .await
            .expect("duplicate");
    assert_eq!(
        duplicate
            .iter()
            .map(|theme| theme.id.to_string())
            .collect::<std::collections::HashSet<_>>()
            .len(),
        duplicate.len()
    );
    assert!(
        original_ids
            .iter()
            .all(|id| duplicate.iter().any(|theme| theme.id.as_ref() == id))
    );
    manifest["contributes"]["themes"]
        .as_array_mut()
        .expect("themes")
        .pop();

    manifest["contributes"]["themes"][2]["path"] = "./themes/missing.json".into();
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let error = import_open_vsx_theme_extension(&import_client(state.clone()), &extension, None)
        .await
        .expect_err("missing theme");
    assert!(error.contains("could not be imported safely"));
    manifest["contributes"]["themes"][2]["path"] = "./themes/demo.json".into();

    manifest["license"] = "Proprietary".into();
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let error = import_open_vsx_theme_extension(&import_client(state.clone()), &extension, None)
        .await
        .expect_err("license mismatch");
    assert!(error.contains("does not match its advertised license"));

    manifest
        .as_object_mut()
        .expect("manifest")
        .remove("license");
    {
        let bytes = build_demo_vsix(&manifest);
        let mut locked = state.lock().expect("state");
        locked.bytes = bytes;
        locked.checksum = checksum_for(&locked.bytes);
    }
    let error = import_open_vsx_theme_extension(&import_client(state), &extension, None)
        .await
        .expect_err("missing license");
    assert!(error.contains("does not match its advertised license"));
}

#[tokio::test]
async fn stops_import_work_when_the_request_is_cancelled() {
    let extension = import_extension();
    let cancel = CancellationToken::new();
    let client = AbortOnChecksumClient {
        inner: FakeOpenVsxClient::new()
            .register_get(
                &extension.manifest_url,
                json_response(serde_json::json!({
                    "contributes": { "themes": [{ "path": "./theme.json" }] }
                })),
            )
            .register_get(
                &extension.sha256_url,
                OpenVsxHttpResponse {
                    status: 200,
                    body: vec![b'0'; 64],
                    content_length: None,
                },
            )
            .register_get(
                &extension.vsix_url,
                OpenVsxHttpResponse {
                    status: 200,
                    body: vec![1, 2, 3],
                    content_length: None,
                },
            ),
        sha256_url: extension.sha256_url.clone(),
        cancel: cancel.clone(),
    };
    let error = import_open_vsx_theme_extension(&client, &extension, Some(&cancel))
        .await
        .expect_err("aborted");
    assert!(error.contains("aborted"));
}

#[tokio::test]
async fn rejects_package_whose_checksum_does_not_match() {
    let extension = import_extension();
    let client = FakeOpenVsxClient::new()
        .register_get(
            &extension.manifest_url,
            json_response(serde_json::json!({
                "contributes": { "themes": [{ "path": "./theme.json" }] }
            })),
        )
        .register_get(
            &extension.sha256_url,
            OpenVsxHttpResponse {
                status: 200,
                body: b"0".repeat(64).to_vec(),
                content_length: None,
            },
        )
        .register_get(
            &extension.vsix_url,
            OpenVsxHttpResponse {
                status: 200,
                body: vec![1, 2, 3],
                content_length: None,
            },
        );
    let error = import_open_vsx_theme_extension(&client, &extension, None)
        .await
        .expect_err("integrity");
    assert!(error.contains("integrity check"));
}

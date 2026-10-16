//! Port of upstream/apps/web/src/markdown-links.ts (+ test) and the
//! `isTerminalLinkActivation` modifier check it uses (terminal-links.ts).
//!
//! Turns a markdown link destination or an inline code span into the
//! [`MarkdownFileLinkMeta`] ChatMarkdown draws a file chip from. The path
//! classification itself is `turin_shared::markdown_links`.
// The meta is assembled ahead of the chip views that read it.
#![allow(dead_code)]

use std::sync::LazyLock;

use regex::Regex;
use turin_shared::file_links::{parse_file_url_href, split_file_path_position};
use turin_shared::keybindings::is_mac_platform;
use turin_shared::markdown_links::{
    inline_code_file_path_candidate, is_relative_file_path, normalize_markdown_link_destination,
    resolve_markdown_file_link_target,
};
use turin_shared::path::{file_basename, workspace_relative_file_path};

use crate::components::chat::timeline::file_path_display::format_workspace_relative_path;

static PDF_EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.pdf$").expect("static regex"));
static HOME_OR_DOT_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:~|\.{1,2})/").expect("static regex"));

/// `MarkdownFileLinkMeta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFileLinkMeta {
    pub file_path: String,
    pub target_path: String,
    pub display_path: String,
    pub workspace_relative_path: Option<String>,
    pub basename: String,
    pub line: Option<u64>,
    pub column: Option<u64>,
}

/// `isTerminalLinkActivation`: command-click on macOS, control-click
/// elsewhere. An empty platform (no `navigator`) never activates.
pub fn is_terminal_link_activation(meta_key: bool, ctrl_key: bool, platform: &str) -> bool {
    if platform.is_empty() {
        return false;
    }
    if is_mac_platform(platform) {
        meta_key && !ctrl_key
    } else {
        ctrl_key && !meta_key
    }
}

pub fn should_open_markdown_file_link_in_editor(
    meta_key: bool,
    ctrl_key: bool,
    platform: &str,
) -> bool {
    is_terminal_link_activation(meta_key, ctrl_key, platform)
}

pub fn should_open_markdown_file_link_in_browser_by_default(path: &str) -> bool {
    let end = path.find(['?', '#']).unwrap_or(path.len());
    PDF_EXTENSION.is_match(&path[..end])
}

pub fn rewrite_markdown_file_uri_href(href: Option<&str>) -> Option<String> {
    let href = href.filter(|href| !href.is_empty())?;
    let (path, hash) = parse_file_url_href(normalize_markdown_link_destination(href))?;
    Some(format!("{path}{hash}"))
}

/// Inline code spans mostly hold identifiers, commands, and refs (`node.meta`,
/// `origin/main`) rather than deliberate link destinations, so auto-linking
/// them demands stronger path evidence than an explicit markdown link does:
/// an unambiguous path prefix, a file extension, or a :line suffix.
///
/// `base_dir` of `None` is upstream's default of `cwd`.
pub fn resolve_inline_code_file_link_meta(
    code_text: &str,
    cwd: Option<&str>,
    base_dir: Option<&str>,
) -> Option<MarkdownFileLinkMeta> {
    let candidate = inline_code_file_path_candidate(code_text)?;
    let base_dir = base_dir.or(cwd);

    let base = if inline_code_path_names_from_workspace_root(&candidate, cwd, base_dir) {
        cwd
    } else {
        base_dir
    };
    resolve_markdown_file_link_meta(Some(&candidate), cwd, base)
}

/// Prose in a workspace file names other files from the repo root (`docs/ai/design.md`),
/// unlike an explicit link. Single-segment names (`design.md:12`) and `./`, `../`
/// paths still read as siblings, and files outside the workspace keep their own base.
fn inline_code_path_names_from_workspace_root(
    candidate: &str,
    cwd: Option<&str>,
    base_dir: Option<&str>,
) -> bool {
    let (Some(cwd), Some(base_dir)) = (
        cwd.filter(|cwd| !cwd.is_empty()),
        base_dir.filter(|dir| !dir.is_empty()),
    ) else {
        return false;
    };
    if !is_relative_file_path(candidate) {
        return false;
    }
    if HOME_OR_DOT_PREFIX.is_match(candidate) {
        return false;
    }
    if !split_file_path_position(candidate, "").path.contains('/') {
        return false;
    }
    workspace_relative_file_path(base_dir, Some(cwd)).is_some()
}

/// `base_dir` of `None` is upstream's default of `cwd`.
pub fn resolve_markdown_file_link_meta(
    href: Option<&str>,
    cwd: Option<&str>,
    base_dir: Option<&str>,
) -> Option<MarkdownFileLinkMeta> {
    let target_path = resolve_markdown_file_link_target(href, cwd, base_dir.or(cwd))?;
    if target_path.is_empty() {
        return None;
    }
    Some(build_file_link_meta_from_target(&target_path, cwd))
}

fn build_file_link_meta_from_target(target_path: &str, cwd: Option<&str>) -> MarkdownFileLinkMeta {
    let position = split_file_path_position(target_path, "");
    MarkdownFileLinkMeta {
        display_path: format_workspace_relative_path(target_path, cwd),
        workspace_relative_path: workspace_relative_file_path(&position.path, cwd),
        basename: file_basename(&position.path),
        file_path: position.path,
        target_path: target_path.to_owned(),
        line: position.line,
        column: position.column,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROJECT: &str = "/Users/julius/project";

    fn inline(code: &str, cwd: Option<&str>) -> Option<MarkdownFileLinkMeta> {
        resolve_inline_code_file_link_meta(code, cwd, None)
    }

    fn inline_in(code: &str, cwd: &str, base_dir: &str) -> Option<MarkdownFileLinkMeta> {
        resolve_inline_code_file_link_meta(code, Some(cwd), Some(base_dir))
    }

    fn link(href: &str, cwd: &str) -> Option<MarkdownFileLinkMeta> {
        resolve_markdown_file_link_meta(Some(href), Some(cwd), None)
    }

    #[test]
    fn uses_command_click_on_macos() {
        assert!(should_open_markdown_file_link_in_editor(
            true, false, "MacIntel"
        ));
        assert!(!should_open_markdown_file_link_in_editor(
            false, true, "MacIntel"
        ));
    }

    #[test]
    fn uses_control_click_on_other_platforms() {
        assert!(should_open_markdown_file_link_in_editor(
            false,
            true,
            "Linux x86_64"
        ));
        assert!(!should_open_markdown_file_link_in_editor(
            true,
            false,
            "Linux x86_64"
        ));
    }

    #[test]
    fn an_empty_platform_never_activates() {
        assert!(!is_terminal_link_activation(true, false, ""));
        assert!(!is_terminal_link_activation(false, true, ""));
    }

    #[test]
    fn keeps_pdfs_browser_first_while_source_files_open_in_the_file_viewer() {
        assert!(should_open_markdown_file_link_in_browser_by_default(
            "report.pdf"
        ));
        assert!(should_open_markdown_file_link_in_browser_by_default(
            "report.PDF?download=1"
        ));
        assert!(!should_open_markdown_file_link_in_browser_by_default(
            "report.html"
        ));
        assert!(!should_open_markdown_file_link_in_browser_by_default(
            "report.xml"
        ));
    }

    #[test]
    fn rewrites_file_uri_hrefs_into_direct_path_hrefs() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some("file:///Users/julius/project/src/main.ts#L42"))
                .as_deref(),
            Some("/Users/julius/project/src/main.ts#L42")
        );
    }

    #[test]
    fn preserves_encoded_octets_so_file_paths_are_decoded_only_once_later() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some("file:///Users/julius/project/file%2520name.md"))
                .as_deref(),
            Some("/Users/julius/project/file%2520name.md")
        );
    }

    #[test]
    fn normalizes_file_uri_hrefs_for_windows_drive_paths() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some(
                "file:///D:/Programme/t3code/apps/web/src/components/chat/OpenInPicker.tsx#L69"
            ))
            .as_deref(),
            Some("D:/Programme/t3code/apps/web/src/components/chat/OpenInPicker.tsx#L69")
        );
    }

    #[test]
    fn preserves_file_uri_authorities_as_windows_unc_paths() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some("file://server/share/workspace-image.svg"))
                .as_deref(),
            Some("\\\\server\\share\\workspace-image.svg")
        );
    }

    #[test]
    fn treats_a_localhost_file_uri_as_a_local_path() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some("file://localhost/home/me/notes.md")).as_deref(),
            Some("/home/me/notes.md")
        );
    }

    #[test]
    fn unwraps_angle_bracketed_file_uri_hrefs() {
        assert_eq!(
            rewrite_markdown_file_uri_href(Some(
                " <file:///D:/Programme/t3code/apps/web/src/markdown-links.ts> "
            ))
            .as_deref(),
            Some("D:/Programme/t3code/apps/web/src/markdown-links.ts")
        );
    }

    #[test]
    fn a_missing_or_non_file_href_does_not_rewrite() {
        assert_eq!(rewrite_markdown_file_uri_href(None), None);
        assert_eq!(rewrite_markdown_file_uri_href(Some("")), None);
        assert_eq!(
            rewrite_markdown_file_uri_href(Some("https://example.com/a")),
            None
        );
    }

    #[test]
    fn relative_links_anchor_to_the_files_directory_while_workspace_membership_follows_cwd() {
        let meta = resolve_markdown_file_link_meta(
            Some("appendix.md"),
            Some("/repo"),
            Some("/tmp/report"),
        )
        .expect("meta");
        assert_eq!(meta.file_path, "/tmp/report/appendix.md");
        assert_eq!(meta.workspace_relative_path, None);

        let inline = inline_in("Makefile:12", "/repo", "/tmp/report").expect("inline");
        assert_eq!(inline.file_path, "/tmp/report/Makefile");
        assert_eq!(inline.line, Some(12));

        let nested =
            resolve_markdown_file_link_meta(Some("src/main.ts"), Some("/repo"), Some("/repo/docs"))
                .expect("nested");
        assert_eq!(nested.file_path, "/repo/docs/src/main.ts");
        assert_eq!(
            nested.workspace_relative_path.as_deref(),
            Some("docs/src/main.ts")
        );
    }

    #[test]
    fn multi_segment_inline_code_resolves_from_the_workspace_root_in_a_workspace_file() {
        assert_eq!(
            inline_in("docs/ai/design.md", "/repo", "/repo/docs/ai")
                .expect("design")
                .file_path,
            "/repo/docs/ai/design.md"
        );
        let index = inline_in("src/index.ts:4", "/repo", "/repo/packages/a").expect("index");
        assert_eq!(index.file_path, "/repo/src/index.ts");
        assert_eq!(index.line, Some(4));
    }

    #[test]
    fn sibling_and_explicitly_relative_inline_code_stay_beside_the_file() {
        let design = inline_in("design.md:12", "/repo", "/repo/docs/ai").expect("design");
        assert_eq!(design.file_path, "/repo/docs/ai/design.md");
        assert_eq!(design.line, Some(12));
        assert_eq!(
            inline_in("./src/index.ts", "/repo", "/repo/packages/a")
                .expect("dot")
                .file_path,
            "/repo/packages/a/./src/index.ts"
        );
        assert_eq!(
            inline_in("../b/notes.md", "/repo", "/repo/docs/a")
                .expect("dotdot")
                .file_path,
            "/repo/docs/a/../b/notes.md"
        );
    }

    #[test]
    fn multi_segment_inline_code_beside_a_file_outside_the_workspace_stays_there() {
        let meta = inline_in("src/main.ts", "/repo", "/tmp/report").expect("meta");
        assert_eq!(meta.file_path, "/tmp/report/src/main.ts");
        assert_eq!(meta.workspace_relative_path, None);
    }

    #[test]
    fn links_relative_paths_with_file_extensions() {
        let meta = inline("docs/internals/workspace-layout.md", Some(PROJECT)).expect("meta");
        assert_eq!(
            meta.target_path,
            "/Users/julius/project/docs/internals/workspace-layout.md"
        );
        assert_eq!(meta.basename, "workspace-layout.md");
    }

    #[test]
    fn links_absolute_posix_paths() {
        assert_eq!(
            inline("/Users/julius/project/AGENTS.md", None)
                .expect("agents")
                .target_path,
            "/Users/julius/project/AGENTS.md"
        );
        assert_eq!(
            inline("/usr/local/bin/tool", None)
                .expect("tool")
                .target_path,
            "/usr/local/bin/tool"
        );
        assert_eq!(
            inline("/workspace/Makefile", None).expect("make").basename,
            "Makefile"
        );
        assert_eq!(inline("/chat/settings", None), None);
    }

    #[test]
    fn links_windows_drive_paths() {
        assert_eq!(
            inline("C:\\Users\\mike\\project\\src\\main.ts", None)
                .expect("windows")
                .basename,
            "main.ts"
        );
    }

    #[test]
    fn links_relative_paths_with_line_positions() {
        let meta = inline("src/processRunner.ts:71", Some(PROJECT)).expect("meta");
        assert_eq!(
            meta.target_path,
            "/Users/julius/project/src/processRunner.ts:71"
        );
        assert_eq!(meta.line, Some(71));
    }

    #[test]
    fn links_bare_filenames_only_when_a_line_suffix_marks_them_as_file_references() {
        let meta = inline("script.ts:10", Some(PROJECT)).expect("meta");
        assert_eq!(meta.target_path, "/Users/julius/project/script.ts:10");
        assert_eq!(meta.line, Some(10));
        assert_eq!(inline("AGENTS.md", Some(PROJECT)), None);
    }

    #[test]
    fn links_extensionless_bare_filenames_with_a_line_suffix() {
        let meta = inline("Makefile:12", Some(PROJECT)).expect("meta");
        assert_eq!(meta.target_path, "/Users/julius/project/Makefile:12");
        assert_eq!(meta.basename, "Makefile");
        assert_eq!(meta.line, Some(12));
        let docker = inline("Dockerfile:8:2", Some(PROJECT)).expect("docker");
        assert_eq!((docker.line, docker.column), (Some(8), Some(2)));
        assert_eq!(inline("Makefile:12", None), None);
    }

    #[test]
    fn does_not_treat_arbitrary_name_digits_shapes_as_files() {
        for code in ["error:1", "TODO:12", "exit:0", "port:3000", "http:80"] {
            assert_eq!(inline(code, Some(PROJECT)), None, "{code}");
        }
    }

    #[test]
    fn links_dot_prefixed_relative_paths_without_extensions() {
        assert_eq!(
            inline("./scripts/deploy", Some(PROJECT))
                .expect("deploy")
                .basename,
            "deploy"
        );
    }

    #[test]
    fn links_relative_windows_style_paths_by_normalizing_backslashes() {
        let meta = inline("src\\main.ts", Some(PROJECT)).expect("meta");
        assert_eq!(meta.target_path, "/Users/julius/project/src/main.ts");
        assert_eq!(meta.basename, "main.ts");
        assert_eq!(
            inline(".\\scripts\\deploy", Some(PROJECT))
                .expect("deploy")
                .basename,
            "deploy"
        );
    }

    #[test]
    fn ignores_hosts_ports_and_versions() {
        for code in [
            "127.0.0.1:3000",
            "localhost:3000",
            "example.com/index.html",
            "example.com:8080",
            "10.0.0.1:80:1",
            "localhost/index.html",
            "example.uk/index.html",
        ] {
            assert_eq!(inline(code, Some(PROJECT)), None, "{code}");
        }
    }

    #[test]
    fn still_links_files_whose_extension_merely_resembles_a_tld() {
        for code in [
            "script.ts:10",
            "src/setup.sh:3",
            "Makefile.in:12",
            "conf.d/nginx.conf",
        ] {
            assert!(inline(code, Some(PROJECT)).is_some(), "{code}");
        }
    }

    #[test]
    fn prefers_file_over_country_code_host_when_a_line_suffix_is_present() {
        let meta = inline("script.pl:10", Some(PROJECT)).expect("meta");
        assert_eq!(meta.target_path, "/Users/julius/project/script.pl:10");
        assert_eq!(meta.line, Some(10));
        assert!(inline("model.pt:3", Some(PROJECT)).is_some());
        assert_eq!(inline("example.pl/index.html", Some(PROJECT)), None);
        assert_eq!(inline("example.com:8080", Some(PROJECT)), None);
    }

    #[test]
    fn ignores_commands_flags_and_expressions() {
        assert_eq!(inline("git worktree list --porcelain", None), None);
        assert_eq!(inline("node.meta", Some(PROJECT)), None);
        assert_eq!(inline("pnpm install", Some(PROJECT)), None);
        assert_eq!(inline("src/**/*.ts", Some(PROJECT)), None);
    }

    #[test]
    fn ignores_extension_less_relative_segments_like_git_refs_and_directories() {
        assert_eq!(inline("origin/main", Some(PROJECT)), None);
        assert_eq!(inline("apps/web", Some(PROJECT)), None);
    }

    #[test]
    fn ignores_external_urls() {
        assert_eq!(inline("https://example.com/docs.html", None), None);
    }

    #[test]
    fn ignores_relative_paths_without_a_cwd_to_resolve_against() {
        assert_eq!(inline("docs/internals/workspace-layout.md", None), None);
    }

    #[test]
    fn keeps_the_final_segment_for_a_posix_directory_path() {
        assert_eq!(
            link("/tmp/favicons/", "/repo/project")
                .expect("meta")
                .basename,
            "favicons"
        );
    }

    #[test]
    fn keeps_the_final_segment_for_a_windows_directory_path() {
        assert_eq!(
            link("C:\\Users\\kelchm\\.claude\\", "/repo/project")
                .expect("meta")
                .basename,
            ".claude"
        );
    }

    #[test]
    fn matches_the_label_of_the_same_path_without_a_trailing_separator() {
        let with_slash = link("/tmp/favicons/", "/repo/project").map(|meta| meta.basename);
        let without_slash = link("/tmp/favicons", "/repo/project").map(|meta| meta.basename);
        assert_eq!(with_slash, without_slash);
    }

    #[test]
    fn does_not_produce_an_empty_label_for_the_filesystem_root() {
        let meta = link("/tmp/", "/repo/project").expect("meta");
        assert_ne!(meta.basename, "");
    }

    #[test]
    fn routes_the_project_root_code_link_to_the_workspace_explorer() {
        let cwd = "/Users/saphid/.t3/worktrees/ov2-standalone-20260918";
        let meta = inline(cwd, Some(cwd)).expect("meta");
        assert_eq!(meta.workspace_relative_path.as_deref(), Some("."));
        assert_eq!(meta.file_path, cwd);
    }
}

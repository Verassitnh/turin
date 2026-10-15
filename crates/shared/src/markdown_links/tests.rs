//! Port of upstream/packages/shared/src/markdownLinks.test.ts

use super::*;

#[test]
fn classifies_file_link_labels() {
    let cases: &[(&str, &str, bool)] = &[
        ("validates the input", "/repo/src/example.ts:12", false),
        ("read src/example.ts", "/repo/src/example.ts:12", false),
        ("example.ts?why this matters", "/repo/src/example.ts", false),
        ("example.ts", "/repo/src/example.ts:12", true),
        ("example.ts:12", "/repo/src/example.ts:12", true),
        ("example.ts:99", "/repo/src/example.ts:12", false),
        ("example.ts:12:2", "/repo/src/example.ts:12:2", true),
        ("example.ts:12:3", "/repo/src/example.ts:12:2", false),
        ("example.ts:12", "/repo/src/example.ts", false),
        ("src/example.ts:12", "/repo/src/example.ts:12", true),
        ("./src/example.ts", "/repo/src/example.ts", true),
        ("/repo/src/example.ts", "/repo/src/example.ts", true),
        ("src/", "/home/me/project/src/", true),
        ("EXAMPLE.TS", "C:/repo/src/example.ts:12", true),
        ("file name.ts", "file:///repo/file%20name.ts", true),
        ("", "/repo/src/example.ts", true),
    ];
    for (label, href, expected) in cases {
        assert_eq!(is_markdown_file_link_label(label, href), *expected, "{label} / {href}");
    }
}

#[test]
fn distinguishes_file_paths_from_code_and_hostnames_in_inline_code() {
    let cases: &[(&str, Option<&str>)] = &[
        ("src\\main.ts", Some("src/main.ts")),
        ("C:\\Users\\demo\\image.png", Some("C:\\Users\\demo\\image.png")),
        ("\\\\server\\share\\image.png", Some("\\\\server\\share\\image.png")),
        ("conf.d/nginx.conf", Some("conf.d/nginx.conf")),
        ("script.pl:10", Some("script.pl:10")),
        ("node.meta", None),
        ("Recorded evidence here: /tmp/image.png", None),
        ("origin/main", None),
        ("127.0.0.1:3000", None),
        ("example.com/index.html", None),
        ("example.pl/index.html", None),
        ("z-ai/glm-5.3", None),
        ("z-ai/glm-5.3:12", None),
        ("python/3.12", None),
        ("Qwen/Qwen2.5-Coder", None),
        ("meta-llama/Llama-3.1-8B", None),
        ("share/man/ls.1", Some("share/man/ls.1")),
        ("usr/lib/libfoo.so.1", Some("usr/lib/libfoo.so.1")),
        ("vendor/jquery-3.6.0.min.js", Some("vendor/jquery-3.6.0.min.js")),
        ("./models/glm-5.3", Some("./models/glm-5.3")),
    ];
    for (source, candidate) in cases {
        assert_eq!(
            inline_code_file_path_candidate(source),
            candidate.map(str::to_owned),
            "{source}"
        );
    }
}

#[test]
fn recognizes_file_links() {
    let cases = [
        ("/Users/julius/project/AGENTS.md", "/Users/julius/project/AGENTS.md"),
        ("/home/me/notes.md", "/home/me/notes.md"),
        ("/usr/local/bin/tool", "/usr/local/bin/tool"),
        ("/workspace/Makefile", "/workspace/Makefile"),
        ("/tmp/favicons/", "/tmp/favicons/"),
        ("C:\\Users\\mike\\project\\src\\main.ts", "C:\\Users\\mike\\project\\src\\main.ts"),
        ("C:%5Crepo%5Cimage.png", "C:\\repo\\image.png"),
        ("\\\\server\\share\\image.png", "\\\\server\\share\\image.png"),
        ("/D:/Programme/t3code/OpenInPicker.tsx", "D:/Programme/t3code/OpenInPicker.tsx"),
        ("</D:/Programme/t3code/ChatMarkdown.tsx:1>", "D:/Programme/t3code/ChatMarkdown.tsx"),
        ("file:///Users/julius/project/file%2520name.md", "/Users/julius/project/file%20name.md"),
        ("file://server/share/workspace-image.svg", "\\\\server\\share\\workspace-image.svg"),
        ("file://localhost/home/me/notes.md", "/home/me/notes.md"),
        ("apps/mobile/src/index.ts:10", "apps/mobile/src/index.ts"),
        ("docs/My%20Folder/checklist.xml", "docs/My Folder/checklist.xml"),
        ("Updated%20cutover%20checklist.md", "Updated cutover checklist.md"),
        ("./scripts/deploy", "./scripts/deploy"),
        ("~/notes/today.md", "~/notes/today.md"),
        ("AGENTS.md", "AGENTS.md"),
        ("script.ts:10", "script.ts"),
        ("/tmp/clip%23one.mp4#t=2", "/tmp/clip#one.mp4"),
    ];
    for (href, path) in cases {
        let parsed = parse_markdown_file_link(href).unwrap_or_else(|| panic!("{href} should parse"));
        assert_eq!(parsed.path, path, "{href}");
    }
}

#[test]
fn does_not_treat_non_files_as_files() {
    for href in [
        "",
        "#anchor",
        "//cdn.example.com/clip.mp4",
        "https://example.com/docs",
        "mailto:someone@example.com",
        "javascript:alert(1)",
        "/chat/settings",
        "/chat/settings#L3",
        "/app#L1",
        "readme",
        "TODO:12",
    ] {
        assert_eq!(parse_markdown_file_link(href), None, "{href}");
    }
}

#[test]
fn accepts_conventional_extensionless_names_with_or_without_a_position() {
    assert_eq!(
        parse_markdown_file_link("Makefile"),
        Some(FilePathPosition { path: "Makefile".into(), line: None, column: None })
    );
    assert_eq!(
        parse_markdown_file_link("Dockerfile:8"),
        Some(FilePathPosition { path: "Dockerfile".into(), line: Some(8), column: None })
    );
    assert_eq!(
        parse_markdown_file_link("/srv/app/Makefile"),
        Some(FilePathPosition { path: "/srv/app/Makefile".into(), line: None, column: None })
    );
}

#[test]
fn reads_positions_from_suffixes_and_line_anchors() {
    assert_eq!(
        parse_markdown_file_link("/Users/julius/project/src/main.ts#L42C7"),
        Some(FilePathPosition {
            path: "/Users/julius/project/src/main.ts".into(),
            line: Some(42),
            column: Some(7),
        })
    );
    let unc = parse_markdown_file_link("file://server/share/src/main.ts#L42C7").expect("parses");
    assert_eq!(unc.path, "\\\\server\\share\\src\\main.ts");
    assert_eq!(unc.line, Some(42));
    assert_eq!(unc.column, Some(7));
}

#[test]
fn classifies_windows_drive_path_hrefs() {
    let cases = [
        ("C:\\repo\\image.png", true),
        ("C:%5Crepo%5Cimage.png", true),
        ("https://example.com/image.png", false),
    ];
    for (href, expected) in cases {
        assert_eq!(is_windows_drive_path_href(href), expected, "{href}");
    }
}

#[test]
fn extracts_links_from_incomplete_and_nested_text() {
    let cases: Vec<(String, Vec<&str>)> = vec![
        ("[".repeat(32_000), vec![]),
        ("[](a".repeat(16_000), vec![]),
        ("[](<a".repeat(16_000), vec![]),
        ("[x](https://e.com/".repeat(8_000), vec![]),
        ("[a](<foo)bar> invalid [b](ok)".to_owned(), vec!["<foo", "ok"]),
        ("[a](foo \"mixed title') [b](< >)".to_owned(), vec!["foo"]),
        ("[a](<one [b](two)> invalid [c](ok)".to_owned(), vec!["two", "ok"]),
        (format!("```text\n{}{}", "[unfinished item\n".repeat(8_000), "```"), vec![]),
        ("[[[nested](src/main.ts) [unfinished".to_owned(), vec!["src/main.ts"]),
        ("[outer [inner] invalid [valid](src/main.ts)".to_owned(), vec!["src/main.ts"]),
        ("[bad](missing title [valid](src/main.ts)".to_owned(), vec!["src/main.ts"]),
        ("```text\n[link](src/main.ts)\n```".to_owned(), vec!["src/main.ts"]),
        ("[empty]() [valid](src/main.ts 'title')".to_owned(), vec!["src/main.ts"]),
        ("[angle](<src/My File.ts> \"title\")".to_owned(), vec!["src/My File.ts"]),
    ];
    for (markdown, hrefs) in cases {
        assert_eq!(extract_markdown_link_hrefs(&markdown), hrefs, "{markdown:.80}");
    }
}

#[test]
fn extracts_angle_bracketed_paths_containing_spaces() {
    assert_eq!(
        extract_markdown_link_hrefs(
            "[Open the Bike Receipts folder](</Users/dara/Downloads/Lime Ride Artifacts/Bike Receipts>)"
        ),
        vec!["/Users/dara/Downloads/Lime Ride Artifacts/Bike Receipts"]
    );
}

#[test]
fn preserves_ordinary_destinations_and_ignores_link_titles() {
    assert_eq!(
        extract_markdown_link_hrefs(
            "[source](apps/web/src/markdown-links.ts \"implementation\") and [docs](https://example.com)"
        ),
        vec!["apps/web/src/markdown-links.ts", "https://example.com"]
    );
}

#[test]
fn resolves_absolute_posix_file_paths() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("/Users/julius/project/AGENTS.md"), None, None),
        Some("/Users/julius/project/AGENTS.md".to_owned())
    );
}

#[test]
fn resolves_relative_file_paths_against_cwd() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("src/processRunner.ts:71"), Some("/Users/julius/project"), None),
        Some("/Users/julius/project/src/processRunner.ts:71".to_owned())
    );
}

#[test]
fn does_not_treat_filename_line_references_as_external_schemes() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("script.ts:10"), Some("/Users/julius/project"), None),
        Some("/Users/julius/project/script.ts:10".to_owned())
    );
}

#[test]
fn resolves_bare_file_names_against_cwd() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("AGENTS.md"), Some("/Users/julius/project"), None),
        Some("/Users/julius/project/AGENTS.md".to_owned())
    );
}

#[test]
fn maps_line_anchors_to_editor_line_suffixes() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("/Users/julius/project/src/main.ts#L42C7"), None, None),
        Some("/Users/julius/project/src/main.ts:42:7".to_owned())
    );
}

#[test]
fn ignores_external_urls() {
    assert_eq!(resolve_markdown_file_link_target(Some("https://example.com/docs"), None, None), None);
    assert_eq!(
        resolve_markdown_file_link_target(Some("//cdn.example.com/clip.mp4"), Some("/workspace"), None),
        None
    );
}

#[test]
fn does_not_double_decode_file_urls() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("file:///Users/julius/project/file%2520name.md"), None, None),
        Some("/Users/julius/project/file%20name.md".to_owned())
    );
}

#[test]
fn resolves_file_uri_authorities_as_windows_unc_paths() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("file://server/share/workspace-image.svg"), None, None),
        Some("\\\\server\\share\\workspace-image.svg".to_owned())
    );
}

#[test]
fn resolves_a_localhost_file_uri_as_a_local_path() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("file://localhost/home/me/notes.md"), None, None),
        Some("/home/me/notes.md".to_owned())
    );
}

#[test]
fn keeps_an_encoded_final_space_in_the_absolute_target() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("/tmp/repo/file.ts%20"), Some("/tmp/repo"), None),
        Some("/tmp/repo/file.ts ".to_owned())
    );
}

#[test]
fn normalizes_slash_prefixed_windows_drive_paths_before_resolving() {
    assert_eq!(
        resolve_markdown_file_link_target(
            Some("/D:/Programme/t3code/apps/web/src/components/chat/OpenInPicker.tsx#L69"),
            None,
            None
        ),
        Some("D:/Programme/t3code/apps/web/src/components/chat/OpenInPicker.tsx:69".to_owned())
    );
}

#[test]
fn resolves_angle_bracketed_windows_drive_paths() {
    assert_eq!(
        resolve_markdown_file_link_target(
            Some("</D:/Programme/t3code/apps/web/src/components/ChatMarkdown.tsx:1>"),
            None,
            None
        ),
        Some("D:/Programme/t3code/apps/web/src/components/ChatMarkdown.tsx:1".to_owned())
    );
}

#[test]
fn does_not_treat_app_routes_as_file_links_even_with_a_line_anchor() {
    assert_eq!(resolve_markdown_file_link_target(Some("/chat/settings"), None, None), None);
    assert_eq!(
        resolve_markdown_file_link_target(Some("/chat/settings#L3"), Some("/repo"), None),
        None
    );
}

#[test]
fn decodes_an_encoded_drive_colon_in_a_file_uri_before_dropping_its_slash() {
    assert_eq!(
        resolve_markdown_file_link_target(Some("file:///c%3A/Users/x/shot.png"), None, None),
        Some("c:/Users/x/shot.png".to_owned())
    );
}

#[test]
fn extracts_inline_code_spans_outside_fences() {
    assert_eq!(extract_inline_code_spans("a `code` b ```fenced `not` code``` c `x`"), vec!["code", "x"]);
}

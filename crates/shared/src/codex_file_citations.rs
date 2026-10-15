//! Port of upstream/packages/shared/src/codexFileCitations.ts

use turin_contracts::base_schemas::js_trim;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexFileCitationLink {
    pub path: String,
    pub href: String,
    pub label: String,
    pub line_range_start: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub struct CodexFileCitationAttributes<'a> {
    pub path: Option<&'a str>,
    pub line_range_start: Option<&'a str>,
}

fn positive_integer(value: Option<&str>) -> Option<u64> {
    let value = value?;
    let trimmed = js_trim(value);
    if trimmed.is_empty() {
        return None;
    }
    let parsed: f64 = trimmed.parse().ok()?;
    if !parsed.is_finite()
        || parsed.fract() != 0.0
        || parsed <= 0.0
        || parsed > 9_007_199_254_740_991.0
    {
        return None;
    }
    Some(parsed as u64)
}

fn citation_label(path: &str) -> String {
    let normalized = path.replace('\\', "/");
    let normalized = normalized.trim_end_matches('/');
    let label = normalized.rsplit('/').next().unwrap_or("").to_string();
    if label.is_empty() {
        if normalized.is_empty() {
            "File".to_string()
        } else {
            normalized.to_string()
        }
    } else {
        label
    }
}

fn markdown_destination_path(path: &str) -> String {
    path.replace('%', "%25")
        .replace('#', "%23")
        .replace('?', "%3F")
}

pub fn resolve_codex_file_citation_link(
    attributes: Option<&CodexFileCitationAttributes<'_>>,
) -> Option<CodexFileCitationLink> {
    let attributes = attributes?;
    let path = attributes
        .path
        .map(js_trim)
        .filter(|path| !path.is_empty())?;
    let line_range_start = positive_integer(attributes.line_range_start);
    let destination = markdown_destination_path(path);
    let href = match line_range_start {
        Some(line) => format!("{destination}#L{line}"),
        None => destination,
    };
    Some(CodexFileCitationLink {
        label: citation_label(path),
        path: path.to_string(),
        href,
        line_range_start,
    })
}

fn markdown_label(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        if matches!(ch, '\\' | '[' | ']' | '*' | '_' | '`' | '<' | '&') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

fn markdown_destination(value: &str) -> String {
    value
        .replace('\\', "%5C")
        .replace('<', "%3C")
        .replace('>', "%3E")
        .replace('\r', "%0D")
        .replace('\n', "%0A")
}

pub fn codex_file_citation_markdown(citation: &CodexFileCitationLink) -> String {
    format!(
        "[{}](<{}>)",
        markdown_label(&citation.label),
        markdown_destination(&citation.href)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_attributes_emitted_by_codex() {
        let link = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("/workspace/outputs/issue-2387-sparse-diagonal.xlsx"),
            line_range_start: None,
        }))
        .expect("link");
        assert_eq!(
            link.path,
            "/workspace/outputs/issue-2387-sparse-diagonal.xlsx"
        );
        assert_eq!(link.href, link.path);
        assert_eq!(link.label, "issue-2387-sparse-diagonal.xlsx");
        assert_eq!(link.line_range_start, None);
    }

    #[test]
    fn carries_the_first_cited_line() {
        let link = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("src/main.ts"),
            line_range_start: Some("42"),
        }))
        .expect("link");
        assert_eq!(link.href, "src/main.ts#L42");
        assert_eq!(link.label, "main.ts");
        assert_eq!(link.line_range_start, Some(42));
    }

    #[test]
    fn rejects_missing_paths_and_invalid_line_numbers() {
        assert!(
            resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
                path: None,
                line_range_start: None,
            }))
            .is_none()
        );
        let link = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("src/main.ts"),
            line_range_start: Some("not-a-line"),
        }))
        .expect("link");
        assert_eq!(link.href, "src/main.ts");
        assert_eq!(link.line_range_start, None);
    }

    #[test]
    fn preserves_url_syntax_characters() {
        let link = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("reports/100% #1? draft.md"),
            line_range_start: Some("7"),
        }))
        .expect("link");
        assert_eq!(link.href, "reports/100%25 %231%3F draft.md#L7");
        assert_eq!(link.label, "100% #1? draft.md");
    }

    #[test]
    fn produces_a_portable_markdown_link() {
        let citation = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("reports/profit and loss.xlsx"),
            line_range_start: None,
        }))
        .expect("link");
        assert_eq!(
            codex_file_citation_markdown(&citation),
            "[profit and loss.xlsx](<reports/profit and loss.xlsx>)"
        );
    }

    #[test]
    fn escapes_markdown_syntax_in_the_visible_filename() {
        let citation = resolve_codex_file_citation_link(Some(&CodexFileCitationAttributes {
            path: Some("reports/*draft*_[copy]`<&.txt"),
            line_range_start: None,
        }))
        .expect("link");
        assert_eq!(
            codex_file_citation_markdown(&citation),
            "[\\*draft\\*\\_\\[copy\\]\\`\\<\\&.txt](<reports/*draft*_[copy]`%3C&.txt>)"
        );
    }
}

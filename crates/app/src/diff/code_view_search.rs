//! Port of upstream `@pierre/diffs` `dist/components/CodeViewSearch.js` (`collectMatches`)
//! and `apps/web/src/components/diffs/codeViewSearch.test.ts`.

use regex::Regex;

use super::pierre::iterate_over_diff::{
    DiffStyle, ExpandedHunks, LineEvent, LineEventKind, iterate_over_diff,
};
use super::pierre::types::{FileDiffMetadata, SelectionSide};

const MAX_SEARCH_MATCHES: usize = 100_000;
const WORD_SEPARATORS: &str = "`~!@#$%^&*()-=+[{]}\\|;:'\",.<>/?";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeViewSearchItem {
    pub id: String,
    pub file_diff: FileDiffMetadata,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeViewSearchParams {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeViewSearchMatch {
    pub id: String,
    pub item_index: usize,
    pub side: SelectionSide,
    pub is_context: bool,
    pub line_number: usize,
    pub unified_line_index: usize,
    pub start: usize,
    pub end: usize,
    pub range_start: usize,
    pub range_end: usize,
}

pub fn collect_matches(
    items: &[CodeViewSearchItem],
    params: &CodeViewSearchParams,
) -> Vec<CodeViewSearchMatch> {
    if params.text.is_empty() || params.text.contains('\n') {
        return Vec::new();
    }
    let pattern = match build_pattern(params) {
        Some(pattern) => pattern,
        None => return Vec::new(),
    };
    let expanded = ExpandedHunks {
        all: true,
        regions: Default::default(),
    };
    let mut entries = Vec::new();
    let mut offset = 0usize;
    'items: for (item_index, item) in items.iter().enumerate() {
        let events = iterate_over_diff(&item.file_diff, DiffStyle::Unified, &expanded, 1);
        for event in events {
            if entries.len() >= MAX_SEARCH_MATCHES {
                break 'items;
            }
            let Some((side, line, is_context, lines)) = line_for_event(&item.file_diff, &event)
            else {
                continue;
            };
            let text =
                trim_line_break(lines.get(line.line_index).map(String::as_str).unwrap_or(""));
            for mat in pattern.find_iter(text) {
                let length = mat.end() - mat.start();
                if length == 0 {
                    continue;
                }
                if params.whole_word && !is_whole_word(text, mat.start(), length) {
                    continue;
                }
                entries.push(CodeViewSearchMatch {
                    id: item.id.clone(),
                    item_index,
                    side,
                    is_context,
                    line_number: line.line_number,
                    unified_line_index: line.unified_line_index,
                    start: mat.start(),
                    end: mat.end(),
                    range_start: offset + mat.start(),
                    range_end: offset + mat.end(),
                });
                if entries.len() >= MAX_SEARCH_MATCHES {
                    break 'items;
                }
            }
            offset += text.len() + 1;
        }
    }
    entries
}

fn build_pattern(params: &CodeViewSearchParams) -> Option<Regex> {
    let source = if params.regex {
        params.text.clone()
    } else {
        regex::escape(&params.text)
    };
    let flags = if params.case_sensitive { "" } else { "(?i)" };
    Regex::new(&format!("{flags}{source}")).ok()
}

fn line_for_event<'a>(
    file: &'a FileDiffMetadata,
    event: &LineEvent,
) -> Option<(
    SelectionSide,
    super::pierre::iterate_over_diff::SideLine,
    bool,
    &'a [String],
)> {
    match event.kind {
        LineEventKind::Change => {
            if let Some(line) = event.deletion {
                return Some((
                    SelectionSide::Deletions,
                    line,
                    false,
                    file.deletion_lines.as_slice(),
                ));
            }
            event.addition.map(|line| {
                (
                    SelectionSide::Additions,
                    line,
                    false,
                    file.addition_lines.as_slice(),
                )
            })
        }
        LineEventKind::Context | LineEventKind::ContextExpanded => event.addition.map(|line| {
            (
                SelectionSide::Additions,
                line,
                true,
                file.addition_lines.as_slice(),
            )
        }),
    }
}

fn trim_line_break(text: &str) -> &str {
    text.strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))
        .unwrap_or(text)
}

fn is_whole_word(text: &str, start: usize, length: usize) -> bool {
    is_separator(text, start.wrapping_sub(1)) && is_separator(text, start + length)
}

fn is_separator(text: &str, index: usize) -> bool {
    text.char_indices()
        .nth(index)
        .map(|(_, ch)| (ch as u32) <= 32 || ch as u32 == 127 || WORD_SEPARATORS.contains(ch))
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::pierre::parse_patch_files::parse_patch_files;

    const PATCH: &str = r"diff --git a/src/a.ts b/src/a.ts
index 1111111..2222222 100644
--- a/src/a.ts
+++ b/src/a.ts
@@ -1,4 +1,5 @@
 const keep = 1;
-const oldName = 2;
+const newName = 2;
+const added = oldName;
 const tail = 4;
@@ -20,3 +21,3 @@ function x() {
 ctx20 name
-del21 Name
+add22 names
 ctx22
diff --git a/src/b.ts b/src/b.ts
index 3333333..4444444 100644
--- a/src/b.ts
+++ b/src/b.ts
@@ -1,1 +1,1 @@
-nil
+: nil
";

    fn items() -> Vec<CodeViewSearchItem> {
        let parsed = parse_patch_files(PATCH, None);
        parsed[0]
            .files
            .iter()
            .enumerate()
            .map(|(index, file_diff)| CodeViewSearchItem {
                id: format!("file-{index}"),
                file_diff: file_diff.clone(),
            })
            .collect()
    }

    fn find(text: &str, mut flags: CodeViewSearchParams) -> Vec<String> {
        flags.text = text.into();
        collect_matches(&items(), &flags)
            .into_iter()
            .map(|entry| {
                format!(
                    "{}:{:?}:{}:{}-{}",
                    entry.id, entry.side, entry.line_number, entry.start, entry.end
                )
            })
            .collect()
    }

    #[test]
    fn finds_removed_added_and_context_lines() {
        assert_eq!(
            find(
                "oldName",
                CodeViewSearchParams {
                    text: String::new(),
                    case_sensitive: false,
                    whole_word: false,
                    regex: false,
                },
            ),
            vec![
                "file-0:Deletions:2:6-13".to_string(),
                "file-0:Additions:3:14-21".to_string(),
            ]
        );
        assert_eq!(
            find(
                "nil",
                CodeViewSearchParams {
                    text: String::new(),
                    case_sensitive: false,
                    whole_word: false,
                    regex: false,
                },
            ),
            vec![
                "file-1:Deletions:1:0-3".to_string(),
                "file-1:Additions:1:2-5".to_string(),
            ]
        );
    }

    #[test]
    fn honours_case_whole_word_and_regex_toggles() {
        let base = CodeViewSearchParams {
            text: String::new(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
        };
        assert!(
            find(
                "Name",
                CodeViewSearchParams {
                    case_sensitive: true,
                    ..base.clone()
                }
            )
            .iter()
            .any(|row| row.contains("file-0:Deletions:2:9-13"))
        );
        assert!(
            find(
                "name",
                CodeViewSearchParams {
                    whole_word: true,
                    ..base.clone()
                }
            )
            .iter()
            .any(|row| row.contains(":21:6-10"))
        );
        assert_eq!(
            find(
                "n[a-z]+e",
                CodeViewSearchParams {
                    regex: true,
                    ..base
                },
            )
            .len(),
            6
        );
    }

    #[test]
    fn marks_context_lines() {
        let matches = collect_matches(
            &items(),
            &CodeViewSearchParams {
                text: "ctx".into(),
                case_sensitive: false,
                whole_word: false,
                regex: false,
            },
        );
        assert_eq!(
            matches
                .iter()
                .map(|entry| (entry.line_number, entry.is_context))
                .collect::<Vec<_>>(),
            vec![(21, true), (23, true)]
        );
    }

    #[test]
    fn rejects_empty_and_invalid_regex() {
        let base = CodeViewSearchParams {
            text: String::new(),
            case_sensitive: false,
            whole_word: false,
            regex: false,
        };
        assert!(collect_matches(&items(), &base).is_empty());
        assert!(
            collect_matches(
                &items(),
                &CodeViewSearchParams {
                    text: "(".into(),
                    regex: true,
                    ..base
                },
            )
            .is_empty()
        );
    }
}

//! Port of upstream/packages/shared/src/delimitedPreview.ts

use turin_contracts::base_schemas::js_trim;

/// A table preview is bounded independently of the source preview's byte limit.
pub fn file_preview_delimiter(name: &str, mime_type: Option<&str>) -> Option<char> {
    let mime = mime_type.map(|value| {
        let head = value.split(';').next().unwrap_or("");
        js_trim(head).to_ascii_lowercase()
    });
    if mime.as_deref() == Some("text/csv") {
        return Some(',');
    }
    if mime.as_deref() == Some("text/tab-separated-values") {
        return Some('\t');
    }
    if mime.as_deref().is_some_and(|mime| {
        !mime.is_empty() && mime != "text/plain" && mime != "application/octet-stream"
    }) {
        return None;
    }
    let lowered = name.to_ascii_lowercase();
    if lowered.ends_with(".csv") {
        return Some(',');
    }
    if lowered.ends_with(".tsv") {
        return Some('\t');
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DelimitedPreview {
    pub rows: Vec<Vec<String>>,
    pub truncated: bool,
}

/// Preserve quoted delimiters, escaped quotes and multiline cells; raw mode retains all text.
///
/// Cell and row limits follow upstream (`2000` UTF-16 units, `30` columns, `100` rows).
/// Indexing walks Unicode scalar values. Upstream walks UTF-16 units; a cut that would
/// land inside a surrogate pair cannot be represented in a Rust string (see PORTING.md).
pub fn parse_delimited_preview(text: &str, delimiter: char) -> DelimitedPreview {
    let chars: Vec<char> = text.chars().collect();
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut cell_units = 0usize;
    let mut quoted = false;
    let mut truncated = false;
    let mut row_start = usize::from(chars.first() == Some(&'\u{feff}'));

    let end_cell =
        |row: &mut Vec<String>, cell: &mut String, cell_units: &mut usize, truncated: &mut bool| {
            if row.len() < 30 {
                row.push(std::mem::take(cell));
            } else {
                *truncated = true;
                cell.clear();
            }
            *cell_units = 0;
        };

    let mut index = row_start;
    while index < chars.len() {
        let ch = chars[index];
        if ch == '"' {
            if quoted && chars.get(index + 1) == Some(&'"') {
                if cell_units < 2000 {
                    cell.push('"');
                    cell_units += 1;
                } else {
                    truncated = true;
                }
                index += 2;
                continue;
            }
            if quoted || cell.is_empty() {
                quoted = !quoted;
                index += 1;
                continue;
            }
        }
        if !quoted && (ch == delimiter || ch == '\n' || ch == '\r') {
            end_cell(&mut row, &mut cell, &mut cell_units, &mut truncated);
            if ch != delimiter {
                rows.push(std::mem::take(&mut row));
                if ch == '\r' && chars.get(index + 1) == Some(&'\n') {
                    index += 1;
                }
                row_start = index + 1;
                if rows.len() == 100 {
                    return DelimitedPreview {
                        rows,
                        truncated: truncated || index < chars.len() - 1,
                    };
                }
            }
        } else if cell_units < 2000 {
            cell.push(ch);
            cell_units += ch.len_utf16();
        } else {
            truncated = true;
        }
        index += 1;
    }
    if row_start < chars.len() {
        end_cell(&mut row, &mut cell, &mut cell_units, &mut truncated);
        rows.push(row);
    }
    DelimitedPreview {
        rows,
        truncated: truncated || quoted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respects_a_specific_mime_type_before_the_extension() {
        assert_eq!(
            file_preview_delimiter("data.CSV", Some("text/plain")),
            Some(',')
        );
        assert_eq!(
            file_preview_delimiter("data.csv", Some("application/pdf")),
            None
        );
    }

    #[test]
    fn preserves_quoted_separators_escaped_quotes_and_line_breaks() {
        assert_eq!(
            parse_delimited_preview(
                "\u{feff}name,notes,empty\r\n\"A, B\",\"say \"\"hi\"\"\nagain\",\r\n",
                ','
            ),
            DelimitedPreview {
                rows: vec![
                    vec!["name".into(), "notes".into(), "empty".into()],
                    vec!["A, B".into(), "say \"hi\"\nagain".into(), String::new()],
                ],
                truncated: false,
            }
        );
        assert_eq!(
            parse_delimited_preview("one\ttwo\n\tthree", '\t').rows,
            vec![
                vec!["one".to_string(), "two".to_string()],
                vec![String::new(), "three".to_string()],
            ]
        );
    }

    #[test]
    fn preserves_a_final_quoted_empty_record() {
        for delimiter in [',', '\t'] {
            for ending in ["", "\n", "\r\n"] {
                let text = format!("name\r\nAlice\r\n\"\"{ending}");
                assert_eq!(
                    parse_delimited_preview(&text, delimiter),
                    DelimitedPreview {
                        rows: vec![
                            vec!["name".into()],
                            vec!["Alice".into()],
                            vec![String::new()],
                        ],
                        truncated: false,
                    }
                );
            }
            assert_eq!(
                parse_delimited_preview("\"\"", delimiter),
                DelimitedPreview {
                    rows: vec![vec![String::new()]],
                    truncated: false,
                }
            );
        }
    }

    #[test]
    fn does_not_invent_records_for_empty_input_or_a_trailing_line_ending() {
        for delimiter in [',', '\t'] {
            for prefix in ["", "\u{feff}"] {
                assert_eq!(
                    parse_delimited_preview(prefix, delimiter).rows,
                    Vec::<Vec<String>>::new()
                );
                assert_eq!(
                    parse_delimited_preview(&format!("{prefix}name\r\n"), delimiter).rows,
                    vec![vec!["name".to_string()]]
                );
                assert_eq!(
                    parse_delimited_preview(&format!("{prefix}\"\""), delimiter).rows,
                    vec![vec![String::new()]]
                );
            }
        }
    }

    #[test]
    fn keeps_a_quoted_empty_record_at_the_row_limit() {
        for delimiter in [',', '\t'] {
            let text = format!("{}\"\"", "name\n".repeat(99));
            let preview = parse_delimited_preview(&text, delimiter);
            assert_eq!(preview.rows.len(), 100);
            assert_eq!(preview.rows.last(), Some(&vec![String::new()]));
            assert!(!preview.truncated);
            assert!(parse_delimited_preview(&format!("{text}\nextra"), delimiter).truncated);
        }
    }

    #[test]
    fn bounds_rows_columns_and_cell_length() {
        let row = "x".repeat(2001);
        let line = std::iter::repeat_n(row, 31).collect::<Vec<_>>().join(",");
        let table_text = std::iter::repeat_n(line, 101)
            .collect::<Vec<_>>()
            .join("\n");
        let table = parse_delimited_preview(&table_text, ',');
        assert!(table.truncated);
        assert_eq!(table.rows.len(), 100);
        assert_eq!(table.rows[0].len(), 30);
        assert_eq!(table.rows[0][0].len(), 2000);
        assert!(parse_delimited_preview("a,\"unfinished", ',').truncated);
    }
}

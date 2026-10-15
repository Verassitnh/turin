//! Port of upstream/apps/server/src/checkpointing/Diffs.ts

use crate::locale_compare::locale_compare;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnDiffFileSummary {
    pub path: String,
    pub previous_path: Option<String>,
    pub additions: u64,
    pub deletions: u64,
}

/// Reads Git's NUL-delimited numstat output without decoding display paths.
pub fn parse_turn_diff_files_from_numstat(numstat: &str) -> Vec<TurnDiffFileSummary> {
    let records: Vec<&str> = numstat.split('\0').collect();
    let mut files = Vec::new();
    let mut index = 0;
    while index < records.len() {
        let record = records[index];
        let Some(counts) = parse_counts(record) else {
            index += 1;
            continue;
        };
        let mut path = record[counts.prefix_len..].to_owned();
        let mut previous_path = None;
        if path.is_empty() {
            previous_path = records.get(index + 1).map(|value| (*value).to_owned());
            path = records.get(index + 2).copied().unwrap_or("").to_owned();
            index += 2;
        }
        index += 1;
        if path.is_empty() {
            continue;
        }
        files.push(TurnDiffFileSummary {
            path,
            previous_path,
            additions: counts.additions,
            deletions: counts.deletions,
        });
    }
    files.sort_by(|left, right| locale_compare(&left.path, &right.path));
    files
}

struct Counts {
    prefix_len: usize,
    additions: u64,
    deletions: u64,
}

fn parse_counts(record: &str) -> Option<Counts> {
    let bytes = record.as_bytes();
    let mut cursor = 0;
    let additions = take_count(bytes, &mut cursor)?;
    if cursor >= bytes.len() || bytes[cursor] != b'\t' {
        return None;
    }
    cursor += 1;
    let deletions = take_count(bytes, &mut cursor)?;
    if cursor >= bytes.len() || bytes[cursor] != b'\t' {
        return None;
    }
    cursor += 1;
    Some(Counts {
        prefix_len: cursor,
        additions,
        deletions,
    })
}

fn take_count(bytes: &[u8], cursor: &mut usize) -> Option<u64> {
    if *cursor >= bytes.len() {
        return None;
    }
    if bytes[*cursor] == b'-' {
        *cursor += 1;
        return Some(0);
    }
    let start = *cursor;
    while *cursor < bytes.len() && bytes[*cursor].is_ascii_digit() {
        *cursor += 1;
    }
    if start == *cursor {
        return None;
    }
    std::str::from_utf8(&bytes[start..*cursor])
        .ok()?
        .parse()
        .ok()
}

/// True when Git brought this file in and no work in the range touched it.
/// `authored_paths` comes from `list_authored_paths`; `None` means nothing was imported.
pub fn is_git_import(
    file: &TurnDiffFileSummary,
    authored_paths: Option<&std::collections::HashSet<String>>,
) -> bool {
    let Some(authored) = authored_paths else {
        return false;
    };
    !authored.contains(&file.path)
        && file
            .previous_path
            .as_ref()
            .is_none_or(|previous| !authored.contains(previous))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_numstat_has_no_files() {
        assert!(parse_turn_diff_files_from_numstat("").is_empty());
    }

    #[test]
    fn sorts_files_and_keeps_counts() {
        let numstat = ["0\t2\tsrc/b.ts", "2\t1\ta.txt", ""].join("\0");
        let files = parse_turn_diff_files_from_numstat(&numstat);
        assert_eq!(files[0].path, "a.txt");
        assert_eq!(files[0].additions, 2);
        assert_eq!(files[1].path, "src/b.ts");
        assert_eq!(files[1].deletions, 2);
    }

    #[test]
    fn orders_paths_like_local_compare_not_bytes() {
        // Byte order puts "B.ts" and "Zed" before "_x"; upstream's localeCompare does not.
        let numstat = [
            "1\t0\tZed",
            "1\t0\tB.ts",
            "1\t0\t_x",
            "1\t0\témile",
            "1\t0\ta.ts",
        ]
        .join("\0");
        let paths: Vec<String> = parse_turn_diff_files_from_numstat(&numstat)
            .into_iter()
            .map(|file| file.path)
            .collect();
        assert_eq!(paths, ["_x", "a.ts", "B.ts", "émile", "Zed"]);
    }

    #[test]
    fn preserves_rename_paths_and_binary_files() {
        let numstat = [
            "0\t0\t",
            "src/old.ts",
            "src/new.ts",
            "2\t1\t",
            "src/source.ts",
            "src/copied.ts",
            "1\t0\tother.ts",
            "",
        ]
        .join("\0");
        let files = parse_turn_diff_files_from_numstat(&numstat);
        assert_eq!(files[0].path, "other.ts");
        assert_eq!(files[1].previous_path.as_deref(), Some("src/source.ts"));
        assert_eq!(files[1].path, "src/copied.ts");
        assert_eq!(files[2].previous_path.as_deref(), Some("src/old.ts"));

        let binary = ["-\t-\timage.png", "0\t0\tempty.txt", ""].join("\0");
        let files = parse_turn_diff_files_from_numstat(&binary);
        assert_eq!(files[0].path, "empty.txt");
        assert_eq!(files[1].path, "image.png");
        assert_eq!(files[1].additions, 0);
    }

    #[test]
    fn preserves_whitespace_inside_paths() {
        let path = " café\tline\r\nname.txt ";
        let numstat = format!("3\t2\t\0old\tname\n.txt\0{path}\0");
        let files = parse_turn_diff_files_from_numstat(&numstat);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].path, path);
        assert_eq!(files[0].previous_path.as_deref(), Some("old\tname\n.txt"));
    }
}

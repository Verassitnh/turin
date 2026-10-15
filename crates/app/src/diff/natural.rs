//! `new Intl.Collator(undefined, { numeric: true, sensitivity: "base" })`,
//! approximately: digit runs compare as numbers, everything else
//! case-insensitively. Used to order a diff's files by path
//! (`filePathCollator` in useReviewFilePatches.ts).

use std::cmp::Ordering;

#[derive(PartialEq, Eq)]
enum Chunk {
    Number(String),
    Text(String),
}

fn chunks(value: &str) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    for c in value.chars() {
        let digit = c.is_ascii_digit();
        match out.last_mut() {
            Some(Chunk::Number(part)) if digit => part.push(c),
            Some(Chunk::Text(part)) if !digit => part.extend(c.to_lowercase()),
            _ => out.push(if digit {
                Chunk::Number(c.to_string())
            } else {
                Chunk::Text(c.to_lowercase().collect())
            }),
        }
    }
    out
}

fn compare_numbers(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

pub fn natural_compare(a: &str, b: &str) -> Ordering {
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ordering = match (x, y) {
            (Chunk::Number(x), Chunk::Number(y)) => compare_numbers(x, y),
            (Chunk::Text(x), Chunk::Text(y)) => x.cmp(y),
            (Chunk::Number(_), Chunk::Text(_)) => Ordering::Less,
            (Chunk::Text(_), Chunk::Number(_)) => Ordering::Greater,
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    ca.len().cmp(&cb.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_numbers_numerically_and_text_without_case() {
        assert_eq!(natural_compare("file2", "file10"), Ordering::Less);
        assert_eq!(natural_compare("README.md", "readme.md"), Ordering::Equal);
        assert_eq!(natural_compare("a/b.ts", "a/B.ts"), Ordering::Equal);
        assert_eq!(natural_compare("src/a.ts", "src/b.ts"), Ordering::Less);
        let mut paths = vec!["docs/z.md", "README.md", "docs/a.md", "crates/x.rs"];
        paths.sort_by(|a, b| natural_compare(a, b));
        assert_eq!(
            paths,
            vec!["crates/x.rs", "docs/a.md", "docs/z.md", "README.md"]
        );
    }
}

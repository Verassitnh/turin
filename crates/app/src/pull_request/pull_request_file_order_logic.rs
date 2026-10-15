//! Port of upstream/apps/web/src/components/pullRequest/pullRequestFileOrder.logic.ts.
//! `@pierre/diffs` metadata is represented by its path and patch lines; the caller resolves
//! old/new file paths before passing them here.
use crate::state::support::collate::locale_compare;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;
static IMPORT_SPECIFIER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?:\bfrom|\bimport|\brequire\s*\()\s*\(?\s*["']([^"']+)["']"#).unwrap()
});
static GENERATED_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.generated\.").unwrap());
static TEST_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\.(?:test|spec)\.").unwrap());
static TESTED_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\.(?:test|spec)\..*$").unwrap());
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffFileTier {
    Source,
    Test,
    Generated,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderableDiffFile {
    pub path: String,
    pub addition_lines: Vec<String>,
    pub deletion_lines: Vec<String>,
}
fn diff_file_tier(path: &str) -> DiffFileTier {
    let segments: Vec<&str> = path.split('/').collect();
    let name = segments.last().copied().unwrap_or("");
    if [
        "pnpm-lock.yaml",
        "yarn.lock",
        "package-lock.json",
        "bun.lockb",
        "Cargo.lock",
        "go.sum",
        "composer.lock",
        "Gemfile.lock",
    ]
    .contains(&name)
        || name.ends_with(".snap")
        || name.ends_with(".min.js")
        || name.ends_with(".min.css")
        || GENERATED_NAME.is_match(name)
        || segments[..segments.len().saturating_sub(1)]
            .iter()
            .any(|part| {
                ["__snapshots__", "__generated__", "dist", "build", "vendor"].contains(part)
            })
    {
        return DiffFileTier::Generated;
    }
    if TEST_NAME.is_match(name)
        || segments[..segments.len().saturating_sub(1)]
            .iter()
            .any(|part| ["__tests__", "tests", "test"].contains(part))
    {
        return DiffFileTier::Test;
    }
    DiffFileTier::Source
}
fn strip_extension(path: &str) -> &str {
    let dot = path.rfind('.');
    let slash = path.rfind('/');
    if let Some(dot) = dot
        && slash.is_none_or(|slash| dot > slash)
    {
        &path[..dot]
    } else {
        path
    }
}
fn base_name(path: &str) -> &str {
    strip_extension(path.rsplit('/').next().unwrap_or(""))
}
fn resolve_relative(from_path: &str, specifier: &str) -> String {
    let mut segments: Vec<&str> = from_path.split('/').collect();
    segments.pop();
    for part in specifier.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            _ => segments.push(part),
        }
    }
    segments.join("/")
}
fn tested_base_name(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or("");
    strip_extension(&TESTED_NAME.replace(name, "")).to_owned()
}
/// Source dependencies first, then covering tests, then generated output.
pub fn order_diff_files(files: &[OrderableDiffFile]) -> Vec<OrderableDiffFile> {
    let mut by_path: HashMap<String, OrderableDiffFile> = HashMap::new();
    for file in files {
        by_path.insert(file.path.clone(), file.clone());
    }
    let mut source_paths: Vec<String> = by_path
        .keys()
        .filter(|path| diff_file_tier(path) == DiffFileTier::Source)
        .cloned()
        .collect();
    source_paths.sort_by(|left, right| locale_compare(left, right));
    let mut by_module_path: HashMap<String, String> = HashMap::new();
    let mut by_base_name: HashMap<String, String> = HashMap::new();
    let mut ambiguous: HashSet<String> = HashSet::new();
    for path in &source_paths {
        by_module_path.insert(strip_extension(path).to_owned(), path.clone());
        let base = base_name(path).to_owned();
        if by_base_name.contains_key(&base) {
            ambiguous.insert(base.clone());
        }
        by_base_name.insert(base, path.clone());
    }
    for base in ambiguous {
        by_base_name.remove(&base);
    }
    let mut imports: HashMap<String, HashSet<String>> = HashMap::new();
    for path in &source_paths {
        let file = &by_path[path];
        let mut imported = HashSet::new();
        for line in file.addition_lines.iter().chain(&file.deletion_lines) {
            for captures in IMPORT_SPECIFIER.captures_iter(line) {
                let specifier = captures.get(1).unwrap().as_str();
                let with_extension = if specifier.starts_with('.') {
                    resolve_relative(path, specifier)
                } else {
                    specifier.to_owned()
                };
                let resolved = [".ts", ".tsx", ".js", ".jsx", ".mjs", ".cjs"]
                    .iter()
                    .find(|extension| with_extension.ends_with(**extension))
                    .map_or(with_extension.as_str(), |extension| {
                        &with_extension[..with_extension.len() - extension.len()]
                    });
                let target = by_module_path
                    .get(resolved)
                    .or_else(|| by_module_path.get(&format!("{resolved}/index")))
                    .or_else(|| by_base_name.get(base_name(resolved)));
                if let Some(target) = target
                    && target != path
                {
                    imported.insert(target.clone());
                }
            }
        }
        imports.insert(path.clone(), imported);
    }
    // Kahn's algorithm takes the lowest ready path. A cycle takes the lowest path still standing.
    let mut remaining: HashSet<String> = source_paths.iter().cloned().collect();
    let mut ordered_source = Vec::new();
    while !remaining.is_empty() {
        let next = source_paths
            .iter()
            .find(|path| {
                remaining.contains(*path)
                    && imports.get(*path).is_none_or(|dependencies| {
                        dependencies
                            .iter()
                            .all(|dependency| !remaining.contains(dependency))
                    })
            })
            .or_else(|| source_paths.iter().find(|path| remaining.contains(*path)))
            .unwrap()
            .clone();
        remaining.remove(&next);
        ordered_source.push(next);
    }
    let mut positions = HashMap::new();
    for (index, path) in ordered_source.iter().enumerate() {
        positions.entry(base_name(path).to_owned()).or_insert(index);
    }
    let mut tests: Vec<String> = by_path
        .keys()
        .filter(|path| diff_file_tier(path) == DiffFileTier::Test)
        .cloned()
        .collect();
    tests.sort_by(|left, right| {
        positions
            .get(&tested_base_name(left))
            .unwrap_or(&usize::MAX)
            .cmp(
                positions
                    .get(&tested_base_name(right))
                    .unwrap_or(&usize::MAX),
            )
            .then_with(|| locale_compare(left, right))
    });
    let mut generated: Vec<String> = by_path
        .keys()
        .filter(|path| diff_file_tier(path) == DiffFileTier::Generated)
        .cloned()
        .collect();
    generated.sort_by(|left, right| locale_compare(left, right));
    ordered_source
        .into_iter()
        .chain(tests)
        .chain(generated)
        .filter_map(|path| by_path.remove(&path))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    fn file(path: &str, lines: &[&str]) -> OrderableDiffFile {
        OrderableDiffFile {
            path: path.to_owned(),
            addition_lines: lines.iter().map(|s| (*s).to_owned()).collect(),
            deletion_lines: vec![],
        }
    }
    fn order(files: Vec<OrderableDiffFile>) -> Vec<String> {
        order_diff_files(&files)
            .into_iter()
            .map(|file| file.path)
            .collect()
    }
    fn names(paths: &[&str]) -> Vec<String> {
        paths.iter().map(|p| (*p).to_owned()).collect()
    }
    #[test]
    fn source_then_tests_then_generated() {
        let source = ["src/app.ts", "src/dist.ts", "src/testing.ts"];
        let tests = [
            "src/__tests__/app.ts",
            "src/app.spec.tsx",
            "src/app.test.ts",
            "test/app.ts",
            "tests/helpers/app.ts",
        ];
        let generated = [
            "apps/web/package-lock.json",
            "dist/app.js",
            "packages/core/vendor/lib.js",
            "pnpm-lock.yaml",
            "public/app.min.js",
            "src/__snapshots__/app.ts",
            "src/api.generated.ts",
            "src/app.test.ts.snap",
        ];
        let files = generated
            .iter()
            .chain(tests.iter())
            .chain(source.iter())
            .rev()
            .map(|p| file(p, &[]))
            .collect();
        assert_eq!(
            order(files),
            source
                .iter()
                .chain(tests.iter())
                .chain(generated.iter())
                .map(|path| (*path).to_owned())
                .collect::<Vec<_>>()
        );
    }
    #[test]
    fn empty_diff() {
        assert_eq!(order(vec![]), Vec::<String>::new());
    }
    #[test]
    fn dependency_before_importer() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"./b\";"]),
                file("src/b.ts", &[])
            ]),
            names(&["src/b.ts", "src/a.ts"])
        );
    }
    #[test]
    fn follows_chain() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"./b\";"]),
                file("src/b.ts", &["import { c } from \"./c\";"]),
                file("src/c.ts", &[])
            ]),
            names(&["src/c.ts", "src/b.ts", "src/a.ts"])
        );
    }
    #[test]
    fn climbs_directory() {
        assert_eq!(
            order(vec![
                file("src/ui/a.ts", &["import { b } from \"../lib/b\";"]),
                file("src/lib/b.ts", &[])
            ]),
            names(&["src/lib/b.ts", "src/ui/a.ts"])
        );
    }
    #[test]
    fn directory_index() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"./lib\";"]),
                file("src/lib/index.ts", &[])
            ]),
            names(&["src/lib/index.ts", "src/a.ts"])
        );
    }
    #[test]
    fn aliased_basename_fallback() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"~/lib/b\";"]),
                file("lib/b.ts", &[])
            ]),
            names(&["lib/b.ts", "src/a.ts"])
        );
    }
    #[test]
    fn ambiguous_basename_ignored() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"~/somewhere/b\";"]),
                file("one/b.ts", &[]),
                file("two/b.ts", &[])
            ]),
            names(&["one/b.ts", "src/a.ts", "two/b.ts"])
        );
    }
    #[test]
    fn js_specifier_matches_ts_file() {
        assert_eq!(
            order(vec![
                file("src/a.ts", &["import { b } from \"./b.js\";"]),
                file("src/b.ts", &[])
            ]),
            names(&["src/b.ts", "src/a.ts"])
        );
    }
    #[test]
    fn require_and_bare_import() {
        assert_eq!(
            order(vec![
                file(
                    "src/a.ts",
                    &["const b = require(\"./b\");", "import \"./c\";"]
                ),
                file("src/b.ts", &[]),
                file("src/c.ts", &[])
            ]),
            names(&["src/b.ts", "src/c.ts", "src/a.ts"])
        );
    }
    #[test]
    fn cycle_falls_back_to_path() {
        assert_eq!(
            order(vec![
                file("src/b.ts", &["import { a } from \"./a\";"]),
                file("src/a.ts", &["import { b } from \"./b\";"])
            ]),
            names(&["src/a.ts", "src/b.ts"])
        );
    }
    #[test]
    fn unrelated_cluster_by_path() {
        assert_eq!(
            order(vec![
                file("src/ui/b.ts", &[]),
                file("src/lib/z.ts", &[]),
                file("src/lib/a.ts", &[])
            ]),
            names(&["src/lib/a.ts", "src/lib/z.ts", "src/ui/b.ts"])
        );
    }
    #[test]
    fn tests_follow_source_order() {
        assert_eq!(
            order(vec![
                file("pnpm-lock.yaml", &[]),
                file("src/a.test.ts", &[]),
                file("src/__tests__/b.ts", &[]),
                file("src/a.ts", &["import { b } from \"./b\";"]),
                file("src/b.ts", &[]),
                file("dist/a.js", &[])
            ]),
            names(&[
                "src/b.ts",
                "src/a.ts",
                "src/__tests__/b.ts",
                "src/a.test.ts",
                "dist/a.js",
                "pnpm-lock.yaml"
            ])
        );
    }
    #[test]
    fn orphan_tests_last() {
        assert_eq!(
            order(vec![
                file("src/orphan.spec.ts", &[]),
                file("src/a.test.ts", &[]),
                file("src/a.ts", &[])
            ]),
            names(&["src/a.ts", "src/a.test.ts", "src/orphan.spec.ts"])
        );
    }
    #[test]
    fn arrival_order_does_not_matter() {
        let files = vec![
            file("src/a.ts", &["import { b } from \"./b\";"]),
            file("src/b.ts", &[]),
            file("src/c.ts", &[]),
            file("src/a.test.ts", &[]),
            file("yarn.lock", &[]),
        ];
        let mut reverse = files.clone();
        reverse.reverse();
        assert_eq!(order(files), order(reverse));
    }
}

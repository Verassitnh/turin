//! Realistic multi-file fixtures for [`super::mock_source::MockDiffSource`]:
//! old/new contents per file, from which the source derives git-style patches
//! with `@@` hunks (3 lines of context) exactly as `git diff` writes them.

/// One changed file: contents before and after (`None` = absent side).
#[derive(Clone, Copy)]
pub struct FixtureFile {
    pub path: &'static str,
    pub previous_path: Option<&'static str>,
    pub old: Option<&'static str>,
    pub new: Option<&'static str>,
    /// A binary file: no hunks, `Binary files … differ`.
    pub binary: bool,
}

const fn modified(path: &'static str, old: &'static str, new: &'static str) -> FixtureFile {
    FixtureFile {
        path,
        previous_path: None,
        old: Some(old),
        new: Some(new),
        binary: false,
    }
}

const fn added(path: &'static str, new: &'static str) -> FixtureFile {
    FixtureFile {
        path,
        previous_path: None,
        old: None,
        new: Some(new),
        binary: false,
    }
}

const fn deleted(path: &'static str, old: &'static str) -> FixtureFile {
    FixtureFile {
        path,
        previous_path: None,
        old: Some(old),
        new: None,
        binary: false,
    }
}

pub const PANEL_V1: &str = r#"import { useCallback, useMemo, useState } from "react";
import { parsePatchFiles } from "@pierre/diffs/utils/parsePatchFiles";

import { cn } from "~/lib/utils";
import { Button } from "./ui/button";

type DiffLayout = "stacked" | "split";

interface DiffPanelProps {
  readonly patch: string | undefined;
  readonly layout: DiffLayout;
}

const COLLAPSED_BY_DEFAULT = true;
const MAX_VISIBLE_FILES = 40;

function countLines(patch: string): number {
  let count = 0;
  for (const line of patch.split("\n")) {
    if (line.startsWith("+") || line.startsWith("-")) count += 1;
  }
  return count;
}

function useCollapsedFiles(fileKeys: ReadonlyArray<string>) {
  const [collapsed, setCollapsed] = useState<ReadonlySet<string>>(
    () => new Set(COLLAPSED_BY_DEFAULT ? fileKeys : []),
  );
  const toggle = useCallback((key: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  }, []);
  return { collapsed, toggle };
}

function summarize(patch: string | undefined) {
  if (!patch) return { files: 0, lines: 0 };
  const files = parsePatchFiles(patch).flatMap((parsed) => parsed.files);
  return { files: files.length, lines: countLines(patch) };
}

export function DiffPanel({ patch, layout }: DiffPanelProps) {
  const summary = useMemo(() => summarize(patch), [patch]);
  const [wrap, setWrap] = useState(true);

  if (!patch) {
    return <p className="text-xs text-muted-foreground">No patch available.</p>;
  }

  return (
    <section className={cn("flex min-h-0 flex-1 flex-col", layout === "split" && "gap-2")}>
      <header className="flex items-center gap-2 px-3 py-2 text-xs">
        <span>{summary.files} files</span>
        <span>{summary.lines} lines</span>
        <Button size="xs" variant="ghost" onClick={() => setWrap((value) => !value)}>
          {wrap ? "No wrap" : "Wrap"}
        </Button>
      </header>
      <pre className={cn("overflow-auto font-mono text-xs", wrap && "whitespace-pre-wrap")}>
        {patch.slice(0, MAX_VISIBLE_FILES * 200)}
      </pre>
    </section>
  );
}
"#;

fn panel_v2() -> &'static str {
    use std::sync::OnceLock;
    static V2: OnceLock<String> = OnceLock::new();
    V2.get_or_init(|| {
        PANEL_V1
            .replace(
                "import { parsePatchFiles } from \"@pierre/diffs/utils/parsePatchFiles\";\n",
                "import { parsePatchFiles } from \"@pierre/diffs/utils/parsePatchFiles\";\nimport type { FileDiffMetadata } from \"@pierre/diffs\";\n",
            )
            .replace(
                "function countLines(patch: string): number {\n  let count = 0;\n  for (const line of patch.split(\"\\n\")) {\n    if (line.startsWith(\"+\") || line.startsWith(\"-\")) count += 1;\n  }\n  return count;\n}",
                "function countLines(files: ReadonlyArray<FileDiffMetadata>): number {\n  return files.reduce(\n    (total, file) =>\n      total + file.hunks.reduce((sum, hunk) => sum + hunk.additionLines + hunk.deletionLines, 0),\n    0,\n  );\n}",
            )
            .replace(
                "  return { files: files.length, lines: countLines(patch) };",
                "  return { files: files.length, lines: countLines(files) };",
            )
    })
}

fn panel_v3() -> &'static str {
    use std::sync::OnceLock;
    static V3: OnceLock<String> = OnceLock::new();
    V3.get_or_init(|| {
        panel_v2()
            .replace("const COLLAPSED_BY_DEFAULT = true;\nconst MAX_VISIBLE_FILES = 40;", "const COLLAPSED_BY_DEFAULT = true;\nconst MAX_VISIBLE_FILES = 60;")
            .replace(
                "  const [wrap, setWrap] = useState(true);\n",
                "  const [wrap, setWrap] = useState(true);\n  const [ignoreWhitespace, setIgnoreWhitespace] = useState(true);\n",
            )
            .replace(
                "          {wrap ? \"No wrap\" : \"Wrap\"}\n        </Button>\n",
                "          {wrap ? \"No wrap\" : \"Wrap\"}\n        </Button>\n        <Button size=\"xs\" variant=\"ghost\" onClick={() => setIgnoreWhitespace((value) => !value)}>\n          {ignoreWhitespace ? \"Show whitespace\" : \"Hide whitespace\"}\n        </Button>\n",
            )
    })
}

const COLLAPSE_NEW: &str = r#"export function areAllDiffFilesCollapsed(
  fileKeys: ReadonlyArray<string>,
  collapsedFileKeys: ReadonlySet<string>,
): boolean {
  return fileKeys.length > 0 && fileKeys.every((fileKey) => collapsedFileKeys.has(fileKey));
}

export function toggleAllDiffFiles(
  fileKeys: ReadonlyArray<string>,
  collapsedFileKeys: ReadonlySet<string>,
): ReadonlySet<string> {
  return areAllDiffFilesCollapsed(fileKeys, collapsedFileKeys) ? new Set() : new Set(fileKeys);
}
"#;

const COLLAPSE_TEST_NEW: &str = r#"import { describe, expect, it } from "vitest";

import { areAllDiffFilesCollapsed, toggleAllDiffFiles } from "./diffCollapse";

const FILE_KEYS = ["src/app.ts", "src/index.ts"];

describe("diff collapse controls", () => {
  it("reports whether every rendered file is collapsed", () => {
    expect(areAllDiffFilesCollapsed(FILE_KEYS, new Set(FILE_KEYS))).toBe(true);
    expect(areAllDiffFilesCollapsed([], new Set())).toBe(false);
  });

  it("expands all files when every rendered file is collapsed", () => {
    expect(toggleAllDiffFiles(FILE_KEYS, new Set(FILE_KEYS))).toEqual(new Set());
  });
});
"#;

const NAMES_OLD: &str = r#"export function displayName(path: string): string {
  const index = path.lastIndexOf("/");
  return index === -1 ? path : path.slice(index + 1);
}

export function parentDirectory(path: string): string {
  const index = path.lastIndexOf("/");
  return index === -1 ? "" : path.slice(0, index);
}
"#;

const NAMES_NEW: &str = r#"/** The last path segment, or the whole path when it has none. */
export function displayName(path: string): string {
  const index = path.lastIndexOf("/");
  return index === -1 ? path : path.slice(index + 1);
}

export function parentDirectory(path: string): string {
  const index = path.lastIndexOf("/");
  return index === -1 ? "" : path.slice(0, index);
}

export function joinPath(...segments: ReadonlyArray<string>): string {
  return segments.filter((segment) => segment.length > 0).join("/");
}
"#;

const LEGACY_OLD: &str = r#"// Superseded by lib/names.ts.
module.exports = {
  displayName(path) {
    return path.split("/").pop();
  },
};
"#;

const ROWS_NEW: &str = r#"//! The row model of one rendered file.

use super::pierre::types::{FileDiffMetadata, SelectionSide};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Addition,
    Deletion,
}

/// One gutter + content cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cell {
    pub kind: LineKind,
    pub number: usize,
    pub line_index: usize,
    pub side: SelectionSide,
}

/// `totalLines` of `createPreElement`: sizes the number column.
pub fn total_lines(file: &FileDiffMetadata) -> usize {
    file.addition_lines
        .len()
        .max(file.deletion_lines.len())
}
"#;

const CARGO_OLD: &str = r#"[package]
name = "turin"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
regex.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true

[dev-dependencies]
pretty_assertions.workspace = true
"#;

const CARGO_NEW: &str = r#"[package]
name = "turin"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
regex.workspace = true
serde.workspace = true
serde_json.workspace = true
similar = "2"
tracing.workspace = true

[dev-dependencies]
pretty_assertions.workspace = true
"#;

const README_OLD: &str = r#"# Turin

A Rust/GPUI port of T3 Code's desktop UI.

## Building

```sh
cargo build -p turin
```

## Gate

`scripts/check.sh -p turin` must pass before a task is handed back.
"#;

const README_NEW: &str = r#"# Turin

A Rust/GPUI port of T3 Code's desktop UI.

## Building

```sh
cargo build -p turin
cargo run -p turin
```

## Gate

`scripts/check.sh -p turin` must pass before a task is handed back.
It runs `cargo fmt`, `cargo build`, `cargo test` and `cargo clippy`.
"#;

pub const PANEL_PATH: &str = "apps/web/src/components/DiffPanel.tsx";

/// Turn sets cycle over a thread's checkpoints.
pub fn turn_sets() -> Vec<Vec<FixtureFile>> {
    vec![
        vec![
            added("crates/app/src/diff/rows.rs", ROWS_NEW),
            modified("crates/app/Cargo.toml", CARGO_OLD, CARGO_NEW),
        ],
        vec![
            modified(PANEL_PATH, PANEL_V1, panel_v2()),
            added("apps/web/src/lib/diffCollapse.ts", COLLAPSE_NEW),
            added("apps/web/src/lib/diffCollapse.test.ts", COLLAPSE_TEST_NEW),
            FixtureFile {
                path: "apps/web/src/lib/names.ts",
                previous_path: Some("apps/web/src/names.ts"),
                old: Some(NAMES_OLD),
                new: Some(NAMES_NEW),
                binary: false,
            },
        ],
        vec![
            modified(PANEL_PATH, panel_v2(), panel_v3()),
            deleted("apps/web/legacy/unused.js", LEGACY_OLD),
            FixtureFile {
                path: "apps/web/public/logo.png",
                previous_path: None,
                old: None,
                new: None,
                binary: true,
            },
            modified("README.md", README_OLD, README_NEW),
        ],
    ]
}

/// `TURIN_DIFF_STRESS=<files>`: that many generated 600-line new files, to
/// exercise virtualization.
fn stress_files(count: usize) -> Vec<FixtureFile> {
    (0..count)
        .map(|index| {
            let body: String = (0..600)
                .map(|line| {
                    format!(
                        "pub fn generated_{index}_{line}(value: usize) -> usize {{ value.wrapping_mul({}) + {line} }}\n",
                        line + 3
                    )
                })
                .collect();
            let path: &'static str = Box::leak(format!("crates/stress/src/module_{index:03}.rs").into_boxed_str());
            let body: &'static str = Box::leak(body.into_boxed_str());
            added(path, body)
        })
        .collect()
}

/// "Changes": everything since the base branch.
pub fn branch_changes() -> Vec<FixtureFile> {
    if let Some(count) = std::env::var("TURIN_DIFF_STRESS")
        .ok()
        .and_then(|value| value.parse().ok())
    {
        return stress_files(count);
    }
    let mut files = vec![
        added("crates/app/src/diff/rows.rs", ROWS_NEW),
        modified("crates/app/Cargo.toml", CARGO_OLD, CARGO_NEW),
        modified(PANEL_PATH, PANEL_V1, panel_v3()),
        added("apps/web/src/lib/diffCollapse.ts", COLLAPSE_NEW),
        added("apps/web/src/lib/diffCollapse.test.ts", COLLAPSE_TEST_NEW),
        FixtureFile {
            path: "apps/web/src/lib/names.ts",
            previous_path: Some("apps/web/src/names.ts"),
            old: Some(NAMES_OLD),
            new: Some(NAMES_NEW),
            binary: false,
        },
        deleted("apps/web/legacy/unused.js", LEGACY_OLD),
        FixtureFile {
            path: "apps/web/public/logo.png",
            previous_path: None,
            old: None,
            new: None,
            binary: true,
        },
        modified("README.md", README_OLD, README_NEW),
    ];
    files.sort_by(|a, b| a.path.cmp(b.path));
    files
}

/// "Uncommitted": the working tree against HEAD.
pub fn uncommitted_changes() -> Vec<FixtureFile> {
    vec![
        modified(PANEL_PATH, panel_v2(), panel_v3()),
        modified("README.md", README_OLD, README_NEW),
    ]
}

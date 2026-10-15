//! Port of upstream/packages/source-control-github/src/server/gitHubChangeRequestTemplate.ts
//!
//! GitHub's pull request template convention: the first single template found at a known path,
//! or the only Markdown file in a template directory, read from the committed base tree.

// `GitCommandError` is the host port's own error type.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use turin_contracts::git::GitCommandError;

use crate::shared::js::js_trim;
use crate::source_control_core::host::{SourceControlGitExecuteInput, SourceControlHost};

const TEMPLATE_MAX_BYTES: usize = 8_000;
const TREE_LIST_MAX_BYTES: usize = 100_000;
const TRUNCATION_MARKER: &str = "[truncated]";

const TEMPLATE_PATHS: [&str; 6] = [
    ".github/pull_request_template.md",
    ".github/PULL_REQUEST_TEMPLATE.md",
    "pull_request_template.md",
    "PULL_REQUEST_TEMPLATE.md",
    "docs/pull_request_template.md",
    "docs/PULL_REQUEST_TEMPLATE.md",
];

const TEMPLATE_DIRECTORIES: [&str; 3] = [
    ".github/PULL_REQUEST_TEMPLATE",
    "PULL_REQUEST_TEMPLATE",
    "docs/PULL_REQUEST_TEMPLATE",
];

static OBJECT_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[0-9a-f]{40,64}$").unwrap());

#[derive(Debug, Clone)]
struct TemplateTreeEntry {
    object_id: String,
    path: String,
}

fn parse_template_tree_entries(output: &str) -> Vec<TemplateTreeEntry> {
    let mut entries = Vec::new();
    for record in output.split('\0') {
        if record.is_empty() {
            continue;
        }
        let Some(separator) = record.find('\t') else {
            continue;
        };
        let mut head = record[..separator].split(' ');
        let mode = head.next();
        let kind = head.next();
        let object_id = head.next().unwrap_or_default();
        if kind != Some("blob")
            || (mode != Some("100644") && mode != Some("100755"))
            || object_id.is_empty()
            || !OBJECT_ID.is_match(object_id)
        {
            continue;
        }
        entries.push(TemplateTreeEntry {
            object_id: object_id.to_owned(),
            path: record[separator + 1..].to_owned(),
        });
    }
    entries
}

async fn read_template_blob(
    cwd: &str,
    host: &dyn SourceControlHost,
    entry: &TemplateTreeEntry,
) -> Result<Option<String>, GitCommandError> {
    let result = host
        .git_execute(SourceControlGitExecuteInput {
            operation: "gitHubChangeRequestTemplate.readTemplateBlob".into(),
            cwd: cwd.to_owned(),
            args: vec!["cat-file".into(), "blob".into(), entry.object_id.clone()],
            max_output_bytes: Some(TEMPLATE_MAX_BYTES),
            append_truncation_marker: true,
        })
        .await?;
    let template = js_trim(&result.stdout);
    if template.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        if result.stdout_truncated && !template.ends_with(TRUNCATION_MARKER) {
            format!("{template}\n\n{TRUNCATION_MARKER}")
        } else {
            template.to_owned()
        },
    ))
}

enum DirectoryTemplateResult {
    None,
    Ambiguous,
    Template(String),
}

async fn read_template_directory(
    cwd: &str,
    host: &dyn SourceControlHost,
    entries: &[TemplateTreeEntry],
    directory: &str,
) -> Result<DirectoryTemplateResult, GitCommandError> {
    let prefix = format!("{directory}/");
    let candidates = entries.iter().filter(|entry| {
        let Some(relative_path) = entry.path.strip_prefix(&prefix) else {
            return false;
        };
        !relative_path.contains('/') && relative_path.to_lowercase().ends_with(".md")
    });

    let mut templates: Vec<String> = Vec::new();
    for entry in candidates {
        if let Some(template) = read_template_blob(cwd, host, entry).await? {
            templates.push(template);
            if templates.len() > 1 {
                return Ok(DirectoryTemplateResult::Ambiguous);
            }
        }
    }
    Ok(match templates.into_iter().next() {
        Some(template) => DirectoryTemplateResult::Template(template),
        None => DirectoryTemplateResult::None,
    })
}

/// The pull request template committed at `treeish`, or None when there is none, when a template
/// directory holds several, or when git cannot say.
pub async fn detect(cwd: &str, treeish: &str, host: &dyn SourceControlHost) -> Option<String> {
    // Any git failure reads as "no template" (upstream `Effect.orElseSucceed(() => Option.none())`).
    detect_inner(cwd, treeish, host).await.unwrap_or(None)
}

async fn detect_inner(
    cwd: &str,
    treeish: &str,
    host: &dyn SourceControlHost,
) -> Result<Option<String>, GitCommandError> {
    // Worktree paths can be replaced between validation and open. Read regular blobs from the
    // committed base tree so repository-controlled symlinks and path races never reach the host
    // filesystem.
    let mut args: Vec<String> = ["ls-tree", "-r", "-z", "--full-tree", treeish, "--"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    args.extend(
        TEMPLATE_PATHS
            .iter()
            .chain(TEMPLATE_DIRECTORIES.iter())
            .map(|path| (*path).to_owned()),
    );
    let result = host
        .git_execute(SourceControlGitExecuteInput {
            operation: "gitHubChangeRequestTemplate.listTemplates".into(),
            cwd: cwd.to_owned(),
            args,
            max_output_bytes: Some(TREE_LIST_MAX_BYTES),
            append_truncation_marker: true,
        })
        .await?;
    if result.stdout_truncated {
        return Ok(None);
    }

    let entries = parse_template_tree_entries(&result.stdout);
    // `new Map(entries.map(...))`: a later entry for the same path wins.
    let entries_by_path: HashMap<&str, &TemplateTreeEntry> = entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    for template_path in TEMPLATE_PATHS {
        let Some(entry) = entries_by_path.get(template_path) else {
            continue;
        };
        if let Some(template) = read_template_blob(cwd, host, entry).await? {
            return Ok(Some(template));
        }
    }

    for directory in TEMPLATE_DIRECTORIES {
        match read_template_directory(cwd, host, &entries, directory).await? {
            DirectoryTemplateResult::Template(template) => return Ok(Some(template)),
            DirectoryTemplateResult::Ambiguous => return Ok(None),
            DirectoryTemplateResult::None => {}
        }
    }

    Ok(None)
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::process::Command;

    use super::*;
    use crate::github::credentials::test_support::TestHost;
    use crate::source_control_core::host::SourceControlGitOutput;

    /// The driver appends this to output it cut at `maxOutputBytes` (GitVcsDriverCore).
    const OUTPUT_TRUNCATED_MARKER: &str = "\n\n[truncated]";

    fn git_command(cwd: &Path) -> Command {
        let mut command = Command::new("git");
        command
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TERMINAL_PROMPT", "0");
        command
    }

    /// Runs the real `git` in a temporary repository, the way the app's git driver does, cutting
    /// output at `max_output_bytes` and marking the cut.
    fn real_git_host() -> TestHost {
        let mut host = TestHost::new();
        host.git_execute = Some(Box::new(|input| {
            let output = git_command(Path::new(&input.cwd))
                .args(&input.args)
                .output()
                .expect("git runs");
            if !output.status.success() {
                return Err(GitCommandError::GitCommandError {
                    operation: input.operation,
                    command: "git".into(),
                    cwd: input.cwd,
                    argument_count: None,
                    exit_code: output.status.code().map(f64::from),
                    stdout_length: None,
                    stderr_length: None,
                    output_length: None,
                    reason: None,
                    detail: String::from_utf8_lossy(&output.stderr).into_owned(),
                    cause: None,
                });
            }
            let mut stdout = output.stdout;
            let mut truncated = false;
            if let Some(max) = input.max_output_bytes
                && stdout.len() > max
            {
                stdout.truncate(max);
                truncated = true;
            }
            let mut text = String::from_utf8_lossy(&stdout).into_owned();
            if truncated && input.append_truncation_marker {
                text.push_str(OUTPUT_TRUNCATED_MARKER);
            }
            Ok(SourceControlGitOutput {
                exit_code: 0,
                stdout: text,
                stderr: String::new(),
                stdout_truncated: truncated,
                stderr_truncated: false,
            })
        }));
        host
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = git_command(cwd).args(args).output().expect("git runs");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn temp_repository() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        run_git(dir.path(), &["init", "--initial-branch=main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        dir
    }

    fn write_template(cwd: &Path, relative_path: &str, contents: &str) -> std::path::PathBuf {
        let path = cwd.join(relative_path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
        path
    }

    fn commit_templates(cwd: &Path) {
        run_git(cwd, &["add", "-A"]);
        run_git(
            cwd,
            &[
                "commit",
                "--allow-empty",
                "-m",
                "Add pull request templates",
            ],
        );
    }

    async fn detect_template(cwd: &Path, treeish: &str) -> Option<String> {
        detect(cwd.to_str().unwrap(), treeish, &real_git_host()).await
    }

    #[tokio::test]
    async fn recognizes_each_single_template_path() {
        for relative_path in TEMPLATE_PATHS {
            let repo = temp_repository();
            write_template(
                repo.path(),
                relative_path,
                &format!("template from {relative_path}"),
            );
            commit_templates(repo.path());
            assert_eq!(
                detect_template(repo.path(), "HEAD").await,
                Some(format!("template from {relative_path}")),
                "{relative_path}"
            );
        }
    }

    #[tokio::test]
    async fn reads_templates_from_the_requested_base_tree() {
        let repo = temp_repository();
        let cwd = repo.path();
        write_template(cwd, "README.md", "initial\n");
        commit_templates(cwd);
        run_git(cwd, &["branch", "feature"]);
        write_template(cwd, ".github/pull_request_template.md", "base template");
        commit_templates(cwd);
        run_git(cwd, &["checkout", "feature"]);

        assert_eq!(detect_template(cwd, "HEAD").await, None);
        assert_eq!(
            detect_template(cwd, "main").await,
            Some("base template".into())
        );
    }

    #[tokio::test]
    async fn uses_the_first_non_empty_template_in_the_configured_path_order() {
        let repo = temp_repository();
        let cwd = repo.path();
        write_template(cwd, ".github/pull_request_template.md", " \n");
        write_template(
            cwd,
            ".github/PULL_REQUEST_TEMPLATE.md",
            "  ## Preferred template  \n",
        );
        write_template(cwd, "pull_request_template.md", "## Later template");
        commit_templates(cwd);
        // Where the filesystem ignores case the two `.github` files are one file, and the
        // second write is the one that survives.
        assert_eq!(
            detect_template(cwd, "HEAD").await,
            Some("## Preferred template".into())
        );
    }

    #[tokio::test]
    async fn recognizes_each_template_directory() {
        for relative_directory in TEMPLATE_DIRECTORIES {
            let repo = temp_repository();
            write_template(
                repo.path(),
                &format!("{relative_directory}/template.MD"),
                "directory template",
            );
            commit_templates(repo.path());
            assert_eq!(
                detect_template(repo.path(), "HEAD").await,
                Some("directory template".into()),
                "{relative_directory}"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skips_unusable_directory_entries_and_uses_the_one_valid_template() {
        let repo = temp_repository();
        let cwd = repo.path();
        let template_directory = cwd.join(".github").join("PULL_REQUEST_TEMPLATE");
        std::fs::create_dir_all(template_directory.join("b-directory.md")).unwrap();
        std::fs::write(template_directory.join("a-empty.md"), " \n").unwrap();
        std::os::unix::fs::symlink(
            template_directory.join("missing.md"),
            template_directory.join("c-broken.md"),
        )
        .unwrap();
        std::fs::write(template_directory.join("z-valid.md"), "valid").unwrap();
        commit_templates(cwd);

        assert_eq!(detect_template(cwd, "HEAD").await, Some("valid".into()));
    }

    #[tokio::test]
    async fn does_not_guess_between_multiple_directory_templates() {
        let repo = temp_repository();
        let cwd = repo.path();
        write_template(cwd, ".github/PULL_REQUEST_TEMPLATE/a.md", "first");
        write_template(cwd, ".github/PULL_REQUEST_TEMPLATE/b.md", "second");
        write_template(cwd, "PULL_REQUEST_TEMPLATE/fallback.md", "fallback");
        commit_templates(cwd);

        assert_eq!(detect_template(cwd, "HEAD").await, None);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejects_a_committed_template_symlink_escaping_the_repository() {
        let repo = temp_repository();
        let cwd = repo.path();
        let outside = tempfile::tempdir().unwrap();
        let outside_template = outside.path().join("secret.md");
        std::fs::write(&outside_template, "LOCAL_SECRET_SENTINEL").unwrap();
        let escaped = cwd.join(".github").join("pull_request_template.md");
        std::fs::create_dir_all(escaped.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&outside_template, &escaped).unwrap();
        write_template(cwd, "pull_request_template.md", "safe template");
        commit_templates(cwd);

        let template = detect_template(cwd, "HEAD").await;
        assert_eq!(template.as_deref(), Some("safe template"));
        assert!(!template.unwrap().contains("LOCAL_SECRET_SENTINEL"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reads_the_committed_template_when_a_worktree_parent_is_replaced() {
        let repo = temp_repository();
        let cwd = repo.path();
        let outside = tempfile::tempdir().unwrap();
        let template_path = write_template(
            cwd,
            ".github/pull_request_template.md",
            "committed template",
        );
        commit_templates(cwd);
        write_template(
            outside.path(),
            "pull_request_template.md",
            "LOCAL_SECRET_SENTINEL",
        );

        let template_directory = template_path.parent().unwrap();
        std::fs::rename(template_directory, cwd.join(".github-original")).unwrap();
        std::os::unix::fs::symlink(outside.path(), template_directory).unwrap();

        let template = detect_template(cwd, "HEAD").await;
        assert_eq!(template.as_deref(), Some("committed template"));
        assert!(!template.unwrap().contains("LOCAL_SECRET_SENTINEL"));
    }

    #[tokio::test]
    async fn bounds_template_reads_and_marks_truncated_content() {
        let repo = temp_repository();
        let cwd = repo.path();
        let prefix = "a".repeat(8_000);
        write_template(
            cwd,
            ".github/pull_request_template.md",
            &format!("{prefix}SECRET_SENTINEL"),
        );
        commit_templates(cwd);

        let template = detect_template(cwd, "HEAD").await.expect("a template");
        assert_eq!(template, format!("{prefix}\n\n[truncated]"));
        assert_eq!(template.matches("[truncated]").count(), 1);
        assert!(!template.contains("SECRET_SENTINEL"));
    }

    #[tokio::test]
    async fn a_git_failure_reads_as_no_template() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(detect_template(dir.path(), "HEAD").await, None);
    }

    #[test]
    fn parses_only_regular_blob_records() {
        let id = "a".repeat(40);
        let output = format!(
            "100644 blob {id}\ta.md\0120000 blob {id}\tlink.md\0100755 blob {id}\tb.md\0040000 tree {id}\tdir\0100644 blob nothex\tc.md\0no tab\0\0100644 blob {id}\tpath with\ttab.md"
        );
        let entries = parse_template_tree_entries(&output);
        assert_eq!(
            entries.iter().map(|e| e.path.as_str()).collect::<Vec<_>>(),
            vec!["a.md", "b.md", "path with\ttab.md"]
        );
    }
}

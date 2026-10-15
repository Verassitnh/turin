//! Best-effort detection of repo files a hook command runs that the trust
//! hash does not cover.
//!
//! Approval hashes everything under `<project>/.turin/`. A command such as
//! `./scripts/setup.sh` or `sh build.sh` runs code that lives elsewhere in the
//! repo, so a `git pull` or an agent edit can change it without re-approval.
//! Those commands are allowed, but the pending notice lists them as
//! [`UnhashedReference`]s so the UI can warn. This is a heuristic scan of the
//! command text, not a shell parser: it can miss paths built at runtime.

use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::config::{HookCwd, HookSpec};

/// A file in the project, outside `.turin/`, that a hook command appears to
/// run or read, and that approval therefore does not cover.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UnhashedReference {
    /// Index of the hook inside the project's `hooks.toml`.
    pub hook_index: usize,
    pub command: String,
    /// The referenced path, absolute (lexically normalized).
    pub path: PathBuf,
    /// The hook runs with `cwd = "thread"`: the path resolves inside the
    /// thread's worktree, which an agent can write.
    pub in_thread_worktree: bool,
}

/// Scans `hooks` for command words that name project files outside
/// `config_dir`. `project_root` and `config_dir` must be canonical.
pub fn unhashed_references(
    hooks: &[HookSpec],
    project_root: &Path,
    config_dir: &Path,
) -> Vec<UnhashedReference> {
    let mut found: Vec<UnhashedReference> = Vec::new();
    for hook in hooks {
        let thread = hook.cwd == HookCwd::Thread;
        for word in command_words(&hook.command) {
            let Some(path) = resolve_reference(&word, project_root, config_dir, thread) else {
                continue;
            };
            if found
                .iter()
                .any(|seen| seen.hook_index == hook.index && seen.path == path)
            {
                continue;
            }
            found.push(UnhashedReference {
                hook_index: hook.index,
                command: hook.command.clone(),
                path,
                in_thread_worktree: thread,
            });
        }
    }
    found
}

/// Splits on whitespace and shell operators, strips quotes, and takes the
/// value of `--flag=value` words.
fn command_words(command: &str) -> Vec<String> {
    command
        .split(|c: char| c.is_whitespace() || ";&|()<>`".contains(c))
        .map(|word| word.trim_matches(|c| c == '"' || c == '\''))
        .map(|word| match word.split_once('=') {
            Some((flag, value)) if flag.starts_with('-') => value,
            _ => word,
        })
        .map(|word| word.trim_matches(|c| c == '"' || c == '\'').to_owned())
        .filter(|word| !word.is_empty())
        .collect()
}

fn resolve_reference(
    word: &str,
    project_root: &Path,
    config_dir: &Path,
    thread: bool,
) -> Option<PathBuf> {
    if word.starts_with('-') || word.contains("://") {
        return None;
    }
    // Variables that point at the project (or its worktree copy) are
    // expanded; anything else dynamic is out of reach.
    let mut word = word.to_owned();
    for var in [
        "TURIN_PROJECT_ROOT",
        "TURIN_WORKTREE_PATH",
        "TURIN_CWD",
        "PWD",
    ] {
        for prefix in [format!("${{{var}}}"), format!("${var}")] {
            if let Some(rest) = word.strip_prefix(&prefix) {
                word = format!("{}{rest}", project_root.display());
                break;
            }
        }
    }
    if word.starts_with('$') || word.starts_with('~') {
        return None;
    }
    let candidate = Path::new(&word);
    let relative = candidate.is_relative();
    let has_slash = word.contains('/');
    let joined = normalize_lexically(&if relative {
        project_root.join(candidate)
    } else {
        candidate.to_path_buf()
    });
    let resolved = std::fs::canonicalize(&joined).unwrap_or(joined);
    if !resolved.starts_with(project_root) || resolved.starts_with(config_dir) {
        return None;
    }
    let is_file = resolved.is_file();
    // Slash-less words are commands and flags unless they name a file in the
    // project root (`sh setup.sh`). Missing paths only count for
    // `cwd = "thread"`, where a relative path lands in the worktree.
    let counts = if has_slash {
        is_file || (thread && relative && !resolved.exists())
    } else {
        is_file
    };
    counts.then_some(resolved)
}

/// Resolves `.` and `..` without touching the filesystem.
pub(crate) fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(component);
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::HookConfigSource;
    use crate::event::HookEventKind;
    use pretty_assertions::assert_eq;

    fn hook(index: usize, command: &str, cwd: HookCwd) -> HookSpec {
        HookSpec {
            event: HookEventKind::ThreadCreated,
            command: command.to_owned(),
            cwd,
            blocking: false,
            timeout_secs: 5,
            source: HookConfigSource::Project,
            index,
        }
    }

    struct Fixture {
        _dir: tempfile::TempDir,
        root: PathBuf,
        config_dir: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let config_dir = root.join(".turin");
        std::fs::create_dir_all(config_dir.join("hooks")).unwrap();
        std::fs::create_dir_all(root.join("scripts")).unwrap();
        std::fs::write(root.join("scripts/setup.sh"), "true").unwrap();
        std::fs::write(root.join("build.sh"), "true").unwrap();
        std::fs::write(config_dir.join("hooks/ok.sh"), "true").unwrap();
        Fixture {
            _dir: dir,
            root,
            config_dir,
        }
    }

    fn paths(f: &Fixture, hooks: &[HookSpec]) -> Vec<PathBuf> {
        unhashed_references(hooks, &f.root, &f.config_dir)
            .into_iter()
            .map(|r| r.path)
            .collect()
    }

    #[test]
    fn flags_repo_scripts_outside_turin() {
        let f = fixture();
        let hooks = [
            hook(0, "./scripts/setup.sh --fast", HookCwd::Project),
            hook(1, "sh build.sh && echo done", HookCwd::Project),
            hook(
                2,
                "\"$TURIN_PROJECT_ROOT/scripts/setup.sh\"",
                HookCwd::Project,
            ),
            hook(3, "bash ../elsewhere/x.sh", HookCwd::Project),
        ];
        let found = unhashed_references(&hooks, &f.root, &f.config_dir);
        let by_hook: Vec<(usize, PathBuf)> = found
            .iter()
            .map(|r| (r.hook_index, r.path.clone()))
            .collect();
        assert_eq!(
            by_hook,
            [
                (0, f.root.join("scripts/setup.sh")),
                (1, f.root.join("build.sh")),
                (2, f.root.join("scripts/setup.sh")),
            ]
        );
        assert!(found.iter().all(|r| !r.in_thread_worktree));
    }

    #[test]
    fn ignores_turin_dir_commands_urls_and_plain_words() {
        let f = fixture();
        let hooks = [
            hook(0, "./.turin/hooks/ok.sh", HookCwd::Project),
            hook(1, "sh .turin/hooks/ok.sh", HookCwd::Project),
            hook(2, "curl https://example.com/x.sh | sh", HookCwd::Project),
            hook(3, "git push --quiet", HookCwd::Project),
            hook(4, "touch out/missing.txt", HookCwd::Project),
            hook(5, "cd scripts && ls", HookCwd::Project),
            hook(6, "echo $HOME/x", HookCwd::Project),
        ];
        assert_eq!(paths(&f, &hooks), Vec::<PathBuf>::new());
    }

    #[test]
    fn thread_cwd_flags_relative_paths_even_when_missing() {
        let f = fixture();
        let hooks = [hook(0, "./scripts/new.sh", HookCwd::Thread)];
        let found = unhashed_references(&hooks, &f.root, &f.config_dir);
        assert_eq!(found.len(), 1);
        assert!(found[0].in_thread_worktree);
        assert_eq!(found[0].path, f.root.join("scripts/new.sh"));
    }
}

//! Port of upstream/apps/server/src/project/ManagedProjectFolders.ts
//!
//! The project folders T3 Code makes for the user under its data dir, rather
//! than ones the user picks:
//!
//! - `<baseDir>/scratch`: the Scratch project ("No project"), with a folder of
//!   its own for each thread;
//! - `<baseDir>/projects/<slug>`: projects started from just a name, each a new
//!   Git repository with a README, an icon, and a first commit.

use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use regex::Regex;
use tokio::sync::OnceCell;
use turin_contracts::base_schemas::TrimmedNonEmptyString;
use turin_contracts::project::{ProjectIconColor, ProjectIconOverride};
use turin_contracts::{ProjectId, ThreadId};
use uuid::Uuid;

use crate::project_folder_name::new_project_folder_name;
use crate::project_ports::PortError;
use crate::project_service::{
    command_id, create_payload, project_id, trimmed, update_payload, ProjectCreateInput,
    ProjectServiceApi, ProjectServiceError, ProjectUpdateInput,
};

const MAX_NAMED_FOLDER_ATTEMPTS: usize = 100;

// Tailwind 600 shades: dark enough for white initials on every hue.
const ICON_BACKGROUNDS: [&str; 15] = [
    "#dc2626", "#ea580c", "#d97706", "#16a34a", "#059669", "#0d9488", "#0891b2", "#0284c7",
    "#2563eb", "#4f46e5", "#7c3aed", "#9333ea", "#c026d3", "#db2777", "#e11d48",
];

static COMMIT_IDENTITY_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)identity unknown|tell me who you are|no (name|email) was given")
        .expect("valid identity regex")
});

/// Why the Scratch folder is not offered.
#[derive(Debug, thiserror::Error)]
pub enum ScratchError {
    #[error("Threads without a project are not available on this environment.")]
    Unavailable,
    #[error(transparent)]
    Folder(#[from] ScratchFolderError),
    #[error("Failed to create the project for threads without a project.")]
    Project {
        workspace_root: String,
        #[source]
        cause: PortError,
    },
}

#[derive(Debug, thiserror::Error)]
#[error("Failed to create the folder for threads without a project.")]
pub struct ScratchFolderError {
    pub folder: String,
    #[source]
    pub cause: PortError,
}

#[derive(Debug, thiserror::Error)]
pub enum NamedProjectError {
    #[error("Failed to create the project folder.")]
    Folder {
        folder: String,
        #[source]
        cause: PortError,
    },
    #[error("Failed to create the project.")]
    Create {
        workspace_root: String,
        #[source]
        cause: PortError,
    },
}

/// A project started from a name. `commit_error` is set when the first commit
/// failed; the project is kept either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedProject {
    pub project_id: ProjectId,
    pub workspace_root: String,
    pub commit_error: Option<String>,
}

/// Input for [`ManagedProjectFolders::folder_for_thread`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadFolderInput {
    pub project_id: ProjectId,
    pub thread_id: ThreadId,
    pub text: String,
}

/// Why a Git probe failed. Upstream tells "git is not installed" apart from
/// every other failure, because the two hide Scratch for different reasons.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RepositoryProbeError {
    #[error("git is not installed")]
    GitNotInstalled,
    #[error("{0}")]
    Failed(String),
}

/// Upstream `GitWorkflowService.isRepository`: is `cwd` inside a work tree?
#[async_trait]
pub trait RepositoryProbe: Send + Sync {
    async fn is_repository(&self, cwd: &str) -> Result<bool, RepositoryProbeError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecute {
    pub operation: &'static str,
    pub cwd: String,
    pub args: Vec<String>,
    pub timeout: Duration,
    pub allow_non_zero_exit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitExecuted {
    pub exit_code: i32,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{operation} failed: {message}")]
pub struct GitCliError {
    pub operation: String,
    pub message: String,
}

/// The git commands this service runs. Upstream `GitVcsDriver`.
#[async_trait]
pub trait GitCli: Send + Sync {
    /// `git config --get`-style read. `None` when the key is unset.
    async fn read_config_value(&self, cwd: &str, key: &str) -> Result<Option<String>, GitCliError>;
    async fn execute(&self, input: GitExecute) -> Result<GitExecuted, GitCliError>;
}

/// Upstream `ManagedProjectFolders`.
pub struct ManagedProjectFolders {
    base_dir: PathBuf,
    named_projects_root: PathBuf,
    probe: Arc<dyn RepositoryProbe>,
    git: Arc<dyn GitCli>,
    projects: Arc<dyn ProjectServiceApi>,
    /// Probed once. An interrupted probe leaves the cell empty, so the next
    /// caller probes again.
    scratch_root: OnceCell<Option<PathBuf>>,
}

impl ManagedProjectFolders {
    pub fn new(
        base_dir: PathBuf,
        probe: Arc<dyn RepositoryProbe>,
        git: Arc<dyn GitCli>,
        projects: Arc<dyn ProjectServiceApi>,
    ) -> Self {
        // Projects started from just a name live beside Scratch and worktrees,
        // away from folders the user organizes by hand. A nested repository is
        // fine here (unlike Scratch) because each project gets its own `git init`.
        let named_projects_root = resolve_path(&base_dir, "projects");
        Self {
            base_dir,
            named_projects_root,
            probe,
            git,
            projects,
            scratch_root: OnceCell::new(),
        }
    }

    pub fn named_projects_root(&self) -> &Path {
        &self.named_projects_root
    }

    /// The Scratch folder, or `None` when this environment does not offer one.
    ///
    /// Inside a checkout (a dev worktree's .t3, a dotfiles home) the folder
    /// would inherit the repo's git status and checkpoints, so Scratch is
    /// offered only when the data dir is outside any work tree. With no Git
    /// installed nothing can see a checkout, so Scratch is offered. Other
    /// detection failures hide Scratch rather than failing callers.
    pub async fn scratch_root(&self) -> Option<PathBuf> {
        self.scratch_root
            .get_or_init(|| async {
                let base = self.base_dir.to_string_lossy().into_owned();
                match self.probe.is_repository(&base).await {
                    Ok(true) | Err(RepositoryProbeError::Failed(_)) => None,
                    Ok(false) | Err(RepositoryProbeError::GitNotInstalled) => {
                        Some(resolve_path(&self.base_dir, "scratch"))
                    }
                }
            })
            .await
            .clone()
    }

    /// Finds or creates the Scratch project and re-creates its folder.
    pub async fn ensure_scratch_project(&self) -> Result<ProjectId, ScratchError> {
        let root = self.scratch_root().await.ok_or(ScratchError::Unavailable)?;
        let workspace_root = path_string(&root);
        make_dir_all(&root).await.map_err(|cause| ScratchFolderError {
            folder: workspace_root.clone(),
            cause: cause.into(),
        })?;
        let id = Uuid::new_v4().to_string();
        let bootstrapped = self
            .projects
            .bootstrap(ProjectCreateInput {
                command_id: command_id(format!("scratch-project:{id}")),
                project_id: project_id(&id),
                payload: create_payload(
                    trimmed("No project"),
                    workspace_root.clone(),
                ),
            })
            .await;
        let (project_id, created) = match bootstrapped {
            Ok(bootstrap) => (bootstrap.project.id, bootstrap.created),
            // bootstrap looks the root up before taking the workspace lock, so a
            // racing create loses with a conflict that names the winner.
            Err(ProjectServiceError::Conflict {
                conflicting_project_id,
                ..
            }) => (conflicting_project_id, false),
            Err(error) => {
                return Err(ScratchError::Project {
                    workspace_root,
                    cause: error.into(),
                })
            }
        };
        if created {
            // A dashed chat bubble in neutral gray marks Scratch. Set once at
            // create, so a user's own icon choice is never overwritten.
            let mut payload = update_payload();
            payload.project_icon = Some(Some(ProjectIconOverride::Lucide {
                name: turin_contracts::project::ProjectLucideIconName::new(
                    "message-square-dashed",
                )
                .expect("lucide icon name is non-empty"),
                color: ProjectIconColor::Gray,
            }));
            self.projects
                .update(ProjectUpdateInput {
                    command_id: command_id(format!("scratch-project-icon:{id}")),
                    project_id: project_id.clone(),
                    payload,
                })
                .await
                .map_err(|error| ScratchError::Project {
                    workspace_root: workspace_root.clone(),
                    cause: error.into(),
                })?;
        }
        Ok(project_id)
    }

    /// Claims a fresh folder for a new thread in the Scratch project, named from
    /// the date, its first message, and its id. `None` for every other project.
    pub async fn folder_for_thread(
        &self,
        input: ThreadFolderInput,
    ) -> Result<Option<PathBuf>, ScratchFolderError> {
        let Some(root) = self.scratch_root().await else {
            return Ok(None);
        };
        if !self.is_scratch_project(&input.project_id, &root).await {
            return Ok(None);
        }
        make_dir_all(&root).await.map_err(|cause| ScratchFolderError {
            folder: path_string(&root),
            cause: cause.into(),
        })?;

        let date = Utc::now().format("%Y-%m-%d").to_string();
        let words = folder_words(&input.text);
        let id: String = input
            .thread_id
            .as_str()
            .to_lowercase()
            .chars()
            .filter(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit())
            .collect();
        let folder_for = |id_part: &str| {
            let name = [date.as_str(), words.as_str(), id_part]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("-");
            root.join(name)
        };
        let full_folder = folder_for(id.as_str());

        // Thread ids often share a prefix ("thread:..."), so the first name uses
        // the id's tail. A taken short name falls back to the full id, which only
        // this thread holds. Ids that normalize alike, or a launch retried without
        // its receipt, can find the full name taken too, so later attempts add a
        // fresh random suffix; the names never run out.
        let claimed = claim_free_folder(|attempt| {
            Some(match attempt {
                1 => folder_for(last_chars(&id, 8)),
                2 => full_folder.clone(),
                _ => {
                    let uuid = Uuid::new_v4().to_string();
                    PathBuf::from(format!(
                        "{}-{}",
                        path_string(&full_folder),
                        &uuid[..8]
                    ))
                }
            })
        })
        .await
        .map_err(|error| ScratchFolderError {
            folder: path_string(&error.folder),
            cause: error.cause.into(),
        })?;
        Ok(claimed)
    }

    /// Starts a project from just a name: claims `<namedProjectsRoot>/<slug>`
    /// (adding `-2`, `-3`, ... when taken), makes it a Git repository with a
    /// README, an icon, and a first commit, then creates the project. A failed
    /// commit (no Git identity, a signing prompt) keeps the project and returns
    /// why in `commit_error`. The folder is removed when the create fails or is
    /// cancelled before the project exists, never once another project owns it.
    pub async fn create_named_project(&self, name: &str) -> Result<NamedProject, NamedProjectError> {
        let workspace_root = self.claim_named_folder(name).await?;
        let root_string = path_string(&workspace_root);
        // Until the create is dispatched nothing else can use the folder, so any
        // exit but success removes it, an interrupt included.
        let mut guard = FolderGuard::armed(workspace_root.clone());
        let title = TrimmedNonEmptyString::new(name).map_err(|error| NamedProjectError::Create {
            workspace_root: root_string.clone(),
            cause: error.into(),
        })?;
        let commit_error = self
            .scaffold_repository(&workspace_root, name)
            .await
            .map_err(|cause| NamedProjectError::Folder {
                folder: root_string.clone(),
                cause,
            })?;
        let id = Uuid::new_v4().to_string();
        guard.disarm();

        let created = self
            .projects
            .create(ProjectCreateInput {
                command_id: command_id(format!("named-project:{id}")),
                project_id: project_id(&id),
                payload: create_payload(title, root_string.clone()),
            })
            .await;
        match created {
            Ok(project) => Ok(NamedProject {
                project_id: project.id,
                workspace_root: root_string,
                commit_error,
            }),
            Err(error) => {
                // A rejected create leaves the folder unused, so remove it. A
                // conflict means another project owns the folder, so it stays;
                // the owner is checked again first, since deleting another
                // project's files is never acceptable.
                if !matches!(error, ProjectServiceError::Conflict { .. }) {
                    if let Ok(None) = self
                        .projects
                        .get_by_workspace_root(&root_string, false)
                        .await
                    {
                        remove_folder_quietly(&workspace_root);
                    }
                }
                Err(NamedProjectError::Create {
                    workspace_root: root_string,
                    cause: error.into(),
                })
            }
        }
    }

    async fn is_scratch_project(&self, project_id: &ProjectId, root: &Path) -> bool {
        match self.projects.get_by_id(project_id, false).await {
            Ok(Some(project)) => {
                normalize_path(Path::new(project.workspace_root.as_str())) == normalize_path(root)
            }
            // An unreadable project is not Scratch; the launch reports its own
            // project lookup failure.
            _ => false,
        }
    }

    async fn claim_named_folder(&self, name: &str) -> Result<PathBuf, NamedProjectError> {
        let root = self.named_projects_root.clone();
        make_dir_all(&root)
            .await
            .map_err(|cause| NamedProjectError::Folder {
                folder: path_string(&root),
                cause: cause.into(),
            })?;
        let folder_name = new_project_folder_name(name);
        let claimed = claim_free_folder(|attempt| {
            (attempt <= MAX_NAMED_FOLDER_ATTEMPTS).then(|| {
                let leaf = if attempt == 1 {
                    folder_name.clone()
                } else {
                    format!("{folder_name}-{attempt}")
                };
                root.join(leaf)
            })
        })
        .await
        .map_err(|error| NamedProjectError::Folder {
            folder: path_string(&error.folder),
            cause: error.cause.into(),
        })?;
        match claimed {
            Some(folder) => Ok(folder),
            None => Err(NamedProjectError::Folder {
                folder: path_string(&root.join(&folder_name)),
                cause: anyhow::anyhow!("Every folder name for \"{folder_name}\" is taken."),
            }),
        }
    }

    /// `git init`, the starter files, and the first commit. Only the commit may
    /// fail softly; everything before it fails the create.
    async fn scaffold_repository(
        &self,
        cwd: &Path,
        name: &str,
    ) -> Result<Option<String>, PortError> {
        let cwd_string = path_string(cwd);
        let branch = match self.git.read_config_value(&cwd_string, "init.defaultBranch").await {
            Ok(Some(branch)) => branch,
            Ok(None) | Err(_) => "main".to_owned(),
        };
        self.git
            .execute(GitExecute {
                operation: "ManagedProjectFolders.init",
                cwd: cwd_string.clone(),
                args: vec!["init".to_owned(), format!("--initial-branch={branch}")],
                timeout: Duration::from_secs(10),
                allow_non_zero_exit: false,
            })
            .await?;
        tokio::fs::write(cwd.join("README.md"), named_project_readme(name)).await?;
        tokio::fs::create_dir(cwd.join("assets")).await?;
        tokio::fs::write(cwd.join("assets").join("icon.svg"), named_project_icon_svg(name)).await?;
        // Named and forced so a global ignore rule (say `*.svg`) cannot drop one.
        self.git
            .execute(GitExecute {
                operation: "ManagedProjectFolders.add",
                cwd: cwd_string.clone(),
                args: ["add", "--force", "--", "README.md", "assets/icon.svg"]
                    .map(str::to_owned)
                    .to_vec(),
                timeout: Duration::from_secs(10),
                allow_non_zero_exit: false,
            })
            .await?;
        let commit = self
            .git
            .execute(GitExecute {
                operation: "ManagedProjectFolders.commit",
                cwd: cwd_string,
                args: ["commit", "--message", "Initial commit"].map(str::to_owned).to_vec(),
                timeout: Duration::from_secs(30),
                allow_non_zero_exit: true,
            })
            .await;
        Ok(match commit {
            Ok(result) if result.exit_code == 0 => None,
            Ok(result) => Some(describe_commit_failure(&result.stderr)),
            Err(error) => Some(error.message),
        })
    }
}

/// Failure to claim a folder with an exclusive create.
struct ClaimError {
    folder: PathBuf,
    cause: std::io::Error,
}

/// Claims the first free folder among `folder_for(1)`, `folder_for(2)`, ...,
/// stopping with `None` when `folder_for` runs out of names. Each folder is
/// created without `recursive`, so creating it is the claim: of two racers for
/// one name, one gets AlreadyExists and moves on to the next.
async fn claim_free_folder(
    folder_for: impl Fn(usize) -> Option<PathBuf>,
) -> Result<Option<PathBuf>, ClaimError> {
    let mut attempt = 1usize;
    loop {
        let Some(folder) = folder_for(attempt) else {
            return Ok(None);
        };
        match tokio::fs::create_dir(&folder).await {
            Ok(()) => return Ok(Some(folder)),
            Err(cause) if cause.kind() == std::io::ErrorKind::AlreadyExists => attempt += 1,
            Err(cause) => return Err(ClaimError { folder, cause }),
        }
    }
}

/// Removes a folder on drop unless it is disarmed. Drop also runs on
/// cancellation, which is what keeps a cancelled create from leaking a folder.
struct FolderGuard {
    folder: Option<PathBuf>,
}

impl FolderGuard {
    fn armed(folder: PathBuf) -> Self {
        Self {
            folder: Some(folder),
        }
    }

    fn disarm(&mut self) {
        self.folder = None;
    }
}

impl Drop for FolderGuard {
    fn drop(&mut self) {
        if let Some(folder) = self.folder.take() {
            remove_folder_quietly(&folder);
        }
    }
}

fn remove_folder_quietly(folder: &Path) {
    // Ignored, as upstream: a folder that cannot be removed is left behind.
    let _ = std::fs::remove_dir_all(folder);
}

async fn make_dir_all(folder: &Path) -> Result<(), std::io::Error> {
    tokio::fs::create_dir_all(folder).await
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Lexical `path.resolve(base, relative)`: joins and removes `.` and `..`
/// without touching the filesystem.
fn resolve_path(base: &Path, relative: &str) -> PathBuf {
    normalize_path(&base.join(relative))
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn last_chars(value: &str, count: usize) -> &str {
    let start = value.len().saturating_sub(count);
    value.get(start..).unwrap_or(value)
}

// Only [a-z0-9] reaches a folder name, so it stays one path segment, and the
// words are capped so a pasted blob cannot outgrow a file name.
fn folder_words(text: &str) -> String {
    let lowered = text.to_lowercase();
    let joined = lowered
        .split(|ch: char| !(ch.is_ascii_lowercase() || ch.is_ascii_digit()))
        .filter(|part| !part.is_empty())
        .take(5)
        .collect::<Vec<_>>()
        .join("-");
    let capped: String = joined.chars().take(48).collect();
    capped.trim_end_matches('-').to_owned()
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// A rounded square with the name's initials, colored by a hash of the name.
/// It lives at `assets/icon.svg`, a path ProjectFaviconResolver already checks,
/// so every machine that clones the project shows the same icon.
fn named_project_icon_svg(name: &str) -> String {
    let words: Vec<&str> = name
        .split(|ch: char| !(ch.is_alphabetic() || ch.is_numeric()))
        .filter(|word| !word.is_empty())
        .collect();
    let joined: String = words
        .iter()
        .take(2)
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_uppercase();
    let initials = if joined.is_empty() {
        name.trim().chars().next().map(String::from).unwrap_or_else(|| "?".to_owned())
    } else {
        joined
    };
    let mut hash: u32 = 0;
    for ch in name.chars() {
        hash = hash.wrapping_mul(31).wrapping_add(ch as u32);
    }
    let background = ICON_BACKGROUNDS[hash as usize % ICON_BACKGROUNDS.len()];
    let font_size = if initials.encode_utf16().count() > 1 { 26 } else { 32 };
    [
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64">"#.to_owned(),
        format!(r#"  <rect width="64" height="64" rx="14" fill="{background}"/>"#),
        format!(
            r##"  <text x="32" y="32" dy="0.35em" text-anchor="middle" font-family="ui-sans-serif, system-ui, -apple-system, sans-serif" font-size="{font_size}" font-weight="600" fill="#ffffff">{}</text>"##,
            escape_xml(&initials)
        ),
        "</svg>".to_owned(),
        String::new(),
    ]
    .join("\n")
}

fn named_project_readme(name: &str) -> String {
    [
        r#"<img src="assets/icon.svg" width="64" height="64" alt="">"#,
        "",
        &format!("# {name}"),
        "",
        "Created in [T3 Code](https://t3.codes).",
        "",
    ]
    .join("\n")
}

// Git's own identity message runs several lines; say what to do instead.
// Otherwise its last line ("error: gpg failed to sign the data") says enough.
fn describe_commit_failure(stderr: &str) -> String {
    if COMMIT_IDENTITY_FAILURE.is_match(stderr) {
        return "Git has no name or email on this machine. Set user.name and user.email, then commit."
            .to_owned();
    }
    stderr
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .next_back()
        .unwrap_or("Git could not make the first commit.")
        .to_owned()
}

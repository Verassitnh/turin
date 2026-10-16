//! Port of upstream/apps/server/src/provider/CodexInstallation.ts.
//!
//! Owns the managed Codex runtime under `<base>/tools/codex`: resolve a local
//! or managed executable, and install an official release package. Network
//! and process validation sit behind traits so tests run on fixtures. Archive
//! extraction shells out to `tar`, as upstream does.

use std::{
    collections::HashMap,
    ffi::OsStr,
    ops::Deref,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant, UNIX_EPOCH},
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use tokio::{io::AsyncWriteExt, sync::watch, task::JoinHandle};
use turin_contracts::{
    TrimmedNonEmptyString,
    provider_instance::ProviderDriverKind,
    provider_setup::{
        ProviderInstallPhase, ProviderInstallSource, ProviderInstallState, SetupOperationId,
    },
};

use turin_provider_core::{
    command_path::resolve_command_path,
    host_process::{Platform, ProcessEnv},
    model_manifest::{ModelManifest, ModelManifestData, bundled_model_manifest},
    provider_compatibility::{
        ProviderCompatibilityPolicy, decode_provider_compatibility_policies,
        resolve_provider_compatibility,
    },
    server_provider::ServerProviderCompatibilityStatus,
};

const DRIVER_SLUG: &str = "codex";
const RECORD_MAX_BYTES: u64 = 8192;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const VERIFY_TIMEOUT: Duration = Duration::from_secs(90);
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(250);

const MSG_NOT_INSTALLED: &str = "Codex is not installed in T3 Code. Install it to continue.";
const MSG_INSTALL_FAILED: &str =
    "Could not install Codex. Check disk space and directory access, then try again.";
const MSG_RECORD_INVALID: &str =
    "The managed Codex installation record is invalid. Reinstall Codex.";
const MSG_UNPACK_FAILED: &str = "Could not unpack or start the downloaded Codex package.";
const MSG_INCOMPLETE: &str = "The managed Codex package is incomplete. Reinstall Codex.";
const MSG_MANIFEST_MISMATCH: &str =
    "The downloaded Codex package does not match the expected release.";
const MSG_WRONG_VERSION: &str = "The downloaded Codex executable has the wrong version.";
const MSG_UNSAFE_ARCHIVE: &str = "The Codex archive contains unsafe entries.";
const MSG_DOWNLOAD_CHECK: &str =
    "The Codex download failed its size or SHA-256 check. Nothing was installed.";
const MSG_DOWNLOAD_EXCEEDED: &str = "The Codex download exceeded the expected release size.";
const MSG_DOWNLOAD_NETWORK: &str =
    "Could not download Codex. Check your network connection, then try again.";
const MSG_RECORD_WRONG_VERSION: &str =
    "The managed Codex installation record has the wrong version.";
const MSG_EXISTING_DIFFERS: &str = "The existing managed Codex release differs from the official package. Remove it and reinstall.";
const MSG_REMOVE_BUSY: &str =
    "Stop Codex sessions and sign-in flows before removing its managed runtime.";
const MSG_REMOVE_CUSTOM_PATH: &str = "A provider instance uses a custom path inside managed Codex. Clear that path before removing it.";
const MSG_REMOVE_FAILED: &str =
    "Could not remove the managed Codex runtime. Check for open processes and try again.";
const MSG_NOT_CURRENT: &str =
    "This installation is no longer current. Refresh its status before cancelling.";
const MSG_CANCELLED: &str = "Installation cancelled. The previous runtime is unchanged.";
const MSG_INCOMPLETE_RUNTIME: &str =
    "The managed Codex runtime is incomplete. Remove it and reinstall.";
const MSG_UPDATE_REQUIRED: &str = "Update the managed Codex installation to continue.";

/// Official complete packages retain the code-mode, search, and resource companions.
const RELEASE_VERSION: &str = "0.156.1";

/// `${platform}-${arch}` (node naming), target triple, sha256, archive bytes.
const RELEASES: &[(&str, &str, &str, u64)] = &[
    (
        "darwin-arm64",
        "aarch64-apple-darwin",
        "fea42f9625091f011e38f059da974d52e57ba31831648bb1c7f0b1a385fde547",
        127_394_863,
    ),
    (
        "darwin-x64",
        "x86_64-apple-darwin",
        "618dbcd55419fa041871f777a14b107ceb3fe2d339ef81e21e6ab5374420dc71",
        138_670_455,
    ),
    (
        "linux-arm64",
        "aarch64-unknown-linux-musl",
        "fdd47ed6aade0360796fd3f6f95a45096f327c15e19e8c7339f9dc5633041786",
        136_933_361,
    ),
    (
        "linux-x64",
        "x86_64-unknown-linux-musl",
        "8b711520beddf385467b8da4d2c93736637c6ba1e46811cf0d8606b7c490b6f6",
        145_976_992,
    ),
    (
        "win32-arm64",
        "aarch64-pc-windows-msvc",
        "85994caecdc7609c49fd585c1cdb5677fa9d0acbf789650cff23a13afc9505db",
        142_037_952,
    ),
    (
        "win32-x64",
        "x86_64-pc-windows-msvc",
        "a2e017db9807e6a2269a26fea0e1d9546469cef4d472a33016bc9f3ad7d3b733",
        153_839_991,
    ),
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexReleaseAsset {
    pub version: String,
    pub target: String,
    pub url: String,
    pub sha256: String,
    pub archive_bytes: u64,
}

/// `resolveCodexReleaseAsset`. `platform` and `arch` use node naming
/// (`darwin`/`linux`/`win32`, `arm64`/`x64`).
pub fn resolve_codex_release_asset(platform: &str, arch: &str) -> Option<CodexReleaseAsset> {
    let key = format!("{platform}-{arch}");
    RELEASES
        .iter()
        .find(|(candidate, ..)| *candidate == key)
        .map(|(_, target, sha256, archive_bytes)| CodexReleaseAsset {
            version: RELEASE_VERSION.to_owned(),
            target: (*target).to_owned(),
            url: format!(
                "https://github.com/openai/codex/releases/download/rust-v{RELEASE_VERSION}/codex-package-{target}.tar.gz"
            ),
            sha256: (*sha256).to_owned(),
            archive_bytes: *archive_bytes,
        })
}

/// Node-style `(platform, arch)` for this host, as the release table expects.
pub fn host_platform_and_arch() -> (String, String) {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    (platform.to_owned(), arch.to_owned())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{detail}")]
pub struct CodexInstallationError {
    pub operation: &'static str,
    pub detail: String,
}

fn installation_error(
    operation: &'static str,
    detail: impl Into<String>,
) -> CodexInstallationError {
    CodexInstallationError {
        operation,
        detail: detail.into(),
    }
}

/// Any failure in the install pipeline that is not already a named error.
fn install_failure<E>(_: E) -> CodexInstallationError {
    installation_error("install", MSG_INSTALL_FAILED)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexExecutable {
    pub executable_path: PathBuf,
    pub source: ProviderInstallSource,
    pub version: String,
    pub managed_version_directory: Option<PathBuf>,
}

/// A compatibility advisory from the model manifest. `None` means no advisory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityAdvisory {
    pub supported: bool,
    pub message: Option<String>,
}

/// `compatibility` in `makeCodexInstallation`: the advisory for one Codex version.
/// `None` means no policy covers it, which leaves the version unsupported.
#[async_trait]
pub trait CodexCompatibility: Send + Sync {
    async fn advisory(&self, version: &str) -> Option<CompatibilityAdvisory>;
}

/// The model manifest as the installation reads it: always the current copy.
#[async_trait]
pub trait CodexManifestSource: Send + Sync {
    async fn current(&self) -> ModelManifestData;
}

#[async_trait]
impl CodexManifestSource for ModelManifest {
    async fn current(&self) -> ModelManifestData {
        ModelManifest::current(self).await
    }
}

/// `CodexInstallation.compatibility`: the manifest's policy first, then the
/// bundled one when the manifest has none that covers this T3 Code release. The
/// manifest is read on every call, so a policy change applies to a cached local runtime.
pub struct ManifestCodexCompatibility {
    manifest: Arc<dyn CodexManifestSource>,
    t3_code_version: String,
}

impl ManifestCodexCompatibility {
    pub fn new(manifest: Arc<dyn CodexManifestSource>, t3_code_version: impl Into<String>) -> Self {
        Self {
            manifest,
            t3_code_version: t3_code_version.into(),
        }
    }
}

fn decoded_policies(manifest: &ModelManifestData) -> Option<Vec<ProviderCompatibilityPolicy>> {
    manifest
        .compatibility
        .as_deref()
        .map(decode_provider_compatibility_policies)
}

#[async_trait]
impl CodexCompatibility for ManifestCodexCompatibility {
    async fn advisory(&self, version: &str) -> Option<CompatibilityAdvisory> {
        let manifest = self.manifest.current().await;
        let kind = driver();
        let resolve = |policies: Option<Vec<ProviderCompatibilityPolicy>>| {
            resolve_provider_compatibility(
                policies.as_deref(),
                &kind,
                Some(version),
                &self.t3_code_version,
            )
        };
        resolve(decoded_policies(&manifest))
            .or_else(|| resolve(decoded_policies(&bundled_model_manifest())))
            .map(|advisory| CompatibilityAdvisory {
                supported: advisory.status == ServerProviderCompatibilityStatus::Supported,
                message: advisory.message.map(|message| message.as_str().to_owned()),
            })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodexDownload {
    pub bytes: u64,
    pub sha256: String,
}

/// Streams a release archive to `destination`. Fails when the stream exceeds
/// `max_bytes`. `on_progress` receives the running byte total.
#[async_trait]
pub trait CodexReleaseDownloader: Send + Sync {
    async fn download(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<CodexDownload, CodexInstallationError>;
}

/// Confirms an unpacked executable starts and reports the expected version.
#[async_trait]
pub trait CodexRuntimeValidator: Send + Sync {
    async fn validate(
        &self,
        executable: &CodexExecutable,
        version: &str,
    ) -> Result<(), CodexInstallationError>;
}

/// Downloads with reqwest and checks the response status.
pub struct HttpReleaseDownloader {
    client: reqwest::Client,
}

impl HttpReleaseDownloader {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for HttpReleaseDownloader {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl CodexReleaseDownloader for HttpReleaseDownloader {
    async fn download(
        &self,
        url: &str,
        destination: &Path,
        max_bytes: u64,
        on_progress: &(dyn Fn(u64) + Send + Sync),
    ) -> Result<CodexDownload, CodexInstallationError> {
        use futures::StreamExt;

        let response = self
            .client
            .get(url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|_| installation_error("download", MSG_DOWNLOAD_NETWORK))?;
        let mut file = create_private_file(destination)
            .await
            .map(tokio::fs::File::from_std)
            .map_err(install_failure)?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| installation_error("download", MSG_DOWNLOAD_NETWORK))?;
            downloaded += chunk.len() as u64;
            if downloaded > max_bytes {
                return Err(installation_error("download", MSG_DOWNLOAD_EXCEEDED));
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await.map_err(install_failure)?;
            on_progress(downloaded);
        }
        file.flush().await.map_err(install_failure)?;
        Ok(CodexDownload {
            bytes: downloaded,
            sha256: format!("{:x}", hasher.finalize()),
        })
    }
}

/// Runs `<executable> --version` and compares it with the expected version.
pub struct CommandRuntimeValidator;

#[async_trait]
impl CodexRuntimeValidator for CommandRuntimeValidator {
    async fn validate(
        &self,
        executable: &CodexExecutable,
        version: &str,
    ) -> Result<(), CodexInstallationError> {
        let output = run_command(&executable.executable_path, ["--version"]).await?;
        if output.trim() != format!("codex-cli {version}") {
            return Err(installation_error("verify", MSG_WRONG_VERSION));
        }
        Ok(())
    }
}

/// Holds a resolved executable. Dropping the lease releases it for removal.
pub struct CodexLease {
    executable: CodexExecutable,
    leases: Arc<AtomicUsize>,
}

impl Deref for CodexLease {
    type Target = CodexExecutable;

    fn deref(&self) -> &CodexExecutable {
        &self.executable
    }
}

impl Drop for CodexLease {
    fn drop(&mut self) {
        self.leases.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct CodexInstallationOptions {
    pub base_dir: PathBuf,
    /// Node-style platform (`darwin`, `linux`, `win32`).
    pub platform: String,
    /// Node-style arch (`arm64`, `x64`).
    pub arch: String,
    pub environment: ProcessEnv,
    /// Overrides the release table when set.
    pub release_asset: Option<CodexReleaseAsset>,
    pub compatibility: Arc<dyn CodexCompatibility>,
    pub downloader: Arc<dyn CodexReleaseDownloader>,
    pub validator: Arc<dyn CodexRuntimeValidator>,
}

struct Running {
    operation_id: String,
    handle: JoinHandle<()>,
}

/// Records persisted under the managed directory.
#[derive(Debug, Serialize, Deserialize)]
struct ActiveRelease {
    version: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct InstalledRelease {
    version: String,
    target: String,
    sha256: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackageManifest {
    layout_version: u32,
    version: String,
    target: String,
    entrypoint: String,
}

pub struct CodexInstallation {
    managed_directory: PathBuf,
    platform: String,
    arch: String,
    environment: ProcessEnv,
    asset: Option<CodexReleaseAsset>,
    compatibility: Arc<dyn CodexCompatibility>,
    downloader: Arc<dyn CodexReleaseDownloader>,
    validator: Arc<dyn CodexRuntimeValidator>,
    gate: tokio::sync::Mutex<()>,
    leases: Arc<AtomicUsize>,
    running: parking_lot::Mutex<Option<Running>>,
    local_cache: parking_lot::Mutex<HashMap<PathBuf, (String, Option<CodexExecutable>)>>,
    state: watch::Sender<ProviderInstallState>,
}

fn driver() -> ProviderDriverKind {
    ProviderDriverKind::new(DRIVER_SLUG).expect("codex driver slug")
}

fn trimmed(value: impl AsRef<str>) -> Option<TrimmedNonEmptyString> {
    TrimmedNonEmptyString::new(value).ok()
}

fn is_running(state: &ProviderInstallState) -> bool {
    matches!(
        state.phase,
        ProviderInstallPhase::Downloading
            | ProviderInstallPhase::Extracting
            | ProviderInstallPhase::Verifying
    )
}

fn is_current(state: &ProviderInstallState, operation_id: &str) -> bool {
    state
        .operation_id
        .as_ref()
        .is_some_and(|id| id.as_str() == operation_id)
}

/// `^\d+\.\d+\.\d+(?:-[a-zA-Z0-9.-]+)?$`
pub fn is_valid_version(value: &str) -> bool {
    let (core, prerelease) = match value.split_once('-') {
        Some((core, prerelease)) => (core, Some(prerelease)),
        None => (value, None),
    };
    let parts: Vec<&str> = core.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        && prerelease.is_none_or(|tag| {
            !tag.is_empty()
                && tag
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        })
}

/// `^codex-cli (\d+\.\d+\.\d+)$` over the trimmed output.
fn version_from_cli_output(output: &str) -> Option<&str> {
    let version = output.trim().strip_prefix("codex-cli ")?;
    let core = version.split('.').collect::<Vec<_>>();
    (core.len() == 3
        && core
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit())))
    .then_some(version)
}

/// The unsafe-entry checks from `tar -tzf` / `tar -tvzf` output, as upstream.
pub fn check_archive_listing(
    entries: &[&str],
    types: &[&str],
) -> Result<(), CodexInstallationError> {
    let unsafe_entry = |entry: &str| {
        if entry.is_empty() || entry.contains('\\') || entry.starts_with('/') {
            return true;
        }
        let trimmed = entry.strip_suffix('/').unwrap_or(entry);
        trimmed
            .split('/')
            .any(|part| part == ".." || part == "." || part.is_empty() || part.contains(':'))
    };
    let bad_type = |line: &&str| !matches!(line.chars().next(), Some('-' | 'd'));
    if entries.len() > MAX_ARCHIVE_ENTRIES
        || entries.len() != types.len()
        || types.iter().any(bad_type)
        || entries.iter().any(|entry| unsafe_entry(entry))
    {
        return Err(installation_error("extract", MSG_UNSAFE_ARCHIVE));
    }
    Ok(())
}

fn platform_of(name: &str) -> Platform {
    match name {
        "darwin" => Platform::Darwin,
        "linux" => Platform::Linux,
        "win32" => Platform::Win32,
        _ => Platform::Other,
    }
}

fn has_execute_bit(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

/// `flag: "wx"` with mode 0600 on unix.
async fn create_private_file(path: &Path) -> std::io::Result<std::fs::File> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)
    })
    .await
    .map_err(std::io::Error::other)?
}

async fn write_private_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = create_private_file(path)
        .await
        .map(tokio::fs::File::from_std)?;
    file.write_all(bytes).await?;
    file.flush().await
}

/// Runs a command and returns its stdout. Non-zero exit is an install error.
async fn run_command<I, S>(
    program: impl AsRef<OsStr>,
    args: I,
) -> Result<String, CodexInstallationError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|_| installation_error("verify", MSG_UNPACK_FAILED))?;
    if !output.status.success() {
        return Err(installation_error("verify", MSG_UNPACK_FAILED));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Reads a small JSON record. A missing or oversize file is an invalid record.
async fn read_record<T: DeserializeOwned>(path: &Path) -> Result<T, CodexInstallationError> {
    let metadata = tokio::fs::metadata(path)
        .await
        .map_err(|_| installation_error("resolve", MSG_NOT_INSTALLED))?;
    if !metadata.is_file() || metadata.len() > RECORD_MAX_BYTES {
        return Err(installation_error("resolve", MSG_RECORD_INVALID));
    }
    let text = tokio::fs::read_to_string(path)
        .await
        .map_err(|_| installation_error("resolve", MSG_RECORD_INVALID))?;
    serde_json::from_str(&text).map_err(|_| installation_error("resolve", MSG_RECORD_INVALID))
}

impl CodexInstallation {
    /// `makeCodexInstallation` plus its startup reconciliation.
    pub async fn open(options: CodexInstallationOptions) -> Arc<Self> {
        let asset = options
            .release_asset
            .or_else(|| resolve_codex_release_asset(&options.platform, &options.arch));
        let (state, _) = watch::channel(ProviderInstallState {
            driver: driver(),
            operation_id: None,
            phase: ProviderInstallPhase::Idle,
            downloaded_bytes: 0,
            total_bytes: asset.as_ref().map(|asset| asset.archive_bytes),
            version: asset.as_ref().and_then(|asset| trimmed(&asset.version)),
            installed_version: None,
            executable_path: Some(None),
            source: None,
            can_remove: false,
            message: None,
        });
        let service = Arc::new(Self {
            managed_directory: options.base_dir.join("tools").join("codex"),
            platform: options.platform,
            arch: options.arch,
            environment: options.environment,
            asset,
            compatibility: options.compatibility,
            downloader: options.downloader,
            validator: options.validator,
            gate: tokio::sync::Mutex::new(()),
            leases: Arc::new(AtomicUsize::new(0)),
            running: parking_lot::Mutex::new(None),
            local_cache: parking_lot::Mutex::new(HashMap::new()),
            state,
        });
        service.initialize().await;
        service
    }

    pub fn managed_directory(&self) -> &Path {
        &self.managed_directory
    }

    pub fn state(&self) -> ProviderInstallState {
        self.state.borrow().clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<ProviderInstallState> {
        self.state.subscribe()
    }

    fn update(&self, change: impl FnOnce(&mut ProviderInstallState)) {
        self.state.send_modify(change);
    }

    fn active_path(&self) -> PathBuf {
        self.managed_directory.join("active.json")
    }

    fn executable_name(&self) -> &'static str {
        if self.platform == "win32" {
            "codex.exe"
        } else {
            "codex"
        }
    }

    fn exe_suffix(&self) -> &'static str {
        if self.platform == "win32" { ".exe" } else { "" }
    }

    async fn initialize(&self) {
        let can_remove = tokio::fs::try_exists(&self.managed_directory)
            .await
            .unwrap_or(false);
        self.update(|state| state.can_remove = can_remove);
        if self.reuse_local().await {
            return;
        }
        if !tokio::fs::try_exists(self.active_path())
            .await
            .unwrap_or(false)
        {
            return;
        }
        match self.resolve_active_installed().await {
            Ok(installed) => self.update(|state| {
                state.installed_version = trimmed(&installed.version);
                state.executable_path = Some(trimmed(installed.executable_path.to_string_lossy()));
                state.source = Some(Some(ProviderInstallSource::Managed));
            }),
            Err(_) => self.update(|state| {
                state.phase = ProviderInstallPhase::Failed;
                state.message = Some(MSG_INCOMPLETE_RUNTIME.to_owned());
            }),
        }
    }

    /// Resolves the managed release named by `active.json`, without the advisory check.
    async fn resolve_active_installed(&self) -> Result<CodexExecutable, CodexInstallationError> {
        let active: ActiveRelease = read_record(&self.active_path()).await?;
        self.completed_release(&active.version).await
    }

    /// `completedRelease`: the release directory named by `version`, if its record and files check out.
    async fn completed_release(
        &self,
        version: &str,
    ) -> Result<CodexExecutable, CodexInstallationError> {
        if !is_valid_version(version) {
            return Err(installation_error("resolve", MSG_RECORD_INVALID));
        }
        let directory = self.managed_directory.join(version);
        let record: InstalledRelease =
            read_record(&directory.join(".install-complete.json")).await?;
        if record.version != version {
            return Err(installation_error("resolve", MSG_RECORD_WRONG_VERSION));
        }
        self.executable_in_directory(&directory, version, &record.target)
            .await
    }

    /// `fromDirectory`: checks the package manifest and the required binaries.
    async fn executable_in_directory(
        &self,
        directory: &Path,
        version: &str,
        target: &str,
    ) -> Result<CodexExecutable, CodexInstallationError> {
        let manifest: PackageManifest = read_record(&directory.join("codex-package.json")).await?;
        if !is_valid_version(&manifest.version) || manifest.layout_version != 1 {
            return Err(installation_error("resolve", MSG_RECORD_INVALID));
        }
        if manifest.version != version
            || manifest.target != target
            || manifest.entrypoint != format!("bin/{}", self.executable_name())
        {
            return Err(installation_error("verify", MSG_MANIFEST_MISMATCH));
        }
        for name in [
            format!("bin/{}", self.executable_name()),
            format!("bin/codex-code-mode-host{}", self.exe_suffix()),
            format!("codex-path/rg{}", self.exe_suffix()),
        ] {
            let incomplete = || installation_error("verify", MSG_INCOMPLETE);
            let metadata = tokio::fs::metadata(directory.join(&name))
                .await
                .map_err(|_| incomplete())?;
            let executable_ok = self.platform == "win32" || has_execute_bit(&metadata);
            if !metadata.is_file() || metadata.len() == 0 || !executable_ok {
                return Err(incomplete());
            }
        }
        Ok(CodexExecutable {
            executable_path: directory.join("bin").join(self.executable_name()),
            source: ProviderInstallSource::Managed,
            version: version.to_owned(),
            managed_version_directory: Some(directory.to_path_buf()),
        })
    }

    /// `resolveManaged`: the active release, if the compatibility advisory allows it.
    async fn resolve_managed(&self) -> Result<CodexExecutable, CodexInstallationError> {
        if !tokio::fs::try_exists(self.active_path())
            .await
            .unwrap_or(false)
        {
            return Err(installation_error("resolve", MSG_NOT_INSTALLED));
        }
        let executable = self.resolve_active_installed().await?;
        match self.compatibility.advisory(&executable.version).await {
            Some(advisory) if advisory.supported => Ok(executable),
            advisory => Err(installation_error(
                "resolve",
                advisory
                    .and_then(|advisory| advisory.message)
                    .unwrap_or_else(|| MSG_UPDATE_REQUIRED.to_owned()),
            )),
        }
    }

    /// `resolveLocal`: a `codex` on PATH outside the managed directory, if it probes and is supported.
    async fn resolve_local(&self) -> Option<CodexExecutable> {
        let executable_path = PathBuf::from(resolve_command_path(
            "codex",
            &self.environment,
            platform_of(&self.platform),
        )?);
        // Keep launcher symlinks intact: version-manager shims dispatch by their invoked name.
        let real_path = tokio::fs::canonicalize(&executable_path).await.ok()?;
        let real_managed = tokio::fs::canonicalize(&self.managed_directory)
            .await
            .unwrap_or_else(|_| self.managed_directory.clone());
        // A PATH entry pointing into T3's download remains a managed installation.
        if real_path.starts_with(&real_managed) {
            return None;
        }
        let info = tokio::fs::metadata(&executable_path).await.ok()?;
        let modified = info
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |time| time.as_millis());
        let fingerprint = format!(
            "{}:{}:{}:{}",
            real_path.display(),
            info.len(),
            modified,
            inode(&info)
        );
        let cached = self.local_cache.lock().get(&executable_path).cloned();
        let executable = match cached {
            Some((cached_fingerprint, executable)) if cached_fingerprint == fingerprint => {
                executable
            }
            _ => {
                let probed = tokio::time::timeout(PROBE_TIMEOUT, probe_local(&executable_path))
                    .await
                    .ok()
                    .flatten();
                self.local_cache
                    .lock()
                    .insert(executable_path.clone(), (fingerprint, probed.clone()));
                probed
            }
        }?;
        // Cache the probe, but reclassify against the current manifest on every resolve.
        let supported = self
            .compatibility
            .advisory(&executable.version)
            .await
            .is_some_and(|advisory| advisory.supported);
        supported.then_some(executable)
    }

    /// `reuseLocal`: adopt a supported local `codex` as the installed runtime.
    async fn reuse_local(&self) -> bool {
        let Some(local) = self.resolve_local().await else {
            return false;
        };
        self.update(|state| {
            state.phase = ProviderInstallPhase::Succeeded;
            state.source = Some(Some(ProviderInstallSource::Local));
            state.operation_id = None;
            state.installed_version = trimmed(&local.version);
            state.executable_path = Some(trimmed(local.executable_path.to_string_lossy()));
            state.downloaded_bytes = 0;
            state.total_bytes = None;
            state.message = None;
        });
        true
    }

    /// `resolve`: a local runtime if one is usable, otherwise the managed one.
    pub async fn resolve(&self) -> Result<CodexExecutable, CodexInstallationError> {
        if let Some(local) = self.resolve_local().await {
            return Ok(local);
        }
        self.resolve_managed().await
    }

    /// `acquire`: a resolved executable held until the lease drops, which blocks removal.
    pub async fn acquire(&self) -> Result<CodexLease, CodexInstallationError> {
        let _gate = self.gate.lock().await;
        let executable = self.resolve().await?;
        self.leases.fetch_add(1, Ordering::SeqCst);
        Ok(CodexLease {
            executable,
            leases: Arc::clone(&self.leases),
        })
    }

    /// `start`: begin an install unless one is running or a local runtime is reusable.
    pub async fn start(self: &Arc<Self>) -> Result<ProviderInstallState, CodexInstallationError> {
        let _gate = self.gate.lock().await;
        let current = self.state();
        if is_running(&current) {
            return Ok(current);
        }
        if self.reuse_local().await {
            return Ok(self.state());
        }
        let asset = self.asset.clone().ok_or_else(|| {
            installation_error(
                "start",
                format!(
                    "OpenAI does not publish a Codex runtime for {}-{}. Use a supported remote environment or a custom executable.",
                    self.platform, self.arch
                ),
            )
        })?;
        let operation_id = uuid::Uuid::new_v4().to_string();
        let next = ProviderInstallState {
            driver: driver(),
            operation_id: SetupOperationId::new(&operation_id).ok(),
            phase: ProviderInstallPhase::Downloading,
            downloaded_bytes: 0,
            total_bytes: Some(asset.archive_bytes),
            version: trimmed(&asset.version),
            installed_version: current.installed_version.clone(),
            executable_path: None,
            source: None,
            can_remove: current.can_remove,
            message: Some("Downloading Codex.".to_owned()),
        };
        self.state.send_replace(next.clone());
        let service = Arc::clone(self);
        let task_operation = operation_id.clone();
        let handle = tokio::spawn(async move {
            let result = service.install(&asset).await;
            service.finish(&task_operation, result);
        });
        *self.running.lock() = Some(Running {
            operation_id,
            handle,
        });
        Ok(next)
    }

    /// Records a failed install on the current operation. Success and superseded operations are left alone.
    fn finish(&self, operation_id: &str, result: Result<(), CodexInstallationError>) {
        {
            let mut running = self.running.lock();
            if running
                .as_ref()
                .is_some_and(|running| running.operation_id == operation_id)
            {
                *running = None;
            }
        }
        if let Err(error) = result {
            self.update(|state| {
                if !is_current(state, operation_id)
                    || state.phase == ProviderInstallPhase::Succeeded
                {
                    return;
                }
                state.phase = ProviderInstallPhase::Failed;
                state.message = Some(error.detail);
            });
        }
    }

    /// `cancel`: aborts the running install for `operation_id`. The previous runtime is unchanged.
    pub async fn cancel(
        &self,
        operation_id: &str,
    ) -> Result<ProviderInstallState, CodexInstallationError> {
        let _gate = self.gate.lock().await;
        let current = self.state();
        if !is_current(&current, operation_id) {
            return Err(installation_error("cancel", MSG_NOT_CURRENT));
        }
        let running = {
            let mut slot = self.running.lock();
            let matches = slot.as_ref().is_some_and(|running| {
                running.operation_id == operation_id && is_running(&current)
            });
            if matches { slot.take() } else { None }
        };
        if let Some(running) = running {
            running.handle.abort();
            self.update(|state| {
                if is_current(state, operation_id) && state.phase != ProviderInstallPhase::Succeeded
                {
                    state.phase = ProviderInstallPhase::Cancelled;
                    state.message = Some(MSG_CANCELLED.to_owned());
                }
            });
        }
        Ok(self.state())
    }

    /// The download, unpack, verify, and activate pipeline behind `start`.
    async fn install(&self, release: &CodexReleaseAsset) -> Result<(), CodexInstallationError> {
        if !is_valid_version(&release.version) {
            return Err(installation_error("install", MSG_INSTALL_FAILED));
        }
        tokio::fs::create_dir_all(&self.managed_directory)
            .await
            .map_err(install_failure)?;
        self.update(|state| state.can_remove = true);
        let destination = self.managed_directory.join(&release.version);
        if tokio::fs::try_exists(&destination)
            .await
            .map_err(install_failure)?
        {
            let existing = self.completed_release(&release.version).await?;
            let record: InstalledRelease =
                read_record(&destination.join(".install-complete.json")).await?;
            if record.sha256 != release.sha256 || record.target != release.target {
                return Err(installation_error("verify", MSG_EXISTING_DIFFERS));
            }
            self.validate(&existing, &release.version).await?;
            return self.activate(&release.version).await;
        }

        let staging = tempfile::Builder::new()
            .prefix(".install-")
            .tempdir_in(&self.managed_directory)
            .map_err(install_failure)?;
        let archive = staging.path().join("download.tar.gz");
        let runtime = staging.path().join("runtime");
        tokio::fs::create_dir(&runtime)
            .await
            .map_err(install_failure)?;

        let last_progress = parking_lot::Mutex::new(Instant::now());
        let progress = |downloaded: u64| {
            let mut last = last_progress.lock();
            if last.elapsed() >= PROGRESS_INTERVAL || downloaded == release.archive_bytes {
                *last = Instant::now();
                self.update(|state| state.downloaded_bytes = downloaded);
            }
        };
        let download = tokio::time::timeout(
            DOWNLOAD_TIMEOUT,
            self.downloader
                .download(&release.url, &archive, release.archive_bytes, &progress),
        )
        .await
        .map_err(install_failure)??;
        if download.bytes != release.archive_bytes || download.sha256 != release.sha256 {
            return Err(installation_error("download", MSG_DOWNLOAD_CHECK));
        }

        self.update(|state| {
            state.phase = ProviderInstallPhase::Extracting;
            state.message = Some("Extracting Codex.".to_owned());
        });
        let entries_output = run_command("tar", [OsStr::new("-tzf"), archive.as_os_str()]).await?;
        let types_output = run_command("tar", [OsStr::new("-tvzf"), archive.as_os_str()]).await?;
        let entries: Vec<&str> = entries_output.trim().split('\n').collect();
        let types: Vec<&str> = types_output.trim().split('\n').collect();
        check_archive_listing(&entries, &types)?;
        run_command(
            "tar",
            [
                OsStr::new("-xzf"),
                archive.as_os_str(),
                OsStr::new("-C"),
                runtime.as_os_str(),
            ],
        )
        .await?;

        self.update(|state| {
            state.phase = ProviderInstallPhase::Verifying;
            state.message = Some("Checking Codex.".to_owned());
        });
        let executable = self
            .executable_in_directory(&runtime, &release.version, &release.target)
            .await?;
        self.validate(&executable, &release.version).await?;
        let record = serde_json::to_vec(&InstalledRelease {
            version: release.version.clone(),
            target: release.target.clone(),
            sha256: release.sha256.clone(),
        })
        .map_err(install_failure)?;
        write_private_file(&runtime.join(".install-complete.json"), &record)
            .await
            .map_err(install_failure)?;
        tokio::fs::rename(&runtime, &destination)
            .await
            .map_err(install_failure)?;
        self.activate(&release.version).await
    }

    /// Runs the validator under its timeout. A timeout is an install failure.
    async fn validate(
        &self,
        executable: &CodexExecutable,
        version: &str,
    ) -> Result<(), CodexInstallationError> {
        tokio::time::timeout(VERIFY_TIMEOUT, self.validator.validate(executable, version))
            .await
            .map_err(install_failure)?
    }

    /// Writes `active.json` atomically and marks the installation succeeded.
    async fn activate(&self, version: &str) -> Result<(), CodexInstallationError> {
        let executable = self.completed_release(version).await?;
        let temporary = tempfile::Builder::new()
            .prefix(".active-")
            .tempdir_in(&self.managed_directory)
            .map_err(install_failure)?;
        let pointer = temporary.path().join("active.json");
        let bytes = serde_json::to_vec(&ActiveRelease {
            version: version.to_owned(),
        })
        .map_err(install_failure)?;
        write_private_file(&pointer, &bytes)
            .await
            .map_err(install_failure)?;
        tokio::fs::rename(&pointer, self.active_path())
            .await
            .map_err(install_failure)?;
        self.update(|state| {
            state.phase = ProviderInstallPhase::Succeeded;
            state.source = Some(Some(ProviderInstallSource::Managed));
            state.installed_version = trimmed(version);
            state.executable_path = Some(trimmed(executable.executable_path.to_string_lossy()));
            state.message = None;
        });
        Ok(())
    }

    /// `remove`: deletes the managed runtime unless it is running or leased.
    pub async fn remove(
        &self,
        protected_binary_paths: &[PathBuf],
    ) -> Result<(), CodexInstallationError> {
        let _gate = self.gate.lock().await;
        if is_running(&self.state()) || self.leases.load(Ordering::SeqCst) > 0 {
            return Err(installation_error("remove", MSG_REMOVE_BUSY));
        }
        if let Ok(real_managed) = tokio::fs::canonicalize(&self.managed_directory).await {
            for binary in protected_binary_paths {
                let candidate = tokio::fs::canonicalize(binary)
                    .await
                    .unwrap_or_else(|_| std::path::absolute(binary).unwrap_or(binary.clone()));
                if candidate != real_managed && candidate.starts_with(&real_managed) {
                    return Err(installation_error("remove", MSG_REMOVE_CUSTOM_PATH));
                }
            }
        }
        if tokio::fs::try_exists(&self.managed_directory)
            .await
            .map_err(|_| installation_error("remove", MSG_REMOVE_FAILED))?
        {
            tokio::fs::remove_dir_all(&self.managed_directory)
                .await
                .map_err(|_| installation_error("remove", MSG_REMOVE_FAILED))?;
        }
        self.update(|state| {
            state.operation_id = None;
            state.phase = ProviderInstallPhase::Idle;
            state.downloaded_bytes = 0;
            state.installed_version = None;
            state.executable_path = Some(None);
            state.source = Some(None);
            state.can_remove = false;
            state.message = None;
        });
        self.reuse_local().await;
        Ok(())
    }
}

#[cfg(unix)]
fn inode(info: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    info.ino()
}

#[cfg(not(unix))]
fn inode(_: &std::fs::Metadata) -> u64 {
    0
}

/// Probes a `codex` binary: `--version` must print `codex-cli X.Y.Z`, and `app-server --help` must succeed.
async fn probe_local(path: &Path) -> Option<CodexExecutable> {
    let output = run_command(path, ["--version"]).await.ok()?;
    let version = version_from_cli_output(&output)?.to_owned();
    run_command(path, ["app-server", "--help"]).await.ok()?;
    Some(CodexExecutable {
        executable_path: path.to_path_buf(),
        source: ProviderInstallSource::Local,
        version,
        managed_version_directory: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    struct Supported;
    #[async_trait]
    impl CodexCompatibility for Supported {
        async fn advisory(&self, _: &str) -> Option<CompatibilityAdvisory> {
            Some(CompatibilityAdvisory {
                supported: true,
                message: None,
            })
        }
    }

    struct NoAdvisory;
    #[async_trait]
    impl CodexCompatibility for NoAdvisory {
        async fn advisory(&self, _: &str) -> Option<CompatibilityAdvisory> {
            None
        }
    }

    struct AcceptValidator;
    #[async_trait]
    impl CodexRuntimeValidator for AcceptValidator {
        async fn validate(
            &self,
            _: &CodexExecutable,
            _: &str,
        ) -> Result<(), CodexInstallationError> {
            Ok(())
        }
    }

    /// Copies a fixture archive into place, as the network would.
    struct FixtureDownloader {
        archive: PathBuf,
    }
    #[async_trait]
    impl CodexReleaseDownloader for FixtureDownloader {
        async fn download(
            &self,
            _: &str,
            destination: &Path,
            max_bytes: u64,
            on_progress: &(dyn Fn(u64) + Send + Sync),
        ) -> Result<CodexDownload, CodexInstallationError> {
            let bytes = tokio::fs::read(&self.archive)
                .await
                .map_err(install_failure)?;
            if bytes.len() as u64 > max_bytes {
                return Err(installation_error("download", MSG_DOWNLOAD_EXCEEDED));
            }
            tokio::fs::write(destination, &bytes)
                .await
                .map_err(install_failure)?;
            on_progress(bytes.len() as u64);
            Ok(CodexDownload {
                bytes: bytes.len() as u64,
                sha256: format!("{:x}", Sha256::digest(&bytes)),
            })
        }
    }

    /// Never completes, so a test can cancel mid-download.
    struct StalledDownloader;
    #[async_trait]
    impl CodexReleaseDownloader for StalledDownloader {
        async fn download(
            &self,
            _: &str,
            _: &Path,
            _: u64,
            _: &(dyn Fn(u64) + Send + Sync),
        ) -> Result<CodexDownload, CodexInstallationError> {
            futures::future::pending().await
        }
    }

    const TARGET: &str = "x86_64-unknown-linux-musl";

    /// A package with the required files and manifest, packed as `tar -czf`.
    /// Returns the archive path, its sha256, and its size.
    fn make_fixture(root: &Path) -> (PathBuf, String, u64) {
        let package = root.join("package");
        for dir in ["bin", "codex-path"] {
            std::fs::create_dir_all(package.join(dir)).unwrap();
        }
        for (file, contents) in [
            ("bin/codex", &b"#!/bin/sh\nexit 0\n"[..]),
            ("bin/codex-code-mode-host", &b"host"[..]),
            ("codex-path/rg", &b"rg"[..]),
        ] {
            let path = package.join(file);
            std::fs::write(&path, contents).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let manifest = serde_json::json!({
            "layoutVersion": 1,
            "version": RELEASE_VERSION,
            "target": TARGET,
            "entrypoint": "bin/codex",
        });
        std::fs::write(package.join("codex-package.json"), manifest.to_string()).unwrap();

        let downloads = root.join("downloads");
        std::fs::create_dir_all(&downloads).unwrap();
        let archive = downloads.join("codex-package.tar.gz");
        let status = Command::new("tar")
            .arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&package)
            .args(["bin", "codex-path", "codex-package.json"])
            .status()
            .unwrap();
        assert!(status.success());
        let bytes = std::fs::read(&archive).unwrap();
        (
            archive,
            format!("{:x}", Sha256::digest(&bytes)),
            bytes.len() as u64,
        )
    }

    fn options(
        base: &Path,
        asset: CodexReleaseAsset,
        downloader: Arc<dyn CodexReleaseDownloader>,
        compatibility: Arc<dyn CodexCompatibility>,
    ) -> CodexInstallationOptions {
        CodexInstallationOptions {
            base_dir: base.to_path_buf(),
            platform: "linux".to_owned(),
            arch: "x64".to_owned(),
            // No PATH, so a developer's real `codex` cannot leak into the resolve.
            environment: ProcessEnv::new(),
            release_asset: Some(asset),
            compatibility,
            downloader,
            validator: Arc::new(AcceptValidator),
        }
    }

    fn asset(sha256: &str, archive_bytes: u64) -> CodexReleaseAsset {
        CodexReleaseAsset {
            version: RELEASE_VERSION.to_owned(),
            target: TARGET.to_owned(),
            url: "https://example.invalid/codex-package.tar.gz".to_owned(),
            sha256: sha256.to_owned(),
            archive_bytes,
        }
    }

    async fn settled(service: &CodexInstallation) -> ProviderInstallState {
        let mut receiver = service.subscribe();
        receiver
            .wait_for(|state| {
                matches!(
                    state.phase,
                    ProviderInstallPhase::Succeeded
                        | ProviderInstallPhase::Failed
                        | ProviderInstallPhase::Cancelled
                )
            })
            .await
            .expect("installation sender alive")
            .clone()
    }

    #[test]
    fn release_table_resolves_by_node_platform_and_arch() {
        let asset = resolve_codex_release_asset("darwin", "arm64").expect("darwin arm64");
        assert_eq!(asset.target, "aarch64-apple-darwin");
        assert_eq!(asset.archive_bytes, 127_394_863);
        assert!(
            asset
                .url
                .ends_with("/codex-package-aarch64-apple-darwin.tar.gz")
        );
        let windows = resolve_codex_release_asset("win32", "x64").expect("win32 x64");
        assert_eq!(windows.target, "x86_64-pc-windows-msvc");
        assert!(resolve_codex_release_asset("freebsd", "x64").is_none());
    }

    #[test]
    fn version_pattern_matches_upstream_schema() {
        assert!(is_valid_version("0.156.1"));
        assert!(is_valid_version("1.0.0-rc.1"));
        assert!(!is_valid_version("1.0"));
        assert!(!is_valid_version("v1.0.0"));
        assert!(!is_valid_version("1.0.0-"));
    }

    #[test]
    fn cli_version_output_must_be_exact() {
        assert_eq!(
            version_from_cli_output("codex-cli 0.156.1\n"),
            Some("0.156.1")
        );
        assert_eq!(version_from_cli_output("codex 0.156.1"), None);
        assert_eq!(version_from_cli_output("codex-cli 0.156.1-beta"), None);
    }

    #[test]
    fn archive_listing_rejects_unsafe_entries() {
        assert!(check_archive_listing(&["bin/", "bin/codex"], &["d", "-"]).is_ok());
        for entries in [
            ["../evil", "bin/codex"],
            ["/abs", "bin/codex"],
            ["a\\b", "x"],
            ["bin/./x", "y"],
        ] {
            assert!(
                check_archive_listing(&entries, &["-", "-"]).is_err(),
                "{entries:?}"
            );
        }
        assert!(check_archive_listing(&["bin/codex"], &["l"]).is_err());
        assert!(check_archive_listing(&["a", "b"], &["-"]).is_err());
    }

    #[tokio::test]
    async fn install_activates_a_verified_package() {
        let temp = tempfile::tempdir().unwrap();
        let (archive, sha256, bytes) = make_fixture(temp.path());
        let base = temp.path().join("base");
        let service = CodexInstallation::open(options(
            &base,
            asset(&sha256, bytes),
            Arc::new(FixtureDownloader { archive }),
            Arc::new(Supported),
        ))
        .await;

        let started = service.start().await.expect("start");
        assert_eq!(started.phase, ProviderInstallPhase::Downloading);
        let done = settled(&service).await;

        assert_eq!(done.phase, ProviderInstallPhase::Succeeded);
        assert_eq!(done.source, Some(Some(ProviderInstallSource::Managed)));
        assert_eq!(
            done.installed_version.as_ref().map(|v| v.as_str()),
            Some(RELEASE_VERSION)
        );
        assert!(base.join("tools/codex/active.json").is_file());

        let resolved = service.resolve().await.expect("managed runtime resolves");
        assert_eq!(resolved.source, ProviderInstallSource::Managed);
        assert!(resolved.executable_path.ends_with("bin/codex"));
    }

    #[tokio::test]
    async fn checksum_mismatch_installs_nothing() {
        let temp = tempfile::tempdir().unwrap();
        let (archive, _, bytes) = make_fixture(temp.path());
        let base = temp.path().join("base");
        let service = CodexInstallation::open(options(
            &base,
            asset(&"0".repeat(64), bytes),
            Arc::new(FixtureDownloader { archive }),
            Arc::new(Supported),
        ))
        .await;

        service.start().await.expect("start");
        let done = settled(&service).await;

        assert_eq!(done.phase, ProviderInstallPhase::Failed);
        assert_eq!(
            done.message.as_deref(),
            Some("The Codex download failed its size or SHA-256 check. Nothing was installed.")
        );
        assert!(!base.join("tools/codex/active.json").exists());
        assert!(!base.join("tools/codex").join(RELEASE_VERSION).exists());
    }

    #[tokio::test]
    async fn cancel_mid_download_reports_cancelled_and_keeps_prior_state() {
        let temp = tempfile::tempdir().unwrap();
        let base = temp.path().join("base");
        let service = CodexInstallation::open(options(
            &base,
            asset(&"0".repeat(64), 1),
            Arc::new(StalledDownloader),
            Arc::new(Supported),
        ))
        .await;

        let started = service.start().await.expect("start");
        let operation_id = started
            .operation_id
            .expect("operation id")
            .as_str()
            .to_owned();
        let cancelled = service.cancel(&operation_id).await.expect("cancel");

        assert_eq!(cancelled.phase, ProviderInstallPhase::Cancelled);
        assert_eq!(
            cancelled.message.as_deref(),
            Some("Installation cancelled. The previous runtime is unchanged.")
        );
    }

    #[tokio::test]
    async fn resolve_without_an_install_says_not_installed() {
        let temp = tempfile::tempdir().unwrap();
        let service = CodexInstallation::open(options(
            &temp.path().join("base"),
            asset(&"0".repeat(64), 1),
            Arc::new(StalledDownloader),
            Arc::new(Supported),
        ))
        .await;

        let error = service.resolve().await.expect_err("nothing installed");
        assert_eq!(error.detail, MSG_NOT_INSTALLED);
    }

    #[tokio::test]
    async fn unsupported_advisory_blocks_a_managed_runtime() {
        let temp = tempfile::tempdir().unwrap();
        let (archive, sha256, bytes) = make_fixture(temp.path());
        let base = temp.path().join("base");
        let installed = CodexInstallation::open(options(
            &base,
            asset(&sha256, bytes),
            Arc::new(FixtureDownloader {
                archive: archive.clone(),
            }),
            Arc::new(Supported),
        ))
        .await;
        installed.start().await.expect("start");
        settled(&installed).await;

        let blocked = CodexInstallation::open(options(
            &base,
            asset(&sha256, bytes),
            Arc::new(FixtureDownloader { archive }),
            Arc::new(NoAdvisory),
        ))
        .await;
        let error = blocked
            .resolve()
            .await
            .expect_err("no advisory means unsupported");
        assert_eq!(error.detail, MSG_UPDATE_REQUIRED);
    }

    #[tokio::test]
    async fn remove_clears_the_managed_directory() {
        let temp = tempfile::tempdir().unwrap();
        let (archive, sha256, bytes) = make_fixture(temp.path());
        let base = temp.path().join("base");
        let service = CodexInstallation::open(options(
            &base,
            asset(&sha256, bytes),
            Arc::new(FixtureDownloader { archive }),
            Arc::new(Supported),
        ))
        .await;
        service.start().await.expect("start");
        settled(&service).await;

        service.remove(&[]).await.expect("remove");

        assert!(!base.join("tools/codex").exists());
        let state = service.state();
        assert_eq!(state.phase, ProviderInstallPhase::Idle);
        assert!(!state.can_remove);
    }

    #[test]
    fn complete_packages_exist_for_every_supported_platform() {
        for platform in ["darwin", "linux", "win32"] {
            for arch in ["x64", "arm64"] {
                let asset = resolve_codex_release_asset(platform, arch).expect("asset");
                assert!(asset.url.contains("codex-package-"), "{platform} {arch}");
            }
        }
        assert!(resolve_codex_release_asset("linux", "riscv64").is_none());
    }

    #[cfg(unix)]
    mod with_local_cli {
        use super::*;
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// The T3 Code release the bundled Codex policy is read against.
        const T3_CODE_VERSION: &str = "0.0.44";

        struct ManifestCell(parking_lot::Mutex<ModelManifestData>);

        #[async_trait]
        impl CodexManifestSource for ManifestCell {
            async fn current(&self) -> ModelManifestData {
                self.0.lock().clone()
            }
        }

        struct CountingDownloader {
            inner: FixtureDownloader,
            count: Arc<AtomicUsize>,
        }

        #[async_trait]
        impl CodexReleaseDownloader for CountingDownloader {
            async fn download(
                &self,
                url: &str,
                destination: &Path,
                max_bytes: u64,
                on_progress: &(dyn Fn(u64) + Send + Sync),
            ) -> Result<CodexDownload, CodexInstallationError> {
                self.count.fetch_add(1, Ordering::SeqCst);
                self.inner
                    .download(url, destination, max_bytes, on_progress)
                    .await
            }
        }

        #[derive(Clone, Copy, Default)]
        struct Local {
            version: &'static str,
            app_server_fails: bool,
            version_fails: bool,
        }

        struct Harness {
            _temp: tempfile::TempDir,
            base: PathBuf,
            local_dir: PathBuf,
            local_bin: PathBuf,
            probe_log: PathBuf,
            downloads: Arc<AtomicUsize>,
            manifest: Arc<ManifestCell>,
            archive: PathBuf,
            sha256: String,
            bytes: u64,
            has_local: bool,
        }

        fn write_executable(path: &Path, contents: &str) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(path, contents).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        fn harness(local: Option<Local>) -> Harness {
            let temp = tempfile::tempdir().unwrap();
            let (archive, sha256, bytes) = make_fixture(temp.path());
            let base = temp.path().join("base");
            let local_dir = base.join("local");
            let local_bin = local_dir.join("codex");
            let probe_log = base.join("local-probes.txt");
            if let Some(local) = local {
                std::fs::create_dir_all(&local_dir).unwrap();
                let version = if local.version_fails {
                    "exit 1".to_owned()
                } else {
                    format!("printf '%s\\n' 'codex-cli {}'", local.version)
                };
                let app_server = if local.app_server_fails { 1 } else { 0 };
                write_executable(
                    &local_bin,
                    &format!(
                        "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\ncase \"$1\" in\n--version) {version};;\napp-server) exit {app_server};;\nesac\n",
                        probe_log.display()
                    ),
                );
            }
            Harness {
                base,
                local_dir,
                local_bin,
                probe_log,
                downloads: Arc::new(AtomicUsize::new(0)),
                manifest: Arc::new(ManifestCell(parking_lot::Mutex::new(
                    bundled_model_manifest(),
                ))),
                archive,
                sha256,
                bytes,
                has_local: local.is_some(),
                _temp: temp,
            }
        }

        impl Harness {
            async fn open_with(&self, asset: CodexReleaseAsset) -> Arc<CodexInstallation> {
                let mut options = options(
                    &self.base,
                    asset,
                    Arc::new(CountingDownloader {
                        inner: FixtureDownloader {
                            archive: self.archive.clone(),
                        },
                        count: Arc::clone(&self.downloads),
                    }),
                    Arc::new(ManifestCodexCompatibility::new(
                        self.manifest.clone(),
                        T3_CODE_VERSION,
                    )),
                );
                if self.has_local {
                    options.environment = ProcessEnv::from([(
                        "PATH".to_owned(),
                        self.local_dir.to_string_lossy().into_owned(),
                    )]);
                }
                CodexInstallation::open(options).await
            }

            async fn open(&self) -> Arc<CodexInstallation> {
                self.open_with(asset(&self.sha256, self.bytes)).await
            }

            fn downloads(&self) -> usize {
                self.downloads.load(Ordering::SeqCst)
            }

            fn probes(&self) -> Vec<String> {
                std::fs::read_to_string(&self.probe_log)
                    .unwrap()
                    .trim()
                    .lines()
                    .map(str::to_owned)
                    .collect()
            }
        }

        #[tokio::test]
        async fn reuses_installed_codex_without_downloading_or_taking_ownership() {
            for version in ["0.156.0", "0.156.1", "0.156.2", "0.157.0"] {
                let h = harness(Some(Local {
                    version,
                    ..Local::default()
                }));
                let service = h.open().await;
                let started = service.start().await.unwrap();
                assert_eq!(started.source, Some(Some(ProviderInstallSource::Local)));
                assert_eq!(started.phase, ProviderInstallPhase::Succeeded);
                assert_eq!(
                    started.installed_version.as_ref().map(|v| v.as_str()),
                    Some(version)
                );
                assert_eq!(
                    started.executable_path,
                    Some(trimmed(h.local_bin.to_string_lossy()))
                );
                assert!(!started.can_remove);
                let resolved = service.resolve().await.unwrap();
                assert_eq!(resolved.source, ProviderInstallSource::Local);
                assert_eq!(resolved.executable_path, h.local_bin);
                assert_eq!(resolved.managed_version_directory, None);
                assert_eq!(resolved.version, version);
                drop(service.acquire().await.unwrap());
                assert_eq!(h.downloads(), 0);
                assert!(!service.managed_directory().exists());
                assert_eq!(h.probes(), ["--version", "app-server --help"]);
                service.remove(&[]).await.unwrap();
                assert!(h.local_bin.exists());
                assert_eq!(
                    service.state().source,
                    Some(Some(ProviderInstallSource::Local))
                );
            }
        }

        #[tokio::test]
        async fn downloads_the_pinned_release_when_the_local_cli_is_unsupported_or_broken() {
            let cases = [
                Local {
                    version: "0.128.9",
                    ..Local::default()
                },
                Local {
                    version: "0.145.0",
                    ..Local::default()
                },
                Local {
                    version: "0.155.1",
                    ..Local::default()
                },
                Local {
                    version: "0.155.9",
                    ..Local::default()
                },
                Local {
                    version: "0.156.1-alpha.1",
                    ..Local::default()
                },
                Local {
                    version: "unknown",
                    ..Local::default()
                },
                Local {
                    version: "0.156.1",
                    app_server_fails: true,
                    ..Local::default()
                },
                Local {
                    version: "0.156.1",
                    version_fails: true,
                    ..Local::default()
                },
            ];
            for local in cases {
                let label = format!(
                    "{} appServerFails={} versionFails={}",
                    local.version, local.app_server_fails, local.version_fails
                );
                let h = harness(Some(local));
                let service = h.open().await;
                assert_eq!(service.state().installed_version, None, "{label}");
                service.start().await.unwrap();
                let installed = settled(&service).await;
                assert_eq!(installed.phase, ProviderInstallPhase::Succeeded, "{label}");
                let executable = service.resolve().await.unwrap();
                assert_eq!(executable.source, ProviderInstallSource::Managed, "{label}");
                assert_eq!(
                    installed.executable_path,
                    Some(trimmed(executable.executable_path.to_string_lossy())),
                    "{label}"
                );
                assert_eq!(h.downloads(), 1, "{label}");
                assert!(h.local_bin.exists(), "{label}");
            }
        }

        #[tokio::test]
        async fn falls_back_to_a_managed_download_when_the_reused_local_executable_disappears() {
            let h = harness(Some(Local {
                version: "0.156.1",
                ..Local::default()
            }));
            let service = h.open().await;
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Local
            );
            std::fs::remove_file(&h.local_bin).unwrap();
            service.start().await.unwrap();
            assert_eq!(
                settled(&service).await.phase,
                ProviderInstallPhase::Succeeded
            );
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Managed
            );
            assert_eq!(h.downloads(), 1);
        }

        #[tokio::test]
        async fn rechecks_compatibility_after_the_local_executable_is_replaced() {
            let h = harness(Some(Local {
                version: "0.156.1",
                ..Local::default()
            }));
            let service = h.open().await;
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Local
            );
            std::fs::remove_file(&h.local_bin).unwrap();
            write_executable(&h.local_bin, "#!/bin/sh\nprintf 'codex-cli 0.128.9\\n'\n");
            service.start().await.unwrap();
            assert_eq!(
                settled(&service).await.phase,
                ProviderInstallPhase::Succeeded
            );
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Managed
            );
            assert_eq!(h.downloads(), 1);
        }

        #[tokio::test]
        async fn rechecks_a_cached_local_executable_when_the_shared_manifest_policy_changes() {
            let h = harness(Some(Local {
                version: "0.156.0",
                ..Local::default()
            }));
            let service = h.open().await;
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Local
            );
            h.manifest.0.lock().compatibility = Some(vec![serde_json::json!({
                "driver": "codex",
                "t3CodeRange": ">=0.0.42",
                "ranges": [
                    {"range": ">=0.156.1", "status": "supported"},
                    {"range": "<0.156.1", "status": "broken"},
                ],
            })]);
            service.start().await.unwrap();
            assert_eq!(
                settled(&service).await.phase,
                ProviderInstallPhase::Succeeded
            );
            assert_eq!(
                service.resolve().await.unwrap().source,
                ProviderInstallSource::Managed
            );
            assert_eq!(h.downloads(), 1);
            // The cached probe is reused: the policy changed, the executable did not.
            assert_eq!(h.probes(), ["--version", "app-server --help"]);
        }

        #[tokio::test]
        async fn uses_bundled_compatibility_when_the_remote_manifest_omits_its_policy() {
            let h = harness(Some(Local {
                version: "0.156.0",
                ..Local::default()
            }));
            h.manifest.0.lock().compatibility = Some(Vec::new());
            let service = h.open().await;
            let started = service.start().await.unwrap();
            assert_eq!(started.source, Some(Some(ProviderInstallSource::Local)));
            assert_eq!(h.downloads(), 0);
        }

        #[tokio::test]
        async fn preserves_the_invoked_name_of_version_manager_launcher_symlinks() {
            let h = harness(Some(Local {
                version: "0.156.1",
                ..Local::default()
            }));
            let launcher = h.base.join("launcher");
            write_executable(
                &launcher,
                "#!/bin/sh\ncase \"$0\" in */codex) ;; *) exit 1;; esac\ncase \"$1\" in --version) printf \"codex-cli 0.156.1\\n\";; app-server) exit 0;; esac\n",
            );
            std::fs::remove_file(&h.local_bin).unwrap();
            std::os::unix::fs::symlink(&launcher, &h.local_bin).unwrap();
            let service = h.open().await;
            let started = service.start().await.unwrap();
            assert_eq!(started.source, Some(Some(ProviderInstallSource::Local)));
            assert_eq!(started.phase, ProviderInstallPhase::Succeeded);
            assert_eq!(
                service.resolve().await.unwrap().executable_path,
                h.local_bin
            );
            assert_eq!(h.downloads(), 0);
        }

        #[tokio::test]
        async fn installs_the_complete_verified_package_and_survives_a_restart() {
            let h = harness(None);
            let service = h.open().await;
            service.start().await.unwrap();
            assert_eq!(
                settled(&service).await.phase,
                ProviderInstallPhase::Succeeded
            );
            let executable = service.resolve().await.unwrap();
            assert_eq!(
                executable.executable_path,
                h.base.join("tools/codex/0.156.1/bin/codex")
            );
            let directory = executable.managed_version_directory.unwrap();
            assert_eq!(
                std::fs::read_to_string(directory.join("bin/codex-code-mode-host")).unwrap(),
                "host"
            );
            assert_eq!(
                std::fs::read_to_string(directory.join("codex-path/rg")).unwrap(),
                "rg"
            );
            let restarted = h.open().await;
            assert_eq!(restarted.resolve().await.unwrap().version, "0.156.1");
        }

        #[tokio::test]
        async fn requires_an_update_before_running_an_older_activated_managed_installation() {
            let h = harness(None);
            let first = h.open().await;
            first.start().await.unwrap();
            settled(&first).await;
            let executable = first.resolve().await.unwrap();
            let old_directory = first.managed_directory().join("0.155.1");
            std::fs::rename(
                executable.managed_version_directory.unwrap(),
                &old_directory,
            )
            .unwrap();
            for name in ["codex-package.json", ".install-complete.json"] {
                let file = old_directory.join(name);
                let text = std::fs::read_to_string(&file).unwrap();
                std::fs::write(&file, text.replace("0.156.1", "0.155.1")).unwrap();
            }
            std::fs::write(
                first.managed_directory().join("active.json"),
                r#"{"version":"0.155.1"}"#,
            )
            .unwrap();
            let downloads_before = h.downloads();
            let restarted = h.open().await;
            let error = restarted.resolve().await.expect_err("older release");
            assert!(
                error.detail.contains("outside the supported range"),
                "{}",
                error.detail
            );
            restarted.start().await.unwrap();
            assert_eq!(
                settled(&restarted).await.phase,
                ProviderInstallPhase::Succeeded
            );
            assert_eq!(restarted.resolve().await.unwrap().version, "0.156.1");
            assert_eq!(h.downloads() - downloads_before, 1);
        }

        #[tokio::test]
        async fn protects_active_process_leases_and_removes_the_runtime_after_release() {
            let h = harness(None);
            let service = h.open().await;
            service.start().await.unwrap();
            settled(&service).await;
            let lease = service.acquire().await.unwrap();
            let error = service.remove(&[]).await.expect_err("leased");
            assert!(
                error.detail.contains("Stop Codex sessions"),
                "{}",
                error.detail
            );
            drop(lease);
            service.remove(&[]).await.unwrap();
            assert!(!service.managed_directory().exists());
            let state = service.state();
            assert_eq!(state.installed_version, None);
            assert_eq!(state.executable_path, Some(None));
        }

        #[tokio::test]
        async fn keeps_an_activated_runtime_when_a_later_update_fails_verification() {
            let h = harness(None);
            let first = h.open().await;
            first.start().await.unwrap();
            settled(&first).await;
            let update = h
                .open_with(CodexReleaseAsset {
                    version: "0.156.2".to_owned(),
                    ..asset(&"0".repeat(64), h.bytes)
                })
                .await;
            assert_eq!(
                update.state().executable_path,
                Some(trimmed(
                    first
                        .resolve()
                        .await
                        .unwrap()
                        .executable_path
                        .to_string_lossy()
                ))
            );
            update.start().await.unwrap();
            assert_eq!(settled(&update).await.phase, ProviderInstallPhase::Failed);
            assert_eq!(update.resolve().await.unwrap().version, "0.156.1");
        }
    }
}
